// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! The wgpu drawing pipeline, shared by two targets:
//!
//! * **On screen** — the app wraps [`Renderer::draw`] in an
//!   `egui_wgpu::CallbackTrait` paint callback. Buffers are uploaded once per
//!   file ([`Renderer::upload`]); pan and zoom only rewrite the small view
//!   uniform ([`Renderer::set_view`]).
//! * **Offscreen** — [`render_png`] draws the same buffers with a headless
//!   device into a texture and encodes it as PNG (used by the MCP server).

use crate::camera::Camera;
use crate::geom::BBox;
use crate::tessellate::{FillVertex, LayerBatch, Mesh, SegmentInstance};
use bytemuck::{Pod, Zeroable};
use std::ops::Range;
use std::sync::{Mutex, OnceLock};
use wgpu::util::DeviceExt;

/// Dark viewport background (sRGB), used by the GUI and dark renders.
pub const BACKGROUND_DARK: [u8; 3] = [24, 26, 31];
/// Light background for `render_png(background = "light")`.
pub const BACKGROUND_LIGHT: [u8; 3] = [255, 255, 255];

/// Largest offscreen image edge, in pixels.
pub const MAX_RENDER_SIZE: u32 = 8192;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ViewUniform {
    pub scale: [f32; 2],
    pub offset: [f32; 2],
    pub viewport: [f32; 2],
    pub line_width: f32,
    pub point_size: f32,
    pub foreground: [f32; 4],
}

impl ViewUniform {
    /// Build the uniform for a camera looking at `mesh` through a viewport
    /// of `size` pixels.
    pub fn new(
        camera: &Camera,
        mesh_origin: crate::geom::Vec2,
        size: (f64, f64),
        line_width: f32,
        dark_background: bool,
    ) -> Self {
        let (scale, offset) = camera.ndc_transform(mesh_origin, size);
        let fg = if dark_background { 1.0 } else { 0.0 };
        ViewUniform {
            scale,
            offset,
            viewport: [size.0 as f32, size.1 as f32],
            line_width,
            point_size: (line_width * 3.0).max(3.0),
            foreground: [fg, fg, fg, 1.0],
        }
    }
}

/// Pipelines and the view uniform. Create one per device and target format.
pub struct Renderer {
    segment_pipeline: wgpu::RenderPipeline,
    fill_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// A mesh uploaded to the GPU.
pub struct GpuMesh {
    segments: Option<wgpu::Buffer>,
    fill_vertices: Option<wgpu::Buffer>,
    fill_indices: Option<wgpu::Buffer>,
    layers: Vec<LayerBatch>,
}

impl GpuMesh {
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

impl Renderer {
    /// `sample_count` must match the render pass the pipelines are used in
    /// (1 for egui's surface pass).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, sample_count: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rustydxf shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustydxf view layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rustydxf view uniform"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rustydxf view"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustydxf layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let multisample = wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };
        let targets = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let segment_attrs = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Unorm8x4,
            3 => Uint32
        ];
        let segment_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustydxf segments"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_segment"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SegmentInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &segment_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_segment"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });

        let fill_attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4];
        let fill_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustydxf fills"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fill"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<FillVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &fill_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_fill"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });

        Renderer {
            segment_pipeline,
            fill_pipeline,
            uniform_buffer,
            bind_group,
        }
    }

    /// Upload a tessellated mesh. Call once per loaded file.
    pub fn upload(&self, device: &wgpu::Device, mesh: &Mesh) -> GpuMesh {
        let make = |label: &str, bytes: &[u8], usage: wgpu::BufferUsages| {
            (!bytes.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytes,
                    usage,
                })
            })
        };
        GpuMesh {
            segments: make(
                "rustydxf segments",
                bytemuck::cast_slice(&mesh.segments),
                wgpu::BufferUsages::VERTEX,
            ),
            fill_vertices: make(
                "rustydxf fill vertices",
                bytemuck::cast_slice(&mesh.fill_vertices),
                wgpu::BufferUsages::VERTEX,
            ),
            fill_indices: make(
                "rustydxf fill indices",
                bytemuck::cast_slice(&mesh.fill_indices),
                wgpu::BufferUsages::INDEX,
            ),
            layers: mesh.layers.clone(),
        }
    }

    /// Update the view; this is all that changes on pan and zoom.
    pub fn set_view(&self, queue: &wgpu::Queue, view: &ViewUniform) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(view));
    }

    /// Draw the visible layers. `visible[i]` controls layer `i`; layers
    /// beyond the slice are drawn.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, mesh: &GpuMesh, visible: &[bool]) {
        let shown = |i: usize| visible.get(i).copied().unwrap_or(true);
        pass.set_bind_group(0, &self.bind_group, &[]);

        // Fills first, so outlines and other linework stay on top.
        if let (Some(vertices), Some(indices)) = (&mesh.fill_vertices, &mesh.fill_indices) {
            let ranges = merged_ranges(&mesh.layers, &shown, |l| l.indices.clone());
            if !ranges.is_empty() {
                pass.set_pipeline(&self.fill_pipeline);
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                for r in ranges {
                    pass.draw_indexed(r, 0, 0..1);
                }
            }
        }
        if let Some(segments) = &mesh.segments {
            let ranges = merged_ranges(&mesh.layers, &shown, |l| l.segments.clone());
            if !ranges.is_empty() {
                pass.set_pipeline(&self.segment_pipeline);
                pass.set_vertex_buffer(0, segments.slice(..));
                for r in ranges {
                    pass.draw(0..4, r);
                }
            }
        }
    }
}

