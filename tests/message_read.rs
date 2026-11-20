use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Barrier;
use std::thread;

use paj::project::Project;
use paj::registry::{
    MAX_PAGE_BYTES, Message, MessageKind, MessageOptions, Registration, Registry, Session,
};
use serde_json::Value;
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

struct Fixture {
    runtime: TempDir,
    _workspace: TempDir,
    registry: Registry,
    project: Project,
    sender: Session,
    recipient: Session,
}

impl Fixture {
    fn new() -> Self {
        let runtime = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let registry = Registry::new(runtime.path().into()).unwrap();
        let project = Project::discover(workspace.path()).unwrap();
        let sender = Self::register(&registry, &project, "sender");
        let recipient = Self::register(&registry, &project, "recipient");
        Self {
            runtime,
            _workspace: workspace,
            registry,
            project,
            sender,
            recipient,
        }
    }

    fn register(registry: &Registry, project: &Project, name: &str) -> Session {
        registry
            .register(
                project,
                Registration {
                    pid: std::process::id(),
                    pi_session_id: None,
                    name: Some(name.into()),
                    role: "primary".into(),
                    parent_pi_session_id: None,
                    task: None,
                    cwd: project.root.clone(),
                    branch: None,
                },
            )
            .unwrap()
    }

    fn command(&self, args: &[&str]) -> Output {
        command(self.runtime.path(), args)
    }

    fn read(&self, after: Option<&Value>, limit: u16) -> Value {
        let id = self.recipient.id.to_string();
        let limit = limit.to_string();
        let mut args = vec!["read", &id, "--limit", &limit];
        if let Some(after) = after {
            args.extend(["--after", after.as_str().unwrap()]);
        }
        page(self.command(&args))
    }

    fn send(&self, text: &str) -> Message {
        self.registry
            .send_message_with_options(
                self.sender.id,
                &self.recipient.id.to_string(),
                text.into(),
                MessageOptions {
                    kind: MessageKind::Update,
                    ..MessageOptions::default()
                },
            )
            .unwrap()
    }

    fn archive(&self) -> PathBuf {
        self.runtime
            .path()
            .join("archive")
            .join(self.recipient.id.to_string())
    }

    fn index(&self) -> PathBuf {
        self.runtime
            .path()
            .join("message-index-v1")
            .join(self.recipient.id.to_string())
    }
}

fn command(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_paj"))
        .env("PAJ_RUNTIME_DIR", root)
        .args(["--json", "message"])
        .args(args)
        .output()
        .unwrap()
}

fn page(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= MAX_PAGE_BYTES);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn ids(page: &Value) -> Vec<Uuid> {
    assert_eq!(page["status"], "ok");
    serde_json::from_value::<Vec<Message>>(page["messages"].clone())
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[test]
fn tails_by_acceptance_order_across_ack_supersession_and_unregister() {
    let f = Fixture::new();
    let empty = f.read(None, 1);
    assert!(ids(&empty).is_empty());
    assert_eq!(empty["hasMore"], false);
    let original = f.send("original");
    let later = f
        .registry
        .send_message_with_options(
            f.sender.id,
            &f.recipient.id.to_string(),
            "replacement".into(),
            MessageOptions {
                kind: MessageKind::Update,
                supersedes: Some(original.id),
                ..MessageOptions::default()
            },
        )
        .unwrap();
    let first = f.read(Some(&empty["nextCursor"]), 1);
    assert_eq!(ids(&first), [original.id]);
    assert_eq!(first["hasMore"], true);
    assert_eq!(
        f.registry
            .pending_messages(f.recipient.id)
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&later)
    );
    assert_eq!(
        f.registry.message_history(f.recipient.id).unwrap(),
        [original]
    );

    let barrier = Barrier::new(2);
    thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            f.registry
                .acknowledge_message(f.recipient.id, later.id)
                .unwrap();
        });
        barrier.wait();
        let second = f.read(Some(&first["nextCursor"]), 1);
        assert_eq!(ids(&second), [later.id]);
        assert_eq!(second["hasMore"], false);
    });
    let second = f.read(Some(&first["nextCursor"]), 1);
    let tail = f.send("next after catch-up");
    f.registry.unregister(f.recipient.id).unwrap();
    let third = f.read(Some(&second["nextCursor"]), 1);
    assert_eq!(ids(&third), [tail.id]);
    assert_eq!(third, f.read(Some(&second["nextCursor"]), 1));
}

