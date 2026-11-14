//! Local IPC between RustyDxf processes.
//!
//! The running GUI listens on a per-user endpoint (a Unix domain socket in
//! the user's runtime/config directory on macOS and Linux, a named pipe on
//! Windows). Two kinds of clients connect to it:
//!
//! * a second `rustydxf file.dxf` invocation, which forwards the file to the
//!   existing window and exits (single-instance behaviour), and
//! * the MCP server process (`rustydxf --mcp`), which drives the window for
//!   the "live" tools.
//!
//! The protocol is one JSON object per line: a [`Request`] answered by a
//! [`Response`].

use interprocess::local_socket::{ListenerOptions, Name, Stream, prelude::*};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the GUI may take to answer a request (loading a big file).
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Identify the client. `role` is "mcp" for the MCP bridge.
    Hello { role: String },
    /// Open (or re-focus) a file, optionally setting layers and view.
    Open {
        path: String,
        /// Names of the layers to show; all others are hidden.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layers: Option<Vec<String>>,
        /// `[min_x, min_y, max_x, max_y]` to zoom to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bbox: Option<[f64; 4]>,
    },
    /// Bring the window to the front.
    Focus,
    /// Report the current viewer state.
    State,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ViewerState {
    /// Absolute path of the open file, if any.
    pub file: Option<String>,
    pub loading: bool,
    /// World rectangle currently visible: `[min_x, min_y, max_x, max_y]`.
    pub viewport_bbox: Option<[f64; 4]>,
    /// Screen points per drawing unit.
    pub zoom: f64,
    pub visible_layers: Vec<String>,
    pub hidden_layers: Vec<String>,
    pub entity_count: usize,
    pub units: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ViewerState>,
}

impl Response {
    pub fn ok(state: Option<ViewerState>) -> Self {
        Response {
            ok: true,
            error: None,
            state,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Response {
            ok: false,
            error: Some(message.into()),
            state: None,
        }
    }
}

/// A request handed to the GUI thread, with the channel for its answer.
pub struct Job {
    pub request: Request,
    /// The connection introduced itself as an MCP server.
    pub from_mcp: bool,
    pub reply: mpsc::Sender<Response>,
}

// ---------------------------------------------------------------------------
// Endpoint

/// Optional instance name from `RUSTYDXF_INSTANCE`. Viewers and MCP servers
/// with the same name share one endpoint; the integration tests use it to
/// stay isolated from a viewer the user has open.
fn instance_name() -> Option<String> {
    let name: String = std::env::var("RUSTYDXF_INSTANCE")
        .ok()?
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Filesystem location of the socket (Unix only).
#[cfg(unix)]
pub fn socket_path() -> io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let dirs = directories::ProjectDirs::from("dev", "art-tech", "rustydxf");
    let mut dir = dirs
        .as_ref()
        .and_then(|d| d.runtime_dir().map(|p| p.join("rustydxf")))
        .or_else(|| dirs.as_ref().map(|d| d.config_dir().to_path_buf()));

    // Unix socket paths are limited to ~104 bytes; fall back to a private
    // directory under the temp dir for unusually long home paths.
    let uid = unsafe { libc_getuid() };
    if dir
        .as_ref()
        .is_none_or(|d| d.as_os_str().len() + "/rustydxf-.sock".len() + 32 > 100)
    {
        dir = Some(std::env::temp_dir().join(format!("rustydxf-{uid}")));
    }
    let dir = dir.expect("directory chosen above");

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    // The directory is the access boundary: it must be ours and private.
    let meta = std::fs::metadata(&dir)?;
    if meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", dir.display()),
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir.join(match instance_name() {
        Some(instance) => format!("rustydxf-{instance}.sock"),
        None => "rustydxf.sock".to_string(),
    }))
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(unix)]
fn endpoint_name() -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericFilePath;
    Ok(socket_path()?.to_fs_name::<GenericFilePath>()?.into_owned())
}

#[cfg(windows)]
fn endpoint_name() -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;
    let user: String = std::env::var("USERNAME")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let instance = instance_name().map(|i| format!("-{i}")).unwrap_or_default();
    Ok(format!("dev.art-tech.rustydxf-{user}{instance}")
        .to_ns_name::<GenericNamespaced>()?
        .into_owned())
}

// ---------------------------------------------------------------------------
// Client

pub struct Client {
    stream: Stream,
}

impl Client {
    /// Connect to a running GUI. Fails quickly when none is listening.
    pub fn connect() -> io::Result<Client> {
        let stream = Stream::connect(endpoint_name()?)?;
        // Not supported on Windows named pipes; harmless to skip there.
        let _ = stream.set_recv_timeout(Some(REPLY_TIMEOUT + Duration::from_secs(10)));
        Ok(Client { stream })
    }

