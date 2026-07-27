use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::jobs::{JobError, JobManager, JobState};
use crate::project::Project;

const METADATA_FILE: &str = "spawn.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundAgent {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub parent_session_id: Option<Uuid>,
    pub project_id: String,
    pub project_root: PathBuf,
    pub branch: String,
    pub worktree: PathBuf,
    pub job_id: Uuid,
    pub job_name: String,
    pub state: JobState,
    pub prompt_path: PathBuf,
    pub created_at_ms: u64,
}

#[derive(Debug)]
pub struct SpawnRequest {
    pub name: Option<String>,
    pub role: String,
    pub parent_session_id: Option<Uuid>,
    pub branch: String,
    pub worktree: Option<PathBuf>,
    pub prompt: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

#[derive(Debug)]
pub struct BackgroundAgentManager {
    state_root: PathBuf,
    jobs: JobManager,
    pi_command: OsString,
}

impl BackgroundAgentManager {
    pub fn from_environment() -> Result<Self, BackgroundAgentError> {
        let runtime_root = if let Some(root) = env::var_os("PAJ_RUNTIME_DIR") {
            PathBuf::from(root)
        } else {
            let runtime = env::var_os("XDG_RUNTIME_DIR")
                .ok_or(BackgroundAgentError::MissingRuntimeDirectory)?;
            PathBuf::from(runtime).join("paj")
        };
        let state_root = if let Some(root) = env::var_os("PAJ_STATE_DIR") {
            PathBuf::from(root)
        } else if let Some(root) = env::var_os("XDG_STATE_HOME") {
            PathBuf::from(root).join("paj")
        } else {
            let home = env::var_os("HOME").ok_or(BackgroundAgentError::MissingHomeDirectory)?;
            PathBuf::from(home).join(".local/state/paj")
        };
        Self::new(
            runtime_root,
            state_root,
            "paj".to_owned(),
            OsString::from("pi"),
        )
    }

    pub fn new(
        runtime_root: PathBuf,
        state_root: PathBuf,
        tmux_socket: String,
        pi_command: OsString,
    ) -> Result<Self, BackgroundAgentError> {
        fs::create_dir_all(&runtime_root)?;
        fs::create_dir_all(&state_root)?;
        let jobs = JobManager::new(runtime_root.clone(), tmux_socket)?;
        Ok(Self {
            state_root,
            jobs,
            pi_command,
        })
    }

    pub fn spawn(
        &self,
        project: &Project,
        request: SpawnRequest,
    ) -> Result<BackgroundAgent, BackgroundAgentError> {
        validate_request(project, &request)?;
        let id = Uuid::now_v7();
        let name = request
            .name
            .unwrap_or_else(|| format!("{}-{}", request.role, short_id(id)));
        let worktree = request
            .worktree
            .unwrap_or_else(|| self.auto_worktree(project, id));
        if worktree.exists() {
            return Err(BackgroundAgentError::WorktreeExists(worktree));
        }
        create_worktree(project, &request.branch, &worktree)?;

        let agent_dir = self.agent_dir(&project.id, id);
        fs::create_dir_all(&agent_dir)?;
        let prompt_path = agent_dir.join("prompt.md");
        let prompt = implementation_prompt(
            &name,
            &request.branch,
            &worktree,
            request.parent_session_id,
            &request.prompt,
        );
        fs::write(&prompt_path, &prompt)?;
        let mut command = vec![
            "env".to_owned(),
            format!("PAJ_ROLE={}", request.role),
            format!("PAJ_AGENT_NAME={name}"),
            format!("PAJ_TASK={}", first_line(&request.prompt)),
            self.pi_command.to_string_lossy().into_owned(),
            "--name".to_owned(),
            name.clone(),
        ];
        if let Some(model) = request.model {
            command.extend(["--model".to_owned(), model]);
        }
        if let Some(thinking) = request.thinking {
            command.extend(["--thinking".to_owned(), thinking]);
        }
        command.push(prompt);

        let job = match self.jobs.start(
            project,
            format!("agent-{name}"),
            request.parent_session_id,
            worktree.clone(),
            command,
        ) {
            Ok(job) => job,
            Err(error) => {
                let _ = remove_worktree(project, &worktree, true);
                let _ = fs::remove_dir_all(agent_dir);
                return Err(error.into());
            }
        };
        let agent = BackgroundAgent {
            id,
            name,
            role: request.role,
            parent_session_id: request.parent_session_id,
            project_id: project.id.clone(),
            project_root: project.root.clone(),
            branch: request.branch,
            worktree,
            job_id: job.id,
            job_name: job.name,
            state: job.state,
            prompt_path,
            created_at_ms: now_ms()?,
        };
        write_metadata(&agent_dir, &agent)?;
        Ok(agent)
    }

