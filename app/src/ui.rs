//! Menu bar, layers panel, status bar and the About window.

use crate::app::{CLOSE_SHORTCUT, OPEN_SHORTCUT, PREFS_SHORTCUT, ViewerApp, accent};
use crate::settings::{RenderBackground, Theme};
use egui::{Color32, RichText};
use rustydxf_core::brand;
use std::time::{Duration, Instant};

pub fn menu_bar(app: &mut ViewerApp, ui: &mut egui::Ui) {
    egui::Panel::top("menu_bar").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            let ctx = ui.ctx().clone();
            ui.menu_button("File", |ui| {
                let open =
                    egui::Button::new("Open…").shortcut_text(ctx.format_shortcut(&OPEN_SHORTCUT));
                if ui.add(open).clicked() {
                    ui.close();
                    app.open_dialog(&ctx);
                }
                let close =
                    egui::Button::new("Close").shortcut_text(ctx.format_shortcut(&CLOSE_SHORTCUT));
                if ui.add_enabled(app.doc.is_some(), close).clicked() {
                    ui.close();
                    app.close_document(&ctx);
                }
                ui.separator();
                let prefs = egui::Button::new("Settings…")
                    .shortcut_text(ctx.format_shortcut(&PREFS_SHORTCUT));
                if ui.add(prefs).clicked() {
                    ui.close();
                    app.show_prefs = true;
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                let fit = egui::Button::new("Zoom extents").shortcut_text("F");
                if ui.add_enabled(app.doc.is_some(), fit).clicked() {
                    ui.close();
                    app.request_fit();
                }
                ui.checkbox(&mut app.show_layers, "Layers panel");
            });
            ui.menu_button("Help", |ui| {
                if ui
                    .button("Copy MCP config")
                    .on_hover_text("Copy the JSON that registers this install as an MCP server")
                    .clicked()
                {
                    ui.close();
                    app.copy_mcp_config(&ctx);
                }
                ui.separator();
                if ui
                    .button(format!("About {}", brand::PRODUCT_NAME))
                    .clicked()
                {
                    ui.close();
                    app.show_about = true;
                }
            });
        });
    });
}

pub fn layers_panel(app: &mut ViewerApp, ui: &mut egui::Ui) {
    let mut expanded = app.show_layers;
    egui::Panel::left("layers")
        .resizable(true)
        .default_size(220.0)
        .min_size(140.0)
        .show_collapsible(ui, &mut expanded, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("Layers");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("«")
                        .on_hover_text("Collapse the layers panel")
                        .clicked()
                    {
                        app.show_layers = false;
                    }
                });
            });
            let Some(doc) = &mut app.doc else {
                ui.weak("No drawing open");
                return;
            };
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    doc.set_all_visible(true);
                }
                if ui.small_button("None").clicked() {
                    doc.set_all_visible(false);
                }
                ui.weak(format!("{} layers", doc.drawing.layers.len()));
            });
            ui.separator();

            let drawing = doc.drawing.clone();
            let row_height = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                row_height,
                drawing.layers.len(),
                |ui, rows| {
                    for i in rows {
                        let layer = &drawing.layers[i];
                        ui.horizontal(|ui| {
                            let mut on = doc.visible[i];
                            if ui.checkbox(&mut on, "").changed() {
                                doc.set_visible(i, on);
                            }
                            let rgb = layer.color.display_rgb(ui.visuals().dark_mode);
                            let (swatch, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                swatch,
                                2.0,
                                Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                            );
                            ui.painter().rect_stroke(
                                swatch,
                                2.0,
                                egui::Stroke::new(1.0, Color32::from_gray(90)),
                                egui::StrokeKind::Inside,
                            );
                            let mut name = RichText::new(&layer.name);
                            if !on {
                                name = name.weak();
                            }
                            let mut hover = format!("{} entities", layer.entity_count);
                            if layer.frozen {
                                hover.push_str(" · frozen in the file");
                            }
                            if !layer.on {
                                hover.push_str(" · off in the file");
                            }
                            ui.add(egui::Label::new(name).truncate())
                                .on_hover_text(hover);
                        });
                    }
                },
            );
        });
}

pub fn status_bar(app: &mut ViewerApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("status_bar").show(ui, |ui| {
        ui.horizontal(|ui| {
            if !app.show_layers
                && ui
                    .small_button("» Layers")
                    .on_hover_text("Show the layers panel")
                    .clicked()
            {
                app.show_layers = true;
            }

            if let Some(error) = &app.error {
                ui.colored_label(Color32::from_rgb(255, 120, 110), error);
            } else if let Some(name) = app.loading_name() {
                ui.spinner();
                ui.label(format!("Loading {name}…"));
            } else if let Some(doc) = &app.doc {
                let d = &doc.drawing;
                ui.label(RichText::new(d.file_name()).strong());
                ui.separator();
                ui.label(format!("{} entities", d.entities.len()));
                let unsupported = d.unsupported_total();
                if unsupported > 0 {
                    ui.separator();
                    let details: Vec<String> = d
                        .unsupported
                        .iter()
                        .map(|(kind, n)| format!("{kind}: {n}"))
                        .collect();
                    ui.colored_label(
                        Color32::from_rgb(230, 190, 90),
                        format!("{unsupported} unsupported"),
                    )
                    .on_hover_text(details.join("\n"));
                }
                ui.separator();
                ui.label(format!("Units: {}", d.header.units));
            } else {
                ui.weak("No drawing open");
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if app.mcp_live() {
                    let hover = "An MCP client is connected to this window";
                    ui.label(RichText::new("MCP live").color(accent()))
                        .on_hover_text(hover);
                    // Painted, because the built-in font has no dot glyph.
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, accent());
                    ui.separator();
                }
                match (app.cursor_world, &app.doc) {
                    (Some(p), Some(_)) => {
                        let digits = coordinate_digits(app.zoom());
                        ui.monospace(format!("X {:.*}   Y {:.*}", digits, p.x, digits, p.y));
                    }
                    _ => {
                        ui.monospace("X —   Y —");
                    }
                }
            });
        });
    });
}