/// Collapse the ranges of consecutive visible layers into as few draw calls
/// as possible.
fn merged_ranges(
    layers: &[LayerBatch],
    shown: &dyn Fn(usize) -> bool,
    pick: impl Fn(&LayerBatch) -> Range<u32>,
) -> Vec<Range<u32>> {
    let mut out: Vec<Range<u32>> = Vec::new();
    for (i, layer) in layers.iter().enumerate() {
        let r = pick(layer);
        if r.is_empty() || !shown(i) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == r.start => last.end = r.end,
            _ => out.push(r),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Offscreen rendering

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no usable GPU adapter for offscreen rendering: {0}")]
    NoAdapter(String),
    #[error("cannot create a GPU device: {0}")]
    Device(String),
    #[error("image size must be between 16 and {MAX_RENDER_SIZE} pixels per side, got {0}x{1}")]
    BadSize(u32, u32),
    #[error("reading back the rendered image failed: {0}")]
    Readback(String),
    #[error("PNG encoding failed: {0}")]
    Encode(String),
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    /// World rectangle to frame. Defaults to the mesh's bounding box.
    pub view: Option<BBox>,
    /// Per-layer visibility (index = layer). Missing entries are visible.
    pub visible: Vec<bool>,
    pub dark_background: bool,
    /// Line width in pixels.
    pub line_width: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 1600,
            height: 1200,
            view: None,
            visible: Vec::new(),
            dark_background: true,
            line_width: 1.5,
        }
    }
}

/// An RGBA8 image, rows top to bottom.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn to_png(&self) -> Result<Vec<u8>, RenderError> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| RenderError::Encode(e.to_string()))?;
        writer
            .write_image_data(&self.rgba)
            .map_err(|e| RenderError::Encode(e.to_string()))?;
        drop(writer);
        Ok(out)
    }
}

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A headless wgpu device with the pipelines ready.
pub struct Headless {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    samples: u32,
    adapter_name: String,
    /// The view uniform is shared, so renders must not interleave.
    lock: Mutex<()>,
}

