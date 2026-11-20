use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    INBOX_DIRECTORY, Message, Registry, RegistryError, set_private_directory_permissions,
    set_private_file_permissions, write_json_atomically,
};

pub const MAX_PAGE_MESSAGES: u16 = 128;
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
const MAX_INDEX_STATE_BYTES: usize = 4096;
const MAX_PAGE_INPUT_BYTES: usize = 2 * MAX_PAGE_BYTES;
const ENVELOPE_BUDGET: usize = 512;
const ENTRY_BYTES: u64 = 32;
const INDEX_DIRECTORY: &str = "message-index-v1";
const STATE_FILE: &str = "state.json";
pub(super) const PROTOCOL_LOCK: &str = ".protocol.lock";

#[derive(Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum MessagePage {
    Ok {
        messages: Vec<Message>,
        next_cursor: String,
        has_more: bool,
    },
    Gap {
        reason: MessageGap,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<Uuid>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageGap {
    Unindexed,
    Reset,
    Changed,
    Missing,
    Oversized,
    Invalid,
}

impl MessagePage {
    fn gap(reason: MessageGap, message_id: Option<Uuid>) -> Self {
        Self::Gap { reason, message_id }
    }
}

struct Cursor {
    recipient: Uuid,
    epoch: Uuid,
    count: u64,
}

impl Cursor {
    fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 96 {
            return None;
        }
        let mut fields = raw.split(':');
        if fields.next()? != "v1" {
            return None;
        }
        let cursor = Self {
            recipient: Uuid::parse_str(fields.next()?).ok()?,
            epoch: Uuid::parse_str(fields.next()?).ok()?,
            count: fields.next()?.parse().ok()?,
        };
        (fields.next().is_none() && cursor.encode() == raw).then_some(cursor)
    }

    fn encode(&self) -> String {
        format!("v1:{}:{}:{}", self.recipient, self.epoch, self.count)
    }
}

#[derive(Deserialize, Serialize)]
struct IndexState {
    epoch: Uuid,
    count: u64,
    project: Option<String>,
    directories: Directories,
    incomplete: bool,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Directories {
    inbox: Option<DirectoryStamp>,
    archive: Option<DirectoryStamp>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    changed: (i64, i64),
    modified: (i64, i64),
}

impl DirectoryStamp {
    fn read(path: &Path) -> io::Result<Option<Self>> {
        let Some(metadata) = optional(fs::symlink_metadata(path))? else {
            return Ok(None);
        };
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a directory",
            ));
        }
        Ok(Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        }))
    }
}

struct RecipientIndex {
    recipient: Uuid,
    index: PathBuf,
    archive: PathBuf,
    projects: PathBuf,
}

impl RecipientIndex {
    fn new(registry: &Registry, recipient: Uuid) -> Self {
        Self {
            recipient,
            index: registry
                .root
                .join(INDEX_DIRECTORY)
                .join(recipient.to_string()),
            archive: registry.archive_dir(recipient),
            projects: registry.root.join("projects"),
        }
    }

    fn journal(&self, epoch: Uuid) -> PathBuf {
        self.index.join(format!("{epoch}.ids"))
    }

    fn inbox(&self, state: &IndexState) -> Option<PathBuf> {
        Some(
            self.projects
                .join(state.project.as_ref()?)
                .join("sessions")
                .join(self.recipient.to_string())
                .join(INBOX_DIRECTORY),
        )
    }

    fn directories(&self, state: &IndexState) -> io::Result<Directories> {
        Ok(Directories {
            inbox: self
                .inbox(state)
                .map(|path| DirectoryStamp::read(&path))
                .transpose()?
                .flatten(),
            archive: DirectoryStamp::read(&self.archive)?,
        })
    }

