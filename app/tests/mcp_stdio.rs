//! End-to-end test of `rustydxf --mcp` over stdio: initialize, tools/list,
//! then tools/call for every tool against the generated fixtures.
//!
//! The live tools are exercised without a viewer: `RUSTYDXF_INSTANCE` points
//! the server at a private IPC endpoint and `RUSTYDXF_NO_LAUNCH` keeps it from
//! opening a window, so they must answer with tool errors.

use base64::Engine as _;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const TOOLS: [&str; 9] = [
    "open_dxf",
    "list_layers",
    "list_entities",
    "get_extents",
    "measure",
    "find_text",
    "render_png",
    "show_in_viewer",
    "get_viewer_state",
];

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rustydxf"))
            .arg("--mcp")
            .env("RUSTYDXF_INSTANCE", format!("test-{}", std::process::id()))
            .env("RUSTYDXF_NO_LAUNCH", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn rustydxf --mcp");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Server {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let init = server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "rustydxf-test", "version": "0" }
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "rustydxf", "{init}");
        assert!(init["capabilities"]["tools"].is_object(), "{init}");
        server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send a request and return its `result`. Every stdout line must be a
    /// JSON-RPC message: anything else means the protocol stream is polluted.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "server closed stdout while waiting for {method}");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("non-JSON line on stdout ({e}): {line:?}"));
            assert_eq!(message["jsonrpc"], "2.0", "{message}");
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// Call a tool that must succeed and return its structured result.
    fn call_ok(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.call(tool, arguments);
        assert_ne!(result["isError"], json!(true), "{tool}: {result}");
        structured(&result)
    }

    /// Call a tool that must fail with a tool error (not a protocol error)
    /// and return the error text.
    fn call_err(&mut self, tool: &str, arguments: Value) -> String {
        let result = self.call(tool, arguments);
        assert_eq!(result["isError"], json!(true), "{tool}: {result}");
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool}: error without text: {result}"))
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn structured(result: &Value) -> Value {
    if let Some(v) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return v.clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {result}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("content is not JSON ({e}): {text}"))
}

fn fixtures() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-fixtures");
    rustydxf_core::fixtures::write_all(&dir).expect("write fixtures");
    dir
}

fn path(dir: &Path, name: &str) -> String {
    dir.join(name).display().to_string()
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("valid PNG header");
    let mut buffer = vec![0; reader.output_buffer_size().expect("PNG size")];
    let info = reader.next_frame(&mut buffer).expect("valid PNG data");
    (info.width, info.height)
}

