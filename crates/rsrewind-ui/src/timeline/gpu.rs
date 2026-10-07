//! The wgpu side of the timeline: one pipeline that draws rounded, bordered quads, each optionally
//! showing a screenshot thumbnail from a texture cache bounded in GPU memory.
//!
//! Layout and culling happen in [`super::model`]; this file only uploads and draws what it is
//! given. Several primitives of this type can be on screen at once (the room and the filament), and
//! iced prepares all of them before drawing any, so each keeps its buffers in its own slot.

use super::FrameKey;
use super::cache::ByteLru;
use crate::thumb::Bgra;
use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Color, Rectangle};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Most GPU memory spent on screenshot textures (mip chains included). Textures on screen are
/// never evicted, so a frame that needs more than this briefly exceeds it.
pub const TEXTURE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Floats per instance: rect, image, fill, border, params (5 x vec4).
const FLOATS: usize = 20;
const STRIDE: u64 = (FLOATS * 4) as u64;

/// One quad to draw, in logical pixels relative to the widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub rect: [f32; 4],
    /// Where the picture goes (`[x, y, w, h]`); `None` draws only the fill.
    pub image: Option<[f32; 4]>,
    pub fill: Color,
    pub border: Color,
    pub radius: f32,
    pub border_width: f32,
    pub opacity: f32,
    pub glow: f32,
    pub texture: Option<FrameKey>,
}

impl Quad {
    pub fn solid(rect: [f32; 4], fill: Color, radius: f32, opacity: f32) -> Self {
        Self {
            rect,
            image: None,
            fill,
            border: Color::TRANSPARENT,
            radius,
            border_width: 0.0,
            opacity,
            glow: 0.0,
            texture: None,
        }
    }
}

/// What one widget draws this frame.
pub struct Cards {
    pub slot: u64,
    pub quads: Vec<Quad>,
    /// Pixels for every texture the quads reference that the CPU cache still holds. Uploaded only
    /// if the GPU cache does not already have them.
    pub pictures: Vec<(FrameKey, Arc<Bgra>)>,
}

impl std::fmt::Debug for Cards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cards")
            .field("slot", &self.slot)
            .field("quads", &self.quads.len())
            .field("pictures", &self.pictures.len())
            .finish()
    }
}

struct Slot {
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    instances: Option<wgpu::Buffer>,
    capacity: u64,
    draws: Vec<Option<FrameKey>>,
}

struct Texture {
    bind: wgpu::BindGroup,
    // Kept alive for the bind group.
    _texture: wgpu::Texture,
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    globals_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blank: wgpu::BindGroup,
    _blank_texture: wgpu::Texture,
    texture_format: wgpu::TextureFormat,
    /// The target is sRGB: colours must be given linear. (With iced's `web-colors` it is not, and
    /// colours are passed as written, blending like CSS.)
    linear_colors: bool,
    slots: HashMap<u64, Slot>,
    textures: ByteLru<FrameKey, Texture>,
    on_screen: HashSet<FrameKey>,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsrewind timeline cards"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cards.wgsl").into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsrewind timeline globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsrewind timeline texture"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsrewind timeline layout"),
            bind_group_layouts: &[&globals_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsrewind timeline pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: STRIDE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rsrewind timeline sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let linear_colors = format.is_srgb();
        let texture_format = if linear_colors {
            wgpu::TextureFormat::Bgra8UnormSrgb
        } else {
            wgpu::TextureFormat::Bgra8Unorm
        };
        let white = Bgra {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        };
        let (blank_texture, blank) = upload(device, queue, &texture_layout, texture_format, &white);
        Self {
            pipeline,
            globals_layout,
            texture_layout,
            sampler,
            blank,
            _blank_texture: blank_texture,
            texture_format,
            linear_colors,
            slots: HashMap::new(),
            textures: ByteLru::new(),
            on_screen: HashSet::new(),
        }
    }

    fn trim(&mut self) {
        // End of frame: what was on screen may be evicted from now on if it leaves.
        self.on_screen.clear();
    }
}

impl shader::Primitive for Cards {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        // Textures first, so the bind groups exist when the draw list is built.
        for (key, picture) in &self.pictures {
            pipeline.on_screen.insert(*key);
            if pipeline.textures.get(key).is_none() {
                let (texture, bind) = upload(
                    device,
                    queue,
                    &pipeline.texture_layout,
                    pipeline.texture_format,
                    picture,
                );
                let bytes = mip_bytes(picture.width, picture.height);
                pipeline.textures.insert(
                    *key,
                    Texture {
                        bind,
                        _texture: texture,
                    },
                    bytes,
                );
            }
        }
        let on_screen = &pipeline.on_screen;
        pipeline
            .textures
            .evict_to(TEXTURE_BUDGET_BYTES, |k| on_screen.contains(k));

