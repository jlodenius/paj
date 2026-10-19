use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;

use paj::project::Project;
use paj::registry::{Message, MessageKind, Registration, Registry, Session};
use paj::subagent::{SpawnRecord, SpawnStore, TaskState};
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

fn paj() -> Command {
    Command::new(env!("CARGO_BIN_EXE_paj"))
}

#[test]
fn register_outputs_session_as_json() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");
    let output = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args([
            "--json",
            "session",
            "register",
            "--pid",
            &std::process::id().to_string(),
            "--name",
            "primary",
        ])
        .output()
        .expect("paj should run");
    assert!(
        output.status.success(),
        "paj failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session: Session =
        serde_json::from_slice(&output.stdout).expect("output should contain a session");

    assert_eq!(session.name, "primary");
    assert_eq!(
        session
            .bridge_socket
            .as_deref()
            .and_then(|path| path.file_name()),
        Some(std::ffi::OsStr::new("bridge.sock"))
    );
}

#[test]
fn rename_updates_session_and_outputs_json() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");
    let registry = Registry::new(runtime.path().to_path_buf()).expect("registry should be created");
    let project_metadata = Project::discover(project.path()).expect("project should be discovered");
    let session = registry
        .register(
            &project_metadata,
            Registration {
                pid: std::process::id(),
                pi_session_id: None,
                name: Some("primary".to_owned()),
                role: "primary".to_owned(),
                parent_pi_session_id: None,
                task: None,
                cwd: project.path().to_path_buf(),
                branch: None,
            },
        )
        .expect("session should register");

    let output = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args([
            "--json",
            "session",
            "rename",
            &session.id.to_string(),
            "reviewer",
        ])
        .output()
        .expect("paj should run");

    assert!(
        output.status.success(),
        "paj failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let renamed: Session =
        serde_json::from_slice(&output.stdout).expect("output should contain a session");
    assert_eq!(renamed.name, "reviewer");
    assert_eq!(
        registry
            .show(session.id)
            .expect("renamed session should load")
            .name,
        "reviewer"
    );
}

#[test]
fn status_updates_session_and_outputs_json() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");
    let registry = Registry::new(runtime.path().to_path_buf()).expect("registry should be created");
    let project_metadata = Project::discover(project.path()).expect("project should be discovered");
    let session = registry
        .register(
            &project_metadata,
            Registration {
                pid: std::process::id(),
                pi_session_id: None,
                name: Some("primary".to_owned()),
                role: "primary".to_owned(),
                parent_pi_session_id: None,
                task: None,
                cwd: project.path().to_path_buf(),
                branch: None,
            },
        )
        .expect("session should register");

    let output = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args([
            "--json",
            "session",
            "status",
            &session.id.to_string(),
            "busy",
        ])
        .output()
        .expect("paj should run");

    assert!(
        output.status.success(),
        "paj failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated: Session =
        serde_json::from_slice(&output.stdout).expect("output should contain a session");
    assert_eq!(updated.status, "busy");
    assert_eq!(
        registry
            .show(session.id)
            .expect("updated session should load")
            .status,
        "busy"
    );
}

#[test]
fn list_outputs_empty_json_array_when_project_has_no_sessions() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");

    let output = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args(["--json", "session", "list"])
        .output()
        .expect("paj should run");

    assert_eq!(output.stdout, b"[]\n");
}

#[test]
fn bridge_request_reads_structured_request_from_stdin() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");
    let registry = Registry::new(runtime.path().to_path_buf()).expect("registry should be created");
    let project_metadata = Project::discover(project.path()).expect("project should be discovered");
    let session = registry
        .register(
            &project_metadata,
            Registration {
                pid: std::process::id(),
                pi_session_id: None,
                name: Some("primary".to_owned()),
                role: "primary".to_owned(),
                parent_pi_session_id: None,
                task: None,
                cwd: project.path().to_path_buf(),
                branch: None,
            },
        )
        .expect("session should register");
    let listener = UnixListener::bind(
        session
            .bridge_socket
            .as_ref()
            .expect("session should advertise a socket"),
    )
    .expect("bridge socket should bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("client should connect");
        let mut request = String::new();
        BufReader::new(stream.try_clone().expect("stream should clone"))
            .read_line(&mut request)
            .expect("request should be read");
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("request should be JSON");
        assert_eq!(request["params"]["kind"], "followup");
        assert_eq!(request["params"]["question"], "from stdin");
        let id = request["id"].as_str().expect("request should have an ID");
        write!(
            stream,
            "{{\"event\":\"accepted\",\"id\":\"{id}\"}}\n\
             {{\"event\":\"complete\",\"id\":\"{id}\",\"text\":\"done\",\"actions\":[]}}\n"
        )
        .expect("events should be written");
    });
    let mut child = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args([
            "bridge",
            "request",
            &session.id.to_string(),
            "--request-stdin",
            "--timeout",
            "1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("paj should start");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(br#"{"kind":"followup","question":"from stdin"}"#)
        .expect("request should be written");

    let output = child.wait_with_output().expect("paj should finish");
    server.join().expect("server should finish");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"done\n");
}

