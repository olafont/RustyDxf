//! Generates the placeholder RustyDxf icon (by art-tech) procedurally:
//! a dark rounded tile with a "D"-shaped drafting profile in the accent
//! color and three white grip squares. Writes PNG, ICNS and ICO.

use anyhow::{Context, Result};
use rustydxf_core::brand::ACCENT;
use std::fs;
use std::path::Path;

const MASTER: usize = 1024;

type Rgba = [f32; 4];

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (p.0 - a.0, p.1 - a.1);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = ((px * dx + py * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - dx * t).powi(2) + (py - dy * t).powi(2)).sqrt()
}

fn rounded_rect_dist(p: (f32, f32), center: (f32, f32), half: f32, radius: f32) -> f32 {
    let qx = (p.0 - center.0).abs() - (half - radius);
    let qy = (p.1 - center.1).abs() - (half - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Coverage for a signed distance (negative = inside), one pixel of AA.
fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn over(dst: &mut Rgba, src: [f32; 3], alpha: f32) {
    for c in 0..3 {
        dst[c] = src[c] * alpha + dst[c] * (1.0 - alpha);
    }
    dst[3] = alpha + dst[3] * (1.0 - alpha);
}

fn master() -> Vec<Rgba> {
    let accent = [
        ACCENT[0] as f32 / 255.0,
        ACCENT[1] as f32 / 255.0,
        ACCENT[2] as f32 / 255.0,
    ];
    let white = [0.96, 0.96, 0.97];
    let grip_edge = [0.08, 0.09, 0.11];

    // Profile geometry on the 1024 canvas.
    let (left, top, bottom, arc_cx) = (372.0, 290.0, 734.0, 490.0);
    let cy = (top + bottom) / 2.0;
    let r = (bottom - top) / 2.0;
    let stroke = 27.0; // half width
    let grips = [(left, top), (left, bottom), (arc_cx + r, cy)];

    let mut img = vec![[0.0f32; 4]; MASTER * MASTER];
    for y in 0..MASTER {
        for x in 0..MASTER {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let px = &mut img[y * MASTER + x];

            // Tile with a soft vertical gradient.
            let tile = rounded_rect_dist(p, (512.0, 512.0), 412.0, 184.0);
            let t = y as f32 / MASTER as f32;
            let bg = [0.135 - 0.04 * t, 0.145 - 0.04 * t, 0.175 - 0.045 * t];
            over(px, bg, cover(tile));
            // Thin inner highlight along the tile edge.
            over(px, [1.0, 1.0, 1.0], 0.07 * cover((tile + 5.0).abs() - 2.5));

            // The "D": three straight edges and a right-hand semicircle.
            let mut d = seg_dist(p, (left, top), (left, bottom))
                .min(seg_dist(p, (left, top), (arc_cx, top)))
                .min(seg_dist(p, (left, bottom), (arc_cx, bottom)));
            if p.0 >= arc_cx {
                let dc = ((p.0 - arc_cx).powi(2) + (p.1 - cy).powi(2)).sqrt();
                d = d.min((dc - r).abs());
            }
            over(px, accent, cover(d - stroke) * cover(tile + 30.0));

            // Grips.
            for g in grips {
                let gd = (p.0 - g.0).abs().max((p.1 - g.1).abs());
                over(px, grip_edge, cover(gd - 50.0));
                over(px, white, cover(gd - 38.0));
            }
        }
    }
    img
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

pub fn generate(assets: &Path) -> Result<()> {
    fs::create_dir_all(assets)?;
    let master = master();
    let mut pngs = Vec::new();
    for size in [16, 32, 48, 64, 128, 256, 512, 1024] {
        let rgba = resize(&master, size);
        pngs.push((size, encode_png(&rgba, size)?));
    }
    let get = |s: usize| {
        &pngs
            .iter()
            .find(|(z, _)| *z == s)
            .expect("size generated")
            .1
    };
    let write = |name: &str, data: &[u8]| {
        fs::write(assets.join(name), data).with_context(|| format!("writing {name}"))
    };
    write("icon.png", get(1024))?;
    write("icon-256.png", get(256))?;
    write("icon.icns", &icns(&pngs))?;
    write("icon.ico", &ico(&pngs))?;
    println!("icons written to {}", assets.display());
    Ok(())
}
