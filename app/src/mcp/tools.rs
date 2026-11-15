//! The MCP tools.
//!
//! Headless tools work on files directly through `rustydxf-core`. The two
//! live tools (`show_in_viewer`, `get_viewer_state`) talk to a running
//! RustyDxf window through the IPC bridge.

use super::bridge::Bridge;
use super::cache::{Cache, Entry, resolve_path};
use crate::ipc;
use crate::settings::{self, Settings};
use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use rustydxf_core::brand;
use rustydxf_core::geom::{BBox, vec2};
use rustydxf_core::query::{self, EntityFilter};
use rustydxf_core::render::{MAX_RENDER_SIZE, RenderOptions, render_png};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const VIEWER_CONTROL_OFF: &str = "MCP control of the viewer is turned off in the RustyDxf \
     settings (Settings… → MCP server). The headless tools still work.";
const DEFAULT_ENTITY_LIMIT: usize = 500;
const MAX_ENTITY_LIMIT: usize = 2000;
const MAX_LAYERS_LISTED: usize = 2000;
const DEFAULT_TEXT_LIMIT: usize = 200;
const MAX_TEXT_LIMIT: usize = 1000;
const MAX_WARNINGS: usize = 20;
/// Largest PNG returned inline (before base64). Bigger renders must be
/// written to `output_path`.
const MAX_INLINE_PNG_BYTES: usize = 6 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Parameters

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
}

fn default_entity_limit() -> usize {
    DEFAULT_ENTITY_LIMIT
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListEntitiesParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Only entities on this layer (case-insensitive).
    #[serde(default)]
    pub layer: Option<String>,
    /// Only entities of this DXF type, e.g. "LINE", "LWPOLYLINE", "CIRCLE",
    /// "ARC", "TEXT", "HATCH" (case-insensitive).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Maximum entities to return (default 500, at most 2000).
    #[serde(default = "default_entity_limit")]
    pub limit: usize,
    /// Number of matching entities to skip (for pagination).
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExtentsParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Restrict the bounding box to one layer.
    #[serde(default)]
    pub layer: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MeasureParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Sum the lengths of entities on this layer.
    #[serde(default)]
    pub layer: Option<String>,
    /// Sum the lengths of entities of this DXF type (e.g. "LINE").
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// First point `[x, y]` for a point-to-point distance.
    #[serde(default)]
    pub from: Option<[f64; 2]>,
    /// Second point `[x, y]` for a point-to-point distance.
    #[serde(default)]
    pub to: Option<[f64; 2]>,
}

fn default_text_limit() -> usize {
    DEFAULT_TEXT_LIMIT
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindTextParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Text to search for (case-insensitive substring). An empty string
    /// lists every text entity.
    pub query: String,
    /// Maximum matches to return (default 200, at most 1000).
    #[serde(default = "default_text_limit")]
    pub limit: usize,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    #[default]
    Dark,
    Light,
}

