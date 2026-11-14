//! Converts a [`Drawing`] into GPU-ready buffers, once, at load time.
//!
//! Lines are stored as instanced segments (expanded to screen-space quads in
//! the vertex shader, so they keep a constant pixel width) and fills as an
//! indexed triangle list. Both are sorted by layer, so toggling a layer only
//! changes which ranges are drawn and never re-uploads anything.

use crate::aci::Color;
use crate::geom::{Affine2, BBox, Vec2, vec2};
use crate::model::{
    Drawing, Geometry, HAlign, TEXT_ASPECT, TEXT_LINE_SPACING, Text, VAlign, flatten_polyline,
};
use crate::spline::flatten_spline;
use bytemuck::{Pod, Zeroable};
use lyon::math::point;
use lyon::path::Path;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex as LyonFillVertex,
    VertexBuffers,
};
use std::ops::Range;

/// Segment flag: draw as a point marker (fixed pixel size).
pub const FLAG_POINT: u32 = 1;

/// One line segment, in coordinates relative to [`Mesh::origin`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct SegmentInstance {
    pub a: [f32; 2],
    pub b: [f32; 2],
    /// RGB plus a marker in alpha: 0 means "use the foreground color".
    pub color: [u8; 4],
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct FillVertex {
    pub pos: [f32; 2],
    pub color: [u8; 4],
}

/// Buffer ranges belonging to one layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerBatch {
    pub segments: Range<u32>,
    pub indices: Range<u32>,
}

/// A text label for the on-screen renderer (which draws text with egui).
#[derive(Debug, Clone)]
pub struct TextItem {
    pub layer: u32,
    pub color: Color,
    pub text: Text,
}

#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// World position that buffer coordinates are relative to. Keeping the
    /// buffers near zero preserves f32 precision for drawings far from the
    /// origin (survey coordinates, for instance).
    pub origin: Vec2,
    pub bbox: Option<BBox>,
    pub segments: Vec<SegmentInstance>,
    pub fill_vertices: Vec<FillVertex>,
    pub fill_indices: Vec<u32>,
    /// One entry per drawing layer, in layer order.
    pub layers: Vec<LayerBatch>,
    pub texts: Vec<TextItem>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TessellateOptions {
    /// Convert text into filled glyph outlines. The GUI leaves this off and
    /// draws text with egui; the headless renderer turns it on.
    pub text_outlines: bool,
}

fn color_bytes(c: Color) -> [u8; 4] {
    [
        c.rgb[0],
        c.rgb[1],
        c.rgb[2],
        if c.adaptive { 0 } else { 255 },
    ]
}

struct Builder {
    mesh: Mesh,
    tolerance: f64,
    fill: FillTessellator,
    font: Option<ttf_parser::Face<'static>>,
}

impl Builder {
    fn local(&self, p: Vec2) -> [f32; 2] {
        [
            (p.x - self.mesh.origin.x) as f32,
            (p.y - self.mesh.origin.y) as f32,
        ]
    }

    fn segment(&mut self, a: Vec2, b: Vec2, color: [u8; 4], flags: u32) {
        if !(a.is_finite() && b.is_finite()) {
            return;
        }
        let (a, b) = (self.local(a), self.local(b));
        if a.iter().chain(b.iter()).all(|v| v.is_finite()) {
            self.mesh
                .segments
                .push(SegmentInstance { a, b, color, flags });
        }
    }

    fn chain(&mut self, pts: &[Vec2], closed: bool, color: [u8; 4]) {
        for w in pts.windows(2) {
            self.segment(w[0], w[1], color, 0);
        }
        if closed && pts.len() > 2 {
            self.segment(pts[pts.len() - 1], pts[0], color, 0);
        }
    }

    /// Fill closed loops (already in buffer-local f32 coordinates).
    fn fill_loops(
        &mut self,
        loops: &[Vec<[f32; 2]>],
        rule: FillRule,
        tolerance: f32,
        color: [u8; 4],
    ) {
        let mut builder = Path::builder();
        let mut any = false;
        for l in loops {
            if l.len() < 3 || l.iter().flatten().any(|v| !v.is_finite()) {
                continue;
            }
            builder.begin(point(l[0][0], l[0][1]));
            for p in &l[1..] {
                builder.line_to(point(p[0], p[1]));
            }
            builder.end(true);
            any = true;
        }
        if !any {
            return;
        }
        let path = builder.build();
        self.fill_path(&path, rule, tolerance, color, |p| p);
    }

