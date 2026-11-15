// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Test fixture generator. Writes a small set of DXF files (built with the
//! `dxf` crate) that the unit and integration tests load.
//!
//! The `dxf` crate cannot write HATCH entities or layer flags, so those are
//! spliced into the ASCII output as raw group codes.

use dxf::entities::{
    Arc, Circle, Ellipse, Entity, EntityType, Insert, Line, LwPolyline, MText, ModelPoint,
    Polyline, Ray, RotatedDimension, Solid, Spline, Text, Vertex,
};
use dxf::enums::{AcadVersion, AttachmentPoint, HorizontalTextJustification, Units};
use dxf::tables::Layer;
use dxf::{Block, Color, Drawing, LwPolylineVertex, Point, Vector};
use std::io;
use std::path::{Path, PathBuf};

/// Names of the generated files.
pub const FILES: &[&str] = &[
    "lines.dxf",
    "arcs.dxf",
    "polylines.dxf",
    "blocks.dxf",
    "text.dxf",
    "hatch.dxf",
    "misc.dxf",
    "binary.dxf",
    "corrupt.dxf",
];

/// `<workspace>/tests/fixtures`.
pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .join("tests")
        .join("fixtures")
}

fn io_err(e: dxf::DxfError) -> io::Error {
    io::Error::other(e.to_string())
}

fn new_drawing() -> Drawing {
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = Units::Millimeters;
    // Fixed dates (the Unix epoch) so regenerating does not churn the files.
    d.header.creation_date = Default::default();
    d.header.creation_date_universal = Default::default();
    d.header.update_date = Default::default();
    d.header.update_date_universal = Default::default();
    d.header.fingerprint_guid = Default::default();
    d.header.version_guid = Default::default();
    d
}

fn layer(d: &mut Drawing, name: &str, aci: u8) {
    d.add_layer(Layer {
        name: name.to_string(),
        color: Color::from_index(aci),
        ..Default::default()
    });
}

fn ent(specific: EntityType, layer: &str) -> Entity {
    let mut e = Entity::new(specific);
    e.common.layer = layer.to_string();
    e
}

fn line(x1: f64, y1: f64, x2: f64, y2: f64) -> EntityType {
    EntityType::Line(Line::new(Point::new(x1, y1, 0.0), Point::new(x2, y2, 0.0)))
}

fn lines() -> Drawing {
    let mut d = new_drawing();
    // True color (group 420) needs R2004 or later.
    d.header.version = AcadVersion::R2004;
    layer(&mut d, "OUTLINE", 1);
    layer(&mut d, "CENTER", 3);
    // A 100 x 50 rectangle: total length 300.
    for (a, b) in [
        ((0.0, 0.0), (100.0, 0.0)),
        ((100.0, 0.0), (100.0, 50.0)),
        ((100.0, 50.0), (0.0, 50.0)),
        ((0.0, 50.0), (0.0, 0.0)),
    ] {
        d.add_entity(ent(line(a.0, a.1, b.0, b.1), "OUTLINE"));
    }
    // Centre line with an explicit color and a true-color diagonal.
    let mut c = ent(line(0.0, 25.0, 100.0, 25.0), "CENTER");
    c.common.color = Color::from_index(5);
    d.add_entity(c);
    let mut t = ent(line(0.0, 0.0, 100.0, 50.0), "CENTER");
    t.common.color_24_bit = 0x12_34_56;
    d.add_entity(t);
    d
}

fn arcs() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "ARCS", 4);
    d.add_entity(ent(
        EntityType::Circle(Circle::new(Point::new(0.0, 0.0, 0.0), 10.0)),
        "ARCS",
    ));
    // Quarter arc, radius 20, 0 -> 90 degrees.
    d.add_entity(ent(
        EntityType::Arc(Arc::new(Point::new(0.0, 0.0, 0.0), 20.0, 0.0, 90.0)),
        "ARCS",
    ));
    // Arc crossing 0 degrees: 315 -> 45.
    d.add_entity(ent(
        EntityType::Arc(Arc::new(Point::new(50.0, 0.0, 0.0), 5.0, 315.0, 45.0)),
        "ARCS",
    ));
    // Mirrored arc (extrusion 0,0,-1): OCS centre (30,0) is world (-30,0).
    let mut flipped = Arc::new(Point::new(30.0, 0.0, 0.0), 5.0, 0.0, 90.0);
    flipped.normal = Vector::new(0.0, 0.0, -1.0);
    d.add_entity(ent(EntityType::Arc(flipped), "ARCS"));
    // Full ellipse, semi-axes 30 x 15.
    d.add_entity(ent(
        EntityType::Ellipse(Ellipse {
            center: Point::new(0.0, 60.0, 0.0),
            major_axis: Vector::new(30.0, 0.0, 0.0),
            minor_axis_ratio: 0.5,
            ..Default::default()
        }),
        "ARCS",
    ));
    d
}