#[test]
fn bridge_request_json_preserves_complete_actions() {
    let runtime = tempdir().expect("runtime directory should be created");
    let project = tempdir().expect("project directory should be created");
    let registry = Registry::new(runtime.path().to_path_buf()).expect("registry should be created");
    let project_metadata = Project::discover(project.path()).expect("project should be discovered");
    let session = registry
        .register(
            &project_metadata,
            Registration {
                pid: std::process::id(),
                pi_session_id: None,
                name: Some("primary".to_owned()),
                role: "primary".to_owned(),
                parent_pi_session_id: None,
                task: None,
                cwd: project.path().to_path_buf(),
                branch: None,
            },
        )
        .expect("session should register");
    let listener = UnixListener::bind(
        session
            .bridge_socket
            .as_ref()
            .expect("session should advertise a socket"),
    )
    .expect("bridge socket should bind");
    let action_id = uuid::Uuid::now_v7();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("client should connect");
        let mut request = String::new();
        BufReader::new(stream.try_clone().expect("stream should clone"))
            .read_line(&mut request)
            .expect("request should be read");
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("request should be JSON");
        let id = request["id"].as_str().expect("request should have an ID");
        write!(
            stream,
            "{{\"event\":\"accepted\",\"id\":\"{id}\"}}\n\
             {{\"event\":\"complete\",\"id\":\"{id}\",\"text\":\"done\",\"actions\":[{{\"id\":\"{action_id}\",\"title\":\"Change\",\"description\":\"Make the change\"}}]}}\n"
        )
        .expect("events should be written");
    });

    let request_path = project.path().join("request.json");
    std::fs::write(&request_path, r#"{"kind":"followup","question":"hello"}"#)
        .expect("request file should be written");
    let output = paj()
        .current_dir(project.path())
        .env("PAJ_RUNTIME_DIR", runtime.path())
        .args([
            "--json",
            "bridge",
            "request",
            &session.id.to_string(),
            "--request-file",
            request_path.to_str().expect("request path should be UTF-8"),
            "--timeout",
            "1",
        ])
        .output()
        .expect("paj should run");
    server.join().expect("server should finish");

    assert!(output.status.success());
    let events: Vec<serde_json::Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("event should be JSON"))
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["event"], "complete");
    assert_eq!(events[1]["actions"][0]["id"], action_id.to_string());
    assert_eq!(events[1]["actions"][0]["title"], "Change");
    assert_eq!(events[1]["actions"][0]["description"], "Make the change");
}

struct ProtocolFixture {
    runtime: TempDir,
    project: TempDir,
    registry: Registry,
    store: SpawnStore,
    parent: Session,
    child: Session,
    outsider: Session,
    record: SpawnRecord,
}

fn register_protocol_session(registry: &Registry, root: &Path, name: &str, pi_id: &str) -> Session {
    registry
        .register(
            &Project::discover(root).unwrap(),
            Registration {
                pid: std::process::id(),
                pi_session_id: Some(pi_id.to_owned()),
                name: Some(name.to_owned()),
                role: "primary".to_owned(),
                parent_pi_session_id: None,
                task: None,
                cwd: root.to_path_buf(),
                branch: None,
            },
        )
        .unwrap()
}

impl ProtocolFixture {
    fn new() -> Self {
        let runtime = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = Registry::new(runtime.path().to_path_buf()).unwrap();
        let store = SpawnStore::new(runtime.path().to_path_buf()).unwrap();
        let parent = register_protocol_session(&registry, project.path(), "parent", "parent-pi");
        let child = register_protocol_session(&registry, project.path(), "child", "child-pi");
        let outsider =
            register_protocol_session(&registry, project.path(), "outsider", "outsider-pi");
        let record = store
            .create(
                "parent-pi".to_owned(),
                std::process::id(),
                project.path().to_path_buf(),
                project.path().to_path_buf(),
                "first task".to_owned(),
            )
            .unwrap();
        let record = store
            .bind(
                record.spawn_id,
                "child-pi".to_owned(),
                child.id,
                child.name.clone(),
            )
            .unwrap();
        Self {
            runtime,
            project,
            registry,
            store,
            parent,
            child,
            outsider,
            record,
        }
    }

    fn sibling(&self) -> SpawnRecord {
        let record = self
            .store
            .create(
                "parent-pi".to_owned(),
                std::process::id(),
                self.project.path().to_owned(),
                self.project.path().to_owned(),
                "sibling task".to_owned(),
            )
            .unwrap();
        self.store
            .bind(
                record.spawn_id,
                "outsider-pi".to_owned(),
                self.outsider.id,
                self.outsider.name.clone(),
            )
            .unwrap()
    }