fn default_width() -> u32 {
    1600
}
fn default_height() -> u32 {
    1200
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenderParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Image width in pixels (default 1600, 16 to 8192).
    #[serde(default = "default_width")]
    pub width: u32,
    /// Image height in pixels (default 1200, 16 to 8192).
    #[serde(default = "default_height")]
    pub height: u32,
    /// Names of the layers to draw. Omit to use the visibility stored in
    /// the file (layers that are on and not frozen).
    #[serde(default)]
    pub layers: Option<Vec<String>>,
    /// Region to render, `[min_x, min_y, max_x, max_y]` in drawing units.
    /// Omit to frame the whole drawing.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// Background color: "dark" or "light". ACI 7 is drawn white on dark and
    /// black on light. Omit to use the default from the RustyDxf settings
    /// (dark unless the user changed it).
    #[serde(default)]
    pub background: Option<Background>,
    /// Optional absolute path to also write the PNG to (`~` is expanded).
    /// An existing file at that path is overwritten.
    #[serde(default)]
    pub output_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShowParams {
    /// Absolute path to the .dxf file. A leading `~` is expanded.
    pub path: String,
    /// Names of the layers to show; every other layer is hidden. Omit to
    /// leave layer visibility alone.
    #[serde(default)]
    pub layers: Option<Vec<String>>,
    /// Region to zoom to, `[min_x, min_y, max_x, max_y]` in drawing units.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
}

// ---------------------------------------------------------------------------
// Helpers

/// A tool-level failure: the tool ran and is reporting a problem the caller
/// can act on (as opposed to a protocol error).
fn failure(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

fn bbox_json(b: Option<BBox>) -> Value {
    match b {
        Some(b) => json!({
            "min": [b.min.x, b.min.y],
            "max": [b.max.x, b.max.y],
            "width": b.width(),
            "height": b.height(),
        }),
        None => Value::Null,
    }
}

fn parse_bbox(b: [f64; 4]) -> Result<BBox, String> {
    if b.iter().any(|v| !v.is_finite()) {
        return Err("bbox values must be finite numbers".into());
    }
    let bbox = BBox::from_corners(vec2(b[0], b[1]), vec2(b[2], b[3]));
    if bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return Err(
            "bbox must have a positive width and height: [min_x, min_y, max_x, max_y]".into(),
        );
    }
    Ok(bbox)
}

/// Check that every requested layer exists; returns per-layer visibility.
fn layer_selection(entry: &Entry, names: &[String]) -> Result<Vec<bool>, String> {
    let d = &entry.drawing;
    let unknown: Vec<&str> = names
        .iter()
        .filter(|n| d.layer_index(n).is_none())
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown layer(s): {}. Use list_layers to see the layer names.",
            unknown.join(", ")
        ));
    }
    Ok(d.layers
        .iter()
        .map(|l| names.iter().any(|n| n.eq_ignore_ascii_case(&l.name)))
        .collect())
}

fn viewer_state_json(state: Option<ipc::ViewerState>) -> Value {
    match state {
        Some(s) => {
            let bbox = s.viewport_bbox.map(|b| {
                json!({ "min": [b[0], b[1]], "max": [b[2], b[3]],
                        "width": b[2] - b[0], "height": b[3] - b[1] })
            });
            json!({
                "file": s.file,
                "loading": s.loading,
                "viewport_bbox": bbox,
                "zoom_points_per_unit": s.zoom,
                "visible_layers": s.visible_layers,
                "hidden_layers": s.hidden_layers,
                "entity_count": s.entity_count,
                "units": s.units,
            })
        }
        None => json!({}),
    }
}

// ---------------------------------------------------------------------------
// Server

#[derive(Clone)]
pub struct RustyDxfServer {
    cache: Arc<Cache>,
    bridge: Arc<Bridge>,
    tool_router: ToolRouter<Self>,
}

impl RustyDxfServer {
    pub fn new(cache: Arc<Cache>, bridge: Arc<Bridge>) -> Self {
        RustyDxfServer {
            cache,
            bridge,
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve the path, load (or reuse) the drawing and run `f` on a
    /// blocking thread. Any failure becomes a tool error, never a crash.
    async fn with_drawing<F>(&self, path: String, f: F) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(&Entry, &std::path::Path) -> Result<CallToolResult, String> + Send + 'static,
    {
        let cache = self.cache.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let path = resolve_path(&path)?;
            let entry = cache.get(&path)?;
            f(&entry, &path)
        })
        .await;
        Ok(match joined {
            Ok(Ok(result)) => result,
            Ok(Err(message)) => failure(message),
            Err(e) => failure(format!("internal error while processing the file: {e}")),
        })
    }

    async fn live<F>(&self, f: F) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(&Cache, &Bridge) -> Result<CallToolResult, String> + Send + 'static,
    {
        let (cache, bridge) = (self.cache.clone(), self.bridge.clone());
        let joined = tokio::task::spawn_blocking(move || {
            if !Settings::load().mcp.allow_viewer_control {
                return Err(VIEWER_CONTROL_OFF.to_string());
            }
            f(&cache, &bridge)
        })
        .await;
        Ok(match joined {
            Ok(Ok(result)) => result,
            Ok(Err(message)) => failure(message),
            Err(e) => failure(format!("internal error while talking to the viewer: {e}")),
        })
    }
}

