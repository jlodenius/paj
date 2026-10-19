use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::registry::{
    Message, MessageKind, MessageSender, Registry, RegistryError, Session, lock_protocol,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    #[default]
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub message_id: Uuid,
    pub from: MessageSender,
    pub text: String,
    pub created_at_ms: u64,
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssignment {
    pub message: Message,
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "SpawnRecordData")]
pub struct SpawnRecord {
    pub spawn_id: Uuid,
    pub parent_pi_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub parent_pid: u32,
    pub child_pi_session_id: Option<String>,
    pub child_paj_session_id: Option<Uuid>,
    pub name: String,
    pub tmux_name: String,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub task: String,
    pub created_at_ms: u64,
    pub registered_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    pub task_id: String,
    pub task_state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    pub result_history: Vec<TaskResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment: Option<TaskAssignment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpawnRecordData {
    spawn_id: Uuid,
    parent_pi_session_id: String,
    parent_task_id: Option<String>,
    parent_pid: u32,
    child_pi_session_id: Option<String>,
    child_paj_session_id: Option<Uuid>,
    name: String,
    tmux_name: String,
    cwd: PathBuf,
    project_root: PathBuf,
    task: String,
    created_at_ms: u64,
    registered_at_ms: Option<u64>,
    requested_model: Option<String>,
    task_id: Option<String>,
    #[serde(default)]
    task_state: TaskState,
    result: Option<TaskResult>,
    #[serde(default)]
    result_history: Vec<TaskResult>,
    assignment: Option<TaskAssignment>,
}

impl From<SpawnRecordData> for SpawnRecord {
    fn from(data: SpawnRecordData) -> Self {
        Self {
            spawn_id: data.spawn_id,
            parent_pi_session_id: data.parent_pi_session_id,
            parent_task_id: data.parent_task_id,
            parent_pid: data.parent_pid,
            child_pi_session_id: data.child_pi_session_id,
            child_paj_session_id: data.child_paj_session_id,
            name: data.name,
            tmux_name: data.tmux_name,
            cwd: data.cwd,
            project_root: data.project_root,
            task: data.task,
            created_at_ms: data.created_at_ms,
            registered_at_ms: data.registered_at_ms,
            requested_model: data.requested_model,
            task_id: data.task_id.unwrap_or_else(|| data.spawn_id.to_string()),
            task_state: data.task_state,
            result: data.result,
            result_history: data.result_history,
            assignment: data.assignment,
        }
    }
}

impl SpawnRecord {
    pub(crate) fn matches_child(&self, session: &Session) -> bool {
        self.child_paj_session_id == Some(session.id)
            || self
                .child_pi_session_id
                .as_ref()
                .is_some_and(|id| session.pi_session_id.as_ref() == Some(id))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub requested_model: Option<String>,
    pub parent_task_id: Option<String>,
}

#[derive(Debug)]
pub struct SpawnStore {
    root: PathBuf,
}

impl SpawnStore {
    pub fn from_environment() -> Result<Self, SpawnError> {
        let root = if let Some(root) = env::var_os("PAJ_RUNTIME_DIR") {
            PathBuf::from(root)
        } else {
            let runtime =
                env::var_os("XDG_RUNTIME_DIR").ok_or(SpawnError::MissingRuntimeDirectory)?;
            PathBuf::from(runtime).join("paj")
        };
        Self::new(root)
    }

    pub fn new(root: PathBuf) -> Result<Self, SpawnError> {
        let directory = root.join("subagents");
        fs::create_dir_all(&directory)?;
        set_private_directory_permissions(&root)?;
        set_private_directory_permissions(&directory)?;
        Ok(Self { root })
    }

    pub fn create(
        &self,
        parent_pi_session_id: String,
        parent_pid: u32,
        cwd: PathBuf,
        project_root: PathBuf,
        task: String,
    ) -> Result<SpawnRecord, SpawnError> {
        self.create_with_model(
            parent_pi_session_id,
            parent_pid,
            cwd,
            project_root,
            task,
            None,
        )
    }