    fn fill_path(
        &mut self,
        path: &Path,
        rule: FillRule,
        tolerance: f32,
        color: [u8; 4],
        map: impl Fn([f32; 2]) -> [f32; 2],
    ) {
        let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
        let options = FillOptions::default()
            .with_fill_rule(rule)
            .with_tolerance(tolerance.max(1e-6));
        let result = self.fill.tessellate_path(
            path,
            &options,
            &mut BuffersBuilder::new(&mut buffers, |v: LyonFillVertex| v.position().to_array()),
        );
        if result.is_err() {
            // Degenerate input: keep the outline, skip the fill.
            return;
        }
        let base = self.mesh.fill_vertices.len() as u32;
        self.mesh
            .fill_vertices
            .extend(buffers.vertices.iter().map(|p| FillVertex {
                pos: map(*p),
                color,
            }));
        self.mesh
            .fill_indices
            .extend(buffers.indices.iter().map(|i| base + i));
    }

    fn text_outline(&mut self, t: &Text, color: [u8; 4]) {
        let Some(font) = self.font.clone() else {
            return;
        };
        let cap = font
            .capital_height()
            .filter(|h| *h > 0)
            .map(f32::from)
            .unwrap_or(font.units_per_em() as f32 * 0.7);
        let scale = t.height as f32 / cap;
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let line_step = (t.height * TEXT_LINE_SPACING) as f32;
        let lines: Vec<&str> = t.content.lines().collect();
        let block_h = t.height as f32 + line_step * (lines.len().saturating_sub(1)) as f32;
        // Baseline of the first line, relative to the anchor.
        let first_baseline = match t.valign {
            VAlign::Baseline | VAlign::Bottom => line_step * (lines.len().saturating_sub(1)) as f32,
            VAlign::Middle => block_h / 2.0 - t.height as f32,
            VAlign::Top => -(t.height as f32),
        };

        let mut builder = Path::builder();
        let mut any = false;
        for (i, line) in lines.iter().enumerate() {
            let advance = |c: char| -> f32 {
                font.glyph_index(c)
                    .and_then(|g| font.glyph_hor_advance(g))
                    .map(|a| a as f32 * scale)
                    .unwrap_or(t.height as f32 * TEXT_ASPECT as f32)
            };
            let width: f32 = line.chars().map(advance).sum();
            let mut x = match t.halign {
                HAlign::Left => 0.0,
                HAlign::Center => -width / 2.0,
                HAlign::Right => -width,
            };
            let y = first_baseline - line_step * i as f32;
            for c in line.chars() {
                if let Some(g) = font.glyph_index(c) {
                    let mut sink = GlyphSink {
                        builder: &mut builder,
                        scale,
                        dx: x,
                        dy: y,
                        open: false,
                    };
                    if font.outline_glyph(g, &mut sink).is_some() {
                        any = true;
                    }
                    sink.finish();
                }
                x += advance(c);
            }
        }
        if !any {
            return;
        }
        let path = builder.build();
        // Text-local -> buffer-local.
        let m =
            Affine2::translate(t.pos - self.mesh.origin).then_after(Affine2::rotate(t.rotation));
        self.fill_path(
            &path,
            FillRule::NonZero,
            (t.height as f32 * 0.01).max(1e-6),
            color,
            |p| {
                let q = m.point(vec2(p[0] as f64, p[1] as f64));
                [q.x as f32, q.y as f32]
            },
        );
    }

    fn entity(&mut self, geom: &Geometry, color: Color, layer: u32, opts: &TessellateOptions) {
        let c = color_bytes(color);
        match geom {
            Geometry::Line { a, b } => self.segment(*a, *b, c, 0),
            Geometry::Polyline { vertices, closed } => {
                let pts = flatten_polyline(vertices, *closed, self.tolerance);
                self.chain(&pts, *closed, c);
            }
            Geometry::Arc(arc) => {
                let pts = arc.flatten(self.tolerance);
                self.chain(&pts, false, c);
            }
            Geometry::Spline(s) => {
                let pts = flatten_spline(s, 24);
                self.chain(&pts, false, c);
            }
            Geometry::Point(p) => self.segment(*p, *p, c, FLAG_POINT),
            Geometry::Text(t) => {
                if opts.text_outlines {
                    self.text_outline(t, c);
                }
                self.mesh.texts.push(TextItem {
                    layer,
                    color,
                    text: t.clone(),
                });
            }
            Geometry::Region { loops, filled } => {
                if *filled {
                    let local: Vec<Vec<[f32; 2]>> = loops
                        .iter()
                        .map(|l| l.iter().map(|p| self.local(*p)).collect())
                        .collect();
                    self.fill_loops(&local, FillRule::EvenOdd, 0.1, c);
                }
                for l in loops {
                    self.chain(l, true, c);
                }
            }
        }
    }
}

