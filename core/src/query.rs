//! Read-only queries over a [`Drawing`]: entity summaries, measuring and
//! text search. The MCP tools are thin wrappers around these.

use crate::geom::{BBox, BBoxBuilder, Vec2};
use crate::model::{Drawing, Entity, Geometry};
use serde::Serialize;
use std::collections::BTreeMap;

/// Selects entities by layer name and/or DXF type name (case-insensitive).
#[derive(Debug, Clone, Default)]
pub struct EntityFilter {
    pub layer: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum QueryError {
    #[error("layer not found: {0}")]
    UnknownLayer(String),
}

impl EntityFilter {
    /// Resolve the layer name once; errors if it does not exist.
    fn layer_id(&self, d: &Drawing) -> Result<Option<u32>, QueryError> {
        match &self.layer {
            Some(name) => d
                .layer_index(name)
                .map(Some)
                .ok_or_else(|| QueryError::UnknownLayer(name.clone())),
            None => Ok(None),
        }
    }

    fn matches(&self, e: &Entity, layer: Option<u32>) -> bool {
        layer.is_none_or(|l| e.layer == l)
            && self
                .kind
                .as_deref()
                .is_none_or(|k| e.kind.eq_ignore_ascii_case(k))
    }
}

fn pt(p: Vec2) -> [f64; 2] {
    [p.x, p.y]
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EntitySummary {
    /// Position in the flattened entity list.
    pub index: usize,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub layer: String,
    pub color: String,
    /// True when the entity came from an expanded block.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_block: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
}

pub fn summarize(d: &Drawing, index: usize, e: &Entity) -> EntitySummary {
    let mut s = EntitySummary {
        index,
        kind: e.kind,
        layer: d.layer_name(e.layer).to_string(),
        color: e.color.hex(),
        from_block: e.from_block,
        start: None,
        end: None,
        center: None,
        radius: None,
        position: None,
        length: e.geom.length(),
        vertex_count: None,
        closed: None,
        text: None,
        height: None,
        bbox: None,
    };
    match &e.geom {
        Geometry::Line { a, b } => {
            s.start = Some(pt(*a));
            s.end = Some(pt(*b));
        }
        Geometry::Polyline { vertices, closed } => {
            s.vertex_count = Some(vertices.len());
            s.closed = Some(*closed);
            s.start = vertices.first().map(|v| pt(v.pos));
            s.end = vertices.last().map(|v| pt(v.pos));
            s.bbox = e.geom.bbox();
        }
        Geometry::Arc(arc) => {
            s.center = Some(pt(arc.center));
            s.radius = arc.radius();
            s.closed = Some(arc.is_closed());
            if !arc.is_closed() {
                s.start = Some(pt(arc.start_point()));
                s.end = Some(pt(arc.end_point()));
            }
            if s.radius.is_none() {
                s.bbox = Some(arc.bbox());
            }
        }
        Geometry::Spline(sp) => {
            s.vertex_count = Some(sp.control_points.len().max(sp.fit_points.len()));
            s.closed = Some(sp.closed);
            s.bbox = e.geom.bbox();
        }
        Geometry::Point(p) => s.position = Some(pt(*p)),
        Geometry::Text(t) => {
            s.position = Some(pt(t.pos));
            s.text = Some(t.content.clone());
            s.height = Some(t.height);
        }
        Geometry::Region { loops, .. } => {
            s.vertex_count = Some(loops.iter().map(Vec::len).sum());
            s.closed = Some(true);
            s.bbox = e.geom.bbox();
        }
    }
    s
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EntityPage {
    /// Number of entities matching the filter.
    pub total: usize,
    pub offset: usize,
    pub returned: usize,
    /// True when more matching entities follow this page.
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub entities: Vec<EntitySummary>,
}

pub fn list_entities(
    d: &Drawing,
    filter: &EntityFilter,
    offset: usize,
    limit: usize,
) -> Result<EntityPage, QueryError> {
    let layer = filter.layer_id(d)?;
    let mut total = 0;
    let mut entities = Vec::new();
    for (i, e) in d.entities.iter().enumerate() {
        if !filter.matches(e, layer) {
            continue;
        }
        if total >= offset && entities.len() < limit {
            entities.push(summarize(d, i, e));
        }
        total += 1;
    }
    let end = offset.saturating_add(entities.len());
    let truncated = end < total;
    Ok(EntityPage {
        total,
        offset,
        returned: entities.len(),
        truncated,
        next_offset: truncated.then_some(end),
        entities,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct TypeLength {
    pub count: usize,
    pub length: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct LengthReport {
    /// Sum of the lengths of all matching entities, in drawing units.
    pub total_length: f64,
    /// Matching entities that have a length.
    pub measured: usize,
    /// Matching entities without a length (points, text).
    pub skipped: usize,
    pub by_type: BTreeMap<&'static str, TypeLength>,
}

/// Total curve length of the matching entities.
pub fn total_length(d: &Drawing, filter: &EntityFilter) -> Result<LengthReport, QueryError> {
    let layer = filter.layer_id(d)?;
    let mut r = LengthReport::default();
    for e in d.entities.iter().filter(|e| filter.matches(e, layer)) {
        match e.geom.length().filter(|l| l.is_finite()) {
            Some(len) => {
                r.total_length += len;
                r.measured += 1;
                let t = r.by_type.entry(e.kind).or_default();
                t.count += 1;
                t.length += len;
            }
            None => r.skipped += 1,
        }
    }
    Ok(r)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Distance {
    pub distance: f64,
    pub dx: f64,
    pub dy: f64,
    /// Direction from the first point to the second, degrees CCW from +X.
    pub angle_deg: f64,
}

pub fn distance(a: Vec2, b: Vec2) -> Distance {
    let d = b - a;
    Distance {
        distance: d.length(),
        dx: d.x,
        dy: d.y,
        angle_deg: d.y.atan2(d.x).to_degrees(),
    }
}

/// Bounding box of the whole drawing, or of one layer.
pub fn extents(d: &Drawing, layer: Option<&str>) -> Result<Option<BBox>, QueryError> {
    match layer {
        None => Ok(d.bbox),
        Some(name) => {
            let id = d
                .layer_index(name)
                .ok_or_else(|| QueryError::UnknownLayer(name.to_string()))?;
            Ok(d.layer_bbox(id))
        }
    }
}

/// Bounding box of the entities on layers marked visible.
pub fn visible_extents(d: &Drawing, visible: &[bool]) -> Option<BBox> {
    let mut b = BBoxBuilder::default();
    for e in &d.entities {
        if visible.get(e.layer as usize).copied().unwrap_or(true)
            && let Some(bb) = e.geom.bbox()
        {
            b.add_box(&bb);
        }
    }
    b.0
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TextHit {
    pub index: usize,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub layer: String,
    pub text: String,
    pub position: [f64; 2],
    pub height: f64,
    pub rotation_deg: f64,
}

/// Case-insensitive substring search over TEXT / MTEXT / ATTRIB content.
/// An empty query matches every text entity.
pub fn find_text(d: &Drawing, query: &str) -> Vec<TextHit> {
    let needle = query.to_lowercase();
    d.entities
        .iter()
        .enumerate()
        .filter_map(|(index, e)| match &e.geom {
            Geometry::Text(t) if t.content.to_lowercase().contains(&needle) => Some(TextHit {
                index,
                kind: e.kind,
                layer: d.layer_name(e.layer).to_string(),
                text: t.content.clone(),
                position: pt(t.pos),
                height: t.height,
                rotation_deg: t.rotation.to_degrees(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::Color;
    use crate::geom::{EllipseArc, vec2};
    use crate::model::{HAlign, Layer, PolyVertex, Text, VAlign};
    use std::f64::consts::PI;

    fn sample() -> Drawing {
        let layer = |name: &str| Layer {
            name: name.into(),
            color: Color::FOREGROUND,
            aci: Some(7),
            on: true,
            frozen: false,
            entity_count: 0,
        };
        let ent = |kind: &'static str, layer: u32, geom: Geometry| Entity {
            kind,
            layer,
            color: Color::rgb(255, 0, 0),
            from_block: false,
            geom,
        };
        let pv = |x: f64, y: f64| PolyVertex {
            pos: vec2(x, y),
            bulge: 0.0,
        };
        Drawing {
            layers: vec![layer("0"), layer("Walls"), layer("Notes")],
            entities: vec![
                ent(
                    "LINE",
                    1,
                    Geometry::Line {
                        a: vec2(0.0, 0.0),
                        b: vec2(3.0, 4.0),
                    },
                ),
                ent(
                    "LWPOLYLINE",
                    1,
                    Geometry::Polyline {
                        vertices: vec![pv(0.0, 0.0), pv(10.0, 0.0), pv(10.0, 10.0), pv(0.0, 10.0)],
                        closed: true,
                    },
                ),
                ent(
                    "CIRCLE",
                    0,
                    Geometry::Arc(EllipseArc::circle(vec2(5.0, 5.0), 1.0)),
                ),
                ent(
                    "TEXT",
                    2,
                    Geometry::Text(Text {
                        pos: vec2(1.0, 2.0),
                        height: 2.5,
                        rotation: PI / 2.0,
                        content: "Kitchen".into(),
                        halign: HAlign::Left,
                        valign: VAlign::Baseline,
                    }),
                ),
                ent("POINT", 0, Geometry::Point(vec2(9.0, 9.0))),
            ],
            ..Drawing::default()
        }
    }

    #[test]
    fn total_length_by_layer_and_type() {
        let d = sample();
        let all = total_length(&d, &EntityFilter::default()).unwrap();
        assert!((all.total_length - (5.0 + 40.0 + 2.0 * PI)).abs() < 1e-9);
        assert_eq!((all.measured, all.skipped), (3, 2));
        assert_eq!(all.by_type["LINE"].count, 1);
        assert!((all.by_type["LWPOLYLINE"].length - 40.0).abs() < 1e-9);

        let walls = total_length(
            &d,
            &EntityFilter {
                layer: Some("walls".into()),
                kind: None,
            },
        )
        .unwrap();
        assert!((walls.total_length - 45.0).abs() < 1e-9);

        let wall_lines = total_length(
            &d,
            &EntityFilter {
                layer: Some("Walls".into()),
                kind: Some("line".into()),
            },
        )
        .unwrap();
        assert!((wall_lines.total_length - 5.0).abs() < 1e-9);

        assert_eq!(
            total_length(
                &d,
                &EntityFilter {
                    layer: Some("nope".into()),
                    kind: None
                }
            ),
            Err(QueryError::UnknownLayer("nope".into()))
        );
    }

    #[test]
    fn point_to_point_distance() {
        let r = distance(vec2(1.0, 1.0), vec2(4.0, 5.0));
        assert!((r.distance - 5.0).abs() < 1e-12);
        assert_eq!((r.dx, r.dy), (3.0, 4.0));
        assert!((r.angle_deg - 53.130_102_354_155_98).abs() < 1e-9);
        assert_eq!(distance(vec2(2.0, 2.0), vec2(2.0, 2.0)).distance, 0.0);
    }

    #[test]
    fn pagination_and_truncation() {
        let d = sample();
        let page = list_entities(&d, &EntityFilter::default(), 0, 2).unwrap();
        assert_eq!((page.total, page.returned), (5, 2));
        assert!(page.truncated);
        assert_eq!(page.next_offset, Some(2));
        assert_eq!(page.entities[0].kind, "LINE");
        assert_eq!(page.entities[0].length, Some(5.0));
        assert_eq!(page.entities[0].color, "#ff0000");

        let last = list_entities(&d, &EntityFilter::default(), 4, 2).unwrap();
        assert_eq!(last.returned, 1);
        assert!(!last.truncated && last.next_offset.is_none());

        let beyond = list_entities(&d, &EntityFilter::default(), 99, 2).unwrap();
        assert_eq!((beyond.total, beyond.returned), (5, 0));
        assert!(!beyond.truncated);

        let circles = list_entities(
            &d,
            &EntityFilter {
                layer: None,
                kind: Some("CIRCLE".into()),
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(circles.total, 1);
        assert_eq!(circles.entities[0].index, 2);
        assert_eq!(circles.entities[0].radius, Some(1.0));
        assert_eq!(circles.entities[0].center, Some([5.0, 5.0]));
    }

    #[test]
    fn text_search_is_case_insensitive() {
        let d = sample();
        let hits = find_text(&d, "KITCH");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].layer, "Notes");
        assert_eq!(hits[0].position, [1.0, 2.0]);
        assert!((hits[0].rotation_deg - 90.0).abs() < 1e-9);
        assert!(find_text(&d, "bathroom").is_empty());
        assert_eq!(find_text(&d, "").len(), 1);
    }

    #[test]
    fn extents_per_layer() {
        let mut d = sample();
        let mut b = BBoxBuilder::default();
        for e in &d.entities {
            if let Some(bb) = e.geom.bbox() {
                b.add_box(&bb);
            }
        }
        d.bbox = b.0;
        let walls = extents(&d, Some("Walls")).unwrap().unwrap();
        assert_eq!((walls.min, walls.max), (vec2(0.0, 0.0), vec2(10.0, 10.0)));
        assert!(extents(&d, Some("missing")).is_err());
        assert_eq!(extents(&d, None).unwrap(), d.bbox);
        // Only layer 0 visible: the circle and the point.
        let v = visible_extents(&d, &[true, false, false]).unwrap();
        assert_eq!((v.min, v.max), (vec2(4.0, 4.0), vec2(9.0, 9.0)));
    }
}