    fn command(&self, args: &[&str]) -> Output {
        paj()
            .current_dir(self.project.path())
            .env("PAJ_RUNTIME_DIR", self.runtime.path())
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> T {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn rejects(&self, args: &[&str]) {
        let output = self.command(args);
        assert!(
            !output.status.success(),
            "command unexpectedly succeeded: {args:?}"
        );
    }

    fn send(&self, from: Uuid, to: Uuid, text: &str, options: &[&str]) -> Message {
        let from = from.to_string();
        let to = to.to_string();
        let mut args = vec!["message", "send", &to, "--from", &from, "--text", text];
        args.extend_from_slice(options);
        self.json(&args)
    }

    fn complete(&self, text: &str) -> SpawnRecord {
        self.json(&[
            "subagent",
            "complete",
            &self.record.spawn_id.to_string(),
            "--from",
            &self.child.id.to_string(),
            "--task-id",
            &self.record.task_id,
            "--text",
            text,
        ])
    }

    fn task_file(&self, text: &str) -> std::path::PathBuf {
        let path = self.project.path().join("task.txt");
        std::fs::write(&path, text).unwrap();
        path
    }
}

#[test]
fn protocol_reads_legacy_session_spawn_and_unstructured_message_json() {
    let fixture = ProtocolFixture::new();
    let mut session = serde_json::to_value(&fixture.child).unwrap();
    session.as_object_mut().unwrap().remove("model");
    let session: Session = serde_json::from_value(session).unwrap();
    assert_eq!(session.model, None);
    let mut record = serde_json::to_value(&fixture.record).unwrap();
    for key in [
        "requestedModel",
        "parentTaskId",
        "taskId",
        "taskState",
        "result",
        "resultHistory",
        "assignment",
    ] {
        record.as_object_mut().unwrap().remove(key);
    }
    let record: SpawnRecord = serde_json::from_value(record).unwrap();
    assert_eq!(record.task_id, record.spawn_id.to_string());
    assert_eq!(record.task_state, TaskState::Active);
    assert_eq!(record.requested_model, None);
    assert_eq!(record.parent_task_id, None);
    assert!(record.result_history.is_empty());
    let legacy = serde_json::json!({ "id": Uuid::now_v7(),
        "from": { "id": fixture.parent.id, "name": "parent" }, "to": fixture.child.id,
        "text": "plain old request", "createdAtMs": 1 });
    let message: Message = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(message.kind, MessageKind::Request);
    assert_eq!(message.task_id, None);
    assert_eq!(message.sender_task_id, None);
    assert_eq!(message.recipient_task_id, None);
    assert_eq!(message.in_reply_to, None);
    assert_eq!(message.supersedes, None);
    let inbox = fixture
        .runtime
        .path()
        .join("projects")
        .join(&fixture.child.project_id)
        .join("sessions")
        .join(fixture.child.id.to_string())
        .join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join(format!("{}.json", message.id)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let pending: Vec<Message> =
        fixture.json(&["message", "pending", &fixture.child.id.to_string()]);
    assert_eq!(pending, vec![message.clone()]);
    fixture
        .registry
        .acknowledge_message(fixture.child.id, message.id)
        .unwrap();
    assert_eq!(
        fixture.registry.message_history(fixture.child.id).unwrap(),
        vec![message]
    );
}

#[test]
fn model_metadata_registers_updates_and_records_requested_model() {
    let fixture = ProtocolFixture::new();
    let session: Session = fixture.json(&[
        "session",
        "register",
        "--pid",
        &std::process::id().to_string(),
        "--model",
        "anthropic/claude-opus-4-6",
    ]);
    assert_eq!(session.model.as_deref(), Some("anthropic/claude-opus-4-6"));
    let updated: Session = fixture.json(&[
        "session",
        "model",
        &session.id.to_string(),
        "openai/gpt-5.4",
    ]);
    assert_eq!(updated.model.as_deref(), Some("openai/gpt-5.4"));
    fixture.registry.heartbeat(session.id).unwrap();
    assert_eq!(
        fixture.registry.show(session.id).unwrap().model,
        updated.model
    );
    fixture.rejects(&["session", "model", &session.id.to_string(), "bare-model"]);
    fixture.rejects(&["session", "model", &session.id.to_string(), "provider/"]);
    let path = fixture.task_file("model task");
    let record: SpawnRecord = fixture.json(&[
        "subagent",
        "create",
        "--parent-pi-session-id",
        "parent-pi",
        "--parent-pid",
        &std::process::id().to_string(),
        "--cwd",
        fixture.project.path().to_str().unwrap(),
        "--project-root",
        fixture.project.path().to_str().unwrap(),
        "--task-file",
        path.to_str().unwrap(),
        "--model",
        "openai/gpt-5.4",
    ]);
    assert_eq!(record.requested_model.as_deref(), Some("openai/gpt-5.4"));
    assert_eq!(record.task_state, TaskState::Active);
    assert_eq!(record.task_id, record.spawn_id.to_string());
    let requested = fixture
        .store
        .create_with_model(
            "parent-pi".to_owned(),
            std::process::id(),
            fixture.project.path().to_owned(),
            fixture.project.path().to_owned(),
            "manual script launch".to_owned(),
            Some("sonnet:high".to_owned()),
        )
        .unwrap();
    assert_eq!(requested.requested_model.as_deref(), Some("sonnet:high"));
    assert!(
        fixture
            .store
            .create_with_model(
                "parent-pi".to_owned(),
                std::process::id(),
                fixture.project.path().to_owned(),
                fixture.project.path().to_owned(),
                "manual script launch".to_owned(),
                Some("  ".to_owned()),
            )
            .is_err()
    );
}

#[test]
fn completion_is_immutable_idempotent_and_acknowledgements_are_archived() {
    let fixture = ProtocolFixture::new();
    assert!(
        fixture
            .store
            .complete(
                fixture.record.spawn_id,
                fixture.child.id,
                &fixture.record.task_id,
                " \n".to_owned(),
            )
            .is_err()
    );
    let first = fixture.complete("first result");
    assert_eq!(first.task_state, TaskState::Completed);
    assert!(first.result.as_ref().unwrap().delivered);
    let pending = fixture
        .registry
        .pending_messages(fixture.parent.id)
        .unwrap();
    assert_eq!(pending.len(), 1);
    let message = &pending[0];
    assert_eq!(message.kind, MessageKind::Result);
    assert_eq!(
        message.task_id.as_deref(),
        Some(fixture.record.task_id.as_str())
    );
    assert_eq!(message.text, "first result");
    assert_eq!(message.id, first.result.as_ref().unwrap().message_id);
    assert_eq!(fixture.complete("attempted replacement"), first);
    fixture.json::<serde_json::Value>(&["subagent", "show", &first.spawn_id.to_string()]);
    let output = fixture.command(&[
        "message",
        "ack",
        &fixture.parent.id.to_string(),
        &message.id.to_string(),
    ]);
    assert!(output.status.success());
    assert!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .is_empty()
    );
    let history: Vec<Message> =
        fixture.json(&["message", "history", &fixture.parent.id.to_string()]);
    assert_eq!(history, pending);
    assert_eq!(fixture.complete("another retry"), first);
    assert!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .is_empty()
    );
    fixture
        .registry
        .acknowledge_message(fixture.parent.id, message.id)
        .unwrap();
    fixture.rejects(&[
        "subagent",
        "complete",
        &first.spawn_id.to_string(),
        "--from",
        &fixture.outsider.id.to_string(),
        "--task-id",
        &first.task_id,
        "--text",
        "forged",
    ]);
    fixture.rejects(&[
        "subagent",
        "complete",
        &first.spawn_id.to_string(),
        "--from",
        &fixture.child.id.to_string(),
        "--task-id",
        "wrong-task",
        "--text",
        "stale",
    ]);

    let mut interrupted = first.clone();
    interrupted.result.as_mut().unwrap().delivered = false;
    std::fs::write(
        fixture
            .runtime
            .path()
            .join("subagents")
            .join(format!("{}.json", first.spawn_id)),
        serde_json::to_vec(&interrupted).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.complete("retry after interrupted outbox write"),
        first
    );
    assert!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.registry.message_history(fixture.parent.id).unwrap(),
        pending
    );
}