impl Headless {
    pub fn new() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::default();
        // Prefer a real GPU, then fall back to a software rasteriser.
        let mut last_error = String::new();
        let mut adapter = None;
        for force_fallback_adapter in [false, true] {
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter,
                compatible_surface: None,
                ..Default::default()
            })) {
                Ok(a) => {
                    adapter = Some(a);
                    break;
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        let adapter = adapter.ok_or(RenderError::NoAdapter(last_error))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustydxf headless"),
            ..Default::default()
        }))
        .map_err(|e| RenderError::Device(e.to_string()))?;

        let samples = if adapter
            .get_texture_format_features(OFFSCREEN_FORMAT)
            .flags
            .sample_count_supported(4)
        {
            4
        } else {
            1
        };
        let renderer = Renderer::new(&device, OFFSCREEN_FORMAT, samples);
        let info = adapter.get_info();
        Ok(Headless {
            device,
            queue,
            renderer,
            samples,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            lock: Mutex::new(()),
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn render(&self, mesh: &Mesh, opts: &RenderOptions) -> Result<Image, RenderError> {
        let (width, height) = (opts.width, opts.height);
        if !(16..=MAX_RENDER_SIZE).contains(&width) || !(16..=MAX_RENDER_SIZE).contains(&height) {
            return Err(RenderError::BadSize(width, height));
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        let size = (width as f64, height as f64);
        let target = opts.view.or(mesh.bbox);
        let camera = match target {
            Some(b) => Camera::fit(&b, size, if opts.view.is_some() { 0.0 } else { 0.04 }),
            None => Camera::default(),
        };
        self.renderer.set_view(
            &self.queue,
            &ViewUniform::new(
                &camera,
                mesh.origin,
                size,
                opts.line_width,
                opts.dark_background,
            ),
        );
        let gpu_mesh = self.renderer.upload(&self.device, mesh);

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = |label: &str, sample_count: u32, usage: wgpu::TextureUsages| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: OFFSCREEN_FORMAT,
                usage,
                view_formats: &[],
            })
        };
        let resolved = texture(
            "rustydxf offscreen",
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let resolved_view = resolved.create_view(&Default::default());
        let msaa_view = (self.samples > 1).then(|| {
            texture(
                "rustydxf offscreen msaa",
                self.samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&Default::default())
        });

        let bg = if opts.dark_background {
            BACKGROUND_DARK
        } else {
            BACKGROUND_LIGHT
        };
        let clear = wgpu::Color {
            r: bg[0] as f64 / 255.0,
            g: bg[1] as f64 / 255.0,
            b: bg[2] as f64 / 255.0,
            a: 1.0,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustydxf offscreen"),
            });
        {
            let (view, resolve_target, store) = match &msaa_view {
                Some(msaa) => (msaa, Some(&resolved_view), wgpu::StoreOp::Discard),
                None => (&resolved_view, None, wgpu::StoreOp::Store),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustydxf offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.draw(&mut pass, &gpu_mesh, &opts.visible);
        }

        // Rows in the readback buffer are padded to the copy alignment.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rustydxf readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;

        let mut rgba = Vec::with_capacity((unpadded * height) as usize);
        {
            let data = slice
                .get_mapped_range()
                .map_err(|e| RenderError::Readback(e.to_string()))?;
            for row in data.chunks(padded as usize).take(height as usize) {
                rgba.extend_from_slice(&row[..unpadded as usize]);
            }
        }
        readback.unmap();
        Ok(Image {
            width,
            height,
            rgba,
        })
    }
}

/// The process-wide headless renderer, created on first use.
pub fn headless() -> Result<&'static Headless, RenderError> {
    static INSTANCE: OnceLock<Result<Headless, String>> = OnceLock::new();
    INSTANCE
        .get_or_init(|| Headless::new().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| RenderError::NoAdapter(e.clone()))
}

/// Render a mesh offscreen and return PNG bytes.
pub fn render_png(mesh: &Mesh, opts: &RenderOptions) -> Result<Vec<u8>, RenderError> {
    headless()?.render(mesh, opts)?.to_png()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_ranges_skip_hidden_and_join_neighbours() {
        let layers = vec![
            LayerBatch {
                segments: 0..10,
                indices: 0..0,
            },
            LayerBatch {
                segments: 10..25,
                indices: 0..6,
            },
            LayerBatch {
                segments: 25..30,
                indices: 6..12,
            },
            LayerBatch {
                segments: 30..31,
                indices: 12..12,
            },
        ];
        let all = |_: usize| true;
        assert_eq!(
            merged_ranges(&layers, &all, |l| l.segments.clone()),
            vec![0..31]
        );
        assert_eq!(
            merged_ranges(&layers, &all, |l| l.indices.clone()),
            vec![0..12]
        );
        let hide_1 = |i: usize| i != 1;
        assert_eq!(
            merged_ranges(&layers, &hide_1, |l| l.segments.clone()),
            vec![0..10, 25..31]
        );
        let none = |_: usize| false;
        assert!(merged_ranges(&layers, &none, |l| l.segments.clone()).is_empty());
    }

    #[test]
    fn view_uniform_matches_shader_layout() {
        // vec2 + vec2 + vec2 + f32 + f32 + vec4 = 48 bytes, vec4 aligned to 16.
        assert_eq!(std::mem::size_of::<ViewUniform>(), 48);
        assert_eq!(std::mem::offset_of!(ViewUniform, foreground), 32);
        assert_eq!(std::mem::size_of::<SegmentInstance>(), 24);
        assert_eq!(std::mem::size_of::<FillVertex>(), 12);
    }
}