fn lw_vertex(x: f64, y: f64, bulge: f64) -> LwPolylineVertex {
    LwPolylineVertex {
        x,
        y,
        bulge,
        ..Default::default()
    }
}

fn polylines() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "PLINES", 2);
    // Closed "stadium": two sides of 10 and two semicircles of radius 1.
    let mut stadium = LwPolyline {
        vertices: vec![
            lw_vertex(0.0, 0.0, 0.0),
            lw_vertex(10.0, 0.0, 1.0),
            lw_vertex(10.0, 2.0, 0.0),
            lw_vertex(0.0, 2.0, 1.0),
        ],
        ..Default::default()
    };
    stadium.set_is_closed(true);
    d.add_entity(ent(EntityType::LwPolyline(stadium), "PLINES"));
    // Open zig-zag: 3 segments of length 5.
    d.add_entity(ent(
        EntityType::LwPolyline(LwPolyline {
            vertices: vec![
                lw_vertex(0.0, 10.0, 0.0),
                lw_vertex(3.0, 14.0, 0.0),
                lw_vertex(6.0, 10.0, 0.0),
                lw_vertex(9.0, 14.0, 0.0),
            ],
            ..Default::default()
        }),
        "PLINES",
    ));
    // Old-style 2D POLYLINE triangle (3-4-5), closed.
    let mut old = Polyline::default();
    old.set_is_closed(true);
    for (x, y) in [(20.0, 0.0), (23.0, 0.0), (23.0, 4.0)] {
        old.add_vertex(&mut d, Vertex::new(Point::new(x, y, 0.0)));
    }
    d.add_entity(ent(EntityType::Polyline(old), "PLINES"));
    d
}

fn insert(name: &str, x: f64, y: f64, scale: f64, rotation: f64) -> EntityType {
    EntityType::Insert(Insert {
        name: name.to_string(),
        location: Point::new(x, y, 0.0),
        x_scale_factor: scale,
        y_scale_factor: scale,
        z_scale_factor: scale,
        rotation,
        ..Default::default()
    })
}

fn blocks() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "PARTS", 6);
    layer(&mut d, "DETAIL", 3);

    // INNER: a unit segment on layer 0 (inherits the insert's layer), a
    // BYBLOCK circle and a segment that stays on its own layer.
    let mut by_block = ent(
        EntityType::Circle(Circle::new(Point::new(0.0, 0.0, 0.0), 0.5)),
        "0",
    );
    by_block.common.color = Color::by_block();
    d.add_block(Block {
        name: "INNER".into(),
        entities: vec![
            ent(line(0.0, 0.0, 1.0, 0.0), "0"),
            by_block,
            ent(line(0.0, 0.0, 0.0, 1.0), "DETAIL"),
        ],
        ..Default::default()
    });

    // OUTER: INNER twice, the second scaled x2 and rotated 90 degrees, with
    // a base point of (1, 1). The nested inserts are BYBLOCK so the color of
    // the top-level insert passes all the way down.
    let nested = |x: f64, scale: f64, rotation: f64| {
        let mut e = ent(insert("INNER", x, 1.0, scale, rotation), "0");
        e.common.color = Color::by_block();
        e
    };
    d.add_block(Block {
        name: "OUTER".into(),
        base_point: Point::new(1.0, 1.0, 0.0),
        entities: vec![nested(1.0, 1.0, 0.0), nested(5.0, 2.0, 90.0)],
        ..Default::default()
    });

    // LOOP references itself: must be cut off by the recursion guard.
    d.add_block(Block {
        name: "LOOP".into(),
        entities: vec![
            ent(line(0.0, 0.0, 1.0, 1.0), "0"),
            ent(insert("LOOP", 1.0, 1.0, 1.0, 0.0), "0"),
        ],
        ..Default::default()
    });

    let mut outer = ent(insert("OUTER", 100.0, 200.0, 10.0, 0.0), "PARTS");
    outer.common.color = Color::from_index(1);
    d.add_entity(outer);
    d.add_entity(ent(insert("LOOP", -50.0, -50.0, 1.0, 0.0), "PARTS"));
    d.add_entity(ent(insert("MISSING", 0.0, 0.0, 1.0, 0.0), "PARTS"));

    // A dimension whose graphics live in an anonymous block.
    d.add_block(Block {
        name: "*D1".into(),
        entities: vec![
            ent(line(0.0, -10.0, 40.0, -10.0), "0"),
            ent(
                EntityType::Text(Text {
                    location: Point::new(15.0, -9.0, 0.0),
                    text_height: 2.5,
                    value: "40".into(),
                    ..Default::default()
                }),
                "0",
            ),
        ],
        ..Default::default()
    });
    let mut dim = RotatedDimension::default();
    dim.dimension_base.block_name = "*D1".into();
    d.add_entity(ent(EntityType::RotatedDimension(dim), "DETAIL"));
    d
}