#[test]
fn completion_retries_when_parent_returns_and_targets_only_stable_pi_identity() {
    let fixture = ProtocolFixture::new();
    fixture.registry.unregister(fixture.parent.id).unwrap();
    let impostor = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "parent-pi",
        "not-parent",
    );
    let first = fixture.complete("saved while parent missing");
    let result = first.result.as_ref().unwrap();
    assert!(!result.delivered);
    assert!(
        fixture
            .registry
            .pending_messages(impostor.id)
            .unwrap()
            .is_empty()
    );
    let returned = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "returned",
        "parent-pi",
    );
    let retried = fixture.complete("must not replace first text");
    assert!(retried.result.as_ref().unwrap().delivered);
    assert_eq!(
        retried.result.as_ref().unwrap().message_id,
        result.message_id
    );
    let messages = fixture.registry.pending_messages(returned.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, result.message_id);
    assert_eq!(messages[0].text, "saved while parent missing");
    assert_eq!(messages[0].to, returned.id);
    fixture
        .registry
        .acknowledge_message(returned.id, result.message_id)
        .unwrap();
    fixture.complete("retry after ack");
    assert!(
        fixture
            .registry
            .pending_messages(returned.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn task_scoping_closed_task_guards_assignment_authorization_and_stale_rejection() {
    let fixture = ProtocolFixture::new();
    let request = fixture.send(fixture.parent.id, fixture.child.id, "follow up", &[]);
    assert_eq!(request.kind, MessageKind::Request);
    assert_eq!(
        request.task_id.as_deref(),
        Some(fixture.record.task_id.as_str())
    );
    let update = fixture.send(
        fixture.child.id,
        fixture.parent.id,
        "progress",
        &["--kind", "update"],
    );
    assert_eq!(update.task_id, request.task_id);
    let path = fixture.task_file("second task");
    let spawn = fixture.record.spawn_id.to_string();
    let parent = fixture.parent.id.to_string();
    let child = fixture.child.id.to_string();
    fixture.rejects(&[
        "subagent",
        "assign",
        &spawn,
        "--from",
        &parent,
        "--task-file",
        path.to_str().unwrap(),
    ]);
    fixture.complete("done");
    for kind in ["request", "question", "answer", "result"] {
        fixture.rejects(&[
            "message", "send", &parent, "--from", &child, "--text", "closed", "--kind", kind,
        ]);
    }
    fixture.rejects(&[
        "message",
        "send",
        &fixture.outsider.id.to_string(),
        "--from",
        &child,
        "--text",
        "closed",
    ]);
    for kind in ["request", "update"] {
        fixture.rejects(&[
            "message", "send", &child, "--from", &parent, "--text", "closed", "--kind", kind,
        ]);
    }
    let update = fixture.send(
        fixture.child.id,
        fixture.parent.id,
        "late progress",
        &["--kind", "update"],
    );
    assert_eq!(update.task_id, request.task_id);
    fixture.rejects(&[
        "subagent",
        "assign",
        &spawn,
        "--from",
        &fixture.outsider.id.to_string(),
        "--task-file",
        path.to_str().unwrap(),
    ]);
    let assigned: SpawnRecord = fixture.json(&[
        "subagent",
        "assign",
        &spawn,
        "--from",
        &parent,
        "--task-file",
        path.to_str().unwrap(),
    ]);
    assert_eq!(assigned.task_state, TaskState::Active);
    assert_ne!(assigned.task_id, fixture.record.task_id);
    assert_eq!(assigned.result_history.len(), 1);
    assert_eq!(assigned.result_history[0].text, "done");
    assert!(assigned.result.is_none());
    let requests = fixture.registry.pending_messages(fixture.child.id).unwrap();
    let new_request = requests
        .iter()
        .find(|message| message.task_id.as_ref() == Some(&assigned.task_id))
        .unwrap();
    assert_eq!(new_request.kind, MessageKind::Request);
    assert_eq!(new_request.text, "second task");
    for (from, to) in [(&parent, &child), (&child, &parent)] {
        fixture.rejects(&[
            "message",
            "send",
            to,
            "--from",
            from,
            "--text",
            "stale",
            "--task-id",
            &fixture.record.task_id,
        ]);
    }
    fixture.rejects(&[
        "subagent",
        "complete",
        &spawn,
        "--from",
        &child,
        "--task-id",
        &fixture.record.task_id,
        "--text",
        "stale",
    ]);
    fixture.rejects(&[
        "subagent",
        "assign",
        &spawn,
        "--from",
        &parent,
        "--task-file",
        path.to_str().unwrap(),
    ]);
    let completed: SpawnRecord = fixture.json(&[
        "subagent",
        "complete",
        &spawn,
        "--from",
        &child,
        "--task-id",
        &assigned.task_id,
        "--text",
        "second result",
    ]);
    assert_eq!(completed.result_history, assigned.result_history);
    assert_eq!(completed.result.as_ref().unwrap().text, "second result");
}

#[test]
fn supersession_only_replaces_a_pending_message_with_matching_participants_and_task() {
    let fixture = ProtocolFixture::new();
    let first = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "original",
        &["--task-id", "topic"],
    );
    let independent = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "independent",
        &["--task-id", "topic"],
    );
    let recipient = fixture.outsider.id.to_string();
    let parent = fixture.parent.id.to_string();
    let original = first.id.to_string();
    fixture.rejects(&[
        "message",
        "send",
        &recipient,
        "--from",
        &fixture.child.id.to_string(),
        "--text",
        "forged",
        "--task-id",
        "topic",
        "--supersedes",
        &original,
    ]);
    fixture.rejects(&[
        "message",
        "send",
        &recipient,
        "--from",
        &parent,
        "--text",
        "wrong task",
        "--task-id",
        "other",
        "--supersedes",
        &original,
    ]);
    fixture.rejects(&[
        "message",
        "send",
        &fixture.child.id.to_string(),
        "--from",
        &parent,
        "--text",
        "wrong recipient",
        "--supersedes",
        &original,
    ]);
    assert_eq!(
        fixture
            .registry
            .pending_messages(fixture.outsider.id)
            .unwrap(),
        vec![first.clone(), independent.clone()]
    );
    let replacement = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "replacement",
        &["--task-id", "topic", "--supersedes", &original],
    );
    assert_eq!(replacement.supersedes, Some(first.id));
    assert_eq!(
        fixture
            .registry
            .pending_messages(fixture.outsider.id)
            .unwrap(),
        vec![independent, replacement]
    );
    assert_eq!(
        fixture
            .registry
            .message_history(fixture.outsider.id)
            .unwrap(),
        vec![first]
    );
    fixture.rejects(&[
        "message",
        "send",
        &recipient,
        "--from",
        &parent,
        "--text",
        "already archived",
        "--task-id",
        "topic",
        "--supersedes",
        &original,
    ]);
}