#[tool_router]
impl RustyDxfServer {
    #[tool(
        name = "open_dxf",
        description = "Parse a DXF file (ASCII or binary) and return a summary: DXF version, units, \
                       extents, layer count, entity counts by type and counts of unsupported entities. \
                       Start here; the parsed drawing is cached for the other tools.",
        annotations(
            title = "Open DXF",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_dxf(
        &self,
        Parameters(p): Parameters<PathParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, |entry, path| {
            let d = &entry.drawing;
            let mut warnings: Vec<&str> = d.warnings.iter().map(String::as_str).collect();
            let warnings_truncated = warnings.len() > MAX_WARNINGS;
            warnings.truncate(MAX_WARNINGS);
            Ok(CallToolResult::structured(json!({
                "path": path.display().to_string(),
                "file_name": d.file_name(),
                "dxf_version": d.header.version,
                "units": d.header.units,
                "insunits": d.header.units_code,
                "extents": bbox_json(d.bbox),
                "header_extents": bbox_json(d.header.header_extents),
                "layer_count": d.layers.len(),
                "entity_count": d.entities.len(),
                "entity_counts_by_type": d.source_counts,
                "flattened_counts_by_type": d.flattened_counts(),
                "unsupported_counts": d.unsupported,
                "unsupported_total": d.unsupported_total(),
                "warnings": warnings,
                "warnings_truncated": warnings_truncated,
                "notes": "entity_counts_by_type counts model-space entities as stored in the file \
                          (an INSERT counts once). entity_count and flattened_counts_by_type count \
                          drawable entities after blocks are expanded.",
            })))
        })
        .await
    }

    #[tool(
        name = "list_layers",
        description = "List the layers of a DXF file: name, color (hex and ACI index), on/off, frozen, \
                       and the number of entities on each.",
        annotations(
            title = "List layers",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_layers(
        &self,
        Parameters(p): Parameters<PathParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, |entry, _| {
            let d = &entry.drawing;
            let layers: Vec<Value> = d
                .layers
                .iter()
                .take(MAX_LAYERS_LISTED)
                .map(|l| {
                    json!({
                        "name": l.name,
                        "color": l.color.hex(),
                        "aci": l.aci,
                        "on": l.on,
                        "frozen": l.frozen,
                        "visible": l.visible_by_default(),
                        "entity_count": l.entity_count,
                    })
                })
                .collect();
            Ok(CallToolResult::structured(json!({
                "total": d.layers.len(),
                "returned": layers.len(),
                "truncated": d.layers.len() > layers.len(),
                "layers": layers,
            })))
        })
        .await
    }

    #[tool(
        name = "list_entities",
        description = "List entities with a geometry summary (type, layer, color, key coordinates, \
                       length, radius). Blocks are expanded, so coordinates are in world space. \
                       Filter by layer and/or type; results are paginated with limit and offset, and \
                       'truncated' / 'next_offset' say whether more follow.",
        annotations(
            title = "List entities",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_entities(
        &self,
        Parameters(p): Parameters<ListEntitiesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, move |entry, _| {
            let filter = EntityFilter {
                layer: p.layer,
                kind: p.kind,
            };
            let limit = p.limit.clamp(1, MAX_ENTITY_LIMIT);
            let page = query::list_entities(&entry.drawing, &filter, p.offset, limit)
                .map_err(|e| e.to_string())?;
            let value = serde_json::to_value(&page).map_err(|e| e.to_string())?;
            Ok(CallToolResult::structured(value))
        })
        .await
    }

    #[tool(
        name = "get_extents",
        description = "Bounding box of the drawing, or of one layer, in drawing units.",
        annotations(
            title = "Get extents",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_extents(
        &self,
        Parameters(p): Parameters<ExtentsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, move |entry, _| {
            let d = &entry.drawing;
            let extents = query::extents(d, p.layer.as_deref()).map_err(|e| e.to_string())?;
            Ok(CallToolResult::structured(json!({
                "layer": p.layer,
                "extents": bbox_json(extents),
                "empty": extents.is_none(),
                "units": d.header.units,
            })))
        })
        .await
    }

