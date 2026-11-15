// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Builds every RustyDxf icon file from the SVG sources.
//!
//! The artwork is a rusted, laser-cut "D" on a steel plate:
//!
//! * `assets/icon.svg`: the app icon;
//! * `assets/src/icon-small.svg`: a simplified drawing for 16 to 48 px;
//! * `assets/src/icon-maskable.svg`: full-bleed, for maskable and touch icons.
//!
//! The SVGs use filters for the rust and brushed steel, so they are rendered
//! with headless Chrome (set `CHROME` to point at another Chromium binary),
//! then scaled and packed here into PNG, ICNS, ICO and the web icon set.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MASTER: usize = 1024;

type Rgba = [f32; 4];

const DEFAULT_CHROME: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chrome",
];

/// Render an SVG to a 1024 px transparent PNG with headless Chrome and load it.
fn render_svg(svg: &Path, scratch: &Path) -> Result<Vec<Rgba>> {
    let name = svg.file_stem().and_then(|s| s.to_str()).unwrap_or("icon");
    let out = scratch.join(format!("{name}.png"));
    let _ = fs::remove_file(&out);
    let svg = svg
        .canonicalize()
        .with_context(|| format!("{} not found", svg.display()))?;
    let candidates: Vec<String> = match std::env::var("CHROME") {
        Ok(chrome) if !chrome.is_empty() => vec![chrome],
        _ => DEFAULT_CHROME.iter().map(|c| c.to_string()).collect(),
    };
    let rendered = candidates.iter().any(|chrome| {
        Command::new(chrome)
            .args([
                "--headless",
                "--disable-gpu",
                "--hide-scrollbars",
                "--force-device-scale-factor=1",
                "--default-background-color=00000000",
            ])
            .arg(format!("--window-size={MASTER},{MASTER}"))
            .arg(format!("--screenshot={}", out.display()))
            .arg(format!("file://{}", svg.display()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
            && out.exists()
    });
    if !rendered {
        bail!(
            "could not render {} with headless Chrome; install Chrome or set CHROME=/path/to/chromium",
            svg.display()
        );
    }
    load_png(&out)
}

fn load_png(path: &Path) -> Result<Vec<Rgba>> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().context("PNG too large")?];
    let info = reader.next_frame(&mut buf)?;
    if info.width as usize != MASTER || info.height as usize != MASTER {
        bail!(
            "{} is {}x{}, expected {MASTER}x{MASTER}",
            path.display(),
            info.width,
            info.height
        );
    }
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => bail!("{}: unexpected PNG color type {other:?}", path.display()),
    };
    Ok(buf[..MASTER * MASTER * channels]
        .chunks_exact(channels)
        .map(|p| {
            let a = if channels == 4 {
                p[3] as f32 / 255.0
            } else {
                1.0
            };
            [
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
                a,
            ]
        })
        .collect())
}