#[test]
fn read_is_recipient_scoped_and_does_not_initialize_runtime() {
    let f = Fixture::new();
    let sent = f.send("recipient only");
    let other = Fixture::register(&f.registry, &f.project, "other");
    let first = f.read(None, 1);
    assert_eq!(ids(&first), [sent.id]);
    assert!(ids(&page(f.command(&["read", &other.id.to_string()]))).is_empty());
    assert!(
        !f.command(&[
            "read",
            &other.id.to_string(),
            "--after",
            first["nextCursor"].as_str().unwrap()
        ])
        .status
        .success()
    );
    assert!(!f.command(&["read", "recipient"]).status.success());
    for limit in ["0", "129"] {
        assert!(
            !f.command(&["read", &f.recipient.id.to_string(), "--limit", limit])
                .status
                .success()
        );
    }
    let missing_runtime = f.runtime.path().join("not-created");
    let recipient = f.recipient.id.to_string();
    assert_eq!(
        page(command(&missing_runtime, &["read", &recipient]))["reason"],
        "unindexed"
    );
    assert_eq!(
        page(command(
            &missing_runtime,
            &[
                "read",
                &recipient,
                "--after",
                first["nextCursor"].as_str().unwrap()
            ]
        ))["reason"],
        "reset"
    );
    assert!(!missing_runtime.exists());
    assert_eq!(f.registry.pending_messages(f.recipient.id).unwrap(), [sent]);
}

#[test]
fn preupgrade_records_are_readable_only_through_legacy_commands() {
    let f = Fixture::new();
    fs::remove_dir_all(f.index()).unwrap();
    let previous = f.send("legacy");
    f.registry
        .acknowledge_message(f.recipient.id, previous.id)
        .unwrap();
    let pending = f.send("still pending");
    assert_eq!(f.read(None, 128)["reason"], "unindexed");
    assert!(!f.index().exists());
    assert_eq!(
        f.registry.message_history(f.recipient.id).unwrap(),
        [previous]
    );
    assert_eq!(
        f.registry.pending_messages(f.recipient.id).unwrap(),
        [pending]
    );
}

#[test]
fn byte_budget_shortens_pages_without_skipping_escaped_records() {
    let f = Fixture::new();
    let text = "\"".repeat(20_000);
    let expected = (0..32).map(|_| f.send(&text).id).collect::<Vec<_>>();
    let mut current = f.read(None, 128);
    assert_eq!(current["hasMore"], true);
    let mut actual = Vec::new();
    loop {
        actual.extend(ids(&current));
        if current["hasMore"] == false {
            break;
        }
        current = f.read(Some(&current["nextCursor"]), 128);
    }
    assert_eq!(actual, expected);
    assert_eq!(
        f.read(Some(&current["nextCursor"]), 128)["nextCursor"],
        current["nextCursor"]
    );
}

#[test]
fn oversized_record_never_advances_the_cursor() {
    let f = Fixture::new();
    let first = f.send("first");
    let large = f.send(&"x".repeat(MAX_PAGE_BYTES));
    let last = f.send("last");
    let prefix = f.read(None, 128);
    assert_eq!(ids(&prefix), [first.id]);
    assert_eq!(prefix["hasMore"], true);
    let gap = f.read(Some(&prefix["nextCursor"]), 128);
    assert_eq!(
        gap,
        serde_json::json!({"status":"gap", "reason":"oversized", "messageId":large.id})
    );
    assert_eq!(gap, f.read(Some(&prefix["nextCursor"]), 128));
    assert_eq!(
        f.registry.pending_messages(f.recipient.id).unwrap(),
        [first, large, last]
    );
}

#[test]
fn damaged_index_fails_closed_without_blocking_legacy_delivery() {
    let f = Fixture::new();
    let first = f.send("first");
    let good = f.read(None, 128);
    let state = f.index().join("state.json");
    fs::write(&state, b"{").unwrap();
    assert_eq!(f.read(Some(&good["nextCursor"]), 128)["reason"], "invalid");
    let second = f.send("second");
    f.registry
        .acknowledge_message(f.recipient.id, first.id)
        .unwrap();
    assert_eq!(
        f.registry
            .pending_messages(f.recipient.id)
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&second)
    );
    assert_eq!(
        f.registry
            .message_history(f.recipient.id)
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&first)
    );
    assert_eq!(f.read(Some(&good["nextCursor"]), 128)["reason"], "reset");
    assert_eq!(f.read(None, 128)["reason"], "unindexed");
}

#[test]
fn same_size_journal_corruption_is_a_gap() {
    let f = Fixture::new();
    let first = f.send("first");
    let second = f.send("second");
    let before = f.read(None, 1);
    let journal = fs::read_dir(f.index())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "ids"))
        .unwrap();
    let mut journal = OpenOptions::new()
        .read(true)
        .write(true)
        .open(journal)
        .unwrap();
    let mut bytes = [0; 32];
    journal.read_exact(&mut bytes).unwrap();
    journal.write_all(&bytes).unwrap();
    assert_eq!(f.read(None, 128), before);
    assert_eq!(
        f.read(Some(&before["nextCursor"]), 128)["reason"],
        "invalid"
    );
    assert_ne!(first.id, second.id);
}

#[test]
fn retention_loss_cannot_silently_resume() {
    let f = Fixture::new();
    let one = f.send("one");
    f.send("two");
    f.registry
        .acknowledge_message(f.recipient.id, one.id)
        .unwrap();
    let first = f.read(None, 1);
    fs::remove_file(f.archive().join(format!("{}.json", one.id))).unwrap();
    assert_eq!(
        f.read(Some(&first["nextCursor"]), 128),
        serde_json::json!({"status":"gap","reason":"changed"})
    );
    assert_eq!(f.read(None, 128)["reason"], "changed");
}
