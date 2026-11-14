//! The viewer application: document state, background loading, the camera,
//! input handling and the IPC jobs coming from other RustyDxf processes.

use crate::gpu;
use crate::ipc::{self, Request, Response, ViewerState};
use egui::{Color32, Key, KeyboardShortcut, Modifiers, PointerButton, Sense};
use rustydxf_core::brand;
use rustydxf_core::camera::Camera;
use rustydxf_core::geom::{BBox, Vec2, vec2};
use rustydxf_core::model::{Drawing, HAlign, VAlign, text_corners};
use rustydxf_core::render::{BACKGROUND_DARK, ViewUniform};
use rustydxf_core::tessellate::{Mesh, TessellateOptions, TextItem, tessellate};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const OPEN_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
pub const CLOSE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);

/// Most text labels drawn in one frame.
const MAX_TEXTS_PER_FRAME: usize = 4000;
/// Text smaller than this (in points) is not drawn.
const MIN_TEXT_POINTS: f32 = 2.5;
/// Ratio of font size to cap height for egui's default proportional font.
const FONT_SIZE_PER_CAP_HEIGHT: f32 = 1.45;

pub fn accent() -> Color32 {
    Color32::from_rgb(brand::ACCENT[0], brand::ACCENT[1], brand::ACCENT[2])
}

pub struct Document {
    pub drawing: Arc<Drawing>,
    pub texts: Vec<TextItem>,
    /// Origin of the GPU buffers (see `Mesh::origin`).
    pub origin: Vec2,
    pub visible: Vec<bool>,
    visible_shared: Arc<[bool]>,
}

impl Document {
    pub fn set_visible(&mut self, layer: usize, on: bool) {
        if let Some(v) = self.visible.get_mut(layer) {
            *v = on;
            self.visible_shared = self.visible.clone().into();
        }
    }

    pub fn set_all_visible(&mut self, on: bool) {
        self.visible.iter_mut().for_each(|v| *v = on);
        self.visible_shared = self.visible.clone().into();
    }

    /// Show exactly the named layers (case-insensitive).
    fn show_only(&mut self, names: &[String]) {
        for (i, layer) in self.drawing.layers.iter().enumerate() {
            self.visible[i] = names.iter().any(|n| n.eq_ignore_ascii_case(&layer.name));
        }
        self.visible_shared = self.visible.clone().into();
    }
}

struct Loaded {
    drawing: Drawing,
    mesh: Mesh,
}

/// View settings to apply once a file requested over IPC has loaded.
#[derive(Default)]
struct PendingView {
    layers: Option<Vec<String>>,
    bbox: Option<[f64; 4]>,
}

struct Loading {
    path: PathBuf,
    started: Instant,
    rx: mpsc::Receiver<Result<Loaded, String>>,
    view: PendingView,
    replies: Vec<mpsc::Sender<Response>>,
}

pub struct ViewerApp {
    pub doc: Option<Document>,
    loading: Option<Loading>,
    /// Camera in screen points (not physical pixels).
    camera: Camera,
    /// Size of the drawing area in points, as of the last frame.
    view_size: (f64, f64),
    fit_requested: bool,
    pub cursor_world: Option<Vec2>,
    pub show_layers: bool,
    pub show_about: bool,
    pub error: Option<String>,
    pub toast: Option<(String, Instant)>,
    render_state: Option<egui_wgpu::RenderState>,
    ipc_jobs: mpsc::Receiver<ipc::Job>,
    ipc_server: Option<ipc::Server>,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = accent().gamma_multiply(0.55);
        visuals.hyperlink_color = accent();
        cc.egui_ctx.set_visuals(visuals);

        if let Some(rs) = &cc.wgpu_render_state {
            gpu::install(rs);
        }