    fn state(&self) -> io::Result<Option<IndexState>> {
        let Some(mut file) = optional(File::open(self.index.join(STATE_FILE)))? else {
            return Ok(None);
        };
        let bytes =
            read_at_most(&mut file, MAX_INDEX_STATE_BYTES)?.ok_or(io::ErrorKind::InvalidData)?;
        let state: IndexState = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.project.as_ref().is_some_and(|project| {
            !matches!(project.as_str(), name if !name.is_empty() && name != "." && name != ".." && !name.contains('/'))
        }) || state.count.checked_mul(ENTRY_BYTES).is_none() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Some(state))
    }

    fn write_state(&self, state: &IndexState) -> Result<(), RegistryError> {
        write_json_atomically(&self.index.join(STATE_FILE), state)
    }

    fn initialize(&self, project: Option<String>) -> Result<(), RegistryError> {
        let mut state = IndexState {
            epoch: Uuid::now_v7(),
            count: 0,
            project,
            incomplete: false,
            directories: Directories {
                inbox: None,
                archive: None,
            },
        };
        state.directories = self.directories(&state)?;
        fs::create_dir_all(&self.index)?;
        set_private_directory_permissions(&self.index)?;
        let journal = self.journal(state.epoch);
        File::create_new(&journal)?;
        set_private_file_permissions(&journal)?;
        self.write_state(&state)
    }

    fn journal_present(&self, state: &IndexState) -> io::Result<bool> {
        Ok(optional(fs::metadata(self.journal(state.epoch)))?
            .is_some_and(|meta| meta.is_file() && meta.len() >= state.count * ENTRY_BYTES))
    }

    fn finish_mutation(
        &self,
        mut state: IndexState,
        success: bool,
        added: Option<Uuid>,
    ) -> Result<(), RegistryError> {
        let after = self.directories(&state)?;
        if !success && (added.is_some() || after != state.directories) {
            return Ok(());
        }
        if let Some(id) = added {
            let mut journal = fs::OpenOptions::new()
                .write(true)
                .open(self.journal(state.epoch))?;
            journal.seek(SeekFrom::Start(state.count * ENTRY_BYTES))?;
            journal.write_all(&index_entry(state.epoch, state.count, id))?;
            state.count += 1;
        }
        state.directories = after;
        state.incomplete = false;
        self.write_state(&state)
    }

    fn page(&self, after: Option<Cursor>, limit: u16) -> Result<MessagePage, RegistryError> {
        let state = match self.state() {
            Ok(Some(state)) => state,
            Ok(None) => return Ok(unavailable_index(after.is_some())),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Ok(MessagePage::gap(MessageGap::Invalid, None));
            }
            Err(error) => return Err(error.into()),
        };
        let mut position = 0;
        if let Some(cursor) = after {
            if cursor.epoch != state.epoch || cursor.count > state.count {
                return Ok(MessagePage::gap(MessageGap::Reset, None));
            }
            position = cursor.count;
        }
        if state.incomplete || self.directories(&state)? != state.directories {
            return Ok(MessagePage::gap(MessageGap::Changed, None));
        }
        let Some(mut journal) = optional(File::open(self.journal(state.epoch)))? else {
            return Ok(MessagePage::gap(MessageGap::Missing, None));
        };
        if journal.metadata()?.len() < state.count * ENTRY_BYTES {
            return Ok(MessagePage::gap(MessageGap::Missing, None));
        }
        journal.seek(SeekFrom::Start(position * ENTRY_BYTES))?;

        let mut messages = Vec::with_capacity(usize::from(limit));
        let mut remaining = MAX_PAGE_BYTES - ENVELOPE_BUDGET;
        let mut input_remaining = MAX_PAGE_INPUT_BYTES;
        let inbox = self.inbox(&state);
        while position < state.count && messages.len() < usize::from(limit) {
            let mut entry = [0; ENTRY_BYTES as usize];
            journal.read_exact(&mut entry)?;
            let id = Uuid::from_bytes(entry[..16].try_into().expect("fixed UUID length"));
            if entry != index_entry(state.epoch, position, id) {
                if messages.is_empty() {
                    return Ok(MessagePage::gap(MessageGap::Invalid, None));
                }
                break;
            }
            let filename = format!("{id}.json");
            let pending = match &inbox {
                Some(inbox) => optional(File::open(inbox.join(&filename)))?,
                None => None,
            };
            let mut file = match pending {
                Some(file) => file,
                None => match optional(File::open(self.archive.join(&filename)))? {
                    Some(file) => file,
                    None if messages.is_empty() => {
                        return Ok(MessagePage::gap(MessageGap::Missing, Some(id)));
                    }
                    None => break,
                },
            };
            let Some(bytes) = read_at_most(&mut file, input_remaining)? else {
                if messages.is_empty() {
                    return Ok(MessagePage::gap(MessageGap::Oversized, Some(id)));
                }
                break;
            };
            input_remaining -= bytes.len();
            let parsed = serde_json::from_slice::<Message>(&bytes)
                .ok()
                .filter(|message| message.id == id && message.to == self.recipient);
            let Some(message) = parsed else {
                if messages.is_empty() {
                    return Ok(MessagePage::gap(MessageGap::Invalid, Some(id)));
                }
                break;
            };
            let encoded = serde_json::to_vec(&message)?.len() + 1;
            if encoded > remaining {
                if messages.is_empty() {
                    return Ok(MessagePage::gap(MessageGap::Oversized, Some(id)));
                }
                break;
            }
            remaining -= encoded;
            messages.push(message);
            position += 1;
        }
        if self.directories(&state)? != state.directories {
            return Ok(MessagePage::gap(MessageGap::Changed, None));
        }
        Ok(MessagePage::Ok {
            messages,
            next_cursor: Cursor {
                recipient: self.recipient,
                epoch: state.epoch,
                count: position,
            }
            .encode(),
            has_more: position < state.count,
        })
    }
}

