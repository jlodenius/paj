use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::thread;

use paj::project::Project;
use paj::registry::{Registration, Registry, Session};
use tempfile::tempdir;

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