    #[tool(
        name = "measure",
        description = "Measure a drawing. With 'layer' and/or 'type': the total length of the matching \
                       entities (lines, polylines including bulges, arcs, circles, ellipses, splines, \
                       hatch boundaries). With 'from' and 'to': the distance between two points. Both \
                       can be combined. With neither, the total length of everything.",
        annotations(
            title = "Measure",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn measure(
        &self,
        Parameters(p): Parameters<MeasureParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, move |entry, _| {
            let d = &entry.drawing;
            let mut out = json!({ "units": d.header.units });

            let distance = match (p.from, p.to) {
                (Some(a), Some(b)) => {
                    if a.iter().chain(b.iter()).any(|v| !v.is_finite()) {
                        return Err("'from' and 'to' must be finite numbers".into());
                    }
                    Some(query::distance(vec2(a[0], a[1]), vec2(b[0], b[1])))
                }
                (None, None) => None,
                _ => return Err("give both 'from' and 'to' to measure a distance".into()),
            };
            if let Some(dist) = &distance {
                out["distance"] = serde_json::to_value(dist).map_err(|e| e.to_string())?;
            }

            let has_filter = p.layer.is_some() || p.kind.is_some();
            if has_filter || distance.is_none() {
                let filter = EntityFilter {
                    layer: p.layer.clone(),
                    kind: p.kind.clone(),
                };
                let report = query::total_length(d, &filter).map_err(|e| e.to_string())?;
                out["length"] = serde_json::to_value(&report).map_err(|e| e.to_string())?;
                out["filter"] = json!({ "layer": p.layer, "type": p.kind });
            }
            Ok(CallToolResult::structured(out))
        })
        .await
    }

    #[tool(
        name = "find_text",
        description = "Search TEXT, MTEXT and block-attribute content (case-insensitive substring) and \
                       return each match with its position, height, rotation and layer.",
        annotations(
            title = "Find text",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_text(
        &self,
        Parameters(p): Parameters<FindTextParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, move |entry, _| {
            let mut hits = query::find_text(&entry.drawing, &p.query);
            let total = hits.len();
            hits.truncate(p.limit.clamp(1, MAX_TEXT_LIMIT));
            Ok(CallToolResult::structured(json!({
                "query": p.query,
                "total": total,
                "returned": hits.len(),
                "truncated": hits.len() < total,
                "matches": serde_json::to_value(&hits).map_err(|e| e.to_string())?,
            })))
        })
        .await
    }

