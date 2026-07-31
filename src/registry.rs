use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::project::Project;

const INBOX_DIRECTORY: &str = "inbox";
const LOCK_FILE: &str = ".lock";
const METADATA_FILE: &str = "metadata.json";
const VALID_SESSION_STATUSES: [&str; 2] = ["idle", "busy"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub pid: u32,
    pub pi_session_id: Option<String>,
    pub project_id: String,
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    pub branch: Option<String>,
    pub role: String,
    #[serde(default)]
    pub parent_pi_session_id: Option<String>,
    pub task: Option<String>,
    pub status: String,
    #[serde(default)]
    pub bridge_socket: Option<PathBuf>,
    pub started_at_ms: u64,
    pub last_heartbeat_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub pid: u32,
    pub pi_session_id: Option<String>,
    pub name: Option<String>,
    pub role: String,
    pub parent_pi_session_id: Option<String>,
    pub task: Option<String>,
    pub cwd: PathBuf,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSender {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: Uuid,
    pub from: MessageSender,
    pub to: Uuid,
    pub text: String,
    pub created_at_ms: u64,
}

#[derive(Debug)]
pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn from_environment() -> Result<Self, RegistryError> {
        if let Some(root) = env::var_os("PAJ_RUNTIME_DIR") {
            return Self::new(PathBuf::from(root));
        }

        let runtime =
            env::var_os("XDG_RUNTIME_DIR").ok_or(RegistryError::MissingRuntimeDirectory)?;
        Self::new(PathBuf::from(runtime).join("paj"))
    }

    pub fn new(root: PathBuf) -> Result<Self, RegistryError> {
        fs::create_dir_all(root.join("projects"))?;
        set_private_directory_permissions(&root)?;
        Ok(Self { root })
    }

    pub fn register(
        &self,
        project: &Project,
        registration: Registration,
    ) -> Result<Session, RegistryError> {
        let id = Uuid::now_v7();
        let now = now_ms()?;
        let simple_id = id.simple().to_string();
        let short_id = &simple_id[simple_id.len() - 8..];
        let session = Session {
            id,
            name: registration
                .name
                .unwrap_or_else(|| format!("agent-{short_id}")),
            pid: registration.pid,
            pi_session_id: registration.pi_session_id,
            project_id: project.id.clone(),
            project_root: project.root.clone(),
            cwd: registration.cwd,
            branch: registration.branch,
            role: registration.role,
            parent_pi_session_id: registration.parent_pi_session_id,
            task: registration.task,
            status: "idle".to_owned(),
            bridge_socket: None,
            started_at_ms: now,
            last_heartbeat_ms: now,
        };
        let session_dir = self.session_dir(&session.project_id, session.id);
        let mut session = session;
        session.bridge_socket = Some(session_dir.join("bridge.sock"));
        fs::create_dir_all(&session_dir)?;
        let lock = create_lock(&session_dir)?;
        lock.lock_exclusive()?;
        write_json_atomically(&session_dir.join(METADATA_FILE), &session)?;

        Ok(session)
    }

    pub fn heartbeat(&self, id: Uuid) -> Result<Session, RegistryError> {
        let session_dir = self.find_session_dir(id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let mut session = read_session(&session_dir)?;
        session.last_heartbeat_ms = now_ms()?;
        write_json_atomically(&session_dir.join(METADATA_FILE), &session)?;
        Ok(session)
    }

    pub fn rename(&self, id: Uuid, name: String) -> Result<Session, RegistryError> {
        if name.trim().is_empty() {
            return Err(RegistryError::InvalidAgentName);
        }
        let session_dir = self.find_session_dir(id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let mut session = read_session(&session_dir)?;
        session.name = name;
        write_json_atomically(&session_dir.join(METADATA_FILE), &session)?;
        Ok(session)
    }

    pub fn set_status(&self, id: Uuid, status: String) -> Result<Session, RegistryError> {
        if !VALID_SESSION_STATUSES.contains(&status.as_str()) {
            return Err(RegistryError::InvalidSessionStatus(status));
        }
        let session_dir = self.find_session_dir(id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let mut session = read_session(&session_dir)?;
        session.status = status;
        write_json_atomically(&session_dir.join(METADATA_FILE), &session)?;
        Ok(session)
    }

    pub fn unregister(&self, id: Uuid) -> Result<Session, RegistryError> {
        let session_dir = self.find_session_dir(id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let session = read_session(&session_dir)?;
        fs::remove_dir_all(session_dir)?;
        Ok(session)
    }

    pub fn show(&self, id: Uuid) -> Result<Session, RegistryError> {
        let session_dir = self.find_session_dir(id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_shared()?;
        read_session(&session_dir)
    }

    pub fn list(&self, project_id: Option<&str>) -> Result<Vec<Session>, RegistryError> {
        let mut sessions = Vec::new();
        let projects_dir = self.root.join("projects");
        for project_entry in fs::read_dir(projects_dir)? {
            let project_entry = project_entry?;
            if !project_entry.file_type()?.is_dir() {
                continue;
            }
            if project_id.is_some_and(|id| project_entry.file_name() != id) {
                continue;
            }

            let sessions_dir = project_entry.path().join("sessions");
            let Ok(entries) = fs::read_dir(sessions_dir) else {
                continue;
            };
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let lock = match open_lock(&entry.path()) {
                    Ok(lock) => lock,
                    Err(RegistryError::Io(source))
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                lock.lock_shared()?;
                match read_session(&entry.path()) {
                    Ok(session) => sessions.push(session),
                    Err(RegistryError::ReadMetadata { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        sessions.sort_by_key(|session| session.started_at_ms);
        Ok(sessions)
    }

    pub fn list_live(
        &self,
        project_id: Option<&str>,
        stale_after: Duration,
    ) -> Result<Vec<Session>, RegistryError> {
        let now = now_ms()?;
        let stale_after_ms = duration_ms(stale_after);
        Ok(self
            .list(project_id)?
            .into_iter()
            .filter(|session| !session_is_stale(session, now, stale_after_ms))
            .collect())
    }

    pub fn send_message(
        &self,
        sender_id: Uuid,
        recipient: &str,
        text: String,
    ) -> Result<Message, RegistryError> {
        let sender = self.show(sender_id)?;
        let recipient = self.resolve_live_session(recipient)?;
        let message = Message {
            id: Uuid::now_v7(),
            from: MessageSender {
                id: sender.id,
                name: sender.name,
            },
            to: recipient.id,
            text,
            created_at_ms: now_ms()?,
        };
        let session_dir = self.session_dir(&recipient.project_id, recipient.id);
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let path = session_dir
            .join(INBOX_DIRECTORY)
            .join(format!("{}.json", message.id));
        write_json_atomically(&path, &message)?;
        Ok(message)
    }

    pub fn pending_messages(&self, session_id: Uuid) -> Result<Vec<Message>, RegistryError> {
        let session_dir = self.find_session_dir(session_id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_shared()?;
        let inbox = session_dir.join(INBOX_DIRECTORY);
        let Ok(entries) = fs::read_dir(inbox) else {
            return Ok(Vec::new());
        };
        let mut messages: Vec<Message> = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            {
                messages.push(read_json(&entry.path())?);
            }
        }
        messages.sort_by_key(|message| message.created_at_ms);
        Ok(messages)
    }

    pub fn acknowledge_message(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<(), RegistryError> {
        let session_dir = self.find_session_dir(session_id)?;
        let lock = open_lock(&session_dir)?;
        lock.lock_exclusive()?;
        let path = session_dir
            .join(INBOX_DIRECTORY)
            .join(format!("{message_id}.json"));
        if !path.is_file() {
            return Err(RegistryError::MessageNotFound(message_id));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn gc(&self, stale_after: Duration) -> Result<Vec<Session>, RegistryError> {
        let now = now_ms()?;
        let stale_after_ms = duration_ms(stale_after);
        let candidates = self.list(None)?;
        let mut removed = Vec::new();
        for candidate in candidates {
            let directory = self.session_dir(&candidate.project_id, candidate.id);
            if !directory.exists() {
                continue;
            }
            let lock = open_lock(&directory)?;
            lock.lock_exclusive()?;
            let session = read_session(&directory)?;
            if session_is_stale(&session, now, stale_after_ms) {
                fs::remove_dir_all(directory)?;
                removed.push(session);
            }
        }

        Ok(removed)
    }

    pub fn resolve_live_session(&self, reference: &str) -> Result<Session, RegistryError> {
        let matches = self
            .list_live(None, Duration::from_secs(60))?
            .into_iter()
            .filter(|session| {
                session.name == reference
                    || session.pi_session_id.as_deref() == Some(reference)
                    || session.id.to_string().starts_with(reference)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Err(RegistryError::RecipientNotFound(reference.to_owned())),
            [session] => Ok(session.clone()),
            _ => Err(RegistryError::AmbiguousRecipient(reference.to_owned())),
        }
    }

    fn session_dir(&self, project_id: &str, session_id: Uuid) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id)
            .join("sessions")
            .join(session_id.to_string())
    }

    fn find_session_dir(&self, id: Uuid) -> Result<PathBuf, RegistryError> {
        let projects_dir = self.root.join("projects");
        for project_entry in fs::read_dir(projects_dir)? {
            let project_entry = project_entry?;
            let candidate = project_entry.path().join("sessions").join(id.to_string());
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }
        Err(RegistryError::SessionNotFound(id))
    }
}

fn read_session(directory: &Path) -> Result<Session, RegistryError> {
    let path = directory.join(METADATA_FILE);
    let contents = fs::read(&path).map_err(|source| RegistryError::ReadMetadata {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&contents)
        .map_err(|source| RegistryError::ParseMetadata { path, source })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RegistryError> {
    let contents = fs::read(path).map_err(|source| RegistryError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&contents).map_err(|source| RegistryError::ParseMetadata {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<(), RegistryError> {
    let parent = path
        .parent()
        .ok_or_else(|| RegistryError::InvalidMetadataPath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| RegistryError::InvalidMetadataPath(path.to_path_buf()))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{filename}.{}.tmp", Uuid::now_v7()));
    let contents = serde_json::to_vec_pretty(value)?;
    fs::write(&temporary, contents)?;
    set_private_file_permissions(&temporary)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn now_ms() -> Result<u64, RegistryError> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis).map_err(|_| RegistryError::TimestampOutOfRange)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn session_is_stale(session: &Session, now: u64, stale_after_ms: u64) -> bool {
    let heartbeat_stale = now.saturating_sub(session.last_heartbeat_ms) > stale_after_ms;
    heartbeat_stale || !process_is_alive(session.pid)
}

fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn create_lock(directory: &Path) -> Result<File, RegistryError> {
    let path = directory.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    set_private_file_permissions(&path)?;
    Ok(file)
}

fn open_lock(directory: &Path) -> Result<File, RegistryError> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join(LOCK_FILE))?)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("XDG_RUNTIME_DIR is not set; set it or PAJ_RUNTIME_DIR")]
    MissingRuntimeDirectory,
    #[error("session {0} was not found")]
    SessionNotFound(Uuid),
    #[error("no live agent matches {0}")]
    RecipientNotFound(String),
    #[error("multiple live agents match {0}")]
    AmbiguousRecipient(String),
    #[error("agent name cannot be empty")]
    InvalidAgentName,
    #[error("invalid session status {0}; expected idle or busy")]
    InvalidSessionStatus(String),
    #[error("message {0} was not found")]
    MessageNotFound(Uuid),
    #[error("failed to read session metadata at {path}")]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse session metadata at {path}")]
    ParseMetadata {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid metadata path {0}")]
    InvalidMetadataPath(PathBuf),
    #[error("system clock is earlier than the Unix epoch")]
    InvalidSystemTime(#[from] SystemTimeError),
    #[error("timestamp does not fit in a 64-bit integer")]
    TimestampOutOfRange,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tempfile::tempdir;
    use uuid::Uuid;

    use crate::project::Project;

    use super::{Registration, Registry, RegistryError};

    fn project() -> Project {
        Project {
            id: "project-id".to_owned(),
            root: "/project".into(),
        }
    }

    fn registration(pid: u32) -> Registration {
        Registration {
            pid,
            pi_session_id: Some("pi-session-id".to_owned()),
            name: Some("primary".to_owned()),
            role: "primary".to_owned(),
            parent_pi_session_id: None,
            task: None,
            cwd: "/project".into(),
            branch: Some("master".to_owned()),
        }
    }

    #[test]
    fn register_persists_session_metadata() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");

        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");
        let loaded = registry
            .show(registered.id)
            .expect("session should be loaded");

        assert_eq!(loaded, registered);
    }

    #[test]
    fn session_metadata_without_bridge_socket_remains_compatible() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");
        let mut metadata = serde_json::to_value(registered).expect("session should serialize");
        metadata
            .as_object_mut()
            .expect("session should be an object")
            .remove("bridgeSocket");

        let session: super::Session =
            serde_json::from_value(metadata).expect("old session metadata should load");

        assert_eq!(session.bridge_socket, None);
    }

    #[test]
    fn register_generates_distinct_default_names() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let mut first_registration = registration(std::process::id());
        first_registration.name = None;
        let mut second_registration = registration(std::process::id());
        second_registration.name = None;
        let first = registry
            .register(&project(), first_registration)
            .expect("first session should be registered");

        let second = registry
            .register(&project(), second_registration)
            .expect("second session should be registered");

        assert_ne!(first.name, second.name);
    }

    #[test]
    fn heartbeat_updates_last_heartbeat() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        let updated = registry
            .heartbeat(registered.id)
            .expect("heartbeat should succeed");

        assert!(updated.last_heartbeat_ms >= registered.last_heartbeat_ms);
    }

    #[test]
    fn rename_updates_persisted_session_name() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        let renamed = registry
            .rename(registered.id, "reviewer".to_owned())
            .expect("session should be renamed");

        assert_eq!(renamed.name, "reviewer");
        assert_eq!(
            registry
                .show(registered.id)
                .expect("renamed session should load")
                .name,
            "reviewer"
        );
    }

    #[test]
    fn status_updates_persisted_session_status() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        let updated = registry
            .set_status(registered.id, "busy".to_owned())
            .expect("status should update");

        assert_eq!(updated.status, "busy");
        assert_eq!(
            registry
                .show(registered.id)
                .expect("updated session should load")
                .status,
            "busy"
        );
    }

    #[test]
    fn status_rejects_unknown_values() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        let result = registry.set_status(registered.id, "working".to_owned());

        assert!(matches!(
            result,
            Err(RegistryError::InvalidSessionStatus(_))
        ));
        assert_eq!(
            registry
                .show(registered.id)
                .expect("original session should load")
                .status,
            "idle"
        );
    }

    #[test]
    fn rename_rejects_an_empty_name() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        let result = registry.rename(registered.id, "  ".to_owned());

        assert!(matches!(result, Err(RegistryError::InvalidAgentName)));
        assert_eq!(
            registry
                .show(registered.id)
                .expect("original session should load")
                .name,
            "primary"
        );
    }

    #[test]
    fn list_filters_sessions_by_project() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        registry
            .register(&project(), registration(std::process::id()))
            .expect("first session should be registered");
        let other = Project {
            id: "other-project".to_owned(),
            root: "/other".into(),
        };
        registry
            .register(&other, registration(std::process::id()))
            .expect("second session should be registered");

        let sessions = registry
            .list(Some("project-id"))
            .expect("sessions should be listed");

        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn list_ignores_incomplete_session_directories() {
        let directory = tempdir().expect("temporary directory should be created");
        let root = directory.path().join("paj");
        let registry = Registry::new(root.clone()).expect("registry should be created");
        let sessions = root.join("projects/project-id/sessions");
        let empty = sessions.join(Uuid::now_v7().to_string());
        let lock_only = sessions.join(Uuid::now_v7().to_string());
        fs::create_dir_all(&empty).expect("empty session directory should be created");
        fs::create_dir_all(&lock_only).expect("lock-only session directory should be created");
        fs::write(lock_only.join(".lock"), []).expect("lock file should be created");

        let listed = registry
            .list(Some("project-id"))
            .expect("incomplete sessions should be ignored");

        assert!(listed.is_empty());
        assert!(!empty.join(".lock").exists());
    }

    #[test]
    fn list_live_excludes_session_with_dead_process() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        registry
            .register(&project(), registration(u32::MAX))
            .expect("session should be registered");

        let sessions = registry
            .list_live(Some("project-id"), Duration::from_secs(60))
            .expect("live sessions should be listed");

        assert!(sessions.is_empty());
    }

    #[test]
    fn unregister_removes_session() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");

        registry
            .unregister(registered.id)
            .expect("session should be unregistered");
        let result = registry.show(registered.id);

        assert!(matches!(result, Err(RegistryError::SessionNotFound(_))));
    }

    #[test]
    fn send_message_delivers_to_named_recipient() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let mut sender_registration = registration(std::process::id());
        sender_registration.name = Some("sender".to_owned());
        let sender = registry
            .register(&project(), sender_registration)
            .expect("sender should be registered");
        let mut recipient_registration = registration(std::process::id());
        recipient_registration.name = Some("recipient".to_owned());
        let recipient = registry
            .register(&project(), recipient_registration)
            .expect("recipient should be registered");

        let sent = registry
            .send_message(sender.id, "recipient", "hello".to_owned())
            .expect("message should be sent");
        let pending = registry
            .pending_messages(recipient.id)
            .expect("messages should be listed");

        assert_eq!(pending, vec![sent]);
    }

    #[test]
    fn acknowledge_message_removes_pending_message() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let sender = registry
            .register(&project(), registration(std::process::id()))
            .expect("sender should be registered");
        let mut recipient_registration = registration(std::process::id());
        recipient_registration.name = Some("recipient".to_owned());
        let recipient = registry
            .register(&project(), recipient_registration)
            .expect("recipient should be registered");
        let message = registry
            .send_message(sender.id, "recipient", "hello".to_owned())
            .expect("message should be sent");

        registry
            .acknowledge_message(recipient.id, message.id)
            .expect("message should be acknowledged");
        let pending = registry
            .pending_messages(recipient.id)
            .expect("messages should be listed");

        assert!(pending.is_empty());
    }

    #[test]
    fn send_message_resolves_recipient_by_session_id_prefix() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let sender = registry
            .register(&project(), registration(std::process::id()))
            .expect("sender should be registered");
        let mut recipient_registration = registration(std::process::id());
        recipient_registration.name = Some("recipient".to_owned());
        let recipient = registry
            .register(&project(), recipient_registration)
            .expect("recipient should be registered");
        let reference = &recipient.id.to_string()[..35];

        let message = registry
            .send_message(sender.id, reference, "hello".to_owned())
            .expect("recipient prefix should resolve");

        assert_eq!(message.to, recipient.id);
    }

    #[test]
    fn send_message_resolves_exact_pi_session_id() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let sender = registry
            .register(&project(), registration(std::process::id()))
            .expect("sender should register");
        let mut recipient_registration = registration(std::process::id());
        recipient_registration.name = Some("recipient".to_owned());
        recipient_registration.pi_session_id = Some("stable-pi-session".to_owned());
        let recipient = registry
            .register(&project(), recipient_registration)
            .expect("recipient should register");

        let message = registry
            .send_message(sender.id, "stable-pi-session", "done".to_owned())
            .expect("exact Pi session ID should resolve");

        assert_eq!(message.to, recipient.id);
    }

    #[test]
    fn send_message_rejects_ambiguous_recipient_name() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let sender = registry
            .register(&project(), registration(std::process::id()))
            .expect("sender should be registered");
        for _ in 0..2 {
            let mut recipient_registration = registration(std::process::id());
            recipient_registration.name = Some("duplicate".to_owned());
            registry
                .register(&project(), recipient_registration)
                .expect("recipient should be registered");
        }

        let result = registry.send_message(sender.id, "duplicate", "hello".to_owned());

        assert!(matches!(result, Err(RegistryError::AmbiguousRecipient(_))));
    }

    #[test]
    fn acknowledge_message_cannot_remove_another_sessions_message() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let sender = registry
            .register(&project(), registration(std::process::id()))
            .expect("sender should be registered");
        let mut recipient_registration = registration(std::process::id());
        recipient_registration.name = Some("recipient".to_owned());
        let recipient = registry
            .register(&project(), recipient_registration)
            .expect("recipient should be registered");
        let other = registry
            .register(&project(), registration(std::process::id()))
            .expect("other session should be registered");
        let message = registry
            .send_message(sender.id, "recipient", "hello".to_owned())
            .expect("message should be sent");

        let result = registry.acknowledge_message(other.id, message.id);

        assert!(matches!(result, Err(RegistryError::MessageNotFound(_))));
        assert_eq!(
            registry
                .pending_messages(recipient.id)
                .expect("recipient inbox should load"),
            vec![message]
        );
    }

    #[test]
    fn gc_removes_session_with_dead_process() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(u32::MAX))
            .expect("session should be registered");

        let removed = registry
            .gc(Duration::from_secs(u64::MAX))
            .expect("garbage collection should succeed");

        assert_eq!(removed, vec![registered]);
    }

    #[test]
    fn metadata_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temporary directory should be created");
        let root = directory.path().join("paj");
        let registry = Registry::new(root.clone()).expect("registry should be created");
        let registered = registry
            .register(&project(), registration(std::process::id()))
            .expect("session should be registered");
        let metadata = root
            .join("projects/project-id/sessions")
            .join(registered.id.to_string())
            .join("metadata.json");

        let mode = fs::metadata(metadata)
            .expect("metadata should exist")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn show_returns_not_found_for_unknown_session() {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let id = Uuid::now_v7();

        let result = registry.show(id);

        assert!(matches!(result, Err(RegistryError::SessionNotFound(_))));
    }
}
