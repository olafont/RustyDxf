//! RustyDxf — a small, fast DXF viewer by art-tech, with a built-in MCP server.

// Windows note: this stays a console-subsystem binary because `--mcp` (stdio)
// and `--register` need standard streams; GUI mode detaches from the console.

mod app;
mod gpu;
mod ipc;
mod mcp;
mod platform;
mod settings;
mod ui;

use anyhow::{Context, Result};
use rustydxf_core::brand;
use std::path::PathBuf;

const ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");

const USAGE: &str = "\
Usage:
  rustydxf [FILE.dxf]                 Open the viewer (optionally with a file)
  rustydxf --mcp                      Run as an MCP server over stdio
  rustydxf --mcp --http [127.0.0.1:PORT]
                                      Run as an MCP server over Streamable HTTP (localhost only;
                                      without an address, the port set in Preferences)
  rustydxf --register                 Make RustyDxf the default app for .dxf files
  rustydxf --mcp-config               Print the MCP client config for this install
  rustydxf --version | --help

Logs go to stderr; set RUST_LOG=debug for more detail.";

#[derive(Debug, PartialEq)]
enum Command {
    Gui(Vec<PathBuf>),
    Mcp { http: Option<String> },
    Register,
    McpConfig,
    Version,
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut files = Vec::new();
    let mut mcp = false;
    let mut http = None;
    let mut command = None;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mcp" => mcp = true,
            "--http" => {
                // The address is optional: without one, the port from the
                // preferences is used.
                http = Some(
                    args.next_if(|next| !next.starts_with('-'))
                        .unwrap_or_default(),
                );
            }
            "--register" => command = Some(Command::Register),
            "--mcp-config" => command = Some(Command::McpConfig),
            "--version" | "-V" => command = Some(Command::Version),
            "--help" | "-h" => command = Some(Command::Help),
            // Legacy process serial number passed by old macOS launchers.
            a if a.starts_with("-psn_") => {}
            a if a.starts_with("--http=") => http = Some(a["--http=".len()..].to_string()),
            a if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option: {a}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if let Some(c) = command {
        return Ok(c);
    }
    if mcp {
        return Ok(Command::Mcp { http });
    }
    if http.is_some() {
        return Err("--http only applies together with --mcp".into());
    }
    Ok(Command::Gui(files))
}

fn main() {
    // stderr only: in MCP stdio mode stdout belongs to the protocol.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let command = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(message) => {
            eprintln!("rustydxf: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let result = match command {
        Command::Gui(files) => run_gui(files),
        Command::Mcp { http } => mcp::run(http),
        Command::Register => platform::register(),
        Command::McpConfig => mcp::client_config_json().map(|json| println!("{json}")),
        Command::Version => {
            println!(
                "{} {} — by {}",
                brand::PRODUCT_NAME,
                env!("CARGO_PKG_VERSION"),
                brand::VENDOR
            );
            Ok(())
        }
        Command::Help => {
            println!(
                "{} — {}\n\n{USAGE}",
                brand::PRODUCT_NAME,
                brand::DESCRIPTION
            );
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("rustydxf: {e:#}");
        std::process::exit(1);
    }
}

fn run_gui(files: Vec<PathBuf>) -> Result<()> {
    let files: Vec<PathBuf> = files
        .into_iter()
        .map(|p| std::path::absolute(&p).unwrap_or(p))
        .collect();

    // Single instance: hand the file to a window that is already running.
    if let Ok(mut running) = ipc::Client::connect() {
        let request = match files.last() {
            Some(path) => ipc::Request::Open {
                path: path.display().to_string(),
                layers: None,
                bbox: None,
            },
            None => ipc::Request::Focus,
        };
        match running.request(&request) {
            Ok(response) if response.ok => return Ok(()),
            Ok(response) => {
                anyhow::bail!(
                    "{}",
                    response
                        .error
                        .unwrap_or_else(|| "the running viewer refused the request".into())
                );
            }
            // The other instance vanished mid-request: start normally.
            Err(e) => log::warn!("could not forward to the running instance: {e}"),
        }
    }

    // SAFETY: FreeConsole has no preconditions; failure is harmless.
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }

    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .context("creating the event loop")?;
    // After the event loop exists (winit has installed its app delegate) and
    // before it runs (a cold launch from Finder delivers its file early).
    platform::install_open_handler();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(brand::PRODUCT_NAME)
        .with_app_id(brand::APP_ID)
        .with_inner_size([1200.0, 800.0])
        .with_min_inner_size([480.0, 320.0])
        .with_drag_and_drop(true);
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => log::warn!("cannot decode the window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let mut app = eframe::create_native(
        brand::PRODUCT_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, files)))),
        &event_loop,
    );
    event_loop
        .run_app(&mut app)
        .context("running the event loop")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn argument_parsing() {
        assert_eq!(parse(&[]), Ok(Command::Gui(vec![])));
        assert_eq!(
            parse(&["a.dxf"]),
            Ok(Command::Gui(vec![PathBuf::from("a.dxf")]))
        );
        assert_eq!(parse(&["--mcp"]), Ok(Command::Mcp { http: None }));
        assert_eq!(
            parse(&["--mcp", "--http", "127.0.0.1:8765"]),
            Ok(Command::Mcp {
                http: Some("127.0.0.1:8765".into())
            })
        );
        assert_eq!(
            parse(&["--http=127.0.0.1:1", "--mcp"]),
            Ok(Command::Mcp {
                http: Some("127.0.0.1:1".into())
            })
        );
        assert_eq!(parse(&["--register"]), Ok(Command::Register));
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(
            parse(&["-psn_0_12345", "x.dxf"]),
            Ok(Command::Gui(vec![PathBuf::from("x.dxf")]))
        );
        assert!(parse(&["--http", "127.0.0.1:1"]).is_err());
        assert_eq!(
            parse(&["--mcp", "--http"]),
            Ok(Command::Mcp {
                http: Some(String::new())
            })
        );
        assert_eq!(
            parse(&["--http", "--mcp"]),
            Ok(Command::Mcp {
                http: Some(String::new())
            })
        );
        assert!(parse(&["--http"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