    #[tool(
        name = "render_png",
        description = "Render the drawing offscreen to a PNG and return it as an image. Optionally \
                       restrict to some layers, zoom to a bbox, choose a dark or light background, and \
                       also write the PNG to output_path. Text is drawn as outlines in an approximate \
                       font.",
        annotations(
            title = "Render PNG",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn render_png(
        &self,
        Parameters(p): Parameters<RenderParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_drawing(p.path, move |entry, _| {
            let d = &entry.drawing;
            let visible = match &p.layers {
                Some(names) => layer_selection(entry, names)?,
                None => d.layers.iter().map(|l| l.visible_by_default()).collect(),
            };
            let view = p.bbox.map(parse_bbox).transpose()?;
            if !(16..=MAX_RENDER_SIZE).contains(&p.width)
                || !(16..=MAX_RENDER_SIZE).contains(&p.height)
            {
                return Err(format!(
                    "width and height must be between 16 and {MAX_RENDER_SIZE} pixels"
                ));
            }
            let output = p.output_path.as_deref().map(resolve_path).transpose()?;
            let dark = match p.background {
                Some(Background::Dark) => true,
                Some(Background::Light) => false,
                None => Settings::load().mcp.render_background == settings::RenderBackground::Dark,
            };

            let png = render_png(
                entry.mesh(),
                &RenderOptions {
                    width: p.width,
                    height: p.height,
                    view,
                    visible: visible.clone(),
                    dark_background: dark,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;

            if let Some(path) = &output {
                std::fs::write(path, &png)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            }

            let shown = query::visible_extents(d, &visible);
            let inline = png.len() <= MAX_INLINE_PNG_BYTES;
            let summary = json!({
                "width": p.width,
                "height": p.height,
                "background": if dark { "dark" } else { "light" },
                "rendered_bbox": bbox_json(view.or(shown).or(d.bbox)),
                "layers_drawn": visible.iter().filter(|v| **v).count(),
                "png_bytes": png.len(),
                "output_path": output.as_ref().map(|p| p.display().to_string()),
                "image_inline": inline,
                "truncated": !inline,
            });
            let mut content = vec![ContentBlock::text(summary.to_string())];
            if inline {
                let data = base64::engine::general_purpose::STANDARD.encode(&png);
                content.push(ContentBlock::image(data, "image/png"));
            } else if output.is_none() {
                return Err(format!(
                    "the rendered PNG is {} bytes, too large to return inline. Use a smaller \
                     width/height or pass output_path to write it to disk.",
                    png.len()
                ));
            }
            Ok(CallToolResult::success(content))
        })
        .await
    }

    #[tool(
        name = "show_in_viewer",
        description = "Live mode: open (or focus) a DXF file in the RustyDxf window, optionally showing \
                       only some layers and zooming to a bbox. Starts the viewer if it is not running. \
                       Returns the resulting viewer state.",
        annotations(
            title = "Show in viewer",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn show_in_viewer(
        &self,
        Parameters(p): Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        self.live(move |cache, bridge| {
            // Validate everything headlessly first, for clear errors.
            let path = resolve_path(&p.path)?;
            let entry = cache.get(&path)?;
            if let Some(names) = &p.layers {
                layer_selection(&entry, names)?;
            }
            let bbox = p
                .bbox
                .map(|b| parse_bbox(b).map(|bb| [bb.min.x, bb.min.y, bb.max.x, bb.max.y]))
                .transpose()?;

            let response = bridge
                .request_launching(&ipc::Request::Open {
                    path: path.display().to_string(),
                    layers: p.layers,
                    bbox,
                })
                .map_err(|e| format!("cannot reach the RustyDxf viewer: {e}"))?;
            if !response.ok {
                return Err(response
                    .error
                    .unwrap_or_else(|| "the viewer reported an error".into()));
            }
            Ok(CallToolResult::structured(viewer_state_json(
                response.state,
            )))
        })
        .await
    }

    #[tool(
        name = "get_viewer_state",
        description = "Live mode: report what the RustyDxf window is showing: the open file, the visible \
                       world rectangle (viewport_bbox) and which layers are visible or hidden. Fails if \
                       the viewer is not running; use show_in_viewer to start it.",
        annotations(
            title = "Get viewer state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_viewer_state(&self) -> Result<CallToolResult, McpError> {
        self.live(|_, bridge| {
            let response = bridge.request(&ipc::Request::State).map_err(|_| {
                "the RustyDxf viewer is not running. Call show_in_viewer with a file path to start it."
                    .to_string()
            })?;
            if !response.ok {
                return Err(response
                    .error
                    .unwrap_or_else(|| "the viewer reported an error".into()));
            }
            Ok(CallToolResult::structured(viewer_state_json(response.state)))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RustyDxfServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("rustydxf", env!("CARGO_PKG_VERSION")).with_title(format!(
                    "{} by {}",
                    brand::PRODUCT_NAME,
                    brand::VENDOR
                )),
            )
            .with_instructions(
                "RustyDxf reads DXF drawings (view only). Paths must be absolute; '~' is expanded. \
                 Call open_dxf first for a summary, then list_layers, list_entities, get_extents, \
                 measure or find_text to query, and render_png to see the drawing. show_in_viewer \
                 and get_viewer_state drive the RustyDxf desktop window.",
            )
    }
}
