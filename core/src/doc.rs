//! Loads a DXF file into RustyDxf's own [`Drawing`] model.
//!
//! This is the only module (with [`crate::fixtures`]) that touches `dxf` crate
//! types. Blocks are expanded here, so the resulting entity list is flat and
//! in world coordinates.

use crate::aci::{Color, ColorSpec};
use crate::geom::{Affine2, BBoxBuilder, EllipseArc, Vec2, ccw_sweep, ocs_to_wcs, vec2};
use crate::model::{
    Drawing, Entity, Geometry, HAlign, HeaderInfo, Layer, PolyVertex, Spline, Text, VAlign,
    units_name,
};
use crate::raw::{self, RawCommon, RawContainer, RawScan};
use dxf::entities::EntityType;
use dxf::enums::{AttachmentPoint, HorizontalTextJustification, VerticalTextJustification};
use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

/// Maximum nesting depth of block references.
const MAX_INSERT_DEPTH: usize = 32;
/// Maximum number of flattened entities; guards against "block bombs".
const MAX_ENTITIES: usize = 5_000_000;
/// Maximum cells of a single arrayed INSERT (rows x columns).
const MAX_ARRAY_CELLS: i64 = 10_000;

/// Entity types RustyDxf draws.
pub const SUPPORTED_TYPES: &[&str] = &[
    "LINE",
    "3DLINE",
    "LWPOLYLINE",
    "POLYLINE",
    "CIRCLE",
    "ARC",
    "ELLIPSE",
    "SPLINE",
    "POINT",
    "TEXT",
    "MTEXT",
    "INSERT",
    "HATCH",
    "DIMENSION",
    "SOLID",
    "TRACE",
    "3DFACE",
    "LEADER",
];

/// Records that belong to another entity or are never drawn on their own;
/// they are neither supported nor counted as unsupported.
const STRUCTURAL_TYPES: &[&str] = &["VERTEX", "SEQEND", "ATTRIB", "ATTDEF"];

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("not a valid DXF file: {0}")]
    Parse(String),
    #[error("the DXF parser failed unexpectedly on this file")]
    ParserPanic,
}

/// Load a DXF file (ASCII or binary) from disk.
pub fn load_file(path: impl AsRef<Path>) -> Result<Drawing, LoadError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| LoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    load_bytes(&bytes, Some(path.to_path_buf()))
}

/// Load a DXF document from memory.
pub fn load_bytes(bytes: &[u8], path: Option<PathBuf>) -> Result<Drawing, LoadError> {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(LoadError::Parse("the file is empty".into()));
    }

    // The dxf crate is not expected to panic, but a viewer must never crash
    // on bad input, so contain it anyway.
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        dxf::Drawing::load(&mut std::io::Cursor::new(bytes))
    }))
    .map_err(|_| LoadError::ParserPanic)?
    .map_err(|e| LoadError::Parse(e.to_string()))?;

    let mut warnings = Vec::new();
    let raw = match catch_unwind(|| raw::scan(bytes)) {
        Ok(r) => Some(r),
        Err(_) => {
            warnings.push(
                "supplementary scan failed; hatches, frozen layers and unsupported-entity \
                 counts are unavailable"
                    .to_string(),
            );
            None
        }
    };

    let has_content = parsed.entities().next().is_some() || parsed.blocks().next().is_some();
    if !has_content && !raw.as_ref().is_some_and(|r| r.saw_section) {
        return Err(LoadError::Parse("no DXF sections found".into()));
    }

    let drawing = catch_unwind(AssertUnwindSafe(|| {
        build(&parsed, raw.as_ref(), path, warnings)
    }))
    .map_err(|_| LoadError::ParserPanic)?;
    Ok(drawing)
}

#[derive(Clone)]
struct Ctx {
    m: Affine2,
    /// Color that BYBLOCK entities take.
    block_color: Color,
    /// Layer that entities on layer "0" inherit inside a block.
    insert_layer: Option<u32>,
}

struct Builder<'a> {
    drawing: Drawing,
    layer_lookup: HashMap<String, u32>,
    blocks: HashMap<String, &'a dxf::Block>,
    raw: Option<&'a RawScan>,
    stack: Vec<String>,
    used_blocks: HashSet<String>,
    over_budget: bool,
}

