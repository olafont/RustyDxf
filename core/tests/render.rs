//! Offscreen rendering tests. They need a GPU adapter (hardware or software);
//! where none exists (some CI machines) they are skipped with a note, unless
//! `RUSTYDXF_REQUIRE_GPU=1` is set.

use rustydxf_core::geom::{BBox, vec2};
use rustydxf_core::render::{
    BACKGROUND_DARK, BACKGROUND_LIGHT, Image, RenderError, RenderOptions, headless,
};
use rustydxf_core::tessellate::{TessellateOptions, tessellate};
use rustydxf_core::{fixtures, load_file};
use std::sync::Once;

fn mesh(name: &str) -> rustydxf_core::tessellate::Mesh {
    static GENERATE: Once = Once::new();
    let dir = fixtures::default_dir();
    GENERATE.call_once(|| fixtures::write_all(&dir).expect("generate fixtures"));
    let drawing = load_file(dir.join(name)).expect("fixture loads");
    tessellate(
        &drawing,
        TessellateOptions {
            text_outlines: true,
        },
    )
}

/// Returns None (after printing why) when no adapter is available.
fn render(name: &str, opts: &RenderOptions) -> Option<Image> {
    let headless = match headless() {
        Ok(h) => h,
        Err(e) => {
            if std::env::var_os("RUSTYDXF_REQUIRE_GPU").is_some() {
                panic!("GPU required but unavailable: {e}");
            }
            eprintln!("skipping render test: {e}");
            return None;
        }
    };
    let image = headless.render(&mesh(name), opts).expect("render succeeds");
    if let Some(dir) = std::env::var_os("RUSTYDXF_RENDER_OUT") {
        let path = std::path::Path::new(&dir).join(format!(
            "{}-{}.png",
            name.trim_end_matches(".dxf"),
            if opts.dark_background {
                "dark"
            } else {
                "light"
            }
        ));
        std::fs::write(path, image.to_png().unwrap()).unwrap();
    }
    Some(image)
}

fn pixel(img: &Image, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
}

fn count(img: &Image, pred: impl Fn([u8; 3]) -> bool) -> usize {
    img.rgba
        .chunks(4)
        .filter(|p| pred([p[0], p[1], p[2]]))
        .count()
}

fn near(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

#[test]
fn lines_render_with_requested_size_and_colors() {
    let opts = RenderOptions {
        width: 800,
        height: 600,
        ..Default::default()
    };
    let Some(img) = render("lines.dxf", &opts) else {
        return;
    };
    assert_eq!((img.width, img.height), (800, 600));
    assert_eq!(img.rgba.len(), 800 * 600 * 4);
    // Corners are background.
    assert!(near(pixel(&img, 2, 2), BACKGROUND_DARK, 1));
    assert!(near(pixel(&img, 797, 597), BACKGROUND_DARK, 1));
    // The red outline, the blue centre line and the true-color diagonal.
    assert!(count(&img, |p| p[0] > 150 && p[1] < 90 && p[2] < 90) > 1000);
    assert!(count(&img, |p| p[2] > 150 && p[0] < 90 && p[1] < 90) > 300);
    // The 100x50 rectangle is centred: its left edge is at x = 32 (4% margin).
    // A row clear of the centre line and the diagonal's end points.
    let row = 200;
    let first_red = (0..800)
        .find(|&x| {
            let p = pixel(&img, x, row);
            p[0] > 150 && p[1] < 90
        })
        .expect("left edge");
    assert!((30..=34).contains(&first_red), "left edge at {first_red}");

    let png = img.to_png().unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn light_background_turns_aci7_black() {
    let dark = RenderOptions {
        width: 400,
        height: 300,
        ..Default::default()
    };
    let light = RenderOptions {
        dark_background: false,
        ..dark.clone()
    };
    let Some(d) = render("text.dxf", &dark) else {
        return;
    };
    let l = render("text.dxf", &light).unwrap();
    assert!(near(pixel(&l, 1, 1), BACKGROUND_LIGHT, 1));
    // Text outlines are white on dark and black on light.
    assert!(count(&d, |p| p[0] > 230 && p[1] > 230 && p[2] > 230) > 200);
    assert_eq!(
        count(&l, |p| p[0] > 230
            && p[1] > 230
            && p[2] > 230
            && p != [255, 255, 255]),
        0
    );
    assert!(count(&l, |p| p[0] < 40 && p[1] < 40 && p[2] < 40) > 200);
}

#[test]
fn hatch_fill_has_a_hole_and_layers_can_be_hidden() {
    let view = BBox::from_corners(vec2(0.0, 0.0), vec2(20.0, 20.0));
    let opts = RenderOptions {
        width: 400,
        height: 400,
        view: Some(view),
        ..Default::default()
    };
    let Some(img) = render("hatch.dxf", &opts) else {
        return;
    };
    let blue = |p: [u8; 3]| p[2] > 200 && p[0] < 60 && p[1] < 60;
    // Square 20x20 fills the frame; the r=5 hole is pi*25/400 of it.
    let filled = count(&img, blue) as f64 / (400.0 * 400.0);
    let expected = 1.0 - std::f64::consts::PI * 25.0 / 400.0;
    assert!(
        (filled - expected).abs() < 0.02,
        "filled {filled}, expected {expected}"
    );
    // Centre of the hole is background; a corner of the square is blue.
    assert!(near(pixel(&img, 200, 200), BACKGROUND_DARK, 1));
    assert!(blue(pixel(&img, 20, 20)));

    // Hide the FILL layer: no blue remains.
    let drawing = load_file(fixtures::default_dir().join("hatch.dxf")).unwrap();
    let fill = drawing.layer_index("FILL").unwrap() as usize;
    let mut visible = vec![true; drawing.layers.len()];
    visible[fill] = false;
    let hidden = render("hatch.dxf", &RenderOptions { visible, ..opts }).unwrap();
    assert_eq!(count(&hidden, blue), 0);
}

#[test]
fn every_fixture_renders() {
    for name in fixtures::FILES {
        if *name == "corrupt.dxf" {
            continue;
        }
        for dark_background in [true, false] {
            let opts = RenderOptions {
                width: 640,
                height: 480,
                dark_background,
                ..Default::default()
            };
            let Some(img) = render(name, &opts) else {
                return;
            };
            let bg = if dark_background {
                BACKGROUND_DARK
            } else {
                BACKGROUND_LIGHT
            };
            let drawn = count(&img, |p| !near(p, bg, 2));
            assert!(drawn > 50, "{name}: only {drawn} non-background pixels");
        }
    }
}

#[test]
fn bad_sizes_are_rejected() {
    let Ok(h) = headless() else { return };
    let m = mesh("lines.dxf");
    for (width, height) in [(0, 100), (100, 8), (20_000, 100)] {
        let r = h.render(
            &m,
            &RenderOptions {
                width,
                height,
                ..Default::default()
            },
        );
        assert!(
            matches!(r, Err(RenderError::BadSize(..))),
            "{width}x{height}"
        );
    }
}