/// Adapts ttf-parser's outline callbacks to a lyon path builder.
struct GlyphSink<'a> {
    builder: &'a mut lyon::path::path::Builder,
    scale: f32,
    dx: f32,
    dy: f32,
    open: bool,
}

impl GlyphSink<'_> {
    fn p(&self, x: f32, y: f32) -> lyon::math::Point {
        point(x * self.scale + self.dx, y * self.scale + self.dy)
    }
    fn finish(&mut self) {
        if self.open {
            self.builder.end(true);
            self.open = false;
        }
    }
}

impl ttf_parser::OutlineBuilder for GlyphSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        let p = self.p(x, y);
        self.builder.begin(p);
        self.open = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        if self.open {
            let p = self.p(x, y);
            self.builder.line_to(p);
        }
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        if self.open {
            let (c, p) = (self.p(x1, y1), self.p(x, y));
            self.builder.quadratic_bezier_to(c, p);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        if self.open {
            let (c1, c2, p) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
            self.builder.cubic_bezier_to(c1, c2, p);
        }
    }
    fn close(&mut self) {
        self.finish();
    }
}

/// Tessellate every entity of the drawing. Layer visibility is not applied
/// here; the renderer picks which [`LayerBatch`]es to draw.
/// Greedy word wrap of `content` (which may already contain line breaks) so
/// that no line is wider than `max_width` drawing units at cap height
/// `height`, measured with the built-in font. A single word wider than the
/// limit stays on its own line.
pub fn wrap_text(content: &str, height: f64, max_width: f64) -> String {
    if !(height > 0.0 && max_width > 0.0 && height.is_finite() && max_width.is_finite()) {
        return content.to_string();
    }
    let font = ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0).ok();
    let scale = font.as_ref().map(|f| {
        let cap = f
            .capital_height()
            .filter(|h| *h > 0)
            .map(f64::from)
            .unwrap_or(f64::from(f.units_per_em()) * 0.7);
        height / cap
    });
    let advance = |c: char| -> f64 {
        font.as_ref()
            .zip(scale)
            .and_then(|(f, scale)| {
                let glyph = f.glyph_index(c)?;
                Some(f64::from(f.glyph_hor_advance(glyph)?) * scale)
            })
            .unwrap_or(height * TEXT_ASPECT)
    };
    let width = |s: &str| -> f64 { s.chars().map(advance).sum() };
    let space = advance(' ');

    let mut out = String::with_capacity(content.len() + 8);
    for (i, line) in content.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if width(line) <= max_width {
            out.push_str(line);
            continue;
        }
        let mut used = 0.0;
        for word in line.split(' ').filter(|w| !w.is_empty()) {
            let w = width(word);
            if used > 0.0 && used + space + w > max_width {
                out.push('\n');
                used = 0.0;
            }
            if used > 0.0 {
                out.push(' ');
                used += space;
            }
            out.push_str(word);
            used += w;
        }
    }
    out
}