#[test]
fn every_tool_over_stdio() {
    let dir = fixtures();
    let mut server = Server::start();

    // tools/list: all nine tools, with schemas and read-only annotations.
    let listed = server.request("tools/list", json!({}));
    let tools = listed["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut expected = TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{name}"
        );
        // show_in_viewer changes the window and render_png can write
        // output_path; everything else only reads.
        let read_only = tool["annotations"]["readOnlyHint"].as_bool();
        let writes = matches!(name, "show_in_viewer" | "render_png");
        assert_eq!(read_only, Some(!writes), "{name}: {tool}");
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            json!(false),
            "{name}"
        );
    }

    // open_dxf
    let lines = path(&dir, "lines.dxf");
    let opened = server.call_ok("open_dxf", json!({ "path": lines }));
    assert_eq!(opened["file_name"], "lines.dxf");
    assert!(opened["entity_count"].as_u64().unwrap() > 0, "{opened}");
    assert!(opened["layer_count"].as_u64().unwrap() > 0, "{opened}");
    assert!(
        opened["extents"].is_object() || opened["extents"].is_array(),
        "{opened}"
    );
    // Binary DXF loads through the same tool.
    let binary = server.call_ok("open_dxf", json!({ "path": path(&dir, "binary.dxf") }));
    assert!(binary["entity_count"].as_u64().unwrap() > 0, "{binary}");

    // list_layers
    let layers = server.call_ok("list_layers", json!({ "path": path(&dir, "hatch.dxf") }));
    let layer_names: Vec<&str> = layers["layers"]
        .as_array()
        .expect("layers array")
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert!(layer_names.contains(&"FILL"), "{layers}");
    assert!(layer_names.contains(&"FROZEN"), "{layers}");

    // list_entities: paging and the truncation marker.
    let page = server.call_ok("list_entities", json!({ "path": lines, "limit": 1 }));
    assert_eq!(page["entities"].as_array().unwrap().len(), 1, "{page}");
    assert_eq!(page["truncated"], json!(true), "{page}");
    let total = page["total"].as_u64().expect("total");
    assert!(total > 1, "{page}");
    let rest = server.call_ok("list_entities", json!({ "path": lines, "offset": 1 }));
    assert_eq!(rest["entities"].as_array().unwrap().len() as u64, total - 1);
    assert_eq!(rest["truncated"], json!(false), "{rest}");
    let only_lines = server.call_ok("list_entities", json!({ "path": lines, "type": "LINE" }));
    for entity in only_lines["entities"].as_array().unwrap() {
        assert_eq!(entity["type"], "LINE", "{entity}");
    }

    // get_extents
    let extents = server.call_ok("get_extents", json!({ "path": lines }));
    assert!(extents.to_string().contains("min"), "{extents}");

    // measure: point to point, and total length.
    let distance = server.call_ok(
        "measure",
        json!({ "path": lines, "from": [0.0, 0.0], "to": [3.0, 4.0] }),
    );
    assert_eq!(
        distance["distance"]["distance"].as_f64(),
        Some(5.0),
        "{distance}"
    );
    let length = server.call_ok("measure", json!({ "path": lines, "type": "LINE" }));
    assert!(
        length["length"]["total_length"].as_f64().unwrap() > 0.0,
        "{length}"
    );

    // find_text
    let text = path(&dir, "text.dxf");
    let all_text = server.call_ok("find_text", json!({ "path": text, "query": "" }));
    let matches = all_text["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty(), "{all_text}");
    let none = server.call_ok(
        "find_text",
        json!({ "path": text, "query": "no such string in the fixture" }),
    );
    assert!(none["matches"].as_array().unwrap().is_empty(), "{none}");

    // render_png: a valid PNG of exactly the requested size.
    let rendered = server.call(
        "render_png",
        json!({ "path": path(&dir, "hatch.dxf"), "width": 320, "height": 200, "background": "light" }),
    );
    if rendered["isError"] == json!(true) {
        // Machines without any GPU adapter cannot render; everything else must.
        let message = rendered["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            std::env::var_os("RUSTYDXF_REQUIRE_GPU").is_none(),
            "render_png failed although a GPU is required: {message}"
        );
        eprintln!("render_png skipped (no GPU adapter?): {message}");
    } else {
        let content = rendered["content"].as_array().expect("content array");
        let image = content
            .iter()
            .find(|c| c["type"] == "image")
            .unwrap_or_else(|| panic!("no image block: {rendered}"));
        assert_eq!(image["mimeType"], "image/png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().unwrap())
            .expect("valid base64");
        assert_eq!(png_size(&bytes), (320, 200));

        // Defaults: 1600 x 1200.
        let default = server.call("render_png", json!({ "path": lines }));
        let image = default["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "image")
            .unwrap_or_else(|| panic!("no image block: {default}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(png_size(&bytes), (1600, 1200));
    }

    // Live tools without a viewer: tool errors that say what to do.
    let state = server.call_err("get_viewer_state", json!({}));
    assert!(state.contains("show_in_viewer"), "{state}");
    let show = server.call_err("show_in_viewer", json!({ "path": lines }));
    assert!(show.contains("viewer"), "{show}");

    // Bad input is a tool error, never a crash or a protocol error.
    let relative = server.call_err("open_dxf", json!({ "path": "relative/file.dxf" }));
    assert!(relative.contains("absolute"), "{relative}");
    server.call_err("open_dxf", json!({ "path": path(&dir, "missing.dxf") }));
    server.call_err("open_dxf", json!({ "path": path(&dir, "corrupt.dxf") }));
    server.call_err(
        "render_png",
        json!({ "path": lines, "width": 0, "height": 10 }),
    );
    server.call_err(
        "list_entities",
        json!({ "path": lines, "layer": "NO_SUCH_LAYER" }),
    );

    // The server is still healthy after all of that.
    let again = server.call_ok("open_dxf", json!({ "path": lines }));
    assert_eq!(again["file_name"], "lines.dxf");
}
