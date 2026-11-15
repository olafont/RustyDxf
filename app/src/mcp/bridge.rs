// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! The MCP server's connection to a running RustyDxf window ("live" mode).

use crate::ipc::{Client, Request, Response};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for a freshly launched GUI to start listening.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the background thread checks the connection.
const KEEPALIVE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Bridge {
    client: Mutex<Option<Client>>,
}

impl Bridge {
    /// Create the bridge and keep it connected in the background, so the
    /// window's "MCP live" indicator reflects reality whichever process
    /// started first.
    pub fn start() -> Arc<Bridge> {
        let bridge = Arc::new(Bridge::default());
        let weak = Arc::downgrade(&bridge);
        let spawned = std::thread::Builder::new()
            .name("rustydxf-mcp-bridge".into())
            .spawn(move || {
                while let Some(bridge) = weak.upgrade() {
                    bridge.keepalive();
                    drop(bridge);
                    std::thread::sleep(KEEPALIVE);
                }
            });
        if let Err(e) = spawned {
            log::warn!("cannot start the bridge keepalive thread: {e}");
        }
        bridge
    }

    fn connect() -> io::Result<Client> {
        let mut client = Client::connect()?;
        client.request(&Request::Hello { role: "mcp".into() })?;
        Ok(client)
    }

    /// Connect if needed; drop the connection if the GUI went away.
    fn keepalive(&self) {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(client) => {
                if client
                    .request(&Request::Hello { role: "mcp".into() })
                    .is_err()
                {
                    *guard = None;
                }
            }
            None => *guard = Self::connect().ok(),
        }
    }

    /// Send a request to the GUI. Fails when no GUI is running.
    pub fn request(&self, request: &Request) -> io::Result<Response> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = guard.as_mut() {
            match client.request(request) {
                Ok(r) => return Ok(r),
                // The GUI may have been restarted: reconnect once below.
                Err(_) => *guard = None,
            }
        }
        let mut client = Self::connect()?;
        let response = client.request(request)?;
        *guard = Some(client);
        Ok(response)
    }

    /// Like [`Self::request`], but starts the GUI first if none is running.
    pub fn request_launching(&self, request: &Request) -> io::Result<Response> {
        match self.request(request) {
            Ok(r) => Ok(r),
            Err(e) => {
                // Lets tests and headless hosts keep windows from opening.
                if std::env::var_os("RUSTYDXF_NO_LAUNCH").is_some_and(|v| !v.is_empty()) {
                    return Err(io::Error::new(
                        e.kind(),
                        "the viewer is not running and RUSTYDXF_NO_LAUNCH is set",
                    ));
                }
                if !crate::settings::Settings::load().mcp.launch_viewer {
                    return Err(io::Error::new(
                        e.kind(),
                        "the viewer is not running, and starting it from MCP is turned off in \
                         the RustyDxf settings. Open RustyDxf first.",
                    ));
                }
                launch_gui()?;
                let deadline = Instant::now() + LAUNCH_TIMEOUT;
                loop {
                    std::thread::sleep(Duration::from_millis(150));
                    match self.request(request) {
                        Ok(r) => return Ok(r),
                        Err(e) if Instant::now() >= deadline => {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                format!("the viewer was started but did not respond: {e}"),
                            ));
                        }
                        Err(_) => {}
                    }
                }
            }
        }
    }
}

/// Start the GUI as a detached process.
fn launch_gui() -> io::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    // stdout must not be inherited: in stdio mode it carries the protocol.
    let mut child = Command::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap the child when it exits so it does not linger as a zombie. The
    // window itself outlives this MCP process if the client disconnects.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