    pub fn list(&self, project_id: &str) -> Result<Vec<BackgroundAgent>, BackgroundAgentError> {
        let agents_dir = self.state_root.join("agents").join(project_id);
        let Ok(entries) = fs::read_dir(agents_dir) else {
            return Ok(Vec::new());
        };
        let mut agents = Vec::new();
        for entry in entries {
            let path = entry?.path().join(METADATA_FILE);
            if !path.is_file() {
                continue;
            }
            let mut agent: BackgroundAgent = serde_json::from_slice(&fs::read(path)?)?;
            match self.jobs.resolve(project_id, &agent.job_id.to_string()) {
                Ok(job) => agent.state = job.state,
                Err(JobError::NotFound(_)) => agent.state = JobState::Lost,
                Err(error) => return Err(error.into()),
            }
            agents.push(agent);
        }
        agents.sort_by_key(|agent| agent.created_at_ms);
        Ok(agents)
    }

    pub fn resolve(
        &self,
        project_id: &str,
        reference: &str,
    ) -> Result<BackgroundAgent, BackgroundAgentError> {
        let matches = self
            .list(project_id)?
            .into_iter()
            .filter(|agent| agent.name == reference || agent.id.to_string().starts_with(reference))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Err(BackgroundAgentError::NotFound(reference.to_owned())),
            [agent] => Ok(agent.clone()),
            _ => Err(BackgroundAgentError::Ambiguous(reference.to_owned())),
        }
    }

