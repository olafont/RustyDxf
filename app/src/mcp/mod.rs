//! MCP server mode (`rustydxf --mcp`).
//!
//! * stdio (default): stdout carries the protocol, so nothing else may be
//!   written to it. Logs go to stderr.
//! * `--http ADDR`: Streamable HTTP, bound to a loopback address only.

mod bridge;
mod cache;
mod tools;

use anyhow::{Context, Result, bail};
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
use std::sync::Arc;
use tools::RustyDxfServer;

/// Run the MCP server until the client disconnects (stdio) or the process is
/// interrupted (HTTP).
pub fn run(http: Option<String>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async {
        let cache = Arc::new(cache::Cache::default());
        let bridge = bridge::Bridge::start();
        match http {
            None => serve_stdio(RustyDxfServer::new(cache, bridge)).await,
            Some(addr) => serve_http(&addr, cache, bridge).await,
        }
    })
}

async fn serve_stdio(server: RustyDxfServer) -> Result<()> {
    log::info!("RustyDxf MCP server on stdio");
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP initialisation failed")?;
    running.waiting().await.context("MCP server stopped")?;
    Ok(())
}

/// Parse and check the `--http` address: loopback only.
pub fn parse_http_addr(addr: &str) -> Result<SocketAddr> {
    let parsed: SocketAddr = match addr.strip_prefix("localhost:") {
        Some(port) => format!("127.0.0.1:{port}").parse(),
        None => addr.parse(),
    }
    .with_context(|| format!("invalid --http address '{addr}' (expected e.g. 127.0.0.1:8765)"))?;
    if !parsed.ip().is_loopback() {
        bail!(
            "refusing to bind the MCP server to {parsed}: only loopback addresses \
             (127.0.0.1 or [::1]) are allowed"
        );
    }
    Ok(parsed)
}

async fn serve_http(
    addr: &str,
    cache: Arc<cache::Cache>,
    bridge: Arc<bridge::Bridge>,
) -> Result<()> {
    let addr = if addr.is_empty() {
        let port = crate::settings::Settings::load().mcp.http_port;
        SocketAddr::from(([127, 0, 0, 1], port))
    } else {
        parse_http_addr(addr)?
    };
    // The default config already restricts the Host header to localhost
    // names (DNS-rebinding protection); also reject non-local browser origins.
    let config = StreamableHttpServerConfig::default().with_allowed_origins([
        "http://localhost:*",
        "http://127.0.0.1:*",
        "http://[::1]:*",
    ]);
    let service: StreamableHttpService<RustyDxfServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(RustyDxfServer::new(cache.clone(), bridge.clone())),
            Arc::new(LocalSessionManager::default()),
            config,
        );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let local = listener.local_addr().unwrap_or(addr);
    log::info!("RustyDxf MCP server on http://{local}/mcp");
    eprintln!("RustyDxf MCP server listening on http://{local}/mcp");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        log::warn!("accept failed: {e}");
                        continue;
                    }
                };
                let service = service.clone();
                tokio::spawn(async move {
                    let handler = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let service = service.clone();
                            async move {
                                Ok::<_, std::convert::Infallible>(service.handle(request).await)
                            }
                        },
                    );
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), handler)
                        .await
                    {
                        log::debug!("HTTP connection ended: {e}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("shutting down");
                return Ok(());
            }
        }
    }
}

/// The `claude mcp add` command that registers this install with Claude Code.
pub fn claude_code_command() -> Result<String> {
    let exe = std::env::current_exe().context("locating the executable")?;
    let exe = exe.canonicalize().unwrap_or(exe).display().to_string();
    let exe = if exe.contains([' ', '"', '\'']) {
        format!("\"{}\"", exe.replace('"', "\\\""))
    } else {
        exe
    };
    Ok(format!("claude mcp add rustydxf -- {exe} --mcp"))
}

/// JSON for an MCP client's config file (e.g. Claude Desktop's
/// `claude_desktop_config.json`), pointing at this install's binary.
pub fn client_config_json() -> Result<String> {
    let exe = std::env::current_exe().context("locating the executable")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let config = serde_json::json!({
        "mcpServers": {
            "rustydxf": {
                "command": exe.display().to_string(),
                "args": ["--mcp"],
            }
        }
    });
    serde_json::to_string_pretty(&config).context("serialising the MCP config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_address_must_be_loopback() {
        assert!(parse_http_addr("127.0.0.1:8765").is_ok());
        assert!(parse_http_addr("[::1]:8765").is_ok());
        assert_eq!(
            parse_http_addr("localhost:9000").unwrap(),
            "127.0.0.1:9000".parse().unwrap()
        );
        assert!(parse_http_addr("0.0.0.0:8765").is_err());
        assert!(parse_http_addr("192.168.1.10:8765").is_err());
        assert!(parse_http_addr("[::]:8765").is_err());
        assert!(parse_http_addr("8765").is_err());
        assert!(parse_http_addr("example.com:80").is_err());
    }

    #[test]
    fn client_config_points_at_this_binary() {
        let json = client_config_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let server = &v["mcpServers"]["rustydxf"];
        assert_eq!(server["args"], serde_json::json!(["--mcp"]));
        let command = server["command"].as_str().unwrap();
        assert!(std::path::Path::new(command).is_absolute());
    }
}
