use std::process::Command;

use paj::registry::Session;
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
