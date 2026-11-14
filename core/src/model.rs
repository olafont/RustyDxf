//! RustyDxf's own drawing model. Everything downstream of loading (rendering,
//! measuring, the MCP tools, the UI) works on these types only and never sees
//! the `dxf` crate.

use crate::aci::Color;
use crate::geom::{
    Affine2, BBox, BBoxBuilder, EllipseArc, Vec2, bulge_arc, bulge_segment_length, chain_length,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A fully loaded drawing: entities are flattened (blocks expanded) and in
/// world coordinates.
#[derive(Debug, Clone, Default)]
pub struct Drawing {
    pub path: Option<PathBuf>,
    pub header: HeaderInfo,
    pub layers: Vec<Layer>,
    pub entities: Vec<Entity>,
    /// Bounding box of all model-space geometry, if there is any.
    pub bbox: Option<BBox>,
    /// Top-level model-space entities by DXF type name (an INSERT counts once).
    pub source_counts: BTreeMap<String, usize>,
    /// Entities that were skipped, by DXF type name.
    pub unsupported: BTreeMap<String, usize>,
    /// Non-fatal problems noticed while loading.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HeaderInfo {
    /// DXF version, e.g. "AC1027 (R2013)".
    pub version: String,
    /// Raw `$INSUNITS` code.
    pub units_code: i32,
    /// Human-readable `$INSUNITS`, e.g. "Millimeters".
    pub units: String,
    /// `$EXTMIN` / `$EXTMAX` as stored in the file (often stale or unset).
    pub header_extents: Option<BBox>,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub color: Color,
    /// ACI index of the layer color, when it is not a true color.
    pub aci: Option<u8>,
    pub on: bool,
    pub frozen: bool,
    /// Number of flattened entities on this layer.
    pub entity_count: usize,
}

impl Layer {
    /// Whether the file says this layer should be drawn.
    pub fn visible_by_default(&self) -> bool {
        self.on && !self.frozen
    }
}

#[derive(Debug, Clone)]
pub struct Entity {
    /// DXF type name of the entity this geometry came from ("LINE", ...).
    pub kind: &'static str,
    /// Index into `Drawing::layers`.
    pub layer: u32,
    pub color: Color,
    /// True when the entity came out of an expanded block (INSERT/DIMENSION).
    pub from_block: bool,
    pub geom: Geometry,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyVertex {
    pub pos: Vec2,
    /// Bulge of the segment that starts at this vertex.
    pub bulge: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    #[default]
    Baseline,
    Bottom,
    Middle,
    Top,
}

#[derive(Debug, Clone)]
pub struct Text {
    /// Anchor point; `halign`/`valign` say which part of the text sits here.
    pub pos: Vec2,
    /// Cap height in drawing units.
    pub height: f64,
    /// Counter-clockwise rotation in radians.
    pub rotation: f64,
    /// Plain text with formatting codes removed; may contain `\n`.
    pub content: String,
    pub halign: HAlign,
    pub valign: VAlign,
}

#[derive(Debug, Clone)]
pub struct Spline {
    pub degree: usize,
    pub control_points: Vec<Vec2>,
    /// Empty, or one weight per control point.
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
    /// Used only when there are no control points.
    pub fit_points: Vec<Vec2>,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub enum Geometry {
    Line {
        a: Vec2,
        b: Vec2,
    },
    Polyline {
        vertices: Vec<PolyVertex>,
        closed: bool,
    },
    /// Circles, arcs and ellipses (full or partial).
    Arc(EllipseArc),
    Spline(Spline),
    Point(Vec2),
    Text(Text),
    /// A filled or outlined region: HATCH boundaries, SOLID, TRACE, 3DFACE.
    /// Loops are closed implicitly and filled with the even-odd rule.
    Region {
        loops: Vec<Vec<Vec2>>,
        filled: bool,
    },
}

impl Geometry {
    /// Apply an affine transform (used when expanding block inserts).
    pub fn transformed(&self, m: &Affine2) -> Geometry {
        match self {
            Geometry::Line { a, b } => Geometry::Line {
                a: m.point(*a),
                b: m.point(*b),
            },
            Geometry::Polyline { vertices, closed } => {
                if m.is_similarity() {
                    // Arcs stay arcs; a mirror flips their direction.
                    let flip = if m.determinant() < 0.0 { -1.0 } else { 1.0 };
                    Geometry::Polyline {
                        vertices: vertices
                            .iter()
                            .map(|v| PolyVertex {
                                pos: m.point(v.pos),
                                bulge: v.bulge * flip,
                            })
                            .collect(),
                        closed: *closed,
                    }
                } else {
                    // Arcs become elliptical: flatten them before transforming.
                    let pts = flatten_polyline(vertices, *closed, 0.0);
                    Geometry::Polyline {
                        vertices: pts
                            .into_iter()
                            .map(|p| PolyVertex {
                                pos: m.point(p),
                                bulge: 0.0,
                            })
                            .collect(),
                        closed: *closed,
                    }
                }
            }
            Geometry::Arc(arc) => Geometry::Arc(arc.transformed(m)),
            Geometry::Spline(s) => Geometry::Spline(Spline {
                degree: s.degree,
                control_points: s.control_points.iter().map(|p| m.point(*p)).collect(),
                weights: s.weights.clone(),
                knots: s.knots.clone(),
                fit_points: s.fit_points.iter().map(|p| m.point(*p)).collect(),
                closed: s.closed,
            }),
            Geometry::Point(p) => Geometry::Point(m.point(*p)),
            Geometry::Text(t) => {
                let (s, c) = t.rotation.sin_cos();
                let dir = m.vector(Vec2 { x: c, y: s });
                let up = m.vector(Vec2 { x: -s, y: c });
                Geometry::Text(Text {
                    pos: m.point(t.pos),
                    height: t.height * up.length(),
                    rotation: dir.y.atan2(dir.x),
                    content: t.content.clone(),
                    halign: t.halign,
                    valign: t.valign,
                })
            }
            Geometry::Region { loops, filled } => Geometry::Region {
                loops: loops
                    .iter()
                    .map(|l| l.iter().map(|p| m.point(*p)).collect())
                    .collect(),
                filled: *filled,
            },
        }
    }

    /// Bounding box. Text uses a rough estimate of its extent.
    pub fn bbox(&self) -> Option<BBox> {
        let mut b = BBoxBuilder::default();
        match self {
            Geometry::Line { a, b: p } => {
                b.add(*a);
                b.add(*p);
            }
            Geometry::Polyline { vertices, closed } => {
                let n = vertices.len();
                for (i, v) in vertices.iter().enumerate() {
                    b.add(v.pos);
                    if v.bulge != 0.0 && (i + 1 < n || *closed) {
                        let next = vertices[(i + 1) % n].pos;
                        if let Some(arc) = bulge_arc(v.pos, next, v.bulge) {
                            let e = bulge_to_ellipse_arc(&arc);
                            b.add_box(&e.bbox());
                        }
                    }
                }
            }
            Geometry::Arc(arc) => {
                let bb = arc.bbox();
                b.add(bb.min);
                b.add(bb.max);
            }
            Geometry::Spline(s) => {
                // Control polygon bounds the curve (convex hull property).
                let pts = if s.control_points.is_empty() {
                    &s.fit_points
                } else {
                    &s.control_points
                };
                for p in pts {
                    b.add(*p);
                }
            }
            Geometry::Point(p) => b.add(*p),
            Geometry::Text(t) => {
                for p in text_corners(t) {
                    b.add(p);
                }
            }
            Geometry::Region { loops, .. } => {
                for p in loops.iter().flatten() {
                    b.add(*p);
                }
            }
        }
        b.0
    }

    /// Curve length in drawing units. `None` for things that have no length
    /// (points, text). Regions report their total boundary length.
    pub fn length(&self) -> Option<f64> {
        match self {
            Geometry::Line { a, b } => Some(a.distance(*b)),
            Geometry::Polyline { vertices, closed } => {
                let n = vertices.len();
                let mut len = 0.0;
                for i in 0..n {
                    if i + 1 < n || (*closed && n > 1) {
                        let next = vertices[(i + 1) % n].pos;
                        len += bulge_segment_length(vertices[i].pos, next, vertices[i].bulge);
                    }
                }
                Some(len)
            }
            Geometry::Arc(arc) => Some(arc.length()),
            Geometry::Spline(s) => {
                let pts = crate::spline::flatten_spline(s, 64);
                Some(chain_length(&pts, false))
            }
            Geometry::Region { loops, .. } => {
                Some(loops.iter().map(|l| chain_length(l, true)).sum())
            }
            Geometry::Point(_) | Geometry::Text(_) => None,
        }
    }
}

fn bulge_to_ellipse_arc(arc: &crate::geom::BulgeArc) -> EllipseArc {
    // EllipseArc sweeps counter-clockwise, so reverse clockwise arcs.
    let (start, sweep) = if arc.sweep >= 0.0 {
        (arc.start_angle, arc.sweep)
    } else {
        (arc.start_angle + arc.sweep, -arc.sweep)
    };
    EllipseArc {
        center: arc.center,
        major: Vec2 {
            x: arc.radius,
            y: 0.0,
        },
        minor: Vec2 {
            x: 0.0,
            y: arc.radius,
        },
        start,
        sweep,
    }
}

/// Flatten a bulged polyline into points. With `tolerance <= 0` a fixed,
/// fairly fine angular step is used. The closing point is not repeated.
pub fn flatten_polyline(vertices: &[PolyVertex], closed: bool, tolerance: f64) -> Vec<Vec2> {
    let n = vertices.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = vertices[i];
        out.push(v.pos);
        let has_next = i + 1 < n || (closed && n > 1);
        if !has_next || v.bulge == 0.0 {
            continue;
        }
        let next = vertices[(i + 1) % n].pos;
        if let Some(arc) = bulge_arc(v.pos, next, v.bulge) {
            let segs = if tolerance > 0.0 {
                crate::geom::arc_segments(arc.radius, arc.sweep, tolerance)
            } else {
                ((arc.sweep.abs() / (std::f64::consts::TAU / 96.0)).ceil() as usize).max(2)
            };
            for k in 1..segs {
                let a = arc.start_angle + arc.sweep * k as f64 / segs as f64;
                out.push(
                    arc.center
                        + Vec2 {
                            x: a.cos(),
                            y: a.sin(),
                        } * arc.radius,
                );
            }
        }
    }
    out
}

/// Approximate advance width of text, as a fraction of its height per char.
pub const TEXT_ASPECT: f64 = 0.6;
/// Line spacing as a multiple of the text height.
pub const TEXT_LINE_SPACING: f64 = 1.6;

/// Estimated (width, height) of a text block in drawing units.
pub fn text_extent(t: &Text) -> (f64, f64) {
    let lines = t.content.lines().count().max(1);
    let longest = t
        .content
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    (
        longest as f64 * t.height * TEXT_ASPECT,
        t.height + (lines - 1) as f64 * t.height * TEXT_LINE_SPACING,
    )
}

/// Estimated corners of a text block, honouring alignment and rotation.
pub fn text_corners(t: &Text) -> [Vec2; 4] {
    let (w, h) = text_extent(t);
    let x0 = match t.halign {
        HAlign::Left => 0.0,
        HAlign::Center => -w / 2.0,
        HAlign::Right => -w,
    };
    // y of the top edge relative to the anchor.
    let top = match t.valign {
        VAlign::Baseline | VAlign::Bottom => t.height,
        VAlign::Middle => h / 2.0,
        VAlign::Top => 0.0,
    };
    let m = Affine2::translate(t.pos).then_after(Affine2::rotate(t.rotation));
    [
        m.point(Vec2 { x: x0, y: top }),
        m.point(Vec2 { x: x0 + w, y: top }),
        m.point(Vec2 {
            x: x0 + w,
            y: top - h,
        }),
        m.point(Vec2 { x: x0, y: top - h }),
    ]
}

impl Drawing {
    pub fn layer_index(&self, name: &str) -> Option<u32> {
        self.layers
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(name))
            .map(|i| i as u32)
    }

    pub fn layer_name(&self, index: u32) -> &str {
        self.layers
            .get(index as usize)
            .map(|l| l.name.as_str())
            .unwrap_or("0")
    }

    /// Bounding box of the entities on one layer.
    pub fn layer_bbox(&self, layer: u32) -> Option<BBox> {
        let mut b = BBoxBuilder::default();
        for e in self.entities.iter().filter(|e| e.layer == layer) {
            if let Some(bb) = e.geom.bbox() {
                b.add_box(&bb);
            }
        }
        b.0
    }

    /// Flattened entity counts by DXF type name (after block expansion).
    pub fn flattened_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut m = BTreeMap::new();
        for e in &self.entities {
            *m.entry(e.kind).or_insert(0) += 1;
        }
        m
    }

    pub fn unsupported_total(&self) -> usize {
        self.unsupported.values().sum()
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Human-readable name for a `$INSUNITS` code.
pub fn units_name(code: i32) -> &'static str {
    match code {
        0 => "Unitless",
        1 => "Inches",
        2 => "Feet",
        3 => "Miles",
        4 => "Millimeters",
        5 => "Centimeters",
        6 => "Meters",
        7 => "Kilometers",
        8 => "Microinches",
        9 => "Mils",
        10 => "Yards",
        11 => "Angstroms",
        12 => "Nanometers",
        13 => "Microns",
        14 => "Decimeters",
        15 => "Decameters",
        16 => "Hectometers",
        17 => "Gigameters",
        18 => "Astronomical units",
        19 => "Light years",
        20 => "Parsecs",
        21 => "US survey feet",
        22 => "US survey inches",
        23 => "US survey yards",
        24 => "US survey miles",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::vec2;
    use std::f64::consts::PI;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn pv(x: f64, y: f64, bulge: f64) -> PolyVertex {
        PolyVertex {
            pos: vec2(x, y),
            bulge,
        }
    }

    #[test]
    fn line_and_polyline_length() {
        let l = Geometry::Line {
            a: vec2(0.0, 0.0),
            b: vec2(3.0, 4.0),
        };
        assert!(close(l.length().unwrap(), 5.0));

        let open = Geometry::Polyline {
            vertices: vec![pv(0.0, 0.0, 0.0), pv(10.0, 0.0, 0.0), pv(10.0, 5.0, 0.0)],
            closed: false,
        };
        assert!(close(open.length().unwrap(), 15.0));

        let closed = Geometry::Polyline {
            vertices: vec![
                pv(0.0, 0.0, 0.0),
                pv(4.0, 0.0, 0.0),
                pv(4.0, 3.0, 0.0),
                pv(0.0, 3.0, 0.0),
            ],
            closed: true,
        };
        assert!(close(closed.length().unwrap(), 14.0));
    }

    #[test]
    fn bulged_polyline_length_and_bbox() {
        // A "stadium": two straight sides of 10 and two semicircles of r=1.
        let g = Geometry::Polyline {
            vertices: vec![
                pv(0.0, 0.0, 0.0),
                pv(10.0, 0.0, 1.0),
                pv(10.0, 2.0, 0.0),
                pv(0.0, 2.0, 1.0),
            ],
            closed: true,
        };
        assert!(close(g.length().unwrap(), 20.0 + 2.0 * PI));
        let b = g.bbox().unwrap();
        assert!(close(b.min.x, -1.0) && close(b.max.x, 11.0));
        assert!(close(b.min.y, 0.0) && close(b.max.y, 2.0));
    }

    #[test]
    fn arc_and_region_length() {
        let c = Geometry::Arc(EllipseArc::circle(vec2(0.0, 0.0), 2.0));
        assert!(close(c.length().unwrap(), 4.0 * PI));
        let r = Geometry::Region {
            loops: vec![vec![
                vec2(0.0, 0.0),
                vec2(2.0, 0.0),
                vec2(2.0, 2.0),
                vec2(0.0, 2.0),
            ]],
            filled: true,
        };
        assert!(close(r.length().unwrap(), 8.0));
        assert!(Geometry::Point(vec2(0.0, 0.0)).length().is_none());
    }

    #[test]
    fn mirror_flips_bulge_direction() {
        let g = Geometry::Polyline {
            vertices: vec![pv(0.0, 0.0, 1.0), pv(2.0, 0.0, 0.0)],
            closed: false,
        };
        let m = g.transformed(&Affine2::scale(-1.0, 1.0));
        let Geometry::Polyline { vertices, .. } = &m else {
            panic!("expected polyline")
        };
        assert_eq!(vertices[0].bulge, -1.0);
        // The arc bulged below the X axis and must still do so when mirrored.
        let b = m.bbox().unwrap();
        assert!(close(b.min.y, -1.0) && close(b.max.y, 0.0));
        assert!(close(b.min.x, -2.0) && close(b.max.x, 0.0));
    }

    #[test]
    fn nonuniform_scale_flattens_bulges() {
        let g = Geometry::Polyline {
            vertices: vec![pv(0.0, 0.0, 1.0), pv(2.0, 0.0, 0.0)],
            closed: false,
        };
        let m = g.transformed(&Affine2::scale(1.0, 3.0));
        let Geometry::Polyline { vertices, .. } = &m else {
            panic!("expected polyline")
        };
        assert!(vertices.len() > 10);
        assert!(vertices.iter().all(|v| v.bulge == 0.0));
        assert!((m.bbox().unwrap().min.y + 3.0).abs() < 1e-2);
    }

    #[test]
    fn text_transform_scales_and_rotates() {
        let t = Geometry::Text(Text {
            pos: vec2(1.0, 0.0),
            height: 2.0,
            rotation: 0.0,
            content: "A".into(),
            halign: HAlign::Left,
            valign: VAlign::Baseline,
        });
        let m = Affine2::rotate(PI / 2.0).then_after(Affine2::scale(3.0, 3.0));
        let Geometry::Text(t) = t.transformed(&m) else {
            panic!("expected text")
        };
        assert!(close(t.height, 6.0));
        assert!(close(t.rotation, PI / 2.0));
        assert!(close(t.pos.x, 0.0) && close(t.pos.y, 3.0));
    }

    #[test]
    fn units_names() {
        assert_eq!(units_name(4), "Millimeters");
        assert_eq!(units_name(1), "Inches");
        assert_eq!(units_name(0), "Unitless");
        assert_eq!(units_name(999), "Unknown");
    }
}