#[test]
fn siblings_can_answer_questions_with_inferred_or_preserved_task_ids() {
    for archived in [false, true] {
        for explicit_task in [false, true] {
            let fixture = ProtocolFixture::new();
            let sibling = fixture.sibling();
            let question = fixture.send(
                fixture.child.id,
                fixture.outsider.id,
                "What is the return format?",
                &["--kind", "question"],
            );
            assert_eq!(question.task_id.as_ref(), Some(&sibling.task_id));
            if archived {
                fixture
                    .registry
                    .acknowledge_message(fixture.outsider.id, question.id)
                    .unwrap();
            }
            let question_id = question.id.to_string();
            let mut options = vec!["--kind", "answer", "--in-reply-to", &question_id];
            if explicit_task {
                options.extend(["--task-id", sibling.task_id.as_str()]);
            }
            let answer = fixture.send(
                fixture.outsider.id,
                fixture.child.id,
                "A JSON object",
                &options,
            );
            assert_eq!(answer.in_reply_to, Some(question.id));
            assert_eq!(answer.task_id, question.task_id);
            let delivered: Vec<Message> =
                fixture.json(&["message", "pending", &fixture.child.id.to_string()]);
            assert_eq!(delivered, vec![answer.clone()]);
            let metadata = serde_json::to_value(&answer).unwrap();
            assert_eq!(metadata["senderTaskId"], sibling.task_id);
            assert_eq!(metadata["recipientTaskId"], fixture.record.task_id);
            fixture.send(
                fixture.child.id,
                fixture.outsider.id,
                "What is the error format?",
                &["--kind", "question"],
            );
        }
    }
}

#[test]
fn sibling_replies_cannot_cross_closed_or_reassigned_tasks() {
    for reassign_sender in [false, true] {
        let fixture = ProtocolFixture::new();
        let sibling = fixture.sibling();
        let question = fixture.send(
            fixture.child.id,
            fixture.outsider.id,
            "What is the return format?",
            &["--kind", "question"],
        );
        let (record, session) = if reassign_sender {
            (&sibling, &fixture.outsider)
        } else {
            (&fixture.record, &fixture.child)
        };
        fixture
            .store
            .complete(
                record.spawn_id,
                session.id,
                &record.task_id,
                "done".to_owned(),
            )
            .unwrap();
        for reopen in [false, true] {
            if reopen {
                fixture
                    .store
                    .assign(record.spawn_id, fixture.parent.id, "new task".to_owned())
                    .unwrap();
            }
            for explicit_task in [false, true] {
                let to = fixture.child.id.to_string();
                let from = fixture.outsider.id.to_string();
                let reply_to = question.id.to_string();
                let mut args = vec![
                    "message",
                    "send",
                    &to,
                    "--from",
                    &from,
                    "--kind",
                    "answer",
                    "--text",
                    "late answer",
                    "--in-reply-to",
                    &reply_to,
                ];
                if explicit_task {
                    args.extend(["--task-id", sibling.task_id.as_str()]);
                }
                fixture.rejects(&args);
            }
        }
        fixture.rejects(&[
            "message",
            "send",
            &fixture.outsider.id.to_string(),
            "--from",
            &fixture.child.id.to_string(),
            "--kind",
            "question",
            "--text",
            "replace old task question",
            "--supersedes",
            &question.id.to_string(),
        ]);
        let current = fixture.send(
            fixture.child.id,
            fixture.outsider.id,
            "Question for the current assignments",
            &["--kind", "question"],
        );
        let answer = fixture.send(
            fixture.outsider.id,
            fixture.child.id,
            "Current answer",
            &["--kind", "answer", "--in-reply-to", &current.id.to_string()],
        );
        assert_eq!(answer.in_reply_to, Some(current.id));
    }
}