impl Registry {
    pub(super) fn initialize_message_index(
        &self,
        recipient: Uuid,
        project: &str,
    ) -> Result<(), RegistryError> {
        RecipientIndex::new(self, recipient).initialize(Some(project.to_owned()))
    }

    // Called under the protocol lock before changing the legacy inbox/archive. On an
    // interrupted mutation the dirty state forces a gap rather than hiding an acceptance.
    pub(super) fn update_message_index<T>(
        &self,
        recipient: Uuid,
        added: Option<Uuid>,
        mutate: impl FnOnce() -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let index = RecipientIndex::new(self, recipient);
        let mut state = match index.state() {
            Ok(Some(state)) => state,
            Ok(None) => return mutate(),
            Err(_) => {
                // Invalidate the epoch before making an untracked change, even if the read
                // failure was temporary. Legacy delivery can then continue without an index.
                fs::rename(
                    index.index.join(STATE_FILE),
                    index.index.join("state.invalid"),
                )?;
                return mutate();
            }
        };
        if state.incomplete {
            return mutate();
        }
        state.incomplete = true;
        index.write_state(&state)?;
        let current = index.directories(&state);
        if !current.is_ok_and(|current| current == state.directories)
            || !index.journal_present(&state).unwrap_or(false)
        {
            return mutate();
        }

        let result = mutate();
        // A failed index commit must not misreport successful legacy delivery.
        // The persisted dirty marker continues to block readers.
        let _ = index.finish_mutation(state, result.is_ok(), added);
        result
    }

    pub fn read_message_page(
        &self,
        recipient: Uuid,
        after: Option<&str>,
        limit: u16,
    ) -> Result<MessagePage, RegistryError> {
        if !(1..=MAX_PAGE_MESSAGES).contains(&limit) {
            return Err(RegistryError::Protocol(format!(
                "page limit must be between 1 and {MAX_PAGE_MESSAGES}"
            )));
        }
        let after = after
            .map(|raw| {
                Cursor::parse(raw)
                    .filter(|cursor| cursor.recipient == recipient)
                    .ok_or_else(|| {
                        RegistryError::Protocol("invalid cursor for this recipient".to_owned())
                    })
            })
            .transpose()?;
        let Some(lock) = optional(File::open(self.root.join(PROTOCOL_LOCK)))? else {
            return Ok(unavailable_index(after.is_some()));
        };
        fs2::FileExt::lock_shared(&lock)?;
        RecipientIndex::new(self, recipient).page(after, limit)
    }
}

