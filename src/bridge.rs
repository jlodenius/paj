use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::registry::Session;

const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditorSource {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditorRequest {
    Query {
        query: String,
        source: EditorSource,
    },
    Explain {
        source: EditorSource,
        focus: Option<String>,
    },
    Review {
        source: EditorSource,
        focus: Option<String>,
    },
    Followup {
        question: String,
    },
    AcceptAction {
        action_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BridgeRequest<'a> {
    id: Uuid,
    method: &'static str,
    params: &'a EditorRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeAction {
    pub id: Uuid,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum BridgeEvent {
    Accepted {
        id: Uuid,
    },
    Delta {
        id: Uuid,
        text: String,
    },
    Complete {
        id: Uuid,
        text: String,
        actions: Vec<BridgeAction>,
    },
    Error {
        id: Uuid,
        code: String,
        message: String,
    },
}

impl BridgeEvent {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Delta { text, .. } | Self::Complete { text, .. } => Some(text),
            Self::Accepted { .. } | Self::Error { .. } => None,
        }
    }

    fn id(&self) -> Uuid {
        match self {
            Self::Accepted { id, .. }
            | Self::Delta { id, .. }
            | Self::Complete { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeClient {
    timeout: Duration,
}

impl BridgeClient {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn request(
        &self,
        session: &Session,
        request: &EditorRequest,
        mut on_event: impl FnMut(&BridgeEvent),
    ) -> Result<(), BridgeError> {
        let socket = session
            .bridge_socket
            .as_ref()
            .ok_or(BridgeError::UnsupportedSession)?;
        let mut stream = UnixStream::connect(socket).map_err(|source| BridgeError::Connect {
            socket: socket.clone(),
            source,
        })?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        let request_id = Uuid::now_v7();
        let envelope = BridgeRequest {
            id: request_id,
            method: "request",
            params: request,
        };
        serde_json::to_writer(&mut stream, &envelope)?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut accepted = false;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    BridgeError::Timeout(self.timeout)
                } else {
                    BridgeError::Io(error)
                }
            })?;
            if bytes == 0 {
                return Err(BridgeError::Disconnected);
            }
            if bytes > MAX_EVENT_BYTES {
                return Err(BridgeError::EventTooLarge);
            }
            let event: BridgeEvent = serde_json::from_str(&line)?;
            if event.id() != request_id {
                return Err(BridgeError::RequestIdMismatch);
            }
            if !accepted
                && !matches!(
                    event,
                    BridgeEvent::Accepted { .. } | BridgeEvent::Error { .. }
                )
            {
                return Err(BridgeError::EventBeforeAcceptance);
            }
            on_event(&event);
            match event {
                BridgeEvent::Accepted { .. } => accepted = true,
                BridgeEvent::Complete { .. } => return Ok(()),
                BridgeEvent::Error { code, message, .. } => {
                    return Err(BridgeError::Remote { code, message });
                }
                BridgeEvent::Delta { .. } => {}
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("session does not advertise a bridge socket; restart Pi after updating Paj")]
    UnsupportedSession,
    #[error("failed to connect to bridge socket {socket}")]
    Connect {
        socket: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("bridge did not complete within {0:?}")]
    Timeout(Duration),
    #[error("bridge disconnected before completing the request")]
    Disconnected,
    #[error("bridge event exceeded the maximum size")]
    EventTooLarge,
    #[error("bridge returned an event for a different request")]
    RequestIdMismatch,
    #[error("bridge returned output before accepting the request")]
    EventBeforeAcceptance,
    #[error("bridge rejected the request ({code}): {message}")]
    Remote { code: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn bridge_is_available(session: &Session) -> bool {
    session.bridge_socket.as_ref().is_some_and(|path| {
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixListener;
    use std::thread;

    use tempfile::{TempDir, tempdir};

    use super::{
        BridgeAction, BridgeClient, BridgeError, BridgeEvent, EditorRequest, EditorSource,
    };
    use crate::project::Project;
    use crate::registry::{Registration, Registry, Session};

    fn registered_bridge() -> (TempDir, Session, UnixListener) {
        let directory = tempdir().expect("temporary directory should be created");
        let registry =
            Registry::new(directory.path().join("paj")).expect("registry should be created");
        let project = Project {
            id: "project-id".to_owned(),
            root: directory.path().to_path_buf(),
        };
        let session = registry
            .register(
                &project,
                Registration {
                    pid: std::process::id(),
                    pi_session_id: None,
                    name: Some("primary".to_owned()),
                    role: "primary".to_owned(),
                    parent_pi_session_id: None,
                    task: None,
                    cwd: directory.path().to_path_buf(),
                    branch: None,
                },
            )
            .expect("session should register");
        let socket = session
            .bridge_socket
            .clone()
            .expect("session should advertise a socket");
        let listener = UnixListener::bind(&socket).expect("listener should bind");
        (directory, session, listener)
    }

    fn editor_request() -> EditorRequest {
        EditorRequest::Query {
            query: "hello".to_owned(),
            source: EditorSource {
                path: "/tmp/example.rs".to_owned(),
                start_line: 1,
                end_line: 1,
                content: "fn main() {}".to_owned(),
            },
        }
    }

    #[test]
    fn request_streams_correlated_events_until_completion() {
        let (_directory, session, listener) = registered_bridge();
        let socket = session
            .bridge_socket
            .clone()
            .expect("session should advertise a socket");
        assert!(
            socket
                .symlink_metadata()
                .expect("socket metadata should load")
                .file_type()
                .is_socket()
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client should connect");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("stream should clone"))
                .read_line(&mut request)
                .expect("request should be read");
            let request: serde_json::Value =
                serde_json::from_str(&request).expect("request should be JSON");
            assert_eq!(request["method"], "request");
            assert_eq!(request["params"]["kind"], "query");
            assert_eq!(request["params"]["query"], "hello");
            let id = request["id"].as_str().expect("request should have an ID");
            write!(
                stream,
                "{{\"event\":\"accepted\",\"id\":\"{id}\"}}\n\
                 {{\"event\":\"delta\",\"id\":\"{id}\",\"text\":\"hel\"}}\n\
                 {{\"event\":\"complete\",\"id\":\"{id}\",\"text\":\"hello\",\"actions\":[{{\"id\":\"019fa92e-a7c2-7072-84a7-8933262464a5\",\"title\":\"Improve it\",\"description\":\"Implement the improvement\"}}]}}\n"
            )
            .expect("events should be written");
        });
        let mut events = Vec::new();

        BridgeClient::new(std::time::Duration::from_secs(1))
            .request(&session, &editor_request(), |event| {
                events.push(event.clone())
            })
            .expect("request should complete");
        server.join().expect("server should stop");

        assert!(matches!(events.as_slice(), [
            BridgeEvent::Accepted { .. },
            BridgeEvent::Delta { text, .. },
            BridgeEvent::Complete { actions, .. }
        ] if text == "hel"
            && actions.len() == 1
            && actions[0].title == "Improve it"
            && actions[0].description == "Implement the improvement"));
    }

    #[test]
    fn complete_event_serializes_actions_with_camel_case_fields() {
        let request_id = uuid::Uuid::now_v7();
        let action_id = uuid::Uuid::now_v7();
        let event = BridgeEvent::Complete {
            id: request_id,
            text: "response".to_owned(),
            actions: vec![BridgeAction {
                id: action_id,
                title: "Change title".to_owned(),
                description: "Change description".to_owned(),
            }],
        };

        assert_eq!(
            serde_json::to_value(event).expect("event should serialize"),
            serde_json::json!({
                "event": "complete",
                "id": request_id,
                "text": "response",
                "actions": [{
                    "id": action_id,
                    "title": "Change title",
                    "description": "Change description"
                }]
            })
        );
    }

    #[test]
    fn complete_event_requires_actions_but_accepts_an_empty_array() {
        let id = uuid::Uuid::now_v7();
        let empty =
            format!("{{\"event\":\"complete\",\"id\":\"{id}\",\"text\":\"done\",\"actions\":[]}}");
        assert!(matches!(
            serde_json::from_str::<BridgeEvent>(&empty),
            Ok(BridgeEvent::Complete { actions, .. }) if actions.is_empty()
        ));
        let missing = format!("{{\"event\":\"complete\",\"id\":\"{id}\",\"text\":\"done\"}}");
        assert!(serde_json::from_str::<BridgeEvent>(&missing).is_err());
    }

    #[test]
    fn request_rejects_an_event_for_a_different_request() {
        let (_directory, session, listener) = registered_bridge();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client should connect");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("stream should clone"))
                .read_line(&mut request)
                .expect("request should be read");
            let other_id = uuid::Uuid::now_v7();
            writeln!(stream, "{{\"event\":\"accepted\",\"id\":\"{other_id}\"}}")
                .expect("event should be written");
        });

        let result = BridgeClient::new(std::time::Duration::from_secs(1)).request(
            &session,
            &editor_request(),
            |_| {},
        );
        server.join().expect("server should stop");

        assert!(matches!(result, Err(BridgeError::RequestIdMismatch)));
    }

    #[test]
    fn request_rejects_output_before_acceptance() {
        let (_directory, session, listener) = registered_bridge();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client should connect");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("stream should clone"))
                .read_line(&mut request)
                .expect("request should be read");
            let request: serde_json::Value =
                serde_json::from_str(&request).expect("request should be JSON");
            let id = request["id"].as_str().expect("request should have an ID");
            writeln!(
                stream,
                "{{\"event\":\"delta\",\"id\":\"{id}\",\"text\":\"early\"}}"
            )
            .expect("event should be written");
        });

        let result = BridgeClient::new(std::time::Duration::from_secs(1)).request(
            &session,
            &editor_request(),
            |_| {},
        );
        server.join().expect("server should stop");

        assert!(matches!(result, Err(BridgeError::EventBeforeAcceptance)));
    }
}