#[test]
fn reply_guards_check_inbox_and_archive_and_require_matching_participants_and_task() {
    let fixture = ProtocolFixture::new();
    let parent = fixture.parent.id.to_string();
    let recipient = fixture.outsider.id.to_string();
    for kind in ["update", "result"] {
        let terminal = fixture.send(
            fixture.parent.id,
            fixture.outsider.id,
            "terminal",
            &["--kind", kind],
        );
        for archived in [false, true] {
            if archived {
                fixture
                    .registry
                    .acknowledge_message(fixture.outsider.id, terminal.id)
                    .unwrap();
            }
            fixture.rejects(&[
                "message",
                "send",
                &parent,
                "--from",
                &recipient,
                "--text",
                "loop",
                "--kind",
                "answer",
                "--in-reply-to",
                &terminal.id.to_string(),
            ]);
        }
    }
    let request = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "request",
        &["--task-id", "topic"],
    );
    fixture
        .registry
        .acknowledge_message(fixture.outsider.id, request.id)
        .unwrap();
    fixture.rejects(&[
        "message",
        "send",
        &parent,
        "--from",
        &recipient,
        "--text",
        "wrong task",
        "--kind",
        "answer",
        "--in-reply-to",
        &request.id.to_string(),
        "--task-id",
        "other",
    ]);
    fixture.rejects(&[
        "message",
        "send",
        &fixture.child.id.to_string(),
        "--from",
        &recipient,
        "--text",
        "wrong sender",
        "--kind",
        "answer",
        "--in-reply-to",
        &request.id.to_string(),
    ]);
    let answer = fixture.send(
        fixture.outsider.id,
        fixture.parent.id,
        "answer",
        &[
            "--kind",
            "answer",
            "--in-reply-to",
            &request.id.to_string(),
            "--task-id",
            "topic",
        ],
    );
    assert_eq!(answer.in_reply_to, Some(request.id));
}

#[test]
fn one_unanswered_question_per_route_and_task_allows_explicit_pending_supersession() {
    let fixture = ProtocolFixture::new();
    let first = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "question",
        &["--kind", "question"],
    );
    let parent = fixture.parent.id.to_string();
    let recipient = fixture.outsider.id.to_string();
    fixture.rejects(&[
        "message", "send", &recipient, "--from", &parent, "--text", "too many", "--kind",
        "question",
    ]);
    let independent = fixture.send(fixture.parent.id, fixture.outsider.id, "independent", &[]);
    let replacement = fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "refined question",
        &["--kind", "question", "--supersedes", &first.id.to_string()],
    );
    fixture
        .registry
        .acknowledge_message(fixture.outsider.id, replacement.id)
        .unwrap();
    fixture.rejects(&[
        "message",
        "send",
        &recipient,
        "--from",
        &parent,
        "--text",
        "still unanswered",
        "--kind",
        "question",
    ]);
    fixture.send(
        fixture.outsider.id,
        fixture.parent.id,
        "answer",
        &[
            "--kind",
            "answer",
            "--in-reply-to",
            &replacement.id.to_string(),
        ],
    );
    fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "next question",
        &["--kind", "question"],
    );
    fixture.send(
        fixture.parent.id,
        fixture.outsider.id,
        "different task question",
        &["--kind", "question", "--task-id", "other"],
    );
    assert!(
        fixture
            .registry
            .pending_messages(fixture.outsider.id)
            .unwrap()
            .contains(&independent)
    );
}

