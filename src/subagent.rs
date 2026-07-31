use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnRecord {
    pub spawn_id: Uuid,
    pub parent_pi_session_id: String,
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
        if task.trim().is_empty() {
            return Err(SpawnError::EmptyTask);
        }
        let spawn_id = Uuid::now_v7();
        let compact = spawn_id.simple().to_string();
        let record = SpawnRecord {
            spawn_id,
            parent_pi_session_id,
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
        let mut record = self.show(spawn_id)?;
        record.child_pi_session_id = Some(child_pi_session_id);
        record.child_paj_session_id = Some(child_paj_session_id);
        record.name = name;
        record.registered_at_ms = Some(now_ms()?);
        self.write(&record)?;
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
                self.remove(record.spawn_id)?;
            }
            removed.push(record);
        }
        Ok(removed)
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