    pub fn create_with_model(
        &self,
        parent_pi_session_id: String,
        parent_pid: u32,
        cwd: PathBuf,
        project_root: PathBuf,
        task: String,
        requested_model: Option<String>,
    ) -> Result<SpawnRecord, SpawnError> {
        self.create_with_options(
            parent_pi_session_id,
            parent_pid,
            cwd,
            project_root,
            task,
            SpawnOptions {
                requested_model,
                ..SpawnOptions::default()
            },
        )
    }

    pub fn create_with_options(
        &self,
        parent_pi_session_id: String,
        parent_pid: u32,
        cwd: PathBuf,
        project_root: PathBuf,
        task: String,
        options: SpawnOptions,
    ) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        if task.trim().is_empty() {
            return Err(SpawnError::EmptyTask);
        }
        if options
            .requested_model
            .as_ref()
            .is_some_and(|model| model.trim().is_empty())
        {
            return Err(SpawnError::Protocol(
                "requested model cannot be empty".to_owned(),
            ));
        }
        let parent_task = self
            .list(None, false)?
            .into_iter()
            .find(|record| record.child_pi_session_id.as_deref() == Some(&parent_pi_session_id));
        let parent_task_id = match parent_task {
            Some(parent) => {
                if parent.task_state != TaskState::Active {
                    return Err(SpawnError::Protocol(
                        "a completed parent task cannot create a subagent".to_owned(),
                    ));
                }
                if options
                    .parent_task_id
                    .as_ref()
                    .is_some_and(|id| id != &parent.task_id)
                {
                    return Err(SpawnError::Protocol(
                        "parent task ID is stale or mismatched".to_owned(),
                    ));
                }
                Some(parent.task_id)
            }
            None => options.parent_task_id,
        };
        if parent_task_id.as_deref() == Some("") {
            return Err(SpawnError::Protocol(
                "parent task ID cannot be empty".to_owned(),
            ));
        }
        let spawn_id = Uuid::now_v7();
        let compact = spawn_id.simple().to_string();
        let record = SpawnRecord {
            spawn_id,
            parent_pi_session_id,
            parent_task_id,
            parent_pid,
            child_pi_session_id: None,
            child_paj_session_id: None,
            name: format!("agent-{}", &compact[compact.len() - 8..]),
            tmux_name: format!("paj-{}", &compact[compact.len() - 12..]),
            cwd,
            project_root,
            task,
            created_at_ms: now_ms()?,
            registered_at_ms: None,
            requested_model: options.requested_model,
            task_id: spawn_id.to_string(),
            task_state: TaskState::Active,
            result: None,
            result_history: Vec::new(),
            assignment: None,
        };
        self.write(&record)?;
        Ok(record)
    }

    pub fn bind(
        &self,
        spawn_id: Uuid,
        child_pi_session_id: String,
        child_paj_session_id: Uuid,
        name: String,
    ) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        let mut record = self.show(spawn_id)?;
        record.child_pi_session_id = Some(child_pi_session_id);
        record.child_paj_session_id = Some(child_paj_session_id);
        record.name = name;
        record.registered_at_ms = Some(now_ms()?);
        self.write(&record)?;
        self.flush_outbox(&mut record)?;
        Ok(record)
    }

