use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use paj::project::Project;
use paj::registry::{Registration, Registry, Session};
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let registry = Registry::from_environment()?;

    match cli.command {
        Commands::Session { command } => run_session_command(&registry, command, cli.json),
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
