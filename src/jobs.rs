use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::project::Project;

const LOCK_FILE: &str = ".lock";
const METADATA_FILE: &str = "metadata.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Running,
    Exited,
    Stopped,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: Uuid,
    pub name: String,
    pub owner_session_id: Option<Uuid>,
    pub project_id: String,
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub tmux_session: String,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
    pub created_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

#[derive(Debug)]
pub struct JobManager {
    root: PathBuf,
    tmux_socket: String,
}

impl JobManager {
    pub fn from_environment() -> Result<Self, JobError> {
        let root = if let Some(root) = env::var_os("PAJ_RUNTIME_DIR") {
            PathBuf::from(root)
        } else {
            let runtime =
                env::var_os("XDG_RUNTIME_DIR").ok_or(JobError::MissingRuntimeDirectory)?;
            PathBuf::from(runtime).join("paj")
        };
        Self::new(root, "paj".to_owned())
    }

    pub fn new(root: PathBuf, tmux_socket: String) -> Result<Self, JobError> {
        fs::create_dir_all(root.join("projects"))?;
        set_directory_permissions(&root)?;
        Ok(Self { root, tmux_socket })
    }

    pub fn start(
        &self,
        project: &Project,
        name: String,
        owner_session_id: Option<Uuid>,
        cwd: PathBuf,
        command: Vec<String>,
    ) -> Result<Job, JobError> {
        if name.trim().is_empty() {
            return Err(JobError::EmptyName);
        }
        if command.is_empty() {
            return Err(JobError::EmptyCommand);
        }
        if self
            .list(Some(&project.id))?
            .iter()
            .any(|job| job.name == name && job.state == JobState::Running)
        {
            return Err(JobError::DuplicateName(name));
        }

        let id = Uuid::now_v7();
        let tmux_session = format!("paj-job-{}", id.simple());
        let job_dir = self.job_dir(&project.id, id);
        fs::create_dir_all(&job_dir)?;
        let lock = open_lock(&job_dir)?;
        lock.lock_exclusive()?;
        let mut job = Job {
            id,
            name,
            owner_session_id,
            project_id: project.id.clone(),
            project_root: project.root.clone(),
            cwd,
            command,
            tmux_session,
            state: JobState::Running,
            exit_code: None,
            pid: None,
            created_at_ms: now_ms()?,
            finished_at_ms: None,
        };

        let result = self.start_tmux_job(&job, &job_dir);
        if let Err(error) = result {
            let _ = self.tmux_output(["kill-session", "-t", &job.tmux_session]);
            fs::remove_dir_all(job_dir)?;
            return Err(error);
        }
        job.pid = self.inspect_pane(&job.tmux_session)?.pid;
        write_json_atomically(&job_dir.join(METADATA_FILE), &job)?;
        Ok(job)
    }