    pub fn show(&self, spawn_id: Uuid) -> Result<SpawnRecord, SpawnError> {
        let path = self.path(spawn_id);
        if !path.is_file() {
            return Err(SpawnError::NotFound(spawn_id));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn list(
        &self,
        parent: Option<&str>,
        active_only: bool,
    ) -> Result<Vec<SpawnRecord>, SpawnError> {
        let mut records = Vec::new();
        for entry in fs::read_dir(self.root.join("subagents"))? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                let record: SpawnRecord = serde_json::from_slice(&fs::read(entry.path())?)?;
                if parent.is_none_or(|id| record.parent_pi_session_id == id)
                    && (!active_only || tmux_session_exists(&record.tmux_name))
                {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| record.created_at_ms);
        Ok(records)
    }

    pub fn remove(&self, spawn_id: Uuid) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        self.remove_unlocked(spawn_id)
    }

    fn remove_unlocked(&self, spawn_id: Uuid) -> Result<SpawnRecord, SpawnError> {
        let record = self.show(spawn_id)?;
        fs::remove_file(self.path(spawn_id))?;
        let launcher = self
            .root
            .join("subagent-launchers")
            .join(spawn_id.to_string());
        if launcher.is_dir() {
            fs::remove_dir_all(launcher)?;
        }
        Ok(record)
    }

    pub fn gc(&self) -> Result<Vec<SpawnRecord>, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        let records = self.list(None, false)?;
        let mut removed = Vec::new();
        for record in records {
            let tmux_exists = tmux_session_exists(&record.tmux_name);
            if tmux_exists && process_is_alive(record.parent_pid) {
                continue;
            }
            if tmux_exists {
                let killed = Command::new("tmux")
                    .args([
                        "-L",
                        "paj",
                        "kill-session",
                        "-t",
                        &format!("={}", record.tmux_name),
                    ])
                    .status()
                    .is_ok_and(|status| status.success());
                if !killed && tmux_session_exists(&record.tmux_name) {
                    continue;
                }
            }
            if self.path(record.spawn_id).exists() {
                self.remove_unlocked(record.spawn_id)?;
            }
            removed.push(record);
        }
        Ok(removed)
    }

