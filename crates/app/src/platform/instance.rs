//! One app per user. Every build shares the same saved workspaces, so a
//! second launch hands its folder/prompt to the running app and exits.
//!
//! The running app listens on a loopback TCP port recorded, with a random
//! token, in `<data dir>/gentle-shell-gpui/instance.json` (mode 0600). A
//! launch that cannot reach a live listener becomes the primary.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gentle_core::workspace_commands::{Command as WorkspaceCommand, Reply as WorkspaceReply};
use serde::{Deserialize, Serialize};

/// What a second launch asks the running app to do.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub folder: Option<PathBuf>,
    pub prompt: Option<String>,
    /// A page to show in the in-app browser (`gentle-open`).
    #[serde(default)]
    pub url: Option<String>,
    /// `gentle-device`'s arguments, for the device panel.
    #[serde(default)]
    pub device: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
struct Message {
    token: String,
    #[serde(default)]
    request: OpenRequest,
    /// A `gentle-browser` operation; answered with one JSON line instead
    /// of "ok".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    browser: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceCommand>,
}

/// What reaches the running app from a later launch or a command.
pub enum Incoming {
    Open(OpenRequest),
    Workspace {
        command: WorkspaceCommand,
        reply: std::sync::mpsc::Sender<WorkspaceReply>,
    },
    /// A `gentle-browser` operation and where its answer goes.
    Browser {
        op: serde_json::Value,
        reply: std::sync::mpsc::Sender<serde_json::Value>,
    },
}

pub enum Role {
    /// This process owns the app; requests from later launches arrive here.
    Primary(async_channel::Receiver<Incoming>),
    /// Another instance took the request; exit.
    Forwarded,
}

fn record_path() -> Option<PathBuf> {
    crate::shared::data_dir::app().map(|dir| dir.join("instance.json"))
}

/// Hand `request` to a running instance, or become the primary.
pub fn claim(request: &OpenRequest) -> Role {
    let Some(path) = record_path() else {
        return Role::Primary(async_channel::unbounded().1);
    };
    if forward(&path, request) {
        return Role::Forwarded;
    }
    match listen(&path) {
        Ok(rx) => Role::Primary(rx),
        // Never block the user from opening the app over the guard itself.
        Err(_) => Role::Primary(async_channel::unbounded().1),
    }
}

/// Hand `request` to the running app; false when none answers. Commands
/// (`gentle-device`) use it: they never become the app themselves.
pub fn send(request: &OpenRequest) -> bool {
    record_path().is_some_and(|path| forward(&path, request))
}

/// The running app's address and token, if one is recorded.
fn record(path: &Path) -> Option<Record> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Record>(&raw).ok())
}

fn forward(path: &Path, request: &OpenRequest) -> bool {
    let Some(record) = record(path) else {
        return false;
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], record.port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let message = Message {
        token: record.token,
        request: request.clone(),
        browser: None,
        workspace: None,
    };
    let Ok(mut line) = serde_json::to_vec(&message) else {
        return false;
    };
    line.push(b'\n');
    if stream.write_all(&line).is_err() {
        return false;
    }
    // The primary acknowledges with "ok" once it accepted the token.
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Hand a `gentle-browser` operation to the running app and wait up to
/// `timeout` for its answer.
pub fn browser(op: &serde_json::Value, timeout: Duration) -> Result<serde_json::Value, String> {
    let not_running = || "the Gentle Shell app isn't running".to_string();
    let record = record_path()
        .and_then(|path| record(&path))
        .ok_or_else(not_running)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], record.port));
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(400)).map_err(|_| not_running())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let message = Message {
        token: record.token,
        request: OpenRequest::default(),
        browser: Some(op.clone()),
        workspace: None,
    };
    let mut line = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|_| "the app didn't answer in time".to_string())?;
    serde_json::from_str(reply.trim()).map_err(|_| "the app didn't answer".to_string())
}

/// None means no connection was made; an error after connecting must not be retried.
pub fn workspace(command: &WorkspaceCommand) -> Result<Option<WorkspaceReply>, String> {
    let Some(path) = record_path() else {
        return Ok(None);
    };
    workspace_at(&path, command)
}

fn workspace_at(path: &Path, command: &WorkspaceCommand) -> Result<Option<WorkspaceReply>, String> {
    let Some(record) = record(path) else {
        return Ok(None);
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], record.port));
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
        Ok(stream) => stream,
        Err(error) => {
            // A closed app leaves its instance record behind. On Windows
            // connecting to that port may time out instead of being refused.
            // Never start a second app if the recorded owner is still alive.
            if unavailable_endpoint(error.kind()) && !owner_is_running(record.pid) {
                return Ok(None);
            }
            return Err(format!("Could not reach Desktop: {error}"));
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let message = Message {
        token: record.token,
        request: OpenRequest::default(),
        browser: None,
        workspace: Some(command.clone()),
    };
    let mut line = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .map_err(|e| format!("Request outcome unknown: {e}"))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| format!("Request outcome unknown: {e}"))?;
    serde_json::from_str(&reply).map(Some).map_err(|_| {
        "Desktop did not return a workspace reply; check its state and version before retrying."
            .into()
    })
}