#[test]
fn assignment_outbox_survives_missing_child_and_binding_flushes_once() {
    let fixture = ProtocolFixture::new();
    fixture.complete("first done");
    fixture.registry.unregister(fixture.child.id).unwrap();
    let assigned = fixture
        .store
        .assign(
            fixture.record.spawn_id,
            fixture.parent.id,
            "next task".to_owned(),
        )
        .unwrap();
    assert!(!assigned.assignment.as_ref().unwrap().delivered);
    let retried = fixture
        .store
        .assign(
            fixture.record.spawn_id,
            fixture.parent.id,
            "next task".to_owned(),
        )
        .unwrap();
    assert_eq!(assigned, retried);
    let returned = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "returned child",
        "child-pi",
    );
    let bound = fixture
        .store
        .bind(
            assigned.spawn_id,
            "child-pi".to_owned(),
            returned.id,
            returned.name.clone(),
        )
        .unwrap();
    assert_eq!(bound.task_id, assigned.task_id);
    assert!(bound.assignment.as_ref().unwrap().delivered);
    let messages = fixture.registry.pending_messages(returned.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].task_id.as_deref(),
        Some(assigned.task_id.as_str())
    );
    fixture
        .registry
        .acknowledge_message(returned.id, messages[0].id)
        .unwrap();
    fixture
        .store
        .bind(
            assigned.spawn_id,
            "child-pi".to_owned(),
            returned.id,
            returned.name,
        )
        .unwrap();
    assert!(
        fixture
            .registry
            .pending_messages(returned.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn nested_subagents_track_parent_assignments_and_refresh_only_on_explicit_assignment() {
    let fixture = ProtocolFixture::new();
    let path = fixture.task_file("nested task");
    let args = [
        "subagent",
        "create",
        "--parent-pi-session-id",
        "child-pi",
        "--parent-pid",
        &std::process::id().to_string(),
        "--cwd",
        fixture.project.path().to_str().unwrap(),
        "--project-root",
        fixture.project.path().to_str().unwrap(),
        "--task-file",
        path.to_str().unwrap(),
    ];
    let mut with_parent = args.to_vec();
    with_parent.extend(["--parent-task-id", fixture.record.task_id.as_str()]);
    let nested: SpawnRecord = fixture.json(&with_parent);
    assert_eq!(
        nested.parent_task_id.as_ref(),
        Some(&fixture.record.task_id)
    );
    let inferred: SpawnRecord = fixture.json(&args);
    assert_eq!(inferred.parent_task_id, nested.parent_task_id);
    let mut stale_parent = args.to_vec();
    stale_parent.extend(["--parent-task-id", "stale-parent-task"]);
    fixture.rejects(&stale_parent);
    let nested = fixture
        .store
        .bind(
            nested.spawn_id,
            "outsider-pi".to_owned(),
            fixture.outsider.id,
            fixture.outsider.name.clone(),
        )
        .unwrap();
    let question = fixture.send(
        fixture.outsider.id,
        fixture.child.id,
        "nested question",
        &["--kind", "question"],
    );
    assert_eq!(question.task_id.as_ref(), Some(&nested.task_id));
    fixture.send(
        fixture.child.id,
        fixture.outsider.id,
        "nested answer",
        &[
            "--kind",
            "answer",
            "--in-reply-to",
            &question.id.to_string(),
        ],
    );
    let completed = fixture
        .store
        .complete(
            nested.spawn_id,
            fixture.outsider.id,
            &nested.task_id,
            "nested result".to_owned(),
        )
        .unwrap();
    assert!(completed.result.unwrap().delivered);
    let messages = fixture.registry.pending_messages(fixture.child.id).unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.kind == MessageKind::Result
                && message.task_id.as_ref() == Some(&nested.task_id))
    );
    fixture.complete("outer done");
    assert!(
        fixture
            .store
            .assign(
                nested.spawn_id,
                fixture.child.id,
                "blocked nested task".to_owned()
            )
            .is_err()
    );
    fixture.rejects(&args);
    let outer = fixture
        .store
        .assign(
            fixture.record.spawn_id,
            fixture.parent.id,
            "new outer task".to_owned(),
        )
        .unwrap();
    assert_eq!(
        fixture.store.show(nested.spawn_id).unwrap().parent_task_id,
        nested.parent_task_id
    );
    let reassigned = fixture
        .store
        .assign(
            nested.spawn_id,
            fixture.child.id,
            "new nested task".to_owned(),
        )
        .unwrap();
    assert_eq!(reassigned.parent_task_id.as_ref(), Some(&outer.task_id));
    assert_ne!(reassigned.task_id, nested.task_id);
    assert_eq!(reassigned.result_history.len(), 1);
    assert_eq!(reassigned.result_history[0].text, "nested result");
    assert_eq!(
        reassigned.result_history[0].parent_task_id,
        Some(fixture.record.task_id.clone())
    );
}