    pub fn list(&self, project_id: Option<&str>) -> Result<Vec<Job>, JobError> {
        let mut jobs = Vec::new();
        for project_entry in fs::read_dir(self.root.join("projects"))? {
            let project_entry = project_entry?;
            if !project_entry.file_type()?.is_dir() {
                continue;
            }
            if project_id.is_some_and(|id| project_entry.file_name() != id) {
                continue;
            }
            let Ok(entries) = fs::read_dir(project_entry.path().join("jobs")) else {
                continue;
            };
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    jobs.push(self.refresh_job(&entry.path())?);
                }
            }
        }
        jobs.sort_by_key(|job| job.created_at_ms);
        Ok(jobs)
    }

    pub fn resolve(&self, project_id: &str, reference: &str) -> Result<Job, JobError> {
        let jobs = self.list(Some(project_id))?;
        if let Some(job) = jobs.iter().rev().find(|job| job.name == reference).cloned() {
            return Ok(job);
        }
        let matches = jobs
            .into_iter()
            .filter(|job| job.id.to_string().starts_with(reference))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Err(JobError::NotFound(reference.to_owned())),
            [job] => Ok(job.clone()),
            _ => Err(JobError::Ambiguous(reference.to_owned())),
        }
    }

    pub fn send(&self, job: &Job, input: &str, enter: bool) -> Result<(), JobError> {
        self.tmux_checked(["send-keys", "-t", &job.tmux_session, "-l", "--", input])?;
        if enter {
            self.tmux_checked(["send-keys", "-t", &job.tmux_session, "Enter"])?;
        }
        Ok(())
    }

    pub fn interrupt(&self, job: &Job) -> Result<(), JobError> {
        self.tmux_checked(["send-keys", "-t", &job.tmux_session, "C-c"])?;
        Ok(())
    }

    pub fn stop(&self, job: &Job) -> Result<Job, JobError> {
        if self
            .tmux_output(["has-session", "-t", &job.tmux_session])?
            .status
            .success()
        {
            self.tmux_checked(["kill-session", "-t", &job.tmux_session])?;
        }
        let job_dir = self.job_dir(&job.project_id, job.id);
        let lock = open_lock(&job_dir)?;
        lock.lock_exclusive()?;
        let mut updated = read_job(&job_dir)?;
        if updated.state == JobState::Running {
            updated.state = JobState::Stopped;
            updated.finished_at_ms = Some(now_ms()?);
            updated.pid = None;
            write_json_atomically(&job_dir.join(METADATA_FILE), &updated)?;
        }
        Ok(updated)
    }

    pub fn remove(&self, job: &Job) -> Result<(), JobError> {
        self.stop(job)?;
        let job_dir = self.job_dir(&job.project_id, job.id);
        if job_dir.exists() {
            fs::remove_dir_all(job_dir)?;
        }
        Ok(())
    }

    pub fn log_path(&self, job: &Job) -> PathBuf {
        self.job_dir(&job.project_id, job.id).join("output.log")
    }

    pub fn attach(&self, job: &Job) -> Result<(), JobError> {
        let status = Command::new("tmux")
            .args([
                "-L",
                &self.tmux_socket,
                "attach-session",
                "-t",
                &job.tmux_session,
            ])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env_remove("TMUX")
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(JobError::Tmux(format!("attach exited with {status}")))
        }
    }

    fn start_tmux_job(&self, job: &Job, job_dir: &Path) -> Result<(), JobError> {
        let cwd = job.cwd.to_string_lossy();
        self.tmux_checked(["new-session", "-d", "-s", &job.tmux_session, "-c", &cwd])?;
        self.tmux_checked([
            "set-option",
            "-w",
            "-t",
            &job.tmux_session,
            "remain-on-exit",
            "on",
        ])?;
        let log_path = job_dir.join("output.log");
        fs::write(&log_path, [])?;
        set_file_permissions(&log_path)?;
        let pipe_command = format!("cat >> {}", shell_quote(&log_path));
        self.tmux_checked(["pipe-pane", "-t", &job.tmux_session, "-o", &pipe_command])?;
        let mut args = vec![
            "respawn-pane".to_owned(),
            "-k".to_owned(),
            "-t".to_owned(),
            job.tmux_session.clone(),
            "--".to_owned(),
        ];
        args.extend(job.command.iter().cloned());
        self.tmux_checked(args.iter().map(String::as_str))?;
        Ok(())
    }

    fn refresh_job(&self, job_dir: &Path) -> Result<Job, JobError> {
        let lock = open_lock(job_dir)?;
        lock.lock_exclusive()?;
        let mut job = read_job(job_dir)?;
        if job.state != JobState::Running {
            return Ok(job);
        }
        let pane = self.inspect_pane(&job.tmux_session)?;
        if !pane.exists {
            job.state = JobState::Lost;
            job.pid = None;
            job.finished_at_ms = Some(now_ms()?);
        } else if pane.dead {
            job.state = JobState::Exited;
            job.exit_code = pane.exit_code;
            job.pid = None;
            job.finished_at_ms = Some(now_ms()?);
        } else {
            job.pid = pane.pid;
        }
        write_json_atomically(&job_dir.join(METADATA_FILE), &job)?;
        Ok(job)
    }

    fn inspect_pane(&self, session: &str) -> Result<PaneState, JobError> {
        if !self
            .tmux_output(["has-session", "-t", session])?
            .status
            .success()
        {
            return Ok(PaneState {
                exists: false,
                dead: false,
                exit_code: None,
                pid: None,
            });
        }
        let output = self.tmux_checked([
            "display-message",
            "-p",
            "-t",
            session,
            "#{pane_dead}\t#{pane_dead_status}\t#{pane_pid}",
        ])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut fields = stdout.trim().split('\t');
        let dead = fields.next() == Some("1");
        let exit_code = fields.next().and_then(|value| value.parse().ok());
        let pid = fields.next().and_then(|value| value.parse().ok());
        Ok(PaneState {
            exists: true,
            dead,
            exit_code,
            pid,
        })
    }

    fn tmux_checked<'a>(
        &self,
        args: impl IntoIterator<Item = &'a str>,
    ) -> Result<Output, JobError> {
        let output = self.tmux_output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(JobError::Tmux(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }

    fn tmux_output<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> Result<Output, JobError> {
        Ok(Command::new("tmux")
            .args(["-L", &self.tmux_socket])
            .args(args)
            .output()?)
    }

    fn job_dir(&self, project_id: &str, job_id: Uuid) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id)
            .join("jobs")
            .join(job_id.to_string())
    }
}

