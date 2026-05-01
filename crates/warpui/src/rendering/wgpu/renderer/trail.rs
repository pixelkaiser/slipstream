use crate::Scene;
use crate::rendering::wgpu::{resources, shader_types};
use crate::scene::Layer;
use std::borrow::Cow;
use std::sync::{Arc, atomic::AtomicBool};
use wgpu::util::BufferInitDescriptor;
use wgpu::{BindGroupLayout, ColorTargetState, Device, RenderPass, RenderPipeline};

use super::util::create_buffer_init;

pub(super) struct Pipeline {
    render_pipeline: RenderPipeline,
}

#[derive(Default)]
pub(super) struct PerFrameState {
    cursor_trail_data: Vec<shader_types::CursorTrailData>,
    buffer: Option<wgpu::Buffer>,
}

pub(super) struct LayerState {
    start_offset: usize,
    len: usize,
}

impl Pipeline {
    pub(super) fn new(
        uniform_bind_group_layout: &BindGroupLayout,
        device: &Device,
        color_target: ColorTargetState,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Cursor Trail Shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/cursor_trail_shader.wgsl"
            ))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Cursor trail pipeline layout"),
            bind_group_layouts: &[Some(uniform_bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Cursor trail render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[
                    shader_types::Vertex::desc(),
                    shader_types::CursorTrailData::desc(),
                ],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target)],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { render_pipeline }
    }

    pub(super) fn initialize_for_layer(
        &self,
        layer: &Layer,
        scene: &Scene,
        per_frame_state: &mut PerFrameState,
    ) -> Option<LayerState> {
        if layer.cursor_trails.is_empty() {
            return None;
        }

        let start_offset = per_frame_state.cursor_trail_data.len();
        let scale_factor = scene.scale_factor();
        per_frame_state
            .cursor_trail_data
            .extend(layer.cursor_trails.iter().map(|trail| {
                shader_types::CursorTrailData::new(
                    trail.corners.map(|corner| corner * scale_factor),
                    trail.cursor_bounds * scale_factor,
                    trail.color,
                )
            }));

        Some(LayerState {
            start_offset,
            len: layer.cursor_trails.len(),
        })
    }

    pub(super) fn finalize_per_frame_state(
        per_frame_state: &mut PerFrameState,
        device: &Device,
        device_lost: &Arc<AtomicBool>,
    ) {
        per_frame_state.buffer = create_buffer_init(
            device,
            device_lost,
            &BufferInitDescriptor {
                label: Some("Cursor trail instance buffer"),
                contents: bytemuck::cast_slice(&per_frame_state.cursor_trail_data),
                usage: wgpu::BufferUsages::VERTEX,
            },
        )
        .ok();
    }

    pub(super) fn draw<'a>(
        &'a self,
        render_pass: &mut RenderPass<'a>,
        layer_state: &LayerState,
        per_frame_state: &'a PerFrameState,
    ) {
        let Some(buffer) = per_frame_state.buffer.as_ref() else {
            return;
        };

        render_pass.set_pipeline(&self.render_pipeline);
        render_pass.set_vertex_buffer(1, buffer.slice(..));

        let end_offset = layer_state.start_offset + layer_state.len;
        render_pass.draw_indexed(
            0..resources::quad::INDICES.len() as u32,
            0,
            layer_state.start_offset as u32..end_offset as u32,
        );
    }
}