    pub fn flush(&self, spawn_id: Uuid) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        let mut record = self.show(spawn_id)?;
        self.flush_outbox(&mut record)?;
        Ok(record)
    }

    pub fn complete(
        &self,
        spawn_id: Uuid,
        from: Uuid,
        task_id: &str,
        text: String,
    ) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        let mut record = self.show(spawn_id)?;
        if record.child_paj_session_id != Some(from) {
            return Err(SpawnError::Protocol(
                "only the bound child may complete its task".to_owned(),
            ));
        }
        if record.task_id != task_id {
            return Err(SpawnError::Protocol(
                "completion task ID is stale or mismatched".to_owned(),
            ));
        }
        if record.task_state == TaskState::Active {
            if text.trim().is_empty() {
                return Err(SpawnError::Protocol(
                    "completion summary cannot be empty".to_owned(),
                ));
            }
            record.result = Some(TaskResult {
                task_id: record.task_id.clone(),
                parent_task_id: record.parent_task_id.clone(),
                message_id: Uuid::now_v7(),
                from: MessageSender {
                    id: from,
                    name: record.name.clone(),
                },
                text,
                created_at_ms: now_ms()?,
                delivered: false,
            });
            record.task_state = TaskState::Completed;
            self.write(&record)?;
        }
        self.flush_outbox(&mut record)?;
        Ok(record)
    }

    pub fn assign(
        &self,
        spawn_id: Uuid,
        from: Uuid,
        task: String,
    ) -> Result<SpawnRecord, SpawnError> {
        let _protocol = lock_protocol(&self.root)?;
        if task.trim().is_empty() {
            return Err(SpawnError::EmptyTask);
        }
        let registry = Registry::new(self.root.clone())?;
        let sender = registry.show(from)?;
        let mut record = self.show(spawn_id)?;
        if sender.pi_session_id.as_deref() != Some(&record.parent_pi_session_id) {
            return Err(SpawnError::Protocol(
                "only the owner parent may assign a new task".to_owned(),
            ));
        }
        let parent_task = self
            .list(None, false)?
            .into_iter()
            .find(|parent| parent.matches_child(&sender));
        if parent_task
            .as_ref()
            .is_some_and(|parent| parent.task_state != TaskState::Active)
        {
            return Err(SpawnError::Protocol(
                "a completed parent task cannot assign subagent work".to_owned(),
            ));
        }
        let parent_task_id = parent_task.map(|parent| parent.task_id);
        if record.task_state != TaskState::Completed {
            if record
                .assignment
                .as_ref()
                .is_some_and(|assignment| !assignment.delivered)
                && record.task == task
                && record.parent_task_id == parent_task_id
            {
                self.flush_outbox(&mut record)?;
                return Ok(record);
            }
            return Err(SpawnError::Protocol(
                "a new task may only be assigned after completion".to_owned(),
            ));
        }
        let child_id = record
            .child_paj_session_id
            .ok_or_else(|| SpawnError::Protocol("subagent has no bound child".to_owned()))?;
        self.flush_outbox(&mut record)?;
        if let Some(result) = record.result.take() {
            record.result_history.push(result);
        }
        record.task_id = Uuid::now_v7().to_string();
        record.parent_task_id = parent_task_id;
        record.task = task.clone();
        record.task_state = TaskState::Active;
        record.assignment = Some(TaskAssignment {
            message: Message {
                id: Uuid::now_v7(),
                from: MessageSender {
                    id: sender.id,
                    name: sender.name,
                },
                to: child_id,
                text: task,
                created_at_ms: now_ms()?,
                kind: MessageKind::Request,
                task_id: Some(record.task_id.clone()),
                sender_task_id: record.parent_task_id.clone(),
                recipient_task_id: Some(record.task_id.clone()),
                in_reply_to: None,
                supersedes: None,
            },
            delivered: false,
        });
        self.write(&record)?;
        self.flush_outbox(&mut record)?;
        Ok(record)
    }

    fn flush_outbox(&self, record: &mut SpawnRecord) -> Result<(), SpawnError> {
        let registry = Registry::new(self.root.clone())?;
        for result in record
            .result_history
            .iter_mut()
            .chain(record.result.iter_mut())
        {
            if result.delivered {
                continue;
            }
            let recipient = match registry.resolve_live_pi_session(&record.parent_pi_session_id) {
                Ok(session) => session,
                Err(RegistryError::RecipientNotFound(_)) => continue,
                Err(error) => return Err(error.into()),
            };
            registry.enqueue_message(&Message {
                id: result.message_id,
                from: result.from.clone(),
                to: recipient.id,
                text: result.text.clone(),
                created_at_ms: result.created_at_ms,
                kind: MessageKind::Result,
                task_id: Some(result.task_id.clone()),
                sender_task_id: Some(result.task_id.clone()),
                recipient_task_id: result.parent_task_id.clone(),
                in_reply_to: None,
                supersedes: None,
            })?;
            result.delivered = true;
        }
        if let Some(assignment) = &mut record.assignment
            && !assignment.delivered
            && record.task_state == TaskState::Active
        {
            let recipient = match record.child_pi_session_id.as_deref() {
                Some(pi_id) => registry.resolve_live_pi_session(pi_id),
                None => registry.resolve_live_session(&assignment.message.to.to_string()),
            };
            match recipient {
                Ok(session) => {
                    assignment.message.to = session.id;
                    registry.enqueue_message(&assignment.message)?;
                    assignment.delivered = true;
                }
                Err(RegistryError::RecipientNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.write(record)
    }

    fn path(&self, spawn_id: Uuid) -> PathBuf {
        self.root.join("subagents").join(format!("{spawn_id}.json"))
    }

    fn write(&self, record: &SpawnRecord) -> Result<(), SpawnError> {
        let path = self.path(record.spawn_id);
        let temporary = path.with_extension(format!("{}.tmp", Uuid::now_v7()));
        fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
        set_private_file_permissions(&temporary)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

pub fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["-L", "paj", "has-session", "-t", &format!("={name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn now_ms() -> Result<u64, SpawnError> {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .map_err(|_| SpawnError::TimestampOutOfRange)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), SpawnError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), SpawnError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), SpawnError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), SpawnError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("XDG_RUNTIME_DIR is not set; set it or PAJ_RUNTIME_DIR")]
    MissingRuntimeDirectory,
    #[error("subagent spawn {0} was not found")]
    NotFound(Uuid),
    #[error("subagent task cannot be empty or whitespace")]
    EmptyTask,
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Registry(#[from] RegistryError),
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
    use tempfile::tempdir;

    use super::SpawnStore;

    #[test]
    fn records_are_scoped_by_stable_parent_pi_session_id() {
        let directory = tempdir().expect("temporary directory should be created");
        let store = SpawnStore::new(directory.path().join("paj")).expect("store should be created");
        let first = store
            .create(
                "parent-one".to_owned(),
                std::process::id(),
                directory.path().to_path_buf(),
                directory.path().to_path_buf(),
                "first task".to_owned(),
            )
            .expect("first record should be created");
        store
            .create(
                "parent-two".to_owned(),
                std::process::id(),
                directory.path().to_path_buf(),
                directory.path().to_path_buf(),
                "second task".to_owned(),
            )
            .expect("second record should be created");

        let records = store
            .list(Some("parent-one"), false)
            .expect("records should list");

        assert_eq!(records, vec![first]);
    }

    #[test]
    fn bind_persists_child_identity_and_name() {
        let directory = tempdir().expect("temporary directory should be created");
        let store = SpawnStore::new(directory.path().join("paj")).expect("store should be created");
        let record = store
            .create(
                "parent".to_owned(),
                std::process::id(),
                directory.path().to_path_buf(),
                directory.path().to_path_buf(),
                "task".to_owned(),
            )
            .expect("record should be created");
        let paj_id = uuid::Uuid::now_v7();

        let bound = store
            .bind(
                record.spawn_id,
                "child-pi".to_owned(),
                paj_id,
                "reviewer".to_owned(),
            )
            .expect("record should bind");

        assert_eq!(bound.child_pi_session_id.as_deref(), Some("child-pi"));
        assert_eq!(bound.child_paj_session_id, Some(paj_id));
        assert_eq!(
            store
                .show(record.spawn_id)
                .expect("record should load")
                .name,
            "reviewer"
        );
    }

    #[test]
    fn gc_removes_records_owned_by_a_dead_parent() {
        let directory = tempdir().expect("temporary directory should be created");
        let store = SpawnStore::new(directory.path().join("paj")).expect("store should be created");
        let record = store
            .create(
                "dead-parent".to_owned(),
                u32::MAX,
                directory.path().to_path_buf(),
                directory.path().to_path_buf(),
                "task".to_owned(),
            )
            .expect("record should be created");

        assert_eq!(store.gc().expect("gc should succeed"), vec![record.clone()]);
        assert!(store.show(record.spawn_id).is_err());
    }

    #[test]
    fn gc_removes_a_missing_tmux_session_even_with_a_live_parent() {
        let directory = tempdir().expect("temporary directory should be created");
        let store = SpawnStore::new(directory.path().join("paj")).expect("store should be created");
        let record = store
            .create(
                "parent".to_owned(),
                std::process::id(),
                directory.path().to_path_buf(),
                directory.path().to_path_buf(),
                "task".to_owned(),
            )
            .expect("record should be created");

        assert_eq!(store.gc().expect("gc should succeed"), vec![record.clone()]);
        assert!(store.show(record.spawn_id).is_err());
    }

    #[test]
    fn create_rejects_an_empty_task() {
        let directory = tempdir().expect("temporary directory should be created");
        let store = SpawnStore::new(directory.path().join("paj")).expect("store should be created");

        let result = store.create(
            "parent".to_owned(),
            std::process::id(),
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
            "  \n".to_owned(),
        );

        assert!(matches!(result, Err(super::SpawnError::EmptyTask)));
    }
}