#[derive(Debug)]
struct PaneState {
    exists: bool,
    dead: bool,
    exit_code: Option<i32>,
    pid: Option<u32>,
}

fn read_job(directory: &Path) -> Result<Job, JobError> {
    let path = directory.join(METADATA_FILE);
    let contents = fs::read(&path)?;
    Ok(serde_json::from_slice(&contents)?)
}

fn write_json_atomically(path: &Path, value: &Job) -> Result<(), JobError> {
    let parent = path
        .parent()
        .ok_or_else(|| JobError::InvalidMetadataPath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{METADATA_FILE}.{}.tmp", Uuid::now_v7()));
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    set_file_permissions(&temporary)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn open_lock(directory: &Path) -> Result<File, JobError> {
    let path = directory.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    set_file_permissions(&path)?;
    Ok(file)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn now_ms() -> Result<u64, JobError> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis).map_err(|_| JobError::TimestampOutOfRange)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), JobError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), JobError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), JobError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), JobError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum JobError {
    #[error("XDG_RUNTIME_DIR is not set; set it or PAJ_RUNTIME_DIR")]
    MissingRuntimeDirectory,
    #[error("job name cannot be empty")]
    EmptyName,
    #[error("job command cannot be empty")]
    EmptyCommand,
    #[error("a running job named {0} already exists in this project")]
    DuplicateName(String),
    #[error("job {0} was not found")]
    NotFound(String),
    #[error("multiple jobs match {0}")]
    Ambiguous(String),
    #[error("tmux failed: {0}")]
    Tmux(String),
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
    use std::thread;
    use std::time::Duration;

    use tempfile::tempdir;
    use uuid::Uuid;

    use crate::project::Project;

    use super::{JobManager, JobState};

    fn project() -> Project {
        Project {
            id: "project-id".to_owned(),
            root: "/project".into(),
        }
    }

    fn manager(root: PathBuf) -> JobManager {
        JobManager::new(root, format!("paj-test-{}", Uuid::now_v7()))
            .expect("job manager should be created")
    }

    use std::path::PathBuf;

    #[test]
    fn start_captures_output_and_exit_status() {
        if std::env::var_os("PAJ_SKIP_TMUX_TESTS").is_some() {
            return;
        }
        let directory = tempdir().expect("temporary directory should be created");
        let manager = manager(directory.path().join("paj"));
        let job = manager
            .start(
                &project(),
                "echo".to_owned(),
                None,
                directory.path().to_path_buf(),
                vec!["sh".to_owned(), "-c".to_owned(), "printf hello".to_owned()],
            )
            .expect("job should start");

        let mut completed = None;
        for _ in 0..200 {
            let current = manager
                .resolve("project-id", "echo")
                .expect("job should resolve");
            if current.state == JobState::Exited {
                completed = Some(current);
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let completed = completed.expect("job should exit");
        let output = fs::read_to_string(manager.log_path(&job)).expect("log should be readable");
        manager.stop(&job).expect("job session should be removed");

        assert_eq!((completed.exit_code, output), (Some(0), "hello".to_owned()));
    }

    use std::fs;

    #[test]
    fn stop_terminates_running_job() {
        if std::env::var_os("PAJ_SKIP_TMUX_TESTS").is_some() {
            return;
        }
        let directory = tempdir().expect("temporary directory should be created");
        let manager = manager(directory.path().join("paj"));
        let job = manager
            .start(
                &project(),
                "sleep".to_owned(),
                None,
                directory.path().to_path_buf(),
                vec!["sleep".to_owned(), "60".to_owned()],
            )
            .expect("job should start");

        let stopped = manager.stop(&job).expect("job should stop");

        assert_eq!(stopped.state, JobState::Stopped);
    }
}
