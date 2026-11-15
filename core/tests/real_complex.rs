// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Regression checks against `tests/fixtures/real/complex.dxf`, a drawing
//! written by ezdxf (see `tests/tools/make_complex.py`) rather than by the
//! `dxf` crate that RustyDxf reads with.

use rustydxf_core::geom::vec2;
use rustydxf_core::model::Geometry;
use rustydxf_core::{fixtures, load_file, query};

const OX: f64 = 512_000.0;
const OY: f64 = 4_180_000.0;

fn load() -> rustydxf_core::Drawing {
    let path = fixtures::default_dir().join("real").join("complex.dxf");
    load_file(path).expect("complex.dxf loads")
}

#[test]
fn counts_match_what_ezdxf_wrote() {
    let d = load();
    let count = |kind: &str| d.source_counts.get(kind).copied().unwrap_or(0);
    assert_eq!(count("INSERT"), 3034);
    assert_eq!(count("LWPOLYLINE"), 3031);
    assert_eq!(count("CIRCLE"), 3041);
    assert_eq!(count("ARC"), 3027);
    assert_eq!(count("HATCH"), 5);
    assert_eq!(count("DIMENSION"), 6);
    assert_eq!(count("SPLINE"), 4);
    assert_eq!(d.layers.len(), 17);
    // Not drawn, but counted.
    for kind in ["IMAGE", "MLINE", "MULTILEADER", "RAY", "SHAPE", "XLINE"] {
        assert_eq!(d.unsupported.get(kind).copied(), Some(1), "{kind}");
    }
    // The cyclic LOOP_A / LOOP_B blocks end in a warning, not a hang.
    assert!(
        d.warnings.iter().any(|w| w.contains("LOOP_")),
        "{:?}",
        d.warnings
    );
    // Paper-space entities stay out of the model view.
    assert!(query::find_text(&d, "PAPER SPACE").is_empty());
}

#[test]
fn layer_states_and_extents() {
    let d = load();
    let layer = |name: &str| d.layers.iter().find(|l| l.name == name).expect(name);
    assert!(!layer("LAYER_OFF").on);
    assert!(layer("LAYER_FROZEN").frozen);
    assert_eq!(layer("TRUECOLOR").color.rgb, [255, 128, 0]);
    assert!(d.layers.iter().any(|l| l.name == "Ünïcode Läyer 図面"));

    let b = d.bbox.expect("extents");
    assert!((b.min.y - (OY - 1700.0)).abs() < 1e-6, "{b:?}");
    assert!((b.max.x - (OX + 1190.0)).abs() < 1e-6, "{b:?}");
}

#[test]
fn hatch_with_a_spline_edge_keeps_its_other_edges() {
    let d = load();
    // The spline-edged hatch spans y = -160..-80 (spline on top, three
    // straight edges below); losing the edges after the spline cut it short.
    let found = d.entities.iter().any(|e| {
        e.kind == "HATCH"
            && e.geom.bbox().is_some_and(|b| {
                (b.min.x - (OX + 850.0)).abs() < 1e-6
                    && (b.min.y - (OY - 160.0)).abs() < 1e-6
                    && (b.max.x - (OX + 980.0)).abs() < 1e-6
            })
    });
    assert!(found, "spline-edge hatch is missing or truncated");
}

#[test]
fn gradient_hatch_uses_its_gradient_color() {
    let d = load();
    let gradient = d
        .entities
        .iter()
        .find(|e| {
            e.kind == "HATCH"
                && e.geom.bbox().is_some_and(|b| {
                    let p = vec2(OX + 1060.0, OY + 140.0);
                    b.min.x < p.x && p.x < b.max.x && b.min.y < p.y && p.y < b.max.y
                })
        })
        .expect("gradient hatch");
    assert_eq!(gradient.color.rgb, [255, 0, 0]);
}

#[test]
fn text_is_decoded_and_wrapped() {
    let d = load();
    // Matches across a word-wrap break.
    let hits = query::find_text(&d, "reference rectangle");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].text.lines().count() >= 7, "{}", hits[0].text);
    assert_eq!(query::find_text(&d, "Ø50").len(), 1);
    assert_eq!(query::find_text(&d, "ATTR-42").len(), 1);
    // Text on off / frozen layers is still in the model, just hidden.
    assert_eq!(query::find_text(&d, "must be hidden").len(), 2);
    let texts = d
        .entities
        .iter()
        .filter(|e| matches!(e.geom, Geometry::Text(_)))
        .count();
    assert!(texts > 15, "{texts}");
}

#[test]
fn measured_lengths_match_the_geometry() {
    let d = load();
    let holes = query::EntityFilter {
        layer: Some("HOLES".into()),
        kind: Some("CIRCLE".into()),
    };
    let expected = std::f64::consts::TAU * (60.0 + 12.0 * 8.0);
    let total = query::total_length(&d, &holes).unwrap().total_length;
    assert!((total - expected).abs() < 1e-6, "{total} vs {expected}");
}