fn text() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "NOTES", 7);
    d.add_entity(ent(
        EntityType::Text(Text {
            location: Point::new(10.0, 10.0, 0.0),
            text_height: 5.0,
            value: "RustyDxf sample text".into(),
            ..Default::default()
        }),
        "NOTES",
    ));
    d.add_entity(ent(
        EntityType::Text(Text {
            location: Point::new(0.0, 0.0, 0.0),
            second_alignment_point: Point::new(50.0, 40.0, 0.0),
            text_height: 3.0,
            rotation: 90.0,
            value: "ROTATED 90%%d".into(),
            horizontal_text_justification: HorizontalTextJustification::Center,
            ..Default::default()
        }),
        "NOTES",
    ));
    d.add_entity(ent(
        EntityType::MText(MText {
            insertion_point: Point::new(10.0, 80.0, 0.0),
            initial_text_height: 4.0,
            attachment_point: AttachmentPoint::TopLeft,
            text: r"{\fArial|b1;General Notes}\PAll dimensions in mm".into(),
            ..Default::default()
        }),
        "NOTES",
    ));
    d
}

fn misc() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "MISC", 30);
    layer(&mut d, "HIDDEN", 8);
    for l in d.layers_mut() {
        if l.name == "HIDDEN" {
            l.is_layer_on = false;
        }
    }
    // Clamped cubic B-spline.
    d.add_entity(ent(
        EntityType::Spline(Spline {
            degree_of_curve: 3,
            control_points: vec![
                Point::new(0.0, 0.0, 0.0),
                Point::new(10.0, 20.0, 0.0),
                Point::new(30.0, 20.0, 0.0),
                Point::new(40.0, 0.0, 0.0),
            ],
            knot_values: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            ..Default::default()
        }),
        "MISC",
    ));
    d.add_entity(ent(
        EntityType::ModelPoint(ModelPoint::new(Point::new(50.0, 50.0, 0.0))),
        "MISC",
    ));
    d.add_entity(ent(
        EntityType::Solid(Solid {
            first_corner: Point::new(60.0, 0.0, 0.0),
            second_corner: Point::new(70.0, 0.0, 0.0),
            third_corner: Point::new(60.0, 10.0, 0.0),
            fourth_corner: Point::new(70.0, 10.0, 0.0),
            ..Default::default()
        }),
        "MISC",
    ));
    d.add_entity(ent(line(0.0, -20.0, 10.0, -20.0), "HIDDEN"));
    // Not supported by the viewer: must be counted, not drawn.
    d.add_entity(ent(EntityType::Ray(Ray::default()), "MISC"));
    d.add_entity(ent(EntityType::Ray(Ray::default()), "MISC"));
    d
}

fn hatch_base() -> Drawing {
    let mut d = new_drawing();
    layer(&mut d, "FILL", 5);
    layer(&mut d, "FROZEN", 1);
    d.add_entity(ent(line(0.0, 0.0, 40.0, 0.0), "0"));
    d.add_entity(ent(line(0.0, 100.0, 5.0, 100.0), "FROZEN"));
    d
}

