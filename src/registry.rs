use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::project::Project;

const LOCK_FILE: &str = ".lock";
const METADATA_FILE: &str = "metadata.json";

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
    pub task: Option<String>,
    pub status: String,
    pub started_at_ms: u64,
    pub last_heartbeat_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub pid: u32,
    pub pi_session_id: Option<String>,
    pub name: Option<String>,
    pub role: String,
    pub task: Option<String>,
    pub cwd: PathBuf,
    pub branch: Option<String>,
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
            task: registration.task,
            status: "idle".to_owned(),
            started_at_ms: now,
            last_heartbeat_ms: now,
        };
        let session_dir = self.session_dir(&session.project_id, session.id);
        fs::create_dir_all(&session_dir)?;
        let lock = open_lock(&session_dir)?;
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
                if entry.file_type()?.is_dir() {
                    let lock = open_lock(&entry.path())?;
                    lock.lock_shared()?;
                    sessions.push(read_session(&entry.path())?);
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

fn write_json_atomically(path: &Path, value: &Session) -> Result<(), RegistryError> {
    let parent = path
        .parent()
        .ok_or_else(|| RegistryError::InvalidMetadataPath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{METADATA_FILE}.{}.tmp", Uuid::now_v7()));
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

fn open_lock(directory: &Path) -> Result<File, RegistryError> {
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