/// Decimal places worth showing at the current zoom (about 0.1 px).
fn coordinate_digits(zoom: f64) -> usize {
    if !zoom.is_finite() || zoom <= 0.0 {
        return 3;
    }
    ((zoom * 10.0).log10().ceil().max(0.0) as usize).clamp(2, 8)
}

pub fn about_window(app: &mut ViewerApp, ctx: &egui::Context) {
    if !app.show_about {
        return;
    }
    let mut open = true;
    egui::Window::new(format!("About {}", brand::PRODUCT_NAME))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("{} — by {}", brand::PRODUCT_NAME, brand::VENDOR))
                        .size(20.0)
                        .color(accent()),
                );
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label(brand::DESCRIPTION);
                ui.add_space(6.0);
                ui.weak(brand::COPYRIGHT);
                ui.add_space(8.0);
            });
        });
    app.show_about = open;
}

pub fn preferences_window(app: &mut ViewerApp, ctx: &egui::Context) {
    if !app.show_prefs {
        return;
    }
    let before = app.settings.clone();
    let mut open = true;
    let mut copied: Option<&str> = None;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            ui.strong("Appearance");
            ui.horizontal(|ui| {
                ui.label("Theme");
                let theme = &mut app.settings.theme;
                ui.selectable_value(theme, Theme::System, "System");
                ui.selectable_value(theme, Theme::Dark, "Dark");
                ui.selectable_value(theme, Theme::Light, "Light");
            });
            ui.weak("White (color 7) entities are drawn black on the light theme.");

            ui.add_space(6.0);
            ui.separator();
            ui.strong("MCP server");
            let mcp = &mut app.settings.mcp;
            ui.checkbox(
                &mut mcp.allow_viewer_control,
                "Let MCP clients control this window",
            )
            .on_hover_text(
                "Allows the show_in_viewer and get_viewer_state tools. The tools that only \
                 read files (open_dxf, render_png, …) always work.",
            );
            ui.add_enabled(
                mcp.allow_viewer_control,
                egui::Checkbox::new(
                    &mut mcp.launch_viewer,
                    "Let show_in_viewer start RustyDxf when it is not running",
                ),
            );
            ui.horizontal(|ui| {
                ui.label("Default render_png background");
                let bg = &mut mcp.render_background;
                ui.selectable_value(bg, RenderBackground::Dark, "Dark");
                ui.selectable_value(bg, RenderBackground::Light, "Light");
            })
            .response
            .on_hover_text("Used when the MCP client does not ask for a background.");
            ui.horizontal(|ui| {
                ui.label("HTTP port");
                ui.add(egui::DragValue::new(&mut mcp.http_port).range(1024..=65535));
                ui.weak(format!(
                    "rustydxf --mcp --http serves http://127.0.0.1:{}/mcp",
                    mcp.http_port
                ));
            });

            ui.add_space(6.0);
            ui.label("Client configuration (stdio)");
            match crate::mcp::client_config_json() {
                Ok(json) => {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(RichText::new(&json).monospace()).wrap());
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .button("Copy JSON")
                            .on_hover_text("For Claude Desktop's claude_desktop_config.json")
                            .clicked()
                        {
                            ctx.copy_text(json);
                            copied = Some("MCP config copied to the clipboard");
                        }
                        if ui.button("Copy Claude Code command").clicked()
                            && let Ok(command) = crate::mcp::claude_code_command()
                        {
                            ctx.copy_text(command);
                            copied = Some("Claude Code command copied to the clipboard");
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, format!("{e:#}"));
                }
            }

            if let Some(error) = &app.settings_error {
                ui.add_space(4.0);
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Settings could not be saved: {error}"),
                );
            } else {
                ui.add_space(4.0);
                ui.weak("Changes are saved immediately; MCP settings apply to the next tool call.");
            }
        });
    app.show_prefs = open;
    if let Some(text) = copied {
        app.toast = Some((text.into(), Instant::now()));
    }
    if app.settings != before {
        app.settings_changed(ctx);
    }
}

pub fn toast(app: &mut ViewerApp, ctx: &egui::Context) {
    let Some((text, since)) = &app.toast else {
        return;
    };
    let shown = Duration::from_secs(3);
    if since.elapsed() > shown {
        app.toast = None;
        return;
    }
    egui::Area::new(egui::Id::unique("toast"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(text);
            });
        });
    ctx.request_repaint_after(shown.saturating_sub(since.elapsed()));
}

#[cfg(test)]
mod tests {
    use super::coordinate_digits;

    #[test]
    fn coordinate_precision_follows_zoom() {
        assert_eq!(coordinate_digits(1.0), 2);
        assert_eq!(coordinate_digits(1000.0), 4);
        assert_eq!(coordinate_digits(1e-6), 2);
        assert_eq!(coordinate_digits(1e12), 8);
        assert_eq!(coordinate_digits(f64::NAN), 3);
    }
}