pub fn tessellate(drawing: &Drawing, opts: TessellateOptions) -> Mesh {
    let origin = drawing.bbox.map(|b| b.center()).unwrap_or(Vec2::ZERO);
    let diagonal = drawing.bbox.map(|b| b.diagonal()).unwrap_or(1.0);
    let tolerance = if diagonal.is_finite() && diagonal > 0.0 {
        diagonal * 2e-5
    } else {
        1e-3
    };
    let font = if opts.text_outlines {
        ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0).ok()
    } else {
        None
    };

    let mut b = Builder {
        mesh: Mesh {
            origin,
            bbox: drawing.bbox,
            ..Mesh::default()
        },
        tolerance,
        fill: FillTessellator::new(),
        font,
    };

    // Bucket entity indices by layer, keeping file order within a layer.
    let mut by_layer: Vec<Vec<u32>> = vec![Vec::new(); drawing.layers.len()];
    for (i, e) in drawing.entities.iter().enumerate() {
        if let Some(bucket) = by_layer.get_mut(e.layer as usize) {
            bucket.push(i as u32);
        }
    }

    for (layer, indices) in by_layer.iter().enumerate() {
        let seg_start = b.mesh.segments.len() as u32;
        let idx_start = b.mesh.fill_indices.len() as u32;
        for &i in indices {
            let e = &drawing.entities[i as usize];
            b.entity(&e.geom, e.color, layer as u32, &opts);
        }
        b.mesh.layers.push(LayerBatch {
            segments: seg_start..b.mesh.segments.len() as u32,
            indices: idx_start..b.mesh.fill_indices.len() as u32,
        });
    }
    b.mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::EllipseArc;
    use crate::model::{Entity, Layer, PolyVertex};

    fn drawing(entities: Vec<(u32, Geometry)>, layers: usize) -> Drawing {
        let mut d = Drawing {
            layers: (0..layers)
                .map(|i| Layer {
                    name: format!("L{i}"),
                    color: Color::FOREGROUND,
                    aci: Some(7),
                    on: true,
                    frozen: false,
                    entity_count: 0,
                })
                .collect(),
            ..Drawing::default()
        };
        let mut bb = crate::geom::BBoxBuilder::default();
        for (layer, geom) in entities {
            if let Some(b) = geom.bbox() {
                bb.add_box(&b);
            }
            d.entities.push(Entity {
                kind: "TEST",
                layer,
                color: Color::rgb(10, 20, 30),
                from_block: false,
                geom,
            });
        }
        d.bbox = bb.0;
        d
    }

    #[test]
    fn layers_get_contiguous_ranges() {
        let line = |x: f64| Geometry::Line {
            a: vec2(x, 0.0),
            b: vec2(x, 1.0),
        };
        // Interleaved layers in file order.
        let d = drawing(vec![(1, line(0.0)), (0, line(1.0)), (1, line(2.0))], 3);
        let m = tessellate(&d, TessellateOptions::default());
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.layers[0].segments, 0..1);
        assert_eq!(m.layers[1].segments, 1..3);
        assert_eq!(m.layers[2].segments, 3..3);
        assert_eq!(m.segments[0].color, [10, 20, 30, 255]);
    }

    #[test]
    fn coordinates_are_relative_to_the_origin() {
        // Far from the world origin, as in survey drawings.
        let d = drawing(
            vec![(
                0,
                Geometry::Line {
                    a: vec2(5_000_000.0, 4_000_000.0),
                    b: vec2(5_000_010.0, 4_000_002.0),
                },
            )],
            1,
        );
        let m = tessellate(&d, TessellateOptions::default());
        assert_eq!(m.origin, vec2(5_000_005.0, 4_000_001.0));
        assert_eq!(m.segments[0].a, [-5.0, -1.0]);
        assert_eq!(m.segments[0].b, [5.0, 1.0]);
    }

    #[test]
    fn circle_tessellation_is_closed_and_accurate() {
        let d = drawing(
            vec![(0, Geometry::Arc(EllipseArc::circle(vec2(0.0, 0.0), 100.0)))],
            1,
        );
        let m = tessellate(&d, TessellateOptions::default());
        assert!(m.segments.len() >= 32);
        let first = m.segments.first().unwrap();
        let last = m.segments.last().unwrap();
        assert!((first.a[0] - last.b[0]).abs() < 1e-3 && (first.a[1] - last.b[1]).abs() < 1e-3);
        for s in &m.segments {
            let r = (s.a[0] as f64).hypot(s.a[1] as f64);
            assert!((r - 100.0).abs() < 1e-3);
            // Chord midpoints stay within the flattening tolerance.
            let mid = ((s.a[0] + s.b[0]) as f64 / 2.0).hypot((s.a[1] + s.b[1]) as f64 / 2.0);
            assert!(100.0 - mid < 0.02, "sagitta {}", 100.0 - mid);
        }
    }

    #[test]
    fn bulged_polyline_adds_arc_segments() {
        let d = drawing(
            vec![(
                0,
                Geometry::Polyline {
                    vertices: vec![
                        PolyVertex {
                            pos: vec2(0.0, 0.0),
                            bulge: 1.0,
                        },
                        PolyVertex {
                            pos: vec2(2.0, 0.0),
                            bulge: 0.0,
                        },
                    ],
                    closed: true,
                },
            )],
            1,
        );
        let m = tessellate(&d, TessellateOptions::default());
        // A semicircle plus the closing straight segment.
        assert!(m.segments.len() > 8);
        let lowest = m
            .segments
            .iter()
            .map(|s| s.a[1] + m.origin.y as f32)
            .fold(f32::MAX, f32::min);
        assert!((lowest + 1.0).abs() < 1e-2, "lowest {lowest}");
    }

    #[test]
    fn filled_region_with_hole_uses_even_odd() {
        let square = |r: f64| vec![vec2(-r, -r), vec2(r, -r), vec2(r, r), vec2(-r, r)];
        let d = drawing(
            vec![(
                0,
                Geometry::Region {
                    loops: vec![square(2.0), square(1.0)],
                    filled: true,
                },
            )],
            1,
        );
        let m = tessellate(&d, TessellateOptions::default());
        assert_eq!(m.segments.len(), 8, "outline of both loops");
        assert_eq!(m.fill_indices.len() % 3, 0);
        // Triangle areas must sum to outer minus hole: 16 - 4.
        let area: f64 = m
            .fill_indices
            .chunks(3)
            .map(|t| {
                let p = |i: u32| {
                    let v = m.fill_vertices[i as usize].pos;
                    vec2(v[0] as f64, v[1] as f64)
                };
                ((p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])) / 2.0).abs()
            })
            .sum();
        assert!((area - 12.0).abs() < 1e-4, "area {area}");
        assert_eq!(m.layers[0].indices, 0..m.fill_indices.len() as u32);
    }

    #[test]
    fn word_wrap_respects_the_width() {
        let text = "alpha beta gamma delta epsilon";
        assert_eq!(wrap_text(text, 1.0, 1e9), text);
        assert_eq!(wrap_text(text, 1.0, 0.0), text);
        let wrapped = wrap_text(text, 1.0, 8.0);
        assert!(wrapped.lines().count() > 1, "{wrapped}");
        assert_eq!(wrapped.replace('\n', " "), text);
        // Existing breaks are kept, and a long word is never split.
        assert_eq!(wrap_text("a\nb", 1.0, 100.0), "a\nb");
        assert_eq!(
            wrap_text("supercalifragilistic", 1.0, 2.0),
            "supercalifragilistic"
        );
    }

    #[test]
    fn points_and_text() {
        let text = Text {
            pos: vec2(0.0, 0.0),
            height: 10.0,
            rotation: 0.0,
            content: "Hi".into(),
            halign: HAlign::Left,
            valign: VAlign::Baseline,
        };
        let d = drawing(
            vec![
                (0, Geometry::Point(vec2(1.0, 1.0))),
                (0, Geometry::Text(text)),
            ],
            1,
        );
        let plain = tessellate(&d, TessellateOptions::default());
        assert_eq!(plain.segments.len(), 1);
        assert_eq!(plain.segments[0].flags, FLAG_POINT);
        assert_eq!(plain.texts.len(), 1);
        assert!(plain.fill_indices.is_empty(), "GUI text is drawn by egui");

        let outlined = tessellate(
            &d,
            TessellateOptions {
                text_outlines: true,
            },
        );
        assert!(!outlined.fill_indices.is_empty());
        // Glyphs sit on the baseline and are about one cap height tall.
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for v in &outlined.fill_vertices {
            lo = lo.min(v.pos[1] + outlined.origin.y as f32);
            hi = hi.max(v.pos[1] + outlined.origin.y as f32);
        }
        assert!(lo.abs() < 0.6, "baseline {lo}");
        assert!((hi - 10.0).abs() < 1.5, "cap height {hi}");
    }

    #[test]
    fn non_finite_geometry_is_skipped() {
        let d = drawing(
            vec![(
                0,
                Geometry::Line {
                    a: vec2(f64::NAN, 0.0),
                    b: vec2(1.0, 1.0),
                },
            )],
            1,
        );
        assert!(
            tessellate(&d, TessellateOptions::default())
                .segments
                .is_empty()
        );
        let empty = tessellate(&Drawing::default(), TessellateOptions::default());
        assert!(empty.segments.is_empty() && empty.layers.is_empty());
    }
}