    pub fn stop(&self, agent: &BackgroundAgent) -> Result<(), BackgroundAgentError> {
        match self
            .jobs
            .resolve(&agent.project_id, &agent.job_id.to_string())
        {
            Ok(job) => {
                self.jobs.stop(&job)?;
                Ok(())
            }
            Err(JobError::NotFound(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn attach(&self, agent: &BackgroundAgent) -> Result<(), BackgroundAgentError> {
        let job = self
            .jobs
            .resolve(&agent.project_id, &agent.job_id.to_string())?;
        self.jobs.attach(&job)?;
        Ok(())
    }

    pub fn remove(&self, agent: &BackgroundAgent, force: bool) -> Result<(), BackgroundAgentError> {
        self.stop(agent)?;
        remove_worktree(
            &Project {
                id: agent.project_id.clone(),
                root: agent.project_root.clone(),
            },
            &agent.worktree,
            force,
        )?;
        if let Ok(job) = self
            .jobs
            .resolve(&agent.project_id, &agent.job_id.to_string())
        {
            self.jobs.remove(&job)?;
        }
        let agent_dir = self.agent_dir(&agent.project_id, agent.id);
        if agent_dir.exists() {
            fs::remove_dir_all(agent_dir)?;
        }
        Ok(())
    }

    fn auto_worktree(&self, project: &Project, id: Uuid) -> PathBuf {
        self.state_root
            .join("worktrees")
            .join(&project.id)
            .join(short_id(id))
    }

    fn agent_dir(&self, project_id: &str, id: Uuid) -> PathBuf {
        self.state_root
            .join("agents")
            .join(project_id)
            .join(id.to_string())
    }
}

fn validate_request(project: &Project, request: &SpawnRequest) -> Result<(), BackgroundAgentError> {
    if request.role.trim().is_empty() {
        return Err(BackgroundAgentError::EmptyRole);
    }
    if request.branch.trim().is_empty() {
        return Err(BackgroundAgentError::EmptyBranch);
    }
    if request.prompt.trim().is_empty() {
        return Err(BackgroundAgentError::EmptyPrompt);
    }
    git_checked(
        &project.root,
        ["check-ref-format", "--branch", &request.branch],
    )?;
    git_checked(&project.root, ["rev-parse", "--show-toplevel"])?;
    Ok(())
}

fn create_worktree(
    project: &Project,
    branch: &str,
    worktree: &Path,
) -> Result<(), BackgroundAgentError> {
    if let Some(parent) = worktree.parent() {
        fs::create_dir_all(parent)?;
    }
    let path = worktree.to_string_lossy();
    let branch_exists = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(&project.root)
        .status()?
        .success();
    if branch_exists {
        git_checked(&project.root, ["worktree", "add", &path, branch])?;
    } else {
        git_checked(
            &project.root,
            ["worktree", "add", "-b", branch, &path, "HEAD"],
        )?;
    }
    Ok(())
}

fn remove_worktree(
    project: &Project,
    worktree: &Path,
    force: bool,
) -> Result<(), BackgroundAgentError> {
    let path = worktree.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    git_checked(&project.root, args)?;
    Ok(())
}

fn git_checked<'a>(
    cwd: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<(), BackgroundAgentError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(BackgroundAgentError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn implementation_prompt(
    name: &str,
    branch: &str,
    worktree: &Path,
    parent: Option<Uuid>,
    task: &str,
) -> String {
    let completion = parent.map_or_else(
        || "Report completion in your final response.".to_owned(),
        |id| format!("When finished, send a concise completion message to parent session {id}."),
    );
    format!(
        "You are background implementation agent {name}.\nWork only in {} on branch {branch}.\nImplement the task end-to-end, run relevant checks, and commit coherent chunks.\nNever modify the parent worktree.\n{completion}\n\nTask:\n{task}",
        worktree.display()
    )
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

fn short_id(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

fn write_metadata(directory: &Path, agent: &BackgroundAgent) -> Result<(), BackgroundAgentError> {
    let path = directory.join(METADATA_FILE);
    let temporary = directory.join(format!(".{METADATA_FILE}.{}.tmp", Uuid::now_v7()));
    fs::write(&temporary, serde_json::to_vec_pretty(agent)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn now_ms() -> Result<u64, BackgroundAgentError> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis).map_err(|_| BackgroundAgentError::TimestampOutOfRange)
}

#[derive(Debug, Error)]
pub enum BackgroundAgentError {
    #[error("XDG_RUNTIME_DIR is not set; set it or PAJ_RUNTIME_DIR")]
    MissingRuntimeDirectory,
    #[error("HOME is not set; set it, XDG_STATE_HOME, or PAJ_STATE_DIR")]
    MissingHomeDirectory,
    #[error("agent role cannot be empty")]
    EmptyRole,
    #[error("agent branch cannot be empty")]
    EmptyBranch,
    #[error("agent prompt cannot be empty")]
    EmptyPrompt,
    #[error("worktree already exists at {0}")]
    WorktreeExists(PathBuf),
    #[error("background agent {0} was not found")]
    NotFound(String),
    #[error("multiple background agents match {0}")]
    Ambiguous(String),
    #[error("git failed: {0}")]
    Git(String),
    #[error("system clock is earlier than the Unix epoch")]
    InvalidSystemTime(#[from] SystemTimeError),
    #[error("timestamp does not fit in a 64-bit integer")]
    TimestampOutOfRange,
    #[error(transparent)]
    Job(#[from] crate::jobs::JobError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use tempfile::tempdir;
    use uuid::Uuid;

    use crate::project::Project;

    use super::{BackgroundAgentManager, SpawnRequest};

    #[test]
    fn spawn_creates_isolated_worktree_and_branch() {
        if std::env::var_os("PAJ_SKIP_TMUX_TESTS").is_some() {
            return;
        }
        let directory = tempdir().expect("temporary directory should be created");
        let repository = directory.path().join("repository");
        fs::create_dir_all(&repository).expect("repository should be created");
        git(&repository, ["init", "-q"]);
        git(&repository, ["config", "user.email", "test@example.com"]);
        git(&repository, ["config", "user.name", "Test"]);
        fs::write(repository.join("README.md"), "test").expect("file should be written");
        git(&repository, ["add", "README.md"]);
        git(&repository, ["commit", "-qm", "initial"]);
        let pi = directory.path().join("fake-pi");
        fs::write(&pi, "#!/bin/sh\nsleep 60").expect("fake pi should be written");
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o700))
            .expect("fake pi should be executable");
        let manager = BackgroundAgentManager::new(
            directory.path().join("runtime"),
            directory.path().join("state"),
            format!("paj-test-{}", Uuid::now_v7()),
            pi.into_os_string(),
        )
        .expect("manager should be created");
        let project = Project::discover(&repository).expect("project should be discovered");

        let agent = manager
            .spawn(
                &project,
                SpawnRequest {
                    name: Some("implementation".to_owned()),
                    role: "implementation".to_owned(),
                    parent_session_id: None,
                    branch: "feature/test".to_owned(),
                    worktree: None,
                    prompt: "Implement the test".to_owned(),
                    model: None,
                    thinking: None,
                },
            )
            .expect("agent should spawn");

        assert!(agent.worktree.join("README.md").is_file());

        manager
            .remove(&agent, true)
            .expect("agent should be removed");
    }

    fn git<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git should run");
        assert!(status.success());
    }

    use std::path::Path;
}