fn index_entry(epoch: Uuid, position: u64, id: Uuid) -> [u8; ENTRY_BYTES as usize] {
    let mut bytes = [0; ENTRY_BYTES as usize];
    bytes[..16].copy_from_slice(id.as_bytes());
    let mut hash = blake3::Hasher::new();
    hash.update(epoch.as_bytes());
    hash.update(&position.to_le_bytes());
    hash.update(id.as_bytes());
    bytes[16..].copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes
}

fn unavailable_index(resuming: bool) -> MessagePage {
    MessagePage::gap(
        if resuming {
            MessageGap::Reset
        } else {
            MessageGap::Unindexed
        },
        None,
    )
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_at_most(file: &mut File, limit: usize) -> io::Result<Option<Vec<u8>>> {
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() <= limit).then_some(bytes))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::project::Project;
    use crate::registry::{MessageKind, MessageSender, Registration, lock_protocol};

    #[test]
    fn failing_index_commit_keeps_the_legacy_success_and_exposes_a_gap() {
        let runtime = tempdir().unwrap();
        let registry = Registry::new(runtime.path().into()).unwrap();
        let _lock = lock_protocol(runtime.path()).unwrap();
        let id = Uuid::now_v7();
        let index = RecipientIndex::new(&registry, id);
        index.initialize(None).unwrap();
        let state = index.state().unwrap().unwrap();
        let result = registry.update_message_index(id, Some(Uuid::now_v7()), || {
            fs::remove_file(index.journal(state.epoch))?;
            Ok(42)
        });
        assert_eq!(result.unwrap(), 42);
        assert!(matches!(
            index.page(None, 1).unwrap(),
            MessagePage::Gap {
                reason: MessageGap::Changed,
                ..
            }
        ));
    }

    #[test]
    fn large_archive_is_bounded_and_keeps_acceptance_order_and_retries() {
        let runtime = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let project = Project::discover(workspace.path()).unwrap();
        let registry = Registry::new(runtime.path().into()).unwrap();
        let recipient = registry
            .register(
                &project,
                Registration {
                    pid: std::process::id(),
                    pi_session_id: None,
                    name: None,
                    role: "primary".into(),
                    parent_pi_session_id: None,
                    task: None,
                    cwd: project.root.clone(),
                    branch: None,
                },
            )
            .unwrap();
        let mut message = Message {
            id: Uuid::from_u128(3),
            from: MessageSender {
                id: Uuid::now_v7(),
                name: "sender".into(),
            },
            to: recipient.id,
            text: "update".into(),
            created_at_ms: 200,
            kind: MessageKind::Update,
            task_id: None,
            sender_task_id: None,
            recipient_task_id: None,
            in_reply_to: None,
            supersedes: None,
        };
        let large = "x".repeat(24_000);
        for id in (1..=550).rev() {
            message.id = Uuid::from_u128(id);
            message.created_at_ms = if id <= 100 { 100 } else { 200 };
            if id == 400 {
                message.text = large.clone();
            }
            {
                let _lock = lock_protocol(runtime.path()).unwrap();
                registry.enqueue_message(&message).unwrap();
                registry.enqueue_message(&message).unwrap();
            }
            registry
                .acknowledge_message(recipient.id, message.id)
                .unwrap();
            let _lock = lock_protocol(runtime.path()).unwrap();
            registry.enqueue_message(&message).unwrap();
        }
        let mut cursor = None;
        let mut ids = Vec::new();
        let mut shortened = false;
        loop {
            let page = registry
                .read_message_page(recipient.id, cursor.as_deref(), MAX_PAGE_MESSAGES)
                .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < MAX_PAGE_BYTES);
            let MessagePage::Ok {
                messages,
                next_cursor,
                has_more,
            } = page
            else {
                panic!("expected an intact page");
            };
            let limit = usize::from(MAX_PAGE_MESSAGES);
            assert!((1..=limit).contains(&messages.len()));
            if cursor.is_none() {
                assert_eq!(messages.len(), limit);
            }
            shortened |= has_more && messages.len() < limit;
            ids.extend(messages.iter().map(|msg| msg.id.as_u128()));
            if !has_more {
                break;
            }
            cursor = Some(next_cursor);
        }
        assert!(shortened);
        assert_eq!(ids, (1..=550).rev().collect::<Vec<_>>());
    }
}
