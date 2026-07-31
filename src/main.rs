use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use paj::bridge::{BridgeClient, BridgeEvent, bridge_is_available};
use paj::project::Project;
use paj::registry::{Message, Registration, Registry, Session};
use paj::subagent::{SpawnRecord, SpawnStore};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local session discovery, messaging, and editor bridge for Pi coding agents"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Register, inspect, and remove Pi sessions.
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Send and acknowledge messages between live agents.
    Message {
        #[command(subcommand)]
        command: MessageCommands,
    },
    /// Inspect and prompt a Pi session's editor bridge.
    Bridge {
        #[command(subcommand)]
        command: BridgeCommands,
    },
    /// Resolve an exact project reference using configured search roots.
    Project {
        #[command(subcommand)]
        command: ProjectCommands,
    },
    /// Manage private tmux subagent spawn records.
    Subagent {
        #[command(subcommand)]
        command: SubagentCommands,
    },
    /// Remove stale sessions and orphaned owned tmux subagents.
    Gc {
        #[arg(long, default_value_t = 60)]
        stale_after: u64,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommands {
    /// Resolve a project name, relative suffix, or direct path without guessing.
    Resolve { reference: String },
}

#[derive(Debug, Subcommand)]
enum SubagentCommands {
    /// Create a spawn record before launching tmux.
    Create {
        #[arg(long)]
        parent_pi_session_id: String,
        #[arg(long)]
        parent_pid: u32,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        task_file: PathBuf,
    },
    /// Bind a registered child Pi/Paj identity to its spawn record.
    Bind {
        spawn_id: Uuid,
        #[arg(long)]
        child_pi_session_id: String,
        #[arg(long)]
        child_paj_session_id: Uuid,
        #[arg(long)]
        name: String,
    },
    /// List active spawn records, optionally scoped to a stable parent Pi ID.
    List {
        #[arg(long)]
        parent_pi_session_id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show a spawn record.
    Show { spawn_id: Uuid },
    /// Remove a spawn record after its tmux session has stopped.
    Remove { spawn_id: Uuid },
}

#[derive(Debug, Subcommand)]
enum SessionCommands {
    /// Register a process as a Pi session in the current project.
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
        parent_pi_session_id: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Refresh a registered session's liveness timestamp.
    Heartbeat { id: Uuid },
    /// Change a registered session's agent name.
    Rename { id: Uuid, name: String },
    /// Change whether a registered session is idle or busy.
    Status {
        id: Uuid,
        #[arg(value_parser = ["idle", "busy"])]
        status: String,
    },
    /// Remove a session and its pending messages from the registry.
    Unregister { id: Uuid },
    /// List live sessions in the current project or all projects.
    List {
        #[arg(long)]
        all: bool,
    },
    /// Show all metadata for a registered session.
    Show { id: Uuid },
}

#[derive(Debug, Subcommand)]
enum MessageCommands {
    /// Queue a message by name, Paj ID prefix, or exact Pi session ID.
    Send {
        recipient: String,
        #[arg(long)]
        from: Uuid,
        #[arg(long)]
        text: String,
    },
    /// List messages awaiting acknowledgement by a session.
    Pending { session: Uuid },
    /// Acknowledge and remove a pending message.
    Ack { session: Uuid, message: Uuid },
}

#[derive(Debug, Subcommand)]
enum BridgeCommands {
    /// Check whether a session advertises a reachable bridge socket.
    Status { session: String },
    /// Send a prompt and stream bridge events until completion.
    Prompt {
        session: String,
        #[arg(long, conflicts_with_all = ["prompt_file", "prompt_stdin"])]
        prompt: Option<String>,
        #[arg(long, conflicts_with_all = ["prompt", "prompt_stdin"])]
        prompt_file: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["prompt", "prompt_file"])]
        prompt_stdin: bool,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeStatus {
    session_id: Uuid,
    session_name: String,
    socket: Option<PathBuf>,
    available: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Session { command } => {
            run_session_command(&Registry::from_environment()?, command, cli.json)
        }
        Commands::Message { command } => {
            run_message_command(&Registry::from_environment()?, command, cli.json)
        }
        Commands::Bridge { command } => {
            run_bridge_command(&Registry::from_environment()?, command, cli.json)
        }
        Commands::Project { command } => run_project_command(command, cli.json),
        Commands::Subagent { command } => run_subagent_command(command, cli.json),
        Commands::Gc { stale_after } => {
            let removed = Registry::from_environment()?.gc(Duration::from_secs(stale_after))?;
            let orphaned = SpawnStore::from_environment()?.gc()?;
            if cli.json {
                print_json(&serde_json::json!({ "sessions": removed, "subagents": orphaned }))
            } else {
                println!(
                    "Removed {} stale session(s) and {} orphaned subagent(s)",
                    removed.len(),
                    orphaned.len()
                );
                Ok(())
            }
        }
    }
}

fn run_project_command(command: ProjectCommands, json: bool) -> Result<()> {
    match command {
        ProjectCommands::Resolve { reference } => {
            let project = Project::resolve(&reference)?;
            if json {
                print_json(&project)
            } else {
                println!("{}", project.root.display());
                Ok(())
            }
        }
    }
}

fn run_subagent_command(command: SubagentCommands, json: bool) -> Result<()> {
    let store = SpawnStore::from_environment()?;
    let record = match command {
        SubagentCommands::Create {
            parent_pi_session_id,
            parent_pid,
            cwd,
            project_root,
            task_file,
        } => {
            let task = std::fs::read_to_string(task_file)?;
            Some(store.create(
                parent_pi_session_id,
                parent_pid,
                cwd.canonicalize()?,
                project_root.canonicalize()?,
                task,
            )?)
        }
        SubagentCommands::Bind {
            spawn_id,
            child_pi_session_id,
            child_paj_session_id,
            name,
        } => Some(store.bind(spawn_id, child_pi_session_id, child_paj_session_id, name)?),
        SubagentCommands::Show { spawn_id } => Some(store.show(spawn_id)?),
        SubagentCommands::Remove { spawn_id } => Some(store.remove(spawn_id)?),
        SubagentCommands::List {
            parent_pi_session_id,
            all,
        } => {
            let environment_parent = env::var("PAJ_PI_SESSION_ID").ok();
            let parent = if all {
                None
            } else {
                Some(parent_pi_session_id.as_deref().or(environment_parent.as_deref()).ok_or_else(|| anyhow::anyhow!("--parent-pi-session-id is required outside a Paj-enabled Pi session"))?)
            };
            let records = store.list(parent, true)?;
            return print_spawn_records(&records, json);
        }
    };
    if json {
        print_json(&record.expect("record command returns a record"))
    } else {
        println!(
            "{}",
            record.expect("record command returns a record").spawn_id
        );
        Ok(())
    }
}

fn run_session_command(registry: &Registry, command: SessionCommands, json: bool) -> Result<()> {
    match command {
        SessionCommands::Register {
            pid,
            pi_session_id,
            name,
            role,
            parent_pi_session_id,
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
                parent_pi_session_id,
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
        SessionCommands::Rename { id, name } => {
            let session = registry.rename(id, name)?;
            if json {
                print_json(&session)
            } else {
                println!("{}", session.name);
                Ok(())
            }
        }
        SessionCommands::Status { id, status } => {
            let session = registry.set_status(id, status)?;
            if json {
                print_json(&session)
            } else {
                println!("{}", session.status);
                Ok(())
            }
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

fn run_bridge_command(registry: &Registry, command: BridgeCommands, json: bool) -> Result<()> {
    match command {
        BridgeCommands::Status { session } => {
            let session = registry.resolve_live_session(&session)?;
            let status = BridgeStatus {
                session_id: session.id,
                session_name: session.name.clone(),
                socket: session.bridge_socket.clone(),
                available: bridge_is_available(&session),
            };
            if json {
                print_json(&status)
            } else {
                println!(
                    "{}\t{}\t{}",
                    status.session_name,
                    if status.available {
                        "available"
                    } else {
                        "unavailable"
                    },
                    status
                        .socket
                        .as_deref()
                        .map_or_else(|| "-".to_owned(), |path| path.display().to_string())
                );
                Ok(())
            }
        }
        BridgeCommands::Prompt {
            session,
            prompt,
            prompt_file,
            prompt_stdin,
            timeout,
        } => {
            let session = registry.resolve_live_session(&session)?;
            let prompt = read_prompt(prompt, prompt_file, prompt_stdin, io::stdin().lock())?;
            let client = BridgeClient::new(Duration::from_secs(timeout));
            let mut received_delta = false;
            client.prompt(&session, &prompt, |event| {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(event).expect("bridge events should serialize")
                    );
                    return;
                }
                match event {
                    BridgeEvent::Delta { text, .. } => {
                        received_delta = true;
                        print!("{text}");
                        let _ = io::stdout().flush();
                    }
                    BridgeEvent::Complete { text, .. } => {
                        if received_delta {
                            println!();
                        } else {
                            println!("{text}");
                        }
                    }
                    BridgeEvent::Accepted { .. } | BridgeEvent::Error { .. } => {}
                }
            })?;
            Ok(())
        }
    }
}

fn read_prompt(
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    prompt_stdin: bool,
    mut stdin: impl Read,
) -> Result<String> {
    match (prompt, prompt_file, prompt_stdin) {
        (Some(prompt), None, false) => Ok(prompt),
        (None, Some(path), false) => Ok(std::fs::read_to_string(path)?),
        (None, None, true) => {
            let mut prompt = String::new();
            stdin.read_to_string(&mut prompt)?;
            Ok(prompt)
        }
        (None, None, false) => Err(anyhow::anyhow!(
            "--prompt, --prompt-file, or --prompt-stdin is required"
        )),
        _ => unreachable!("clap prevents conflicting prompt sources"),
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            session.id,
            session.name,
            session.role,
            session.status,
            session.parent_pi_session_id.as_deref().unwrap_or("-"),
            session.branch.as_deref().unwrap_or("-"),
            session.project_root.display()
        );
    }
    Ok(())
}

fn print_spawn_records(records: &[SpawnRecord], json: bool) -> Result<()> {
    if json {
        return print_json(records);
    }
    for record in records {
        println!(
            "{}\t{}\t{}\t{}\ttmux -L paj attach-session -t ={}",
            record.spawn_id,
            record.name,
            record.project_root.display(),
            record.task.lines().next().unwrap_or(""),
            record.tmux_name
        );
    }
    Ok(())
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
    println!(
        "parent Pi session: {}",
        session.parent_pi_session_id.as_deref().unwrap_or("-")
    );
    println!("status: {}", session.status);
    println!("task: {}", session.task.as_deref().unwrap_or("-"));
}

fn print_json(value: &(impl Serialize + ?Sized)) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::Parser;

    use super::{Cli, read_prompt};

    #[test]
    fn prompt_can_be_read_from_stdin() {
        let prompt = read_prompt(None, None, true, Cursor::new("from stdin"))
            .expect("stdin prompt should be read");

        assert_eq!(prompt, "from stdin");
    }

    #[test]
    fn clap_rejects_multiple_prompt_sources() {
        let result = Cli::try_parse_from([
            "paj",
            "bridge",
            "prompt",
            "primary",
            "--prompt",
            "one",
            "--prompt-stdin",
        ]);

        assert!(result.is_err());
    }
}
