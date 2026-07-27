use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::registry::Session;

const PROTOCOL_VERSION: u8 = 1;
const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeRequest<'a> {
    version: u8,
    id: Uuid,
    method: &'static str,
    params: PromptParams<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PromptParams<'a> {
    text: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum BridgeEvent {
    Accepted {
        version: u8,
        id: Uuid,
    },
    Delta {
        version: u8,
        id: Uuid,
        text: String,
    },
    Complete {
        version: u8,
        id: Uuid,
        text: String,
    },
    Error {
        version: u8,
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

    fn version(&self) -> u8 {
        match self {
            Self::Accepted { version, .. }
            | Self::Delta { version, .. }
            | Self::Complete { version, .. }
            | Self::Error { version, .. } => *version,
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

    pub fn prompt(
        &self,
        session: &Session,
        text: &str,
        mut on_event: impl FnMut(&BridgeEvent),
    ) -> Result<(), BridgeError> {
        if text.trim().is_empty() {
            return Err(BridgeError::EmptyPrompt);
        }
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
        let request = BridgeRequest {
            version: PROTOCOL_VERSION,
            id: request_id,
            method: "prompt",
            params: PromptParams { text },
        };
        serde_json::to_writer(&mut stream, &request)?;
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
            if event.version() != PROTOCOL_VERSION {
                return Err(BridgeError::ProtocolVersion(event.version()));
            }
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
    #[error("prompt cannot be empty")]
    EmptyPrompt,
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
    #[error("bridge returned unsupported protocol version {0}")]
    ProtocolVersion(u8),
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

    use tempfile::tempdir;

    use super::{BridgeClient, BridgeEvent};
    use crate::project::Project;
    use crate::registry::{Registration, Registry};

    #[test]
    fn prompt_streams_correlated_events_until_completion() {
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
            assert_eq!(request["params"]["text"], "hello");
            let id = request["id"].as_str().expect("request should have an ID");
            write!(
                stream,
                "{{\"event\":\"accepted\",\"version\":1,\"id\":\"{id}\"}}\n\
                 {{\"event\":\"delta\",\"version\":1,\"id\":\"{id}\",\"text\":\"hel\"}}\n\
                 {{\"event\":\"complete\",\"version\":1,\"id\":\"{id}\",\"text\":\"hello\"}}\n"
            )
            .expect("events should be written");
        });
        let mut events = Vec::new();

        BridgeClient::new(std::time::Duration::from_secs(1))
            .prompt(&session, "hello", |event| events.push(event.clone()))
            .expect("prompt should complete");
        server.join().expect("server should stop");

        assert!(matches!(events.as_slice(), [
            BridgeEvent::Accepted { .. },
            BridgeEvent::Delta { text, .. },
            BridgeEvent::Complete { .. }
        ] if text == "hel"));
    }
}