        let linear = pipeline.linear_colors;
        let mut data = Vec::with_capacity(self.quads.len() * FLOATS * 4);
        let mut draws = Vec::with_capacity(self.quads.len());
        for quad in &self.quads {
            let texture = quad.texture.filter(|k| pipeline.textures.peek(k).is_some());
            let image = match (quad.image, texture) {
                (Some(image), Some(_)) => image,
                _ => [0.0; 4],
            };
            let fill = color(quad.fill, linear);
            let border = color(quad.border, linear);
            for value in quad
                .rect
                .iter()
                .chain(&image)
                .chain(&fill)
                .chain(&border)
                .chain(&[quad.radius, quad.border_width, quad.opacity, quad.glow])
            {
                data.extend_from_slice(&value.to_ne_bytes());
            }
            draws.push(texture);
        }

        let slot = pipeline.slots.entry(self.slot).or_insert_with(|| {
            let globals = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsrewind timeline globals"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rsrewind timeline globals"),
                layout: &pipeline.globals_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: globals.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                ],
            });
            Slot {
                globals,
                globals_bind,
                instances: None,
                capacity: 0,
                draws: Vec::new(),
            }
        });
        let size = [bounds.width, bounds.height, 0.0, 0.0];
        let mut globals = Vec::with_capacity(16);
        for value in size {
            globals.extend_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&slot.globals, 0, &globals);

        let needed = draws.len() as u64;
        if needed > slot.capacity || slot.instances.is_none() {
            let capacity = needed.max(64).next_power_of_two();
            slot.instances = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsrewind timeline instances"),
                size: capacity * STRIDE,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            slot.capacity = capacity;
        }
        if let Some(buffer) = &slot.instances
            && !data.is_empty()
        {
            queue.write_buffer(buffer, 0, &data);
        }
        slot.draws = draws;
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = pipeline.slots.get(&self.slot) else {
            return true;
        };
        let Some(instances) = &slot.instances else {
            return true;
        };
        if slot.draws.is_empty() {
            return true;
        }
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &slot.globals_bind, &[]);
        pass.set_vertex_buffer(0, instances.slice(..));
        for (index, texture) in slot.draws.iter().enumerate() {
            let bind = texture
                .and_then(|k| pipeline.textures.peek(&k))
                .map_or(&pipeline.blank, |t| &t.bind);
            pass.set_bind_group(1, bind, &[]);
            let i = index as u32;
            pass.draw(0..6, i..i + 1);
        }
        true
    }
}

/// Straight-alpha RGBA floats, converted to linear when the target is sRGB.
fn color(c: Color, linear: bool) -> [f32; 4] {
    if linear {
        let [r, g, b, a] = c.into_linear();
        [r, g, b, a]
    } else {
        [c.r, c.g, c.b, c.a]
    }
}

/// Bytes a texture of this size takes with its full mip chain.
fn mip_bytes(width: u32, height: u32) -> usize {
    mip_sizes(width, height)
        .iter()
        .map(|(w, h)| *w as usize * *h as usize * 4)
        .sum()
}

fn mip_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let mut sizes = vec![(width.max(1), height.max(1))];
    while let Some(&(w, h)) = sizes.last() {
        if w <= 8 && h <= 8 {
            break;
        }
        sizes.push(((w / 2).max(1), (h / 2).max(1)));
    }
    sizes
}

/// Creates a mip-mapped texture from a thumbnail (levels reduced on the CPU with the same box
/// filter as the thumbnail itself, so deep, small cards do not shimmer).
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    picture: &Bgra,
) -> (wgpu::Texture, wgpu::BindGroup) {
    let sizes = mip_sizes(picture.width, picture.height);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rsrewind frame"),
        size: wgpu::Extent3d {
            width: sizes[0].0,
            height: sizes[0].1,
            depth_or_array_layers: 1,
        },
        mip_level_count: sizes.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut level = picture.clone();
    for (mip, (w, h)) in sizes.iter().copied().enumerate() {
        if mip > 0 {
            match crate::thumb::downscale(
                &level.pixels,
                level.width,
                level.height,
                level.width * 4,
                w,
                h,
            ) {
                Some(next) => level = next,
                None => break,
            }
        }
        if level.pixels.len() < (level.width * level.height * 4) as usize {
            break;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &level.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(level.width * 4),
                rows_per_image: Some(level.height),
            },
            wgpu::Extent3d {
                width: level.width,
                height: level.height,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rsrewind frame"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    (texture, bind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chains_end_small_and_count_their_bytes() {
        assert_eq!(mip_sizes(320, 200).last(), Some(&(5, 3)));
        assert_eq!(mip_sizes(1, 1), vec![(1, 1)]);
        assert_eq!(mip_bytes(4, 4), 64);
        let full = 320 * 200 * 4;
        let bytes = mip_bytes(320, 200);
        assert!(bytes > full && bytes < full * 4 / 3 + 1024, "{bytes}");
    }
}