        // Other RustyDxf processes (a second launch, the MCP server) reach
        // this window through the IPC endpoint.
        let (tx, ipc_jobs) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let ipc_server = match ipc::Server::start(tx, move || ctx.request_repaint()) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("IPC endpoint unavailable (single-instance and live MCP disabled): {e}");
                None
            }
        };

        crate::platform::register_repaint(cc.egui_ctx.clone());

        let mut app = ViewerApp {
            doc: None,
            loading: None,
            camera: Camera::default(),
            view_size: (0.0, 0.0),
            fit_requested: false,
            cursor_world: None,
            show_layers: true,
            show_about: false,
            error: None,
            toast: None,
            render_state: cc.wgpu_render_state.clone(),
            ipc_jobs,
            ipc_server,
        };
        if let Some(path) = initial.into_iter().next_back() {
            app.open(&cc.egui_ctx, path, PendingView::default(), None);
        }
        app
    }

    pub fn mcp_live(&self) -> bool {
        self.ipc_server
            .as_ref()
            .is_some_and(|s| s.mcp_clients() > 0)
    }

    pub fn loading_name(&self) -> Option<String> {
        self.loading.as_ref().map(|l| file_name(&l.path))
    }

    pub fn zoom(&self) -> f64 {
        self.camera.zoom
    }

    pub fn request_fit(&mut self) {
        self.fit_requested = true;
    }

    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Open DXF")
            .add_filter("DXF drawing", &["dxf", "DXF"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.open(ctx, path, PendingView::default(), None);
        }
    }

    /// Start loading a file on a background thread.
    fn open(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        view: PendingView,
        reply: Option<mpsc::Sender<Response>>,
    ) {
        // A newer request supersedes one still loading.
        if let Some(old) = self.loading.take() {
            for r in old.replies {
                let _ = r.send(Response::error("superseded by a newer open request"));
            }
        }
        let (tx, rx) = mpsc::channel();
        let (thread_path, thread_ctx) = (path.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("rustydxf-load".into())
            .spawn(move || {
                let result = rustydxf_core::load_file(&thread_path)
                    .map(|drawing| {
                        let mesh = tessellate(&drawing, TessellateOptions::default());
                        Loaded { drawing, mesh }
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(result);
                thread_ctx.request_repaint();
            });
        if let Err(e) = spawned {
            let message = format!("cannot start the loader thread: {e}");
            if let Some(r) = reply {
                let _ = r.send(Response::error(message.clone()));
            }
            self.error = Some(message);
            return;
        }
        self.error = None;
        self.loading = Some(Loading {
            path,
            started: Instant::now(),
            rx,
            view,
            replies: reply.into_iter().collect(),
        });
    }

    pub fn close_document(&mut self, ctx: &egui::Context) {
        self.doc = None;
        self.error = None;
        if let Some(rs) = &self.render_state {
            gpu::set_mesh(rs, None);
        }
        set_title(ctx, None);
    }

    fn fit(&mut self) {
        let Some(doc) = &self.doc else { return };
        let bbox =
            rustydxf_core::query::visible_extents(&doc.drawing, &doc.visible).or(doc.drawing.bbox);
        if let Some(b) = bbox {
            self.camera = Camera::fit(&b, self.effective_view_size(), 0.04);
        }
    }

    fn effective_view_size(&self) -> (f64, f64) {
        if self.view_size.0 >= 1.0 && self.view_size.1 >= 1.0 {
            self.view_size
        } else {
            (1000.0, 700.0)
        }
    }

    fn state(&self) -> ViewerState {
        let mut s = ViewerState {
            loading: self.loading.is_some(),
            zoom: self.camera.zoom,
            ..Default::default()
        };
        if let Some(doc) = &self.doc {
            s.file = doc.drawing.path.as_ref().map(|p| p.display().to_string());
            let b = self.camera.visible_bbox(self.effective_view_size());
            s.viewport_bbox = Some([b.min.x, b.min.y, b.max.x, b.max.y]);
            for (layer, visible) in doc.drawing.layers.iter().zip(&doc.visible) {
                if *visible {
                    s.visible_layers.push(layer.name.clone());
                } else {
                    s.hidden_layers.push(layer.name.clone());
                }
            }
            s.entity_count = doc.drawing.entities.len();
            s.units = doc.drawing.header.units.clone();
        }
        s
    }

    fn apply_view(&mut self, view: &PendingView) {
        if let (Some(names), Some(doc)) = (&view.layers, &mut self.doc) {
            doc.show_only(names);
        }
        match view.bbox {
            Some([x0, y0, x1, y1]) if [x0, y0, x1, y1].iter().all(|v| v.is_finite()) => {
                let b = BBox::from_corners(vec2(x0, y0), vec2(x1, y1));
                self.camera = Camera::fit(&b, self.effective_view_size(), 0.0);
                self.fit_requested = false;
            }
            _ if view.layers.is_some() => self.fit_requested = true,
            _ => {}
        }
    }

    fn finish_loading(&mut self, ctx: &egui::Context) {
        let Some(loading) = &self.loading else { return };
        let result = match loading.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the loader stopped unexpectedly".into()),
        };
        let Some(loading) = self.loading.take() else {
            return;
        };
        match result {
            Ok(loaded) => {
                log::info!(
                    "loaded {} ({} entities) in {:.0?}",
                    loading.path.display(),
                    loaded.drawing.entities.len(),
                    loading.started.elapsed()
                );
                if let Some(rs) = &self.render_state {
                    gpu::set_mesh(rs, Some(&loaded.mesh));
                }
                let visible: Vec<bool> = loaded
                    .drawing
                    .layers
                    .iter()
                    .map(|l| l.visible_by_default())
                    .collect();
                set_title(ctx, Some(&loaded.drawing.file_name()));
                self.doc = Some(Document {
                    visible_shared: visible.clone().into(),
                    visible,
                    origin: loaded.mesh.origin,
                    texts: loaded.mesh.texts,
                    drawing: Arc::new(loaded.drawing),
                });
                self.error = None;
                self.fit_requested = true;
                self.apply_view(&loading.view);
                if self.fit_requested && self.view_size.0 >= 1.0 {
                    self.fit();
                    self.fit_requested = false;
                }
                let state = self.state();
                for r in loading.replies {
                    let _ = r.send(Response::ok(Some(state.clone())));
                }
            }
            Err(message) => {
                let text = format!("Cannot open {}: {message}", file_name(&loading.path));
                log::warn!("{text}");
                for r in loading.replies {
                    let _ = r.send(Response::error(text.clone()));
                }
                self.error = Some(text);
            }
        }
    }

    fn handle_ipc(&mut self, ctx: &egui::Context) {
        while let Ok(job) = self.ipc_jobs.try_recv() {
            match job.request {
                Request::Hello { .. } => {
                    let _ = job.reply.send(Response::ok(None));
                }
                Request::State => {
                    let _ = job.reply.send(Response::ok(Some(self.state())));
                }
                Request::Focus => {
                    focus_window(ctx);
                    let _ = job.reply.send(Response::ok(Some(self.state())));
                }
                Request::Open { path, layers, bbox } => {
                    focus_window(ctx);
                    let path = PathBuf::from(path);
                    let view = PendingView { layers, bbox };
                    let already_open = self.loading.is_none()
                        && self
                            .doc
                            .as_ref()
                            .and_then(|d| d.drawing.path.as_deref())
                            .is_some_and(|open| same_file(open, &path));
                    if already_open {
                        // Same file: just adjust the view.
                        self.apply_view(&view);
                        if self.fit_requested {
                            self.fit();
                            self.fit_requested = false;
                        }
                        let _ = job.reply.send(Response::ok(Some(self.state())));
                    } else {
                        self.open(ctx, path, view, Some(job.reply));
                    }
                }
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (open, close, fit) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&OPEN_SHORTCUT),
                i.consume_shortcut(&CLOSE_SHORTCUT),
                i.modifiers.is_none() && i.key_pressed(Key::F),
            )
        });
        if open {
            self.open_dialog(ctx);
        }
        if close {
            if self.doc.is_some() {
                self.close_document(ctx);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if fit && !ctx.egui_wants_keyboard_input() {
            self.fit_requested = true;
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next_back()
            && !path.as_os_str().is_empty()
        {
            self.open(ctx, path, PendingView::default(), None);
        }
    }

    /// The drawing area: input, the GPU paint callback and text labels.
    pub fn viewport(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let size = (rect.width() as f64, rect.height() as f64);
        let resized = self.view_size != size;
        self.view_size = size;
        if size.0 < 1.0 || size.1 < 1.0 {
            return;
        }
        if self.fit_requested || (resized && self.camera == Camera::default()) {
            self.fit();
            self.fit_requested = false;
        }

        self.navigate(ui, &response, rect);

        let painter = ui.painter_at(rect);
        if let Some(doc) = &self.doc {
            // Geometry: one paint callback; only the view uniform changes.
            let ppp = ui.ctx().pixels_per_point() as f64;
            let px_camera = Camera {
                center: self.camera.center,
                zoom: self.camera.zoom * ppp,
            };
            let view = ViewUniform::new(
                &px_camera,
                doc.origin,
                (size.0 * ppp, size.1 * ppp),
                (1.25 * ppp) as f32,
                true,
            );
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                gpu::DrawCallback {
                    view,
                    visible: doc.visible_shared.clone(),
                },
            ));
            self.paint_texts(&painter, rect, doc);
        } else if self.loading.is_none() {
            self.paint_welcome(&painter, rect);
        }

        if let Some(loading) = &self.loading {
            let center = rect.center();
            egui::Spinner::new().size(36.0).color(accent()).paint_at(
                ui,
                egui::Rect::from_center_size(center, egui::vec2(36.0, 36.0)),
            );
            painter.text(
                center + egui::vec2(0.0, 36.0),
                egui::Align2::CENTER_CENTER,
                format!("Loading {}…", file_name(&loading.path)),
                egui::FontId::proportional(14.0),
                Color32::from_gray(200),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }

        // Drag-and-drop hint.
        if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(140));
            painter.rect_stroke(
                rect.shrink(12.0),
                8.0,
                egui::Stroke::new(2.0, accent()),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop a DXF file to open it",
                egui::FontId::proportional(20.0),
                Color32::WHITE,
            );
        }
    }

    fn navigate(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) {
        let size = self.view_size;
        let to_local = |p: egui::Pos2| vec2((p.x - rect.left()) as f64, (p.y - rect.top()) as f64);

        let hover = response.hover_pos();
        self.cursor_world = hover.map(|p| self.camera.screen_to_world(to_local(p), size));
        if self.doc.is_none() {
            return;
        }

        // Drag to pan: middle button, space + drag, or a plain primary drag.
        let space = ui.input(|i| i.key_down(Key::Space));
        if response.dragged_by(PointerButton::Middle)
            || response.dragged_by(PointerButton::Primary)
            || (space && response.dragged())
        {
            let d = response.drag_delta();
            self.camera.pan(vec2(d.x as f64, d.y as f64));
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if space && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        let Some(hover) = hover else { return };
        let anchor = to_local(hover);

        // Pinch gestures and ctrl/cmd + scroll arrive as a zoom factor.
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            self.camera.zoom_about(anchor, zoom as f64, size);
        }

        // A mouse wheel (line/page units) zooms around the cursor; a
        // trackpad or Magic Mouse (point units) pans, as in other macOS apps.
        let (wheel, pan) = ui.input(|i| {
            let mut wheel = 0.0f32;
            let mut pan = egui::Vec2::ZERO;
            for event in &i.raw.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } = event
                {
                    if modifiers.ctrl || modifiers.command {
                        continue; // Already reported through zoom_delta().
                    }
                    match unit {
                        egui::MouseWheelUnit::Point => pan += *delta,
                        egui::MouseWheelUnit::Line => wheel += delta.y,
                        egui::MouseWheelUnit::Page => wheel += delta.y * 10.0,
                    }
                }
            }
            (wheel, pan)
        });
        if wheel != 0.0 {
            let factor = (wheel as f64 * 0.18).clamp(-2.0, 2.0).exp();
            self.camera.zoom_about(anchor, factor, size);
        }
        if pan != egui::Vec2::ZERO {
            self.camera.pan(vec2(pan.x as f64, pan.y as f64));
        }
    }

    /// TEXT / MTEXT are drawn with egui, positioned, sized and rotated to
    /// match the drawing. Fonts are approximate.
    fn paint_texts(&self, painter: &egui::Painter, rect: egui::Rect, doc: &Document) {
        let size = self.view_size;
        let view = self.camera.visible_bbox(size);
        let mut drawn = 0;
        for item in &doc.texts {
            if !doc
                .visible
                .get(item.layer as usize)
                .copied()
                .unwrap_or(true)
            {
                continue;
            }
            let t = &item.text;
            let cap_points = (t.height * self.camera.zoom) as f32;
            if cap_points < MIN_TEXT_POINTS {
                continue;
            }
            let corners = text_corners(t);
            let mut bounds = BBox::from_point(corners[0]);
            corners.iter().for_each(|c| bounds.expand(*c));
            if !bounds.intersects(&view) {
                continue;
            }
            drawn += 1;
            if drawn > MAX_TEXTS_PER_FRAME {
                break;
            }

            let rgb = item.color.display_rgb(true);
            let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let font_size = (cap_points * FONT_SIZE_PER_CAP_HEIGHT).min(2000.0);
            let galley = painter.layout_no_wrap(
                t.content.clone(),
                egui::FontId::proportional(font_size),
                color,
            );
            let gs = galley.size();
            // Anchor inside the unrotated galley (y down).
            let ax = match t.halign {
                HAlign::Left => 0.0,
                HAlign::Center => gs.x / 2.0,
                HAlign::Right => gs.x,
            };
            let ay = match t.valign {
                // The baseline sits a little above the bottom of the last row.
                VAlign::Baseline => gs.y - font_size * 0.22,
                VAlign::Bottom => gs.y,
                VAlign::Middle => gs.y / 2.0,
                VAlign::Top => font_size * 0.12,
            };
            // egui angles are clockwise on screen; drawing angles are CCW.
            let angle = -(t.rotation as f32);
            let (s, c) = angle.sin_cos();
            let offset = egui::vec2(ax * c - ay * s, ax * s + ay * c);
            let p = self.camera.world_to_screen(t.pos, size);
            let anchor = egui::pos2(rect.left() + p.x as f32, rect.top() + p.y as f32);
            painter.add(egui::Shape::Text(
                egui::epaint::TextShape::new(anchor - offset, galley, color).with_angle(angle),
            ));
        }
    }

    fn paint_welcome(&self, painter: &egui::Painter, rect: egui::Rect) {
        let center = rect.center();
        painter.text(
            center - egui::vec2(0.0, 18.0),
            egui::Align2::CENTER_CENTER,
            brand::PRODUCT_NAME,
            egui::FontId::proportional(30.0),
            accent(),
        );
        let hint = format!(
            "Drop a .dxf file here, or press {} to open one",
            painter.ctx().format_shortcut(&OPEN_SHORTCUT)
        );
        painter.text(
            center + egui::vec2(0.0, 16.0),
            egui::Align2::CENTER_CENTER,
            hint,
            egui::FontId::proportional(14.0),
            Color32::from_gray(150),
        );
    }
}

impl eframe::App for ViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Files opened from Finder arrive as Apple Events (macOS).
        for path in crate::platform::take_opened_files() {
            focus_window(ctx);
            self.open(ctx, path, PendingView::default(), None);
        }
        self.handle_ipc(ctx);
        self.finish_loading(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        crate::ui::menu_bar(self, ui);
        crate::ui::status_bar(self, ui);
        crate::ui::layers_panel(self, ui);

        let bg = Color32::from_rgb(BACKGROUND_DARK[0], BACKGROUND_DARK[1], BACKGROUND_DARK[2]);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg))
            .show(ui, |ui| self.viewport(ui));

        crate::ui::about_window(self, &ctx);
        crate::ui::toast(self, &ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = BACKGROUND_DARK;
        [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
            1.0,
        ]
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn set_title(ctx: &egui::Context, file: Option<&str>) {
    let title = match file {
        Some(name) if !name.is_empty() => format!("{} — {name}", brand::PRODUCT_NAME),
        _ => brand::PRODUCT_NAME.to_string(),
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
}

fn focus_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