fn unavailable_endpoint(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
    )
}

fn owner_is_running(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), ProcessRefreshKind::new());
    system.process(pid).is_some()
}

fn listen(path: &Path) -> std::io::Result<async_channel::Receiver<Incoming>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string(&Record {
        port,
        token: token.clone(),
        pid: std::process::id(),
    })?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)?;

    let (tx, rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Message>(&line) else {
                    continue;
                };
                if message.token != token {
                    continue;
                }
                if let Some(command) = message.workspace {
                    let tx = tx.clone();
                    let _ = std::thread::Builder::new()
                        .name("workspace-op".into())
                        .spawn(move || answer_workspace(stream, command, &tx));
                    continue;
                }
                if let Some(op) = message.browser {
                    // An operation may wait (for the page, or for the user to
                    // pick an element): answered on a thread of its own.
                    let tx = tx.clone();
                    let _ = std::thread::Builder::new()
                        .name("browser-op".into())
                        .spawn(move || answer_browser(stream, op, &tx));
                    continue;
                }
                let _ = stream.write_all(b"ok\n");
                if tx.send_blocking(Incoming::Open(message.request)).is_err() {
                    return;
                }
            }
        })?;
    Ok(rx)
}

fn answer_workspace(
    mut stream: TcpStream,
    command: WorkspaceCommand,
    tx: &async_channel::Sender<Incoming>,
) {
    let (reply, answer) = std::sync::mpsc::channel();
    let result = if tx
        .send_blocking(Incoming::Workspace { command, reply })
        .is_err()
    {
        Err("The app is closing".into())
    } else {
        answer
            .recv_timeout(Duration::from_secs(25))
            .unwrap_or_else(|_| {
                Err("Request outcome unknown; check the workspace before retrying".into())
            })
    };
    if let Ok(mut line) = serde_json::to_vec(&result) {
        line.push(b'\n');
        let _ = stream.write_all(&line);
    }
}

/// Hand `op` to the app and write its answer back as one JSON line.
fn answer_browser(
    mut stream: TcpStream,
    op: serde_json::Value,
    tx: &async_channel::Sender<Incoming>,
) {
    let (reply, answer) = std::sync::mpsc::channel();
    let wait = gentle_core::browser::answer_within(&op);
    let answer = if tx.send_blocking(Incoming::Browser { op, reply }).is_err() {
        gentle_core::browser::error("closed", "the app is closing")
    } else {
        answer.recv_timeout(wait).unwrap_or_else(|_| {
            gentle_core::browser::error("timeout", "the app didn't answer in time")
        })
    };
    let mut line = answer.to_string();
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

/// 128 random bits from the OS clock, pid and address-space layout; enough
/// to keep other local programs from steering the app by guessing.
fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut out = String::new();
    for salt in 0..2u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Incoming, OpenRequest, forward, listen};

    #[test]
    fn stale_workspace_endpoint_can_be_refused_or_time_out() {
        use std::io::ErrorKind;

        assert!(super::unavailable_endpoint(ErrorKind::ConnectionRefused));
        assert!(super::unavailable_endpoint(ErrorKind::TimedOut));
        assert!(!super::unavailable_endpoint(ErrorKind::PermissionDenied));
        assert!(super::owner_is_running(std::process::id()));
    }

    #[test]
    fn workspace_request_waits_for_real_reply() {
        use gentle_core::workspace_commands::{Command, Output};
        let path = std::env::temp_dir()
            .join(format!("gs-workspace-ipc-test-{}", std::process::id()))
            .join("instance.json");
        let rx = listen(&path).unwrap();
        let client_path = path.clone();
        let client = std::thread::spawn(move || super::workspace_at(&client_path, &Command::List));
        match rx.recv_blocking().unwrap() {
            Incoming::Workspace { command, reply } => {
                assert_eq!(command, Command::List);
                reply.send(Ok(Output::Workspaces(Vec::new()))).unwrap();
            }
            _ => panic!("expected workspace request"),
        }
        assert_eq!(
            client.join().unwrap().unwrap(),
            Some(Ok(Output::Workspaces(Vec::new())))
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn second_launch_is_forwarded_to_the_primary() {
        let path = std::env::temp_dir()
            .join(format!("gs-instance-test-{}", std::process::id()))
            .join("instance.json");
        let rx = listen(&path).expect("primary listens");
        let request = OpenRequest {
            folder: Some("/tmp".into()),
            prompt: Some("hola".into()),
            url: None,
            device: None,
        };
        assert!(forward(&path, &request), "a live primary accepts");
        match rx.recv_blocking().unwrap() {
            Incoming::Open(received) => assert_eq!(received, request),
            Incoming::Browser { .. } | Incoming::Workspace { .. } => {
                panic!("an open request arrives as Open")
            }
        }

        // A record pointing at a dead listener means "become primary".
        std::fs::write(&path, r#"{"port":1,"token":"x","pid":1}"#).unwrap();
        assert!(!forward(&path, &request));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
