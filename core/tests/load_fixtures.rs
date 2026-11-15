// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Loads every generated fixture and checks the resulting model.

use rustydxf_core::aci::Color;
use rustydxf_core::geom::{Vec2, vec2};
use rustydxf_core::model::{Drawing, Geometry};
use rustydxf_core::{fixtures, load_file};
use std::path::PathBuf;
use std::sync::Once;

fn fixture(name: &str) -> PathBuf {
    static GENERATE: Once = Once::new();
    let dir = fixtures::default_dir();
    GENERATE.call_once(|| fixtures::write_all(&dir).expect("generate fixtures"));
    dir.join(name)
}

fn load(name: &str) -> Drawing {
    load_file(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn pclose(a: Vec2, b: Vec2) -> bool {
    a.distance(b) < 1e-6
}

fn layer_length(d: &Drawing, layer: &str) -> f64 {
    let id = d.layer_index(layer).expect("layer exists");
    d.entities
        .iter()
        .filter(|e| e.layer == id)
        .filter_map(|e| e.geom.length())
        .sum()
}

#[test]
fn lines() {
    let d = load("lines.dxf");
    assert_eq!(d.entities.len(), 6);
    assert_eq!(d.source_counts["LINE"], 6);
    assert_eq!(d.header.units, "Millimeters");
    assert_eq!(d.header.units_code, 4);
    assert!(
        d.header.version.starts_with("AC1018"),
        "{}",
        d.header.version
    );
    let b = d.bbox.unwrap();
    assert!(pclose(b.min, vec2(0.0, 0.0)) && pclose(b.max, vec2(100.0, 50.0)));
    assert!(close(layer_length(&d, "OUTLINE"), 300.0));

    let outline = &d.layers[d.layer_index("OUTLINE").unwrap() as usize];
    assert_eq!(outline.color, Color::rgb(255, 0, 0));
    assert_eq!(outline.entity_count, 4);
    assert!(outline.on && !outline.frozen);

    // BYLAYER, explicit ACI and true color.
    assert_eq!(d.entities[0].color, Color::rgb(255, 0, 0));
    assert_eq!(d.entities[4].color, Color::rgb(0, 0, 255));
    assert_eq!(d.entities[5].color, Color::rgb(0x12, 0x34, 0x56));
    assert!(d.unsupported.is_empty());
}

#[test]
fn binary_matches_ascii() {
    let a = load("lines.dxf");
    let b = load("binary.dxf");
    assert_eq!(a.entities.len(), b.entities.len());
    assert_eq!(a.bbox, b.bbox);
    assert_eq!(a.source_counts, b.source_counts);
    assert_eq!(a.layers.len(), b.layers.len());
    assert_eq!(a.header.version, b.header.version);
}

#[test]
fn corrupt_file_is_an_error_not_a_panic() {
    let err = load_file(fixture("corrupt.dxf")).expect_err("corrupt file must not load");
    let msg = err.to_string();
    assert!(msg.contains("not a valid DXF"), "{msg}");
}

#[test]
fn arcs() {
    let d = load("arcs.dxf");
    assert_eq!(d.entities.len(), 5);
    let arc = |i: usize| match &d.entities[i].geom {
        Geometry::Arc(a) => *a,
        g => panic!("expected arc, got {g:?}"),
    };
    assert_eq!(arc(0).radius(), Some(10.0));
    assert!(arc(0).is_closed());
    assert!(close(arc(1).length(), 20.0 * std::f64::consts::FRAC_PI_2));
    assert!(pclose(arc(1).start_point(), vec2(20.0, 0.0)));
    assert!(pclose(arc(1).end_point(), vec2(0.0, 20.0)));
    // 315 -> 45 degrees passes through angle 0.
    assert!(close(arc(2).bbox().max.x, 55.0));
    // Mirrored by its extrusion direction.
    let m = arc(3);
    assert!(pclose(m.center, vec2(-30.0, 0.0)));
    assert!(pclose(m.start_point(), vec2(-35.0, 0.0)));
    assert!(pclose(m.end_point(), vec2(-30.0, 5.0)));
    // Ellipse.
    let e = arc(4);
    assert_eq!(e.radius(), None);
    let b = e.bbox();
    assert!(pclose(b.min, vec2(-30.0, 45.0)) && pclose(b.max, vec2(30.0, 75.0)));
    assert_eq!(d.entities[4].kind, "ELLIPSE");
}

#[test]
fn polylines() {
    let d = load("polylines.dxf");
    assert_eq!(d.entities.len(), 3);
    let len: Vec<f64> = d
        .entities
        .iter()
        .map(|e| e.geom.length().unwrap())
        .collect();
    assert!(close(len[0], 20.0 + 2.0 * std::f64::consts::PI));
    assert!(close(len[1], 15.0));
    assert!(close(len[2], 12.0));
    assert_eq!(d.entities[2].kind, "POLYLINE");
    // Bulges extend the bounding box beyond the vertices.
    let b = d.entities[0].geom.bbox().unwrap();
    assert!(close(b.min.x, -1.0) && close(b.max.x, 11.0));
}

#[test]
fn nested_blocks() {
    let d = load("blocks.dxf");
    assert_eq!(d.source_counts["INSERT"], 3);
    assert_eq!(d.source_counts["DIMENSION"], 1);

    let parts = d.layer_index("PARTS").unwrap();
    let detail = d.layer_index("DETAIL").unwrap();
    let red = Color::rgb(255, 0, 0);

    // OUTER at (100,200) scale 10, base point (1,1); first INNER at block
    // (1,1) => world (100,200).
    let lines: Vec<(Vec2, Vec2, u32)> = d
        .entities
        .iter()
        .filter_map(|e| match &e.geom {
            Geometry::Line { a, b } if e.from_block => Some((*a, *b, e.layer)),
            _ => None,
        })
        .collect();
    assert!(
        lines.iter().any(|(a, b, l)| pclose(*a, vec2(100.0, 200.0))
            && pclose(*b, vec2(110.0, 200.0))
            && *l == parts),
        "first INNER x segment: {lines:?}"
    );
    // The DETAIL segment keeps its own layer.
    assert!(lines.iter().any(|(a, b, l)| pclose(*a, vec2(100.0, 200.0))
        && pclose(*b, vec2(100.0, 210.0))
        && *l == detail));
    // Second INNER: at block (5,1), scale 2, rotated 90 degrees. Its unit X
    // segment becomes (0,0)->(0,2) in OUTER, i.e. world (140,200)->(140,220).
    assert!(lines.iter().any(|(a, b, l)| pclose(*a, vec2(140.0, 200.0))
        && pclose(*b, vec2(140.0, 220.0))
        && *l == parts));

    // BYBLOCK circles take the insert's color (ACI 1), radius scaled.
    let circles: Vec<_> = d.entities.iter().filter(|e| e.kind == "CIRCLE").collect();
    assert_eq!(circles.len(), 2);
    for c in &circles {
        assert_eq!(c.color, red);
        assert_eq!(c.layer, parts);
    }
    let radii: Vec<f64> = circles
        .iter()
        .map(|c| match &c.geom {
            Geometry::Arc(a) => a.radius().unwrap(),
            _ => unreachable!(),
        })
        .collect();
    assert!(close(radii[0], 5.0) && close(radii[1], 10.0));

    // The self-referencing block is expanded once and then cut off.
    assert!(
        d.warnings.iter().any(|w| w.contains("LOOP")),
        "{:?}",
        d.warnings
    );
    assert_eq!(d.unsupported.get("INSERT (missing block)"), Some(&1));

    // The dimension's anonymous block is drawn on the dimension's layer.
    let dim_text = d
        .entities
        .iter()
        .find(|e| matches!(&e.geom, Geometry::Text(t) if t.content == "40"))
        .expect("dimension text");
    assert_eq!(dim_text.layer, detail);
    assert!(dim_text.from_block);
}

#[test]
fn text() {
    let d = load("text.dxf");
    let texts: Vec<_> = d
        .entities
        .iter()
        .filter_map(|e| match &e.geom {
            Geometry::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 3);
    assert_eq!(texts[0].content, "RustyDxf sample text");
    assert!(pclose(texts[0].pos, vec2(10.0, 10.0)) && close(texts[0].height, 5.0));
    assert_eq!(texts[1].content, "ROTATED 90°");
    assert!(pclose(texts[1].pos, vec2(50.0, 40.0)));
    assert!(close(texts[1].rotation, std::f64::consts::FRAC_PI_2));
    assert_eq!(texts[2].content, "General Notes\nAll dimensions in mm");
    assert_eq!(d.entities[2].kind, "MTEXT");
    // ACI 7 adapts to the background.
    assert!(d.entities[0].color.adaptive);
}

#[test]
fn hatch_and_frozen_layer() {
    let d = load("hatch.dxf");
    assert_eq!(d.source_counts["HATCH"], 2);
    let hatches: Vec<_> = d.entities.iter().filter(|e| e.kind == "HATCH").collect();
    assert_eq!(hatches.len(), 2);

    let Geometry::Region { loops, filled } = &hatches[0].geom else {
        panic!("expected region")
    };
    assert!(*filled);
    assert_eq!(loops.len(), 2);
    assert_eq!(loops[0].len(), 4);
    assert!(loops[1].len() > 32, "circle edge is flattened");
    assert!(
        loops[1]
            .iter()
            .all(|p| close(p.distance(vec2(10.0, 10.0)), 5.0))
    );
    assert_eq!(hatches[0].color, Color::rgb(0, 0, 255));

    let Geometry::Region { loops, filled } = &hatches[1].geom else {
        panic!("expected region")
    };
    assert!(!*filled, "pattern hatches are outline only");
    assert_eq!(loops.len(), 1);
    assert_eq!(hatches[1].color, Color::rgb(255, 255, 0));

    let frozen = &d.layers[d.layer_index("FROZEN").unwrap() as usize];
    assert!(frozen.frozen && !frozen.visible_by_default());
    let fill = &d.layers[d.layer_index("FILL").unwrap() as usize];
    assert!(!fill.frozen && fill.visible_by_default());
    assert_eq!(fill.entity_count, 2);
    assert!(d.unsupported.is_empty(), "{:?}", d.unsupported);
}

#[test]
fn misc_entities_and_unsupported_counts() {
    let d = load("misc.dxf");
    let kinds: Vec<&str> = d.entities.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, ["SPLINE", "POINT", "SOLID", "LINE"]);
    assert_eq!(d.unsupported.get("RAY"), Some(&2));
    assert_eq!(d.unsupported_total(), 2);

    let hidden = &d.layers[d.layer_index("HIDDEN").unwrap() as usize];
    assert!(!hidden.on && !hidden.visible_by_default());

    // SOLID corners are reordered into a proper quad.
    let Geometry::Region { loops, filled } = &d.entities[2].geom else {
        panic!("expected region")
    };
    assert!(*filled);
    assert_eq!(
        loops[0],
        vec![
            vec2(60.0, 0.0),
            vec2(70.0, 0.0),
            vec2(70.0, 10.0),
            vec2(60.0, 10.0)
        ]
    );
    // Spline endpoints are interpolated (clamped knots).
    let len = d.entities[0].geom.length().unwrap();
    assert!(len > 40.0 && len < 80.0, "{len}");
}

#[test]
fn every_good_fixture_loads() {
    for name in fixtures::FILES {
        if *name == "corrupt.dxf" {
            continue;
        }
        let d = load(name);
        assert!(!d.entities.is_empty(), "{name}");
        assert!(d.bbox.is_some(), "{name}");
    }
}