#[test]
fn generic_child_results_and_uncorrelated_answers_are_rejected() {
    let fixture = ProtocolFixture::new();
    for recipient in [fixture.parent.id, fixture.outsider.id] {
        fixture.rejects(&[
            "message",
            "send",
            &recipient.to_string(),
            "--from",
            &fixture.child.id.to_string(),
            "--text",
            "must complete instead",
            "--kind",
            "result",
        ]);
    }
    for (from, to) in [
        (fixture.parent.id, fixture.outsider.id),
        (fixture.child.id, fixture.parent.id),
    ] {
        fixture.rejects(&[
            "message",
            "send",
            &to.to_string(),
            "--from",
            &from.to_string(),
            "--text",
            "missing correlation",
            "--kind",
            "answer",
        ]);
    }
    assert!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .registry
            .pending_messages(fixture.outsider.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn flush_retries_pending_completion_without_repeating_text_and_remains_idempotent_after_ack() {
    let fixture = ProtocolFixture::new();
    fixture.registry.unregister(fixture.parent.id).unwrap();
    let first = fixture.complete("immutable first result");
    assert!(!first.result.as_ref().unwrap().delivered);
    let missing: SpawnRecord = fixture.json(&["subagent", "flush", &first.spawn_id.to_string()]);
    assert_eq!(missing, first);
    let returned = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "returned parent",
        "parent-pi",
    );
    let delivered: SpawnRecord = fixture.json(&["subagent", "flush", &first.spawn_id.to_string()]);
    assert!(delivered.result.as_ref().unwrap().delivered);
    assert_eq!(
        delivered.result.as_ref().unwrap().message_id,
        first.result.as_ref().unwrap().message_id
    );
    let messages = fixture.registry.pending_messages(returned.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "immutable first result");
    fixture
        .registry
        .acknowledge_message(returned.id, messages[0].id)
        .unwrap();
    let retry: SpawnRecord = fixture.json(&["subagent", "flush", &first.spawn_id.to_string()]);
    assert_eq!(retry, delivered);
    assert!(
        fixture
            .registry
            .pending_messages(returned.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn flush_retries_assignment_to_returned_child_without_generating_a_new_task() {
    let fixture = ProtocolFixture::new();
    fixture.complete("done");
    fixture.registry.unregister(fixture.child.id).unwrap();
    let assigned = fixture
        .store
        .assign(
            fixture.record.spawn_id,
            fixture.parent.id,
            "next task".to_owned(),
        )
        .unwrap();
    assert!(!assigned.assignment.as_ref().unwrap().delivered);
    let returned = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "returned child",
        "child-pi",
    );
    let delivered: SpawnRecord =
        fixture.json(&["subagent", "flush", &assigned.spawn_id.to_string()]);
    assert!(delivered.assignment.as_ref().unwrap().delivered);
    assert_eq!(delivered.task_id, assigned.task_id);
    let messages = fixture.registry.pending_messages(returned.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].to, returned.id);
    assert_eq!(messages[0].id, assigned.assignment.unwrap().message.id);
    fixture
        .registry
        .acknowledge_message(returned.id, messages[0].id)
        .unwrap();
    fixture.store.flush(assigned.spawn_id).unwrap();
    assert!(
        fixture
            .registry
            .pending_messages(returned.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn durable_history_survives_unregistration_and_collection_without_dropping_pending_messages() {
    let fixture = ProtocolFixture::new();
    let acknowledged = fixture.send(fixture.parent.id, fixture.outsider.id, "acknowledged", &[]);
    fixture
        .registry
        .acknowledge_message(fixture.outsider.id, acknowledged.id)
        .unwrap();
    let pending = fixture.send(fixture.parent.id, fixture.outsider.id, "pending", &[]);
    fixture.registry.unregister(fixture.outsider.id).unwrap();
    let archived: Vec<Message> =
        fixture.json(&["message", "history", &fixture.outsider.id.to_string()]);
    assert_eq!(archived, vec![acknowledged, pending]);
    let message = fixture.send(
        fixture.parent.id,
        fixture.child.id,
        "pending at collection",
        &[],
    );
    let metadata = fixture
        .runtime
        .path()
        .join("projects")
        .join(&fixture.child.project_id)
        .join("sessions")
        .join(fixture.child.id.to_string())
        .join("metadata.json");
    let mut session = fixture.child.clone();
    session.pid = u32::MAX;
    std::fs::write(metadata, serde_json::to_vec(&session).unwrap()).unwrap();
    fixture
        .registry
        .gc(std::time::Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        fixture.registry.message_history(fixture.child.id).unwrap(),
        vec![message]
    );
}

#[test]
fn interrupted_completion_delivery_deduplicates_after_parent_reregisters() {
    let fixture = ProtocolFixture::new();
    let mut completed = fixture.complete("first result");
    completed.result.as_mut().unwrap().delivered = false;
    std::fs::write(
        fixture
            .runtime
            .path()
            .join("subagents")
            .join(format!("{}.json", completed.spawn_id)),
        serde_json::to_vec(&completed).unwrap(),
    )
    .unwrap();
    fixture.registry.unregister(fixture.parent.id).unwrap();
    let returned = register_protocol_session(
        &fixture.registry,
        fixture.project.path(),
        "returned",
        "parent-pi",
    );
    let retried = fixture.complete("must not duplicate");
    assert!(retried.result.unwrap().delivered);
    assert!(
        fixture
            .registry
            .pending_messages(returned.id)
            .unwrap()
            .is_empty()
    );
    let history = fixture.registry.message_history(fixture.parent.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, completed.result.unwrap().message_id);
}

#[test]
fn concurrent_completion_retries_enqueue_one_immutable_result() {
    let fixture = ProtocolFixture::new();
    let barrier = std::sync::Barrier::new(8);
    let records = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let barrier = &barrier;
                let fixture = &fixture;
                scope.spawn(move || {
                    barrier.wait();
                    fixture.complete(&format!("result {index}"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    for record in &records {
        assert_eq!(record, &records[0]);
    }
    let messages = fixture
        .registry
        .pending_messages(fixture.parent.id)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, records[0].result.as_ref().unwrap().text);
    fixture
        .registry
        .acknowledge_message(fixture.parent.id, messages[0].id)
        .unwrap();
    thread::scope(|scope| {
        for _ in 0..8 {
            let fixture = &fixture;
            scope.spawn(move || {
                fixture.complete("retry");
            });
        }
    });
    assert!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.registry.message_history(fixture.parent.id).unwrap(),
        messages
    );
}

#[test]
fn concurrent_assignment_binding_and_old_completion_do_not_lose_new_task() {
    let fixture = ProtocolFixture::new();
    fixture.complete("first done");
    let barrier = std::sync::Barrier::new(3);
    thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            fixture
                .store
                .assign(
                    fixture.record.spawn_id,
                    fixture.parent.id,
                    "next task".to_owned(),
                )
                .unwrap();
        });
        scope.spawn(|| {
            barrier.wait();
            fixture
                .store
                .bind(
                    fixture.record.spawn_id,
                    "child-pi".to_owned(),
                    fixture.child.id,
                    "renamed".to_owned(),
                )
                .unwrap();
        });
        scope.spawn(|| {
            barrier.wait();
            let _ = fixture.store.complete(
                fixture.record.spawn_id,
                fixture.child.id,
                &fixture.record.task_id,
                "old retry".to_owned(),
            );
        });
    });
    let record = fixture.store.show(fixture.record.spawn_id).unwrap();
    assert_eq!(record.name, "renamed");
    assert_eq!(record.task_state, TaskState::Active);
    assert_eq!(record.task, "next task");
    assert_ne!(record.task_id, fixture.record.task_id);
    assert!(record.result.is_none());
    assert_eq!(record.result_history.len(), 1);
    assert_eq!(record.result_history[0].text, "first done");
    assert_eq!(
        fixture
            .registry
            .pending_messages(fixture.parent.id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .registry
            .pending_messages(fixture.child.id)
            .unwrap()
            .len(),
        1
    );
}
