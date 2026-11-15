// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Glue between egui's wgpu renderer and the shared core pipeline.
//!
//! The pipelines and the uploaded mesh live in egui's `callback_resources`.
//! Each frame only a small [`DrawCallback`] is queued: it rewrites the view
//! uniform and issues the draw calls. No geometry goes through
//! `egui::Painter`.

use rustydxf_core::render::{GpuMesh, Renderer, ViewUniform};
use rustydxf_core::tessellate::Mesh;
use std::sync::Arc;

/// Stored in `egui_wgpu::Renderer::callback_resources`.
pub struct GpuState {
    renderer: Renderer,
    mesh: Option<GpuMesh>,
}

/// Create the pipelines for egui's surface format. Call once at startup.
pub fn install(render_state: &egui_wgpu::RenderState) {
    // eframe's surface pass has one sample and no depth buffer (the
    // NativeOptions defaults), which is what the pipelines are built for.
    let renderer = Renderer::new(&render_state.device, render_state.target_format, 1);
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(GpuState {
            renderer,
            mesh: None,
        });
}

/// Upload a freshly tessellated mesh, replacing the previous one.
pub fn set_mesh(render_state: &egui_wgpu::RenderState, mesh: Option<&Mesh>) {
    let mut renderer = render_state.renderer.write();
    if let Some(state) = renderer.callback_resources.get_mut::<GpuState>() {
        state.mesh = mesh.map(|m| state.renderer.upload(&render_state.device, m));
    }
}

/// One frame's worth of drawing parameters.
pub struct DrawCallback {
    pub view: ViewUniform,
    pub visible: Arc<[bool]>,
}

impl egui_wgpu::CallbackTrait for DrawCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(state) = resources.get::<GpuState>() {
            state.renderer.set_view(queue, &self.view);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(state) = resources.get::<GpuState>()
            && let Some(mesh) = &state.mesh
        {
            state.renderer.draw(render_pass, mesh, &self.visible);
        }
    }
}
