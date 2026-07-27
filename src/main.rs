use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use paj::agents::{AgentRunRequest, AgentRunState, AgentRunner};
use paj::jobs::{Job, JobManager};
use paj::project::Project;
use paj::registry::{Message, Registration, Registry, Session};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(version, about = "Local runtime and toolbox for the Pi coding agent")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    Message {
        #[command(subcommand)]
        command: MessageCommands,
    },
    Job {
        #[command(subcommand)]
        command: JobCommands,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    Gc {
        #[arg(long, default_value_t = 60)]
        stale_after: u64,
    },
}

#[derive(Debug, Subcommand)]
enum SessionCommands {
    Register {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        pi_session_id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "primary")]
        role: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    Heartbeat {
        id: Uuid,
    },
    Unregister {
        id: Uuid,
    },
    List {
        #[arg(long)]
        all: bool,
    },
    Show {
        id: Uuid,
    },
}

#[derive(Debug, Subcommand)]
enum MessageCommands {
    Send {
        recipient: String,
        #[arg(long)]
        from: Uuid,
        #[arg(long)]
        text: String,
    },
    Pending {
        session: Uuid,
    },
    Ack {
        session: Uuid,
        message: Uuid,
    },
}

#[derive(Debug, Subcommand)]
enum JobCommands {
    Start {
        #[arg(long)]
        name: String,
        #[arg(long)]
        owner: Option<Uuid>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    List {
        #[arg(long)]
        all: bool,
    },
    Status {
        job: String,
    },
    Log {
        job: String,
        #[arg(long, default_value_t = 200)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    Send {
        job: String,
        input: String,
        #[arg(long)]
        no_enter: bool,
    },
    Interrupt {
        job: String,
    },
    Stop {
        job: String,
    },
    Attach {
        job: String,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommands {
    Run {
        #[arg(long)]
        role: String,
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        #[arg(long, conflicts_with = "prompt")]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        artifact: Option<PathBuf>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        thinking: Option<String>,
        #[arg(long)]
        allow_write: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let registry = Registry::from_environment()?;

    match cli.command {
        Commands::Session { command } => run_session_command(&registry, command, cli.json),
        Commands::Message { command } => run_message_command(&registry, command, cli.json),
        Commands::Job { command } => {
            let manager = JobManager::from_environment()?;
            run_job_command(&manager, command, cli.json)
        }
        Commands::Agent { command } => {
            let runner = AgentRunner::from_environment()?;
            run_agent_command(&runner, command, cli.json)
        }
        Commands::Gc { stale_after } => {
            let removed = registry.gc(Duration::from_secs(stale_after))?;
            print_sessions(&removed, cli.json, "No stale sessions found")
        }
    }
}

fn run_session_command(registry: &Registry, command: SessionCommands, json: bool) -> Result<()> {
    match command {
        SessionCommands::Register {
            pid,
            pi_session_id,
            name,
            role,
            task,
            cwd,
        } => {
            let cwd = cwd.map_or_else(env::current_dir, Ok)?;
            let project = Project::discover(&cwd)?;
            let registration = Registration {
                pid,
                pi_session_id,
                name,
                role,
                task,
                branch: git_branch(&project.root),
                cwd: cwd
                    .canonicalize()
                    .context("failed to resolve working directory")?,
            };
            let session = registry.register(&project, registration)?;
            if json {
                print_json(&session)
            } else {
                println!("{}\t{}", session.id, session.name);
                Ok(())
            }
        }
        SessionCommands::Heartbeat { id } => {
            let session = registry.heartbeat(id)?;
            if json { print_json(&session) } else { Ok(()) }
        }
        SessionCommands::Unregister { id } => {
            let session = registry.unregister(id)?;
            if json { print_json(&session) } else { Ok(()) }
        }
        SessionCommands::List { all } => {
            let project = if all {
                None
            } else {
                Some(Project::discover(&env::current_dir()?)?)
            };
            let sessions = registry.list_live(
                project.as_ref().map(|project| project.id.as_str()),
                Duration::from_secs(60),
            )?;
            print_sessions(&sessions, json, "No sessions found")
        }
        SessionCommands::Show { id } => {
            let session = registry.show(id)?;
            if json {
                print_json(&session)
            } else {
                print_session(&session);
                Ok(())
            }
        }
    }
}

fn run_message_command(registry: &Registry, command: MessageCommands, json: bool) -> Result<()> {
    match command {
        MessageCommands::Send {
            recipient,
            from,
            text,
        } => {
            let message = registry.send_message(from, &recipient, text)?;
            if json {
                print_json(&message)
            } else {
                println!("{}", message.id);
                Ok(())
            }
        }
        MessageCommands::Pending { session } => {
            let messages = registry.pending_messages(session)?;
            if json {
                print_json(&messages)
            } else {
                print_messages(&messages);
                Ok(())
            }
        }
        MessageCommands::Ack { session, message } => {
            registry.acknowledge_message(session, message)?;
            Ok(())
        }
    }
}

fn run_agent_command(runner: &AgentRunner, command: AgentCommands, json: bool) -> Result<()> {
    match command {
        AgentCommands::Run {
            role,
            prompt,
            prompt_file,
            artifact,
            cwd,
            provider,
            model,
            thinking,
            allow_write,
        } => {
            let prompt = match (prompt, prompt_file) {
                (Some(prompt), None) => prompt,
                (None, Some(path)) => std::fs::read_to_string(path)?,
                (None, None) => {
                    return Err(anyhow::anyhow!("--prompt or --prompt-file is required"));
                }
                (Some(_), Some(_)) => unreachable!(),
            };
            let cwd = cwd.unwrap_or(env::current_dir()?).canonicalize()?;
            let project = Project::discover(&cwd)?;
            let run = runner.run(
                &project,
                AgentRunRequest {
                    role,
                    prompt,
                    cwd,
                    artifact_path: artifact,
                    provider,
                    model,
                    thinking,
                    allow_write,
                },
            )?;
            if json {
                print_json(&run)?;
            } else if let Some(artifact) = &run.artifact_path {
                println!("{}\t{}", run.id, artifact.display());
            } else {
                print!("{}", std::fs::read_to_string(&run.output_path)?);
            }
            if run.state == AgentRunState::Failed {
                return Err(anyhow::anyhow!(
                    "subagent failed with exit code {:?}; stderr: {}",
                    run.exit_code,
                    run.stderr_path.display()
                ));
            }
            Ok(())
        }
    }
}

fn run_job_command(manager: &JobManager, command: JobCommands, json: bool) -> Result<()> {
    let current_dir = env::current_dir()?;
    let project = Project::discover(&current_dir)?;
    match command {
        JobCommands::Start {
            name,
            owner,
            cwd,
            command,
        } => {
            let cwd = cwd.unwrap_or(current_dir).canonicalize()?;
            let job = manager.start(&project, name, owner, cwd, command)?;
            if json {
                print_json(&job)
            } else {
                println!("{}\t{}", job.id, job.name);
                Ok(())
            }
        }
        JobCommands::List { all } => {
            let jobs = manager.list((!all).then_some(project.id.as_str()))?;
            if json {
                print_json(&jobs)
            } else {
                print_jobs(&jobs);
                Ok(())
            }
        }
        JobCommands::Status { job } => {
            let job = manager.resolve(&project.id, &job)?;
            if json {
                print_json(&job)
            } else {
                print_jobs(&[job]);
                Ok(())
            }
        }
        JobCommands::Log { job, lines, follow } => {
            let job = manager.resolve(&project.id, &job)?;
            let mut tail = Command::new("tail");
            if follow {
                tail.arg("-f");
            }
            let status = tail
                .args(["-n", &lines.to_string()])
                .arg(manager.log_path(&job))
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("tail exited with {status}"))
            }
        }
        JobCommands::Send {
            job,
            input,
            no_enter,
        } => {
            let job = manager.resolve(&project.id, &job)?;
            manager.send(&job, &input, !no_enter)?;
            Ok(())
        }
        JobCommands::Interrupt { job } => {
            let job = manager.resolve(&project.id, &job)?;
            manager.interrupt(&job)?;
            Ok(())
        }
        JobCommands::Stop { job } => {
            let job = manager.resolve(&project.id, &job)?;
            let stopped = manager.stop(&job)?;
            if json { print_json(&stopped) } else { Ok(()) }
        }
        JobCommands::Attach { job } => {
            let job = manager.resolve(&project.id, &job)?;
            manager.attach(&job)?;
            Ok(())
        }
    }
}

fn git_branch(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout).ok()?;
    let branch = branch.trim();
    (!branch.is_empty()).then(|| branch.to_owned())
}

fn print_sessions(sessions: &[Session], json: bool, empty_message: &str) -> Result<()> {
    if json {
        return print_json(sessions);
    }
    if sessions.is_empty() {
        println!("{empty_message}");
        return Ok(());
    }

    for session in sessions {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            session.id,
            session.name,
            session.status,
            session.branch.as_deref().unwrap_or("-"),
            session.project_root.display()
        );
    }
    Ok(())
}

fn print_jobs(jobs: &[Job]) {
    for job in jobs {
        println!(
            "{}\t{}\t{:?}\t{}",
            job.id,
            job.name,
            job.state,
            job.command.join(" ")
        );
    }
}

fn print_messages(messages: &[Message]) {
    for message in messages {
        println!("{}\t{}\t{}", message.id, message.from.name, message.text);
    }
}

fn print_session(session: &Session) {
    println!("id: {}", session.id);
    println!("name: {}", session.name);
    println!("pid: {}", session.pid);
    println!(
        "pi session: {}",
        session.pi_session_id.as_deref().unwrap_or("-")
    );
    println!("project: {}", session.project_root.display());
    println!("cwd: {}", session.cwd.display());
    println!("branch: {}", session.branch.as_deref().unwrap_or("-"));
    println!("role: {}", session.role);
    println!("status: {}", session.status);
    println!("task: {}", session.task.as_deref().unwrap_or("-"));
}

fn print_json(value: &(impl Serialize + ?Sized)) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
