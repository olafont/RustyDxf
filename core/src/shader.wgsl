// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

// RustyDxf drawing shader.
//
// Segments are instanced: each instance is one line segment that the vertex
// shader expands into a screen-space quad of constant pixel width, with a
// one-pixel feather for anti-aliasing. Fills are plain indexed triangles.
// Colors are written in gamma space (the targets are non-sRGB formats) with
// premultiplied alpha.

struct View {
    // Mesh-local position -> NDC: ndc = pos * scale + offset.
    scale: vec2<f32>,
    offset: vec2<f32>,
    // Viewport size in pixels.
    viewport: vec2<f32>,
    line_width: f32,
    point_size: f32,
    // Color used by vertices whose alpha marker is 0 (ACI 7).
    foreground: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

fn resolve_color(c: vec4<f32>) -> vec3<f32> {
    if (c.a < 0.5) {
        return view.foreground.rgb;
    }
    return c.rgb;
}

struct SegmentOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
    // Signed distance from the centre line, in pixels.
    @location(1) across: f32,
    @location(2) half_width: f32,
};

@vertex
fn vs_segment(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) flags: u32,
) -> SegmentOut {
    let half_vp = 0.5 * view.viewport;
    let pa = (a * view.scale + view.offset) * half_vp;
    let pb = (b * view.scale + view.offset) * half_vp;
    let d = pb - pa;
    let len = length(d);
    var dir = vec2<f32>(1.0, 0.0);
    if (len > 1e-6) {
        dir = d / len;
    }
    let normal = vec2<f32>(-dir.y, dir.x);

    var width = view.line_width;
    if ((flags & 1u) != 0u) {
        width = view.point_size;
    }
    let half_w = 0.5 * width;
    let reach = half_w + 0.5;

    // Triangle strip: bit 0 picks the end, bit 1 the side.
    let end = f32(vi & 1u);
    let side = f32((vi >> 1u) & 1u) * 2.0 - 1.0;
    let base = mix(pa, pb, end);
    // Square caps close the gaps at polyline joints.
    let along = (end * 2.0 - 1.0) * half_w;
    let p = base + dir * along + normal * side * reach;

    var out: SegmentOut;
    out.position = vec4<f32>(p / half_vp, 0.0, 1.0);
    out.color = resolve_color(color);
    out.across = side * reach;
    out.half_width = half_w;
    return out;
}

@fragment
fn fs_segment(in: SegmentOut) -> @location(0) vec4<f32> {
    let alpha = clamp(in.half_width + 0.5 - abs(in.across), 0.0, 1.0);
    return vec4<f32>(in.color * alpha, alpha);
}

struct FillOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_fill(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> FillOut {
    var out: FillOut;
    out.position = vec4<f32>(pos * view.scale + view.offset, 0.0, 1.0);
    out.color = resolve_color(color);
    return out;
}

@fragment
fn fs_fill(in: FillOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