fn build(
    parsed: &dxf::Drawing,
    raw: Option<&RawScan>,
    path: Option<PathBuf>,
    warnings: Vec<String>,
) -> Drawing {
    let mut b = Builder {
        drawing: Drawing {
            path,
            header: header_info(parsed),
            warnings,
            ..Drawing::default()
        },
        layer_lookup: HashMap::new(),
        blocks: HashMap::new(),
        raw,
        stack: Vec::new(),
        used_blocks: HashSet::new(),
        over_budget: false,
    };

    for l in parsed.layers() {
        let raw_layer = raw.and_then(|r| r.layers.get(&l.name.to_uppercase()));
        let aci = l.color.index();
        let color = match raw_layer.and_then(|r| r.true_color) {
            Some(tc) => Color::from_true_color(tc),
            None => Color::from_aci(aci.map(i32::from).unwrap_or(7)),
        };
        b.add_layer(Layer {
            name: l.name.clone(),
            color,
            aci,
            on: l.is_layer_on,
            frozen: raw_layer.is_some_and(|r| r.frozen()),
            entity_count: 0,
        });
    }
    b.layer_id("0");

    for block in parsed.blocks() {
        b.blocks.entry(block.name.to_uppercase()).or_insert(block);
    }

    let root = Ctx {
        m: Affine2::IDENTITY,
        block_color: Color::FOREGROUND,
        insert_layer: None,
    };
    let model_raw = raw.map(|r| &r.model);
    b.expand(parsed.entities(), model_raw, &root);

    // Counts.
    match raw {
        Some(r) => {
            b.drawing.source_counts = r
                .model
                .type_counts
                .iter()
                .filter(|(k, _)| !STRUCTURAL_TYPES.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            let used: Vec<&RawContainer> = std::iter::once(&r.model)
                .chain(
                    r.blocks
                        .iter()
                        .filter(|(name, _)| b.used_blocks.contains(*name))
                        .map(|(_, c)| c),
                )
                .collect();
            for c in used {
                for (kind, n) in &c.type_counts {
                    let k = kind.as_str();
                    if !SUPPORTED_TYPES.contains(&k) && !STRUCTURAL_TYPES.contains(&k) {
                        *b.drawing.unsupported.entry(kind.clone()).or_insert(0) += n;
                    }
                }
            }
        }
        None => {
            for e in parsed.entities() {
                if e.common.is_in_paper_space {
                    continue;
                }
                let name = variant_name(&e.specific);
                *b.drawing.source_counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut bbox = BBoxBuilder::default();
    let mut drawing = b.drawing;
    for e in &drawing.entities {
        if let Some(bb) = e.geom.bbox() {
            bbox.add_box(&bb);
        }
        if let Some(l) = drawing.layers.get_mut(e.layer as usize) {
            l.entity_count += 1;
        }
    }
    drawing.bbox = bbox.0;
    drawing
}

impl<'a> Builder<'a> {
    fn add_layer(&mut self, layer: Layer) -> u32 {
        let key = layer.name.to_uppercase();
        if let Some(&i) = self.layer_lookup.get(&key) {
            return i;
        }
        let i = self.drawing.layers.len() as u32;
        self.drawing.layers.push(layer);
        self.layer_lookup.insert(key, i);
        i
    }

    /// Index of a layer, creating it (with default properties) if the file
    /// references a layer that has no table entry.
    fn layer_id(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.layer_lookup.get(&name.to_uppercase()) {
            return i;
        }
        self.add_layer(Layer {
            name: name.to_string(),
            color: Color::FOREGROUND,
            aci: Some(7),
            on: true,
            frozen: false,
            entity_count: 0,
        })
    }

    fn resolve(&mut self, layer_name: &str, spec: ColorSpec, ctx: &Ctx) -> (u32, Color) {
        let layer = match ctx.insert_layer {
            Some(l) if layer_name == "0" => l,
            _ => self.layer_id(layer_name),
        };
        let layer_color = self.drawing.layers[layer as usize].color;
        (layer, spec.resolve(layer_color, ctx.block_color))
    }

    fn emit(&mut self, kind: &'static str, layer: u32, color: Color, geom: Geometry, ctx: &Ctx) {
        if self.drawing.entities.len() >= MAX_ENTITIES {
            if !self.over_budget {
                self.over_budget = true;
                self.drawing.warnings.push(format!(
                    "drawing truncated at {MAX_ENTITIES} entities after block expansion"
                ));
            }
            return;
        }
        let geom = if ctx.m == Affine2::IDENTITY {
            geom
        } else {
            geom.transformed(&ctx.m)
        };
        if !geom_is_finite(&geom) {
            return;
        }
        self.drawing.entities.push(Entity {
            kind,
            layer,
            color,
            from_block: ctx.insert_layer.is_some(),
            geom,
        });
    }

    fn unsupported(&mut self, kind: &str) {
        *self
            .drawing
            .unsupported
            .entry(kind.to_string())
            .or_insert(0) += 1;
    }

    fn expand<I>(&mut self, entities: I, raw: Option<&RawContainer>, ctx: &Ctx)
    where
        I: Iterator<Item = &'a dxf::entities::Entity>,
    {
        // Fills first so outlines drawn later stay on top; order within the
        // final buffers is by layer anyway.
        if let Some(raw) = raw {
            for h in &raw.hatches {
                if !h.common.visible || h.loops.is_empty() {
                    continue;
                }
                let (layer, color) = self.resolve(&h.common.layer, h.common.color, ctx);
                self.emit(
                    "HATCH",
                    layer,
                    color,
                    Geometry::Region {
                        loops: h.loops.clone(),
                        filled: h.solid,
                    },
                    ctx,
                );
            }
        }

        for e in entities {
            if self.over_budget {
                return;
            }
            if !e.common.is_visible || e.common.is_in_paper_space {
                continue;
            }
            let spec = color_spec(&e.common.color, e.common.color_24_bit);
            let (layer, color) = self.resolve(&e.common.layer, spec, ctx);
            self.entity(e, layer, color, ctx);
        }

        if let Some(raw) = raw {
            for d in &raw.dimensions {
                self.dimension(&d.common, &d.block_name, ctx);
            }
        }
    }

    fn dimension(&mut self, common: &RawCommon, block_name: &str, ctx: &Ctx) {
        if !common.visible {
            return;
        }
        let (layer, color) = self.resolve(&common.layer, common.color, ctx);
        if !self.expand_block(block_name, Affine2::IDENTITY, layer, color, ctx) {
            self.unsupported("DIMENSION (no block)");
        }
    }

    /// Expand a block definition under `local`. Returns false when the block
    /// does not exist or cannot be expanded (cycle, depth).
    fn expand_block(
        &mut self,
        name: &str,
        local: Affine2,
        layer: u32,
        color: Color,
        ctx: &Ctx,
    ) -> bool {
        let key = name.to_uppercase();
        let Some(block) = self.blocks.get(&key).copied() else {
            return false;
        };
        if self.stack.len() >= MAX_INSERT_DEPTH || self.stack.contains(&key) {
            let msg = format!("recursive or too deeply nested block reference: {name}");
            if !self.drawing.warnings.contains(&msg) {
                self.drawing.warnings.push(msg);
            }
            return false;
        }
        let base = vec2(block.base_point.x, block.base_point.y);
        let m = ctx
            .m
            .then_after(local)
            .then_after(Affine2::translate(base * -1.0));
        if !m.is_finite() {
            return false;
        }
        let inner = Ctx {
            m,
            block_color: color,
            insert_layer: Some(layer),
        };
        let raw = self.raw.and_then(|r| r.blocks.get(&key));
        self.used_blocks.insert(key.clone());
        self.stack.push(key);
        self.expand(block.entities.iter(), raw, &inner);
        self.stack.pop();
        true
    }

    fn entity(&mut self, e: &'a dxf::entities::Entity, layer: u32, color: Color, ctx: &Ctx) {
        match &e.specific {
            EntityType::Line(l) => self.emit(
                "LINE",
                layer,
                color,
                Geometry::Line {
                    a: p2(&l.p1),
                    b: p2(&l.p2),
                },
                ctx,
            ),
            EntityType::Circle(c) => {
                let ocs = ocs(&c.normal);
                let arc = EllipseArc::circle(p2(&c.center), c.radius).transformed(&ocs);
                self.emit("CIRCLE", layer, color, Geometry::Arc(arc), ctx);
            }
            EntityType::Arc(a) => {
                let ocs = ocs(&a.normal);
                let arc = EllipseArc::circular(
                    p2(&a.center),
                    a.radius,
                    a.start_angle.to_radians(),
                    a.end_angle.to_radians(),
                )
                .transformed(&ocs);
                self.emit("ARC", layer, color, Geometry::Arc(arc), ctx);
            }
            EntityType::Ellipse(el) => {
                // Ellipses are stored in WCS; the minor axis is N x major.
                let (a, n) = (&el.major_axis, &el.normal);
                let nlen = (n.x * n.x + n.y * n.y + n.z * n.z).sqrt();
                let (nx, ny, nz) = if nlen > 1e-12 {
                    (n.x / nlen, n.y / nlen, n.z / nlen)
                } else {
                    (0.0, 0.0, 1.0)
                };
                let minor = vec2(ny * a.z - nz * a.y, nz * a.x - nx * a.z) * el.minor_axis_ratio;
                let full = (el.end_parameter - el.start_parameter).abs() >= TAU - 1e-9;
                let arc = EllipseArc {
                    center: p2(&el.center),
                    major: vec2(a.x, a.y),
                    minor,
                    start: el.start_parameter,
                    sweep: if full {
                        TAU
                    } else {
                        ccw_sweep(el.start_parameter, el.end_parameter)
                    },
                };
                self.emit("ELLIPSE", layer, color, Geometry::Arc(arc), ctx);
            }
            EntityType::LwPolyline(p) => {
                let ocs = ocs(&p.extrusion_direction);
                let vertices: Vec<PolyVertex> = p
                    .vertices
                    .iter()
                    .map(|v| PolyVertex {
                        pos: vec2(v.x, v.y),
                        bulge: v.bulge,
                    })
                    .collect();
                if vertices.is_empty() {
                    return;
                }
                let geom = Geometry::Polyline {
                    vertices,
                    closed: p.is_closed(),
                }
                .transformed(&ocs);
                self.emit("LWPOLYLINE", layer, color, geom, ctx);
            }
            EntityType::Polyline(p) => {
                if p.is_polyface_mesh() || p.is_3d_polygon_mesh() {
                    self.unsupported("POLYLINE (mesh)");
                    return;
                }
                let vertices: Vec<PolyVertex> = p
                    .vertices()
                    .filter(|v| !v.is_spline_frame_control_point())
                    .map(|v| PolyVertex {
                        pos: p2(&v.location),
                        bulge: v.bulge,
                    })
                    .collect();
                if vertices.is_empty() {
                    return;
                }
                let geom = Geometry::Polyline {
                    vertices,
                    closed: p.is_closed(),
                };
                // 3D polylines are in WCS; 2D ones in the entity's OCS.
                let geom = if p.is_3d_polyline() {
                    geom
                } else {
                    geom.transformed(&ocs(&p.normal))
                };
                self.emit("POLYLINE", layer, color, geom, ctx);
            }
            EntityType::Spline(s) => {
                let spline = Spline {
                    degree: s.degree_of_curve.clamp(1, 16) as usize,
                    control_points: s.control_points.iter().map(p2).collect(),
                    weights: s.weight_values.clone(),
                    knots: s.knot_values.clone(),
                    fit_points: s.fit_points.iter().map(p2).collect(),
                    closed: s.is_closed(),
                };
                if spline.control_points.len() < 2 && spline.fit_points.len() < 2 {
                    return;
                }
                self.emit("SPLINE", layer, color, Geometry::Spline(spline), ctx);
            }
            EntityType::ModelPoint(p) => {
                self.emit("POINT", layer, color, Geometry::Point(p2(&p.location)), ctx)
            }
            EntityType::Text(t) => {
                let geom = single_line_text(
                    &t.location,
                    &t.second_alignment_point,
                    t.text_height,
                    t.rotation,
                    &t.value,
                    t.horizontal_text_justification,
                    t.vertical_text_justification,
                    &t.normal,
                );
                if let Some(g) = geom {
                    self.emit("TEXT", layer, color, g, ctx);
                }
            }
            EntityType::MText(t) => {
                let mut raw_text = t.extended_text.concat();
                raw_text.push_str(&t.text);
                let content = clean_mtext(&raw_text);
                if content.trim().is_empty() {
                    return;
                }
                let dir = &t.x_axis_direction;
                let rotation = if t.rotation_angle != 0.0 {
                    t.rotation_angle.to_radians()
                } else if dir.x != 0.0 || dir.y != 0.0 {
                    dir.y.atan2(dir.x)
                } else {
                    0.0
                };
                let (halign, valign) = attachment(t.attachment_point);
                let geom = Geometry::Text(Text {
                    pos: p2(&t.insertion_point),
                    height: positive_or(t.initial_text_height, 1.0),
                    rotation,
                    content,
                    halign,
                    valign,
                })
                .transformed(&ocs(&t.extrusion_direction));
                self.emit("MTEXT", layer, color, geom, ctx);
            }
            EntityType::Insert(ins) => {
                let rows = i64::from(ins.row_count.max(1));
                let cols = i64::from(ins.column_count.max(1));
                let (rows, cols) = if rows * cols > MAX_ARRAY_CELLS {
                    (1, 1)
                } else {
                    (rows, cols)
                };
                let place = ocs(&ins.extrusion_direction)
                    .then_after(Affine2::translate(p2(&ins.location)))
                    .then_after(Affine2::rotate(ins.rotation.to_radians()));
                let scale = Affine2::scale(ins.x_scale_factor, ins.y_scale_factor);
                let mut found = true;
                'cells: for r in 0..rows {
                    for c in 0..cols {
                        let offset =
                            vec2(c as f64 * ins.column_spacing, r as f64 * ins.row_spacing);
                        let local = place
                            .then_after(Affine2::translate(offset))
                            .then_after(scale);
                        if !self.expand_block(&ins.name, local, layer, color, ctx) {
                            found = false;
                            break 'cells;
                        }
                    }
                }
                if !found && !self.blocks.contains_key(&ins.name.to_uppercase()) {
                    self.unsupported("INSERT (missing block)");
                }
                for a in ins.attributes() {
                    if a.is_invisible() {
                        continue;
                    }
                    let geom = single_line_text(
                        &a.location,
                        &a.second_alignment_point,
                        a.text_height,
                        a.rotation,
                        &a.value,
                        a.horizontal_text_justification,
                        a.vertical_text_justification,
                        &a.normal,
                    );
                    if let Some(g) = geom {
                        self.emit("ATTRIB", layer, color, g, ctx);
                    }
                }
            }
            EntityType::Solid(s) => self.quad(
                "SOLID",
                [
                    &s.first_corner,
                    &s.second_corner,
                    &s.fourth_corner,
                    &s.third_corner,
                ],
                Some(&s.extrusion_direction),
                true,
                layer,
                color,
                ctx,
            ),
            EntityType::Trace(s) => self.quad(
                "TRACE",
                [
                    &s.first_corner,
                    &s.second_corner,
                    &s.fourth_corner,
                    &s.third_corner,
                ],
                Some(&s.extrusion_direction),
                true,
                layer,
                color,
                ctx,
            ),
            EntityType::Face3D(f) => self.quad(
                "3DFACE",
                [
                    &f.first_corner,
                    &f.second_corner,
                    &f.third_corner,
                    &f.fourth_corner,
                ],
                None,
                false,
                layer,
                color,
                ctx,
            ),
            EntityType::Leader(l) => {
                let vertices: Vec<PolyVertex> = l
                    .vertices
                    .iter()
                    .map(|p| PolyVertex {
                        pos: p2(p),
                        bulge: 0.0,
                    })
                    .collect();
                if vertices.len() >= 2 {
                    self.emit(
                        "LEADER",
                        layer,
                        color,
                        Geometry::Polyline {
                            vertices,
                            closed: false,
                        },
                        ctx,
                    );
                }
            }
            // Dimensions normally come from the raw scan (which also sees
            // the ones the dxf crate drops); use the crate's only without it.
            EntityType::RotatedDimension(d) => self.crate_dimension(&d.dimension_base, e, ctx),
            EntityType::RadialDimension(d) => self.crate_dimension(&d.dimension_base, e, ctx),
            EntityType::DiameterDimension(d) => self.crate_dimension(&d.dimension_base, e, ctx),
            EntityType::AngularThreePointDimension(d) => {
                self.crate_dimension(&d.dimension_base, e, ctx)
            }
            EntityType::OrdinateDimension(d) => self.crate_dimension(&d.dimension_base, e, ctx),
            other => {
                // With the raw scan available, unsupported types are counted
                // from it (it also sees what the dxf crate drops).
                if self.raw.is_none() {
                    let name = variant_name(other);
                    if !STRUCTURAL_TYPES.contains(&name) {
                        self.unsupported(name);
                    }
                }
            }
        }
    }

    fn crate_dimension(
        &mut self,
        base: &dxf::entities::DimensionBase,
        e: &dxf::entities::Entity,
        ctx: &Ctx,
    ) {
        if self.raw.is_some() {
            return;
        }
        let common = RawCommon {
            layer: e.common.layer.clone(),
            color: color_spec(&e.common.color, e.common.color_24_bit),
            visible: true,
        };
        self.dimension(&common, &base.block_name, ctx);
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(
        &mut self,
        kind: &'static str,
        corners: [&dxf::Point; 4],
        extrusion: Option<&dxf::Vector>,
        filled: bool,
        layer: u32,
        color: Color,
        ctx: &Ctx,
    ) {
        let mut pts: Vec<Vec2> = corners.iter().map(|p| p2(p)).collect();
        pts.dedup();
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 3 {
            return;
        }
        let geom = Geometry::Region {
            loops: vec![pts],
            filled,
        };
        let geom = match extrusion {
            Some(n) => geom.transformed(&ocs(n)),
            None => geom,
        };
        self.emit(kind, layer, color, geom, ctx);
    }
}

fn p2(p: &dxf::Point) -> Vec2 {
    vec2(p.x, p.y)
}

fn ocs(n: &dxf::Vector) -> Affine2 {
    ocs_to_wcs(n.x, n.y, n.z)
}

fn positive_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        fallback
    }
}

fn color_spec(c: &dxf::Color, true_color: i32) -> ColorSpec {
    if true_color != 0 {
        ColorSpec::True(true_color)
    } else if c.is_by_block() {
        ColorSpec::ByBlock
    } else if let Some(i) = c.index() {
        ColorSpec::Aci(i)
    } else {
        ColorSpec::ByLayer
    }
}

#[allow(clippy::too_many_arguments)]
fn single_line_text(
    location: &dxf::Point,
    second: &dxf::Point,
    height: f64,
    rotation_deg: f64,
    value: &str,
    h: HorizontalTextJustification,
    v: VerticalTextJustification,
    normal: &dxf::Vector,
) -> Option<Geometry> {
    let content = decode_text_codes(value);
    if content.trim().is_empty() {
        return None;
    }
    let (halign, mut valign) = match h {
        HorizontalTextJustification::Center => (HAlign::Center, VAlign::Baseline),
        HorizontalTextJustification::Right => (HAlign::Right, VAlign::Baseline),
        HorizontalTextJustification::Middle => (HAlign::Center, VAlign::Middle),
        // Left, plus Aligned/Fit, which are approximated as left-aligned.
        _ => (HAlign::Left, VAlign::Baseline),
    };
    match v {
        VerticalTextJustification::Bottom => valign = VAlign::Bottom,
        VerticalTextJustification::Middle => valign = VAlign::Middle,
        VerticalTextJustification::Top => valign = VAlign::Top,
        _ => {}
    }
    // With any non-default justification the anchor is the second point.
    let anchored = halign != HAlign::Left || valign != VAlign::Baseline;
    let pos = if anchored { p2(second) } else { p2(location) };
    Some(
        Geometry::Text(Text {
            pos,
            height: positive_or(height, 1.0),
            rotation: rotation_deg.to_radians(),
            content,
            halign,
            valign,
        })
        .transformed(&ocs(normal)),
    )
}

fn attachment(a: AttachmentPoint) -> (HAlign, VAlign) {
    let n = a as i32;
    let h = match (n - 1).rem_euclid(3) {
        0 => HAlign::Left,
        1 => HAlign::Center,
        _ => HAlign::Right,
    };
    let v = match (n - 1).div_euclid(3) {
        0 => VAlign::Top,
        1 => VAlign::Middle,
        _ => VAlign::Bottom,
    };
    (h, v)
}

/// Replace the `%%` control codes used by TEXT (and tolerated in MTEXT).
pub fn decode_text_codes(s: &str) -> String {
    if !s.contains("%%") {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && chars.get(i + 1) == Some(&'%') {
            if let Some(&c) = chars.get(i + 2) {
                match c.to_ascii_lowercase() {
                    'd' => out.push('°'),
                    'p' => out.push('±'),
                    'c' => out.push('⌀'),
                    '%' => out.push('%'),
                    // Underline / overline / strike-through toggles.
                    'u' | 'o' | 'k' => {}
                    d if d.is_ascii_digit() => {
                        // %%nnn: character by decimal code.
                        let digits: String = chars[i + 2..]
                            .iter()
                            .take(3)
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if let Some(ch) = digits.parse::<u32>().ok().and_then(char::from_u32) {
                            out.push(ch);
                        }
                        i += 2 + digits.len();
                        continue;
                    }
                    other => out.push(other),
                }
                i += 3;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Strip MTEXT inline formatting, keeping the readable text.
pub fn clean_mtext(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&code) = chars.get(i + 1) else { break };
                i += 2;
                match code {
                    'P' | 'X' => out.push('\n'),
                    'N' => out.push('\n'),
                    '~' => out.push(' '),
                    '\\' | '{' | '}' => out.push(code),
                    'L' | 'l' | 'O' | 'o' | 'K' | 'k' => {}
                    'U' if chars.get(i) == Some(&'+') => {
                        let hex: String = chars[i + 1..].iter().take(4).collect();
                        if let Some(ch) =
                            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                        i += 1 + hex.chars().count();
                    }
                    'S' => {
                        // Stacked text "a^b;" or "a/b;" -> "a/b".
                        while i < chars.len() && chars[i] != ';' {
                            match chars[i] {
                                '^' | '#' => out.push('/'),
                                ch => out.push(ch),
                            }
                            i += 1;
                        }
                        i += 1;
                    }
                    // Codes with an argument terminated by ';'.
                    'f' | 'F' | 'H' | 'C' | 'c' | 'T' | 'Q' | 'W' | 'A' | 'p' => {
                        while i < chars.len() && chars[i] != ';' {
                            i += 1;
                        }
                        i += 1;
                    }
                    other => out.push(other),
                }
            }
            '{' | '}' => i += 1,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    decode_text_codes(&out)
}

fn geom_is_finite(g: &Geometry) -> bool {
    match g {
        Geometry::Line { a, b } => a.is_finite() && b.is_finite(),
        Geometry::Polyline { vertices, .. } => vertices
            .iter()
            .all(|v| v.pos.is_finite() && v.bulge.is_finite()),
        Geometry::Arc(a) => {
            a.center.is_finite()
                && a.major.is_finite()
                && a.minor.is_finite()
                && a.start.is_finite()
                && a.sweep.is_finite()
        }
        // Splines are sanitised when flattened.
        Geometry::Spline(_) => true,
        Geometry::Point(p) => p.is_finite(),
        Geometry::Text(t) => t.pos.is_finite() && t.height.is_finite() && t.rotation.is_finite(),
        Geometry::Region { loops, .. } => loops.iter().flatten().all(|p| p.is_finite()),
    }
}

fn header_info(d: &dxf::Drawing) -> HeaderInfo {
    let code = d.header.version.to_string();
    let release = match code.as_str() {
        "AC1006" => "R10",
        "AC1009" => "R12",
        "AC1012" => "R13",
        "AC1014" => "R14",
        "AC1015" => "R2000",
        "AC1018" => "R2004",
        "AC1021" => "R2007",
        "AC1024" => "R2010",
        "AC1027" => "R2013",
        "AC1032" => "R2018",
        _ => "",
    };
    let version = if release.is_empty() {
        code
    } else {
        format!("{code} ({release})")
    };
    let units_code = d.header.default_drawing_units as i32;
    let (min, max) = (
        &d.header.minimum_drawing_extents,
        &d.header.maximum_drawing_extents,
    );
    let plausible = |v: f64| v.is_finite() && v.abs() < 1e19;
    let header_extents = (plausible(min.x)
        && plausible(min.y)
        && plausible(max.x)
        && plausible(max.y)
        && min.x <= max.x
        && min.y <= max.y
        && (min.x != max.x || min.y != max.y))
        .then(|| crate::geom::BBox::from_corners(p2(min), p2(max)));
    HeaderInfo {
        version,
        units_code,
        units: units_name(units_code).to_string(),
        header_extents,
    }
}

/// DXF type name for the `dxf` crate variants we may meet. Used only when the
/// raw scan is unavailable.
fn variant_name(e: &EntityType) -> &'static str {
    match e {
        EntityType::Line(_) => "LINE",
        EntityType::Circle(_) => "CIRCLE",
        EntityType::Arc(_) => "ARC",
        EntityType::Ellipse(_) => "ELLIPSE",
        EntityType::LwPolyline(_) => "LWPOLYLINE",
        EntityType::Polyline(_) => "POLYLINE",
        EntityType::Spline(_) => "SPLINE",
        EntityType::ModelPoint(_) => "POINT",
        EntityType::Text(_) => "TEXT",
        EntityType::MText(_) => "MTEXT",
        EntityType::Insert(_) => "INSERT",
        EntityType::Solid(_) => "SOLID",
        EntityType::Trace(_) => "TRACE",
        EntityType::Face3D(_) => "3DFACE",
        EntityType::Leader(_) => "LEADER",
        EntityType::RotatedDimension(_)
        | EntityType::RadialDimension(_)
        | EntityType::DiameterDimension(_)
        | EntityType::AngularThreePointDimension(_)
        | EntityType::OrdinateDimension(_) => "DIMENSION",
        EntityType::Attribute(_) => "ATTRIB",
        EntityType::AttributeDefinition(_) => "ATTDEF",
        EntityType::Vertex(_) => "VERTEX",
        EntityType::Seqend(_) => "SEQEND",
        EntityType::Image(_) => "IMAGE",
        EntityType::Wipeout(_) => "WIPEOUT",
        EntityType::Ray(_) => "RAY",
        EntityType::XLine(_) => "XLINE",
        EntityType::MLine(_) => "MLINE",
        EntityType::Solid3D(_) => "3DSOLID",
        EntityType::Region(_) => "REGION",
        EntityType::Body(_) => "BODY",
        EntityType::Shape(_) => "SHAPE",
        EntityType::Tolerance(_) => "TOLERANCE",
        EntityType::Helix(_) => "HELIX",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_codes() {
        assert_eq!(decode_text_codes("45%%d"), "45°");
        assert_eq!(decode_text_codes("%%p0.1 %%c20"), "±0.1 ⌀20");
        assert_eq!(decode_text_codes("%%uTitle%%u"), "Title");
        assert_eq!(decode_text_codes("100%%%"), "100%");
        assert_eq!(decode_text_codes("plain"), "plain");
        assert_eq!(decode_text_codes("%%"), "%%");
        assert_eq!(decode_text_codes("%%065"), "A");
    }

    #[test]
    fn mtext_formatting_is_stripped() {
        assert_eq!(clean_mtext(r"Line one\PLine two"), "Line one\nLine two");
        assert_eq!(
            clean_mtext(r"{\fArial|b0|i0|c0|p34;Hello} {\H2.5x;\C1;World}"),
            "Hello World"
        );
        assert_eq!(clean_mtext(r"\A1;\LUnder\l done"), "Under done");
        assert_eq!(clean_mtext(r"1\S1^2; in"), "11/2 in");
        assert_eq!(clean_mtext(r"a\\b \{c\}"), r"a\b {c}");
        assert_eq!(clean_mtext(r"\U+00B0 and 90%%d"), "° and 90°");
        assert_eq!(clean_mtext("trailing\\"), "trailing");
    }

    #[test]
    fn attachment_points() {
        assert_eq!(
            attachment(AttachmentPoint::TopLeft),
            (HAlign::Left, VAlign::Top)
        );
        assert_eq!(
            attachment(AttachmentPoint::MiddleCenter),
            (HAlign::Center, VAlign::Middle)
        );
        assert_eq!(
            attachment(AttachmentPoint::BottomRight),
            (HAlign::Right, VAlign::Bottom)
        );
    }

    #[test]
    fn empty_and_garbage_input_is_an_error() {
        assert!(matches!(load_bytes(b"", None), Err(LoadError::Parse(_))));
        assert!(matches!(
            load_bytes(b"  \n ", None),
            Err(LoadError::Parse(_))
        ));
        assert!(load_bytes(b"this is not a dxf file\nat all\n", None).is_err());
        assert!(load_bytes(&[0xff, 0xfe, 0x00, 0x01, 0x02, 0x80, 0x90], None).is_err());
    }

    #[test]
    fn missing_file_is_an_io_error() {
        assert!(matches!(
            load_file("/definitely/not/here.dxf"),
            Err(LoadError::Io { .. })
        ));
    }
}