/// Raw group codes for two hatches: a solid square with a round hole on
/// layer FILL, and a pattern-filled triangle.
const HATCH_CODES: &[(i32, &str)] = &[
    (0, "HATCH"),
    (5, "F001"),
    (100, "AcDbEntity"),
    (8, "FILL"),
    (100, "AcDbHatch"),
    (10, "0.0"),
    (20, "0.0"),
    (30, "0.0"),
    (210, "0.0"),
    (220, "0.0"),
    (230, "1.0"),
    (2, "SOLID"),
    (70, "1"),
    (71, "0"),
    (91, "2"),
    // Loop 1: polyline square 0..20.
    (92, "2"),
    (72, "0"),
    (73, "1"),
    (93, "4"),
    (10, "0.0"),
    (20, "0.0"),
    (10, "20.0"),
    (20, "0.0"),
    (10, "20.0"),
    (20, "20.0"),
    (10, "0.0"),
    (20, "20.0"),
    (97, "0"),
    // Loop 2: a full circle of radius 5 as a single arc edge.
    (92, "1"),
    (93, "1"),
    (72, "2"),
    (10, "10.0"),
    (20, "10.0"),
    (40, "5.0"),
    (50, "0.0"),
    (51, "360.0"),
    (73, "1"),
    (97, "0"),
    (75, "0"),
    (76, "1"),
    (98, "0"),
    (0, "HATCH"),
    (5, "F002"),
    (100, "AcDbEntity"),
    (8, "FILL"),
    (62, "2"),
    (100, "AcDbHatch"),
    (10, "0.0"),
    (20, "0.0"),
    (30, "0.0"),
    (210, "0.0"),
    (220, "0.0"),
    (230, "1.0"),
    (2, "ANSI31"),
    (70, "0"),
    (71, "0"),
    (91, "1"),
    (92, "1"),
    (93, "3"),
    (72, "1"),
    (10, "30.0"),
    (20, "0.0"),
    (11, "40.0"),
    (21, "0.0"),
    (72, "1"),
    (10, "40.0"),
    (20, "0.0"),
    (11, "30.0"),
    (21, "10.0"),
    (72, "1"),
    (10, "30.0"),
    (20, "10.0"),
    (11, "30.0"),
    (21, "0.0"),
    (97, "0"),
    (75, "0"),
    (76, "1"),
    (52, "0.0"),
    (41, "1.0"),
    (77, "0"),
    (78, "0"),
    (98, "0"),
];

fn to_ascii(d: &Drawing) -> io::Result<String> {
    let mut buf = Vec::new();
    d.save(&mut buf).map_err(io_err)?;
    String::from_utf8(buf).map_err(io::Error::other)
}

/// Splice HATCH entities into the ENTITIES section and set the frozen flag
/// on the layer named FROZEN.
fn hatch_text() -> io::Result<String> {
    let text = to_ascii(&hatch_base())?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    // Layer flag: the 70 that follows "2 / FROZEN" inside the LAYER table.
    let name_at = lines
        .windows(2)
        .position(|w| w[0].trim() == "2" && w[1].trim() == "FROZEN")
        .ok_or_else(|| io::Error::other("FROZEN layer not found in fixture"))?;
    let flag_at = (name_at + 2..lines.len())
        .step_by(2)
        .take_while(|&i| lines[i].trim() != "0")
        .find(|&i| lines[i].trim() == "70")
        .ok_or_else(|| io::Error::other("layer flags not found in fixture"))?;
    lines[flag_at + 1] = "     1".to_string();

    // Insert before the ENDSEC that closes ENTITIES.
    let section = lines
        .windows(2)
        .position(|w| w[0].trim() == "2" && w[1].trim() == "ENTITIES")
        .ok_or_else(|| io::Error::other("ENTITIES section not found in fixture"))?;
    let end = (section + 2..lines.len())
        .step_by(2)
        .find(|&i| lines[i].trim() == "0" && lines[i + 1].trim() == "ENDSEC")
        .ok_or_else(|| io::Error::other("ENTITIES end not found in fixture"))?;
    let extra: Vec<String> = HATCH_CODES
        .iter()
        .flat_map(|(code, value)| [format!("{code:>3}"), value.to_string()])
        .collect();
    lines.splice(end..end, extra);

    let mut out = lines.join(newline);
    out.push_str(newline);
    Ok(out)
}

/// Write every fixture into `dir`, creating it if needed.
pub fn write_all(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let save = |name: &str, d: &Drawing| d.save_file(dir.join(name)).map_err(io_err);
    save("lines.dxf", &lines())?;
    save("arcs.dxf", &arcs())?;
    save("polylines.dxf", &polylines())?;
    save("blocks.dxf", &blocks())?;
    save("text.dxf", &text())?;
    save("misc.dxf", &misc())?;
    std::fs::write(dir.join("hatch.dxf"), hatch_text()?)?;
    lines()
        .save_file_binary(dir.join("binary.dxf"))
        .map_err(io_err)?;

    // Corrupt: a valid start that turns into garbage part-way through.
    let good = to_ascii(&lines())?;
    let mut bad = good.as_bytes()[..good.len() * 2 / 5].to_vec();
    bad.extend_from_slice(
        b"\r\nthis is not a group code\r\n\x00\xff\xfe###\r\nLINE\r\n 10\r\nnot-a-number\r\n",
    );
    std::fs::write(dir.join("corrupt.dxf"), bad)?;
    Ok(())
}