    pub fn request(&mut self, request: &Request) -> io::Result<Response> {
        let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
        line.push('\n');
        (&self.stream).write_all(line.as_bytes())?;
        (&self.stream).flush()?;

        // One response line per request, so a fresh reader cannot over-read.
        let mut reply = String::new();
        let n = BufReader::new(&self.stream).read_line(&mut reply)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the viewer closed the connection",
            ));
        }
        serde_json::from_str(&reply).map_err(io::Error::other)
    }
}

// ---------------------------------------------------------------------------
// Server

pub struct Server {
    mcp_clients: Arc<AtomicUsize>,
}

impl Server {
    /// Number of MCP bridge connections currently open.
    pub fn mcp_clients(&self) -> usize {
        self.mcp_clients.load(Ordering::Relaxed)
    }

    /// Start listening. `jobs` receives requests for the GUI thread and
    /// `wake` is called after each one so the GUI processes it promptly.
    ///
    /// Fails with `AddrInUse` when another instance already owns the
    /// endpoint.
    pub fn start(
        jobs: mpsc::Sender<Job>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Server> {
        let listener = create_listener()?;
        let mcp_clients = Arc::new(AtomicUsize::new(0));
        let counter = mcp_clients.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        std::thread::Builder::new()
            .name("rustydxf-ipc".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    let stream = match conn {
                        Ok(s) => s,
                        Err(e) => {
                            log::warn!("IPC accept failed: {e}");
                            continue;
                        }
                    };
                    let (jobs, counter, wake) = (jobs.clone(), counter.clone(), wake.clone());
                    let spawned = std::thread::Builder::new()
                        .name("rustydxf-ipc-conn".into())
                        .spawn(move || serve_connection(stream, jobs, counter, wake));
                    if let Err(e) = spawned {
                        log::warn!("cannot start IPC connection thread: {e}");
                    }
                }
            })?;
        Ok(Server { mcp_clients })
    }
}

fn create_listener() -> io::Result<interprocess::local_socket::Listener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = socket_path()?;
        if path.exists() {
            // A live instance answers; a stale file (after a crash) refuses.
            if Stream::connect(endpoint_name()?).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another RustyDxf instance is running",
                ));
            }
            std::fs::remove_file(&path)?;
        }
        let listener = ListenerOptions::new()
            .name(endpoint_name()?)
            .create_sync()?;
        // The 0700 parent directory already keeps other users out; tighten
        // the socket itself as well.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
    #[cfg(windows)]
    {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        let options = ListenerOptions::new().name(endpoint_name()?);
        let options = match crate::platform::windows::current_user_only_descriptor() {
            Ok(sd) => options.security_descriptor(sd),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "cannot restrict the IPC pipe to the current user: {e}"
                )));
            }
        };
        options.create_sync()
    }
}

struct McpGuard(Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>);

impl Drop for McpGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        (self.1)();
    }
}

fn serve_connection(
    stream: Stream,
    jobs: mpsc::Sender<Job>,
    counter: Arc<AtomicUsize>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let mut reader = BufReader::new(&stream);
    let mut mcp_guard: Option<McpGuard> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::error(format!("bad request: {e}")),
            Ok(Request::Hello { role }) => {
                if role == "mcp" && mcp_guard.is_none() {
                    counter.fetch_add(1, Ordering::Relaxed);
                    mcp_guard = Some(McpGuard(counter.clone(), wake.clone()));
                    wake();
                }
                Response::ok(None)
            }
            Ok(request) => {
                let (reply, answer) = mpsc::channel();
                let job = Job {
                    request,
                    from_mcp: mcp_guard.is_some(),
                    reply,
                };
                if jobs.send(job).is_err() {
                    break; // The GUI is shutting down.
                }
                wake();
                answer
                    .recv_timeout(REPLY_TIMEOUT)
                    .unwrap_or_else(|_| Response::error("the viewer did not answer in time"))
            }
        };
        let Ok(mut out) = serde_json::to_string(&response) else {
            break;
        };
        out.push('\n');
        if (&stream).write_all(out.as_bytes()).is_err() || (&stream).flush().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let open = Request::Open {
            path: "/tmp/a.dxf".into(),
            layers: Some(vec!["Walls".into()]),
            bbox: None,
        };
        let json = serde_json::to_string(&open).unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"open","path":"/tmp/a.dxf","layers":["Walls"]}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), open);
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"state"}"#).unwrap(),
            Request::State
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"hello","role":"mcp"}"#).unwrap(),
            Request::Hello { role: "mcp".into() }
        );
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"rm -rf"}"#).is_err());
    }

    #[test]
    fn response_round_trip() {
        let r = Response::ok(Some(ViewerState {
            file: Some("/x.dxf".into()),
            zoom: 2.0,
            visible_layers: vec!["0".into()],
            ..Default::default()
        }));
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), r);
        let e = Response::error("nope");
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"ok":false,"error":"nope"}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = socket_path().unwrap();
        let dir = path.parent().unwrap();
        let mode = std::fs::metadata(dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{} has mode {:o}", dir.display(), mode);
        assert!(path.as_os_str().len() < 104);
    }
}