/// Area-average downsample of the master image to `size` x `size` RGBA8.
fn resize(master: &[Rgba], size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size * size * 4);
    let scale = MASTER as f32 / size as f32;
    for y in 0..size {
        for x in 0..size {
            let (x0, x1) = (
                (x as f32 * scale) as usize,
                (((x + 1) as f32 * scale) as usize).min(MASTER),
            );
            let (y0, y1) = (
                (y as f32 * scale) as usize,
                (((y + 1) as f32 * scale) as usize).min(MASTER),
            );
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let p = master[sy.min(MASTER - 1) * MASTER + sx.min(MASTER - 1)];
                    // Accumulate premultiplied so edges do not darken.
                    acc[0] += p[0] * p[3];
                    acc[1] += p[1] * p[3];
                    acc[2] += p[2] * p[3];
                    acc[3] += p[3];
                    n += 1.0;
                }
            }
            let a = acc[3] / n;
            for c in acc.iter().take(3) {
                let v = if acc[3] > 0.0 { c / acc[3] } else { 0.0 };
                out.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push((a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    out
}

fn encode_png(rgba: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, size as u32, size as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(rgba)?;
    }
    Ok(buf)
}

fn icns(pngs: &[(usize, Vec<u8>)]) -> Vec<u8> {
    // PNG-payload icon types: (pixel size, OSType).
    const TYPES: [(usize, &[u8; 4]); 10] = [
        (16, b"icp4"),
        (32, b"icp5"),
        (32, b"ic11"),
        (64, b"ic12"),
        (128, b"ic07"),
        (256, b"ic08"),
        (256, b"ic13"),
        (512, b"ic09"),
        (512, b"ic14"),
        (1024, b"ic10"),
    ];
    let mut body = Vec::new();
    for (size, ty) in TYPES {
        if let Some((_, data)) = pngs.iter().find(|(s, _)| *s == size) {
            body.extend_from_slice(ty);
            body.extend_from_slice(&((data.len() + 8) as u32).to_be_bytes());
            body.extend_from_slice(data);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn ico(pngs: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let entries: Vec<&(usize, Vec<u8>)> = pngs.iter().filter(|(s, _)| *s <= 256).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len();
    for (size, data) in entries.iter().map(|e| (e.0, &e.1)) {
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    out
}

const MANIFEST: &str = r##"{
  "name": "RustyDxf",
  "short_name": "RustyDxf",
  "description": "A small, fast DXF viewer",
  "icons": [
    { "src": "icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any" },
    { "src": "icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any" },
    { "src": "icon-maskable-192.png", "sizes": "192x192", "type": "image/png", "purpose": "maskable" },
    { "src": "icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" }
  ],
  "theme_color": "#b4531f",
  "background_color": "#dfe2e6",
  "display": "standalone"
}
"##;

pub fn generate(root: &Path) -> Result<()> {
    let assets = root.join("assets");
    let web = assets.join("web");
    let scratch: PathBuf = root.join("target").join("icon-render");
    fs::create_dir_all(&web)?;
    fs::create_dir_all(&scratch)?;

    let full = render_svg(&assets.join("icon.svg"), &scratch)?;
    let small = render_svg(&assets.join("src/icon-small.svg"), &scratch)?;
    let maskable = render_svg(&assets.join("src/icon-maskable.svg"), &scratch)?;

    let png = |master: &[Rgba], size: usize| encode_png(&resize(master, size), size);
    // Small sizes use the simplified drawing: the rust texture and the
    // layout lines turn to mud below about 64 px.
    let app = |size: usize| png(if size <= 48 { &small } else { &full }, size);

    let mut pngs = Vec::new();
    for size in [16, 32, 48, 64, 128, 256, 512, 1024] {
        pngs.push((size, app(size)?));
    }
    let write = |path: PathBuf, data: &[u8]| {
        fs::write(&path, data).with_context(|| format!("writing {}", path.display()))
    };

    // Desktop apps.
    write(assets.join("icon.png"), &app(1024)?)?;
    write(assets.join("icon-256.png"), &app(256)?)?;
    write(assets.join("icon.icns"), &icns(&pngs))?;
    write(assets.join("icon.ico"), &ico(&pngs))?;

    // Web.
    fs::copy(assets.join("src/icon-small.svg"), web.join("favicon.svg"))
        .context("copying favicon.svg")?;
    let favicon: Vec<(usize, Vec<u8>)> = pngs.iter().filter(|(s, _)| *s <= 48).cloned().collect();
    write(web.join("favicon.ico"), &ico(&favicon))?;
    write(web.join("favicon-32.png"), &app(32)?)?;
    write(web.join("icon-192.png"), &app(192)?)?;
    write(web.join("icon-512.png"), &app(512)?)?;
    write(web.join("icon-maskable-192.png"), &png(&maskable, 192)?)?;
    write(web.join("icon-maskable-512.png"), &png(&maskable, 512)?)?;
    // iOS rounds the corners itself and shows transparency as black.
    write(web.join("apple-touch-icon.png"), &png(&maskable, 180)?)?;
    write(web.join("site.webmanifest"), MANIFEST.as_bytes())?;

    println!(
        "icons written to {} and {}",
        assets.display(),
        web.display()
    );
    Ok(())
}
