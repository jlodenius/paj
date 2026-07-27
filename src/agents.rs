use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::project::Project;

const METADATA_FILE: &str = "metadata.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRunState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub id: Uuid,
    pub role: String,
    pub project_id: String,
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    pub prompt_path: PathBuf,
    pub output_path: PathBuf,
    pub stderr_path: PathBuf,
    pub artifact_path: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub state: AgentRunState,
    pub exit_code: Option<i32>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

#[derive(Debug)]
pub struct AgentRunRequest {
    pub role: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub artifact_path: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub allow_write: bool,
}

#[derive(Debug)]
pub struct AgentRunner {
    root: PathBuf,
    pi_command: OsString,
}

impl AgentRunner {
    pub fn from_environment() -> Result<Self, AgentError> {
        let root = if let Some(root) = env::var_os("PAJ_RUNTIME_DIR") {
            PathBuf::from(root)
        } else {
            let runtime =
                env::var_os("XDG_RUNTIME_DIR").ok_or(AgentError::MissingRuntimeDirectory)?;
            PathBuf::from(runtime).join("paj")
        };
        Self::new(root, OsString::from("pi"))
    }

    pub fn new(root: PathBuf, pi_command: OsString) -> Result<Self, AgentError> {
        fs::create_dir_all(root.join("projects"))?;
        set_directory_permissions(&root)?;
        Ok(Self { root, pi_command })
    }

    pub fn run(&self, project: &Project, request: AgentRunRequest) -> Result<AgentRun, AgentError> {
        if request.role.trim().is_empty() {
            return Err(AgentError::EmptyRole);
        }
        if request.prompt.trim().is_empty() {
            return Err(AgentError::EmptyPrompt);
        }

        let id = Uuid::now_v7();
        let run_dir = self.run_dir(&project.id, id);
        fs::create_dir_all(&run_dir)?;
        let prompt_path = run_dir.join("prompt.md");
        let output_path = run_dir.join("output.md");
        let stderr_path = run_dir.join("stderr.log");
        let effective_prompt =
            effective_prompt(&request.role, &request.prompt, request.allow_write);
        write_private(&prompt_path, effective_prompt.as_bytes())?;
        let stdout = create_private_file(&output_path)?;
        let stderr = create_private_file(&stderr_path)?;
        let artifact_path = request
            .artifact_path
            .map(|path| absolute_path(&request.cwd, path))
            .transpose()?;
        let started_at_ms = now_ms()?;
        let mut run = AgentRun {
            id,
            role: request.role.clone(),
            project_id: project.id.clone(),
            project_root: project.root.clone(),
            cwd: request.cwd.clone(),
            prompt_path,
            output_path: output_path.clone(),
            stderr_path,
            artifact_path: artifact_path.clone(),
            provider: request.provider.clone(),
            model: request.model.clone(),
            thinking: request.thinking.clone(),
            state: AgentRunState::Running,
            exit_code: None,
            started_at_ms,
            finished_at_ms: None,
        };
        write_metadata(&run_dir, &run)?;

        let mut command = Command::new(&self.pi_command);
        command
            .args(["--print", "--no-session"])
            .current_dir(&request.cwd)
            .env("PAJ_ROLE", &request.role)
            .env(
                "PAJ_TASK",
                request.prompt.lines().next().unwrap_or_default(),
            )
            .env("PAJ_AGENT_NAME", agent_name(&request.role, id))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if !request.allow_write {
            command.args(["--tools", "read,bash"]);
        }
        if let Some(provider) = &request.provider {
            command.args(["--provider", provider]);
        }
        if let Some(model) = &request.model {
            command.args(["--model", model]);
        }
        if let Some(thinking) = &request.thinking {
            command.args(["--thinking", thinking]);
        }
        command.arg(effective_prompt);

        let status = match command.status() {
            Ok(status) => status,
            Err(source) => {
                run.state = AgentRunState::Failed;
                run.finished_at_ms = Some(now_ms()?);
                write_metadata(&run_dir, &run)?;
                return Err(AgentError::Io(source));
            }
        };
        run.exit_code = status.code();
        run.state = if status.success() {
            AgentRunState::Completed
        } else {
            AgentRunState::Failed
        };
        run.finished_at_ms = Some(now_ms()?);
        if let Some(artifact_path) = artifact_path {
            if let Some(parent) = artifact_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&output_path, artifact_path)?;
        }
        write_metadata(&run_dir, &run)?;
        Ok(run)
    }

    fn run_dir(&self, project_id: &str, run_id: Uuid) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id)
            .join("agents")
            .join(run_id.to_string())
    }
}

fn effective_prompt(role: &str, prompt: &str, allow_write: bool) -> String {
    let access = if allow_write {
        "You may modify files when required by the task."
    } else {
        "Work read-only. Do not modify files, create commits, or change repository state."
    };
    format!(
        "You are a foreground {role} subagent.\n{access}\nReturn self-contained findings with concrete file paths and line references.\n\nTask:\n{prompt}"
    )
}

fn agent_name(role: &str, id: Uuid) -> String {
    let simple_id = id.simple().to_string();
    format!("{role}-{}", &simple_id[simple_id.len() - 8..])
}

fn absolute_path(cwd: &Path, path: PathBuf) -> Result<PathBuf, AgentError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(cwd.join(path))
    }
}

fn write_metadata(directory: &Path, run: &AgentRun) -> Result<(), AgentError> {
    let path = directory.join(METADATA_FILE);
    let temporary = directory.join(format!(".{METADATA_FILE}.{}.tmp", Uuid::now_v7()));
    write_private(&temporary, &serde_json::to_vec_pretty(run)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File, AgentError> {
    let file = File::create(path)?;
    set_file_permissions(path)?;
    Ok(file)
}

fn write_private(path: &Path, contents: &[u8]) -> Result<(), AgentError> {
    fs::write(path, contents)?;
    set_file_permissions(path)?;
    Ok(())
}

fn now_ms() -> Result<u64, AgentError> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis).map_err(|_| AgentError::TimestampOutOfRange)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), AgentError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), AgentError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), AgentError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), AgentError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("XDG_RUNTIME_DIR is not set; set it or PAJ_RUNTIME_DIR")]
    MissingRuntimeDirectory,
    #[error("agent role cannot be empty")]
    EmptyRole,
    #[error("agent prompt cannot be empty")]
    EmptyPrompt,
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
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use crate::project::Project;

    use super::{AgentRunRequest, AgentRunState, AgentRunner};

    #[test]
    fn run_captures_output_and_writes_artifact() {
        let directory = tempdir().expect("temporary directory should be created");
        let pi = directory.path().join("fake-pi");
        fs::write(&pi, "#!/bin/sh\nprintf 'review complete'").expect("fake pi should be written");
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o700))
            .expect("fake pi should be executable");
        let runner = AgentRunner::new(directory.path().join("runtime"), pi.into_os_string())
            .expect("runner should be created");
        let project = Project {
            id: "project-id".to_owned(),
            root: directory.path().to_path_buf(),
        };
        let artifact = directory.path().join("review.md");

        let run = runner
            .run(
                &project,
                AgentRunRequest {
                    role: "review".to_owned(),
                    prompt: "Review the code".to_owned(),
                    cwd: directory.path().to_path_buf(),
                    artifact_path: Some(artifact.clone()),
                    provider: None,
                    model: None,
                    thinking: None,
                    allow_write: false,
                },
            )
            .expect("agent should run");

        assert_eq!(
            (run.state, fs::read_to_string(artifact).ok()),
            (AgentRunState::Completed, Some("review complete".to_owned()))
        );
    }
}
