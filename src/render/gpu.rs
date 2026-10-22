//! wgpu pipeline for canvas batches, plus offscreen rendering for screenshots and PNG export.

use super::mesh::Vertex;
use super::scene::{CanvasFrame, Item};
use super::tess;
use crate::assets;
use crate::geom::Rect;
use crate::model::BlendMode;
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Instance options for every wgpu instance in the app: `WGPU_BACKEND` when it is set, else
/// every backend except GL on Windows.
///
/// On Windows, starting Vulkan and GL in one process can crash the app at launch with
/// STATUS_ACCESS_VIOLATION (0xc0000005). Seen on a laptop with an AMD iGPU and an NVIDIA
/// RTX GPU. Windows always has Vulkan or DX12, so GL stays available through `WGPU_BACKEND=gl`.
pub fn instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = default_backends(desc.backends, cfg!(windows));
    desc.with_env()
}

fn default_backends(backends: wgpu::Backends, windows: bool) -> wgpu::Backends {
    if windows {
        backends - wgpu::Backends::GL
    } else {
        backends
    }
}

pub const MSAA: u32 = 4;

/// Colors are authored in sRGB and blended in gamma space (like egui), so targets are non-sRGB.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
struct Globals {
    size_px: vec2<f32>,
    scale: f32,
    zoom: f32,
    pan: vec2<f32>,
    offset: vec2<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var ramp_tex: texture_2d<f32>;
@group(1) @binding(1) var ramp_samp: sampler;
@group(2) @binding(0) var img_tex: texture_2d<f32>;
@group(2) @binding(1) var img_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) kind: u32,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>, @location(2) uv: vec2<f32>,
      @location(3) kind: u32) -> VsOut {
    let logical = (pos - g.pan) * g.zoom + g.offset;
    let px = logical * g.scale;
    var o: VsOut;
    o.pos = vec4<f32>(px.x / g.size_px.x * 2.0 - 1.0, 1.0 - px.y / g.size_px.y * 2.0, 0.0, 1.0);
    o.color = color;
    o.uv = uv;
    o.kind = kind;
    return o;
}

fn ramp(t: f32, row: u32) -> vec4<f32> {
    let v = (f32(row) + 0.5) / 256.0;
    return textureSampleLevel(ramp_tex, ramp_samp, vec2<f32>(clamp(t, 0.0, 1.0), v), 0.0);
}

@fragment
fn fs(i: VsOut) -> @location(0) vec4<f32> {
    let k = i.kind & 255u;
    let row = i.kind >> 8u;
    var c = i.color;
    if (k == 1u) {
        let r = ramp(i.uv.x, row);
        c = vec4<f32>(r.rgb, r.a * i.color.a);
    } else if (k == 2u) {
        let r = ramp(length(i.uv), row);
        c = vec4<f32>(r.rgb, r.a * i.color.a);
    } else if (k >= 3u) {
        if (k == 3u && (i.uv.x < 0.0 || i.uv.y < 0.0 || i.uv.x > 1.0 || i.uv.y > 1.0)) {
            discard;
        }
        let t = textureSample(img_tex, img_samp, i.uv);
        c = vec4<f32>(t.rgb, t.a * i.color.a);
    }
    // Premultiplied output; every pipeline blends premultiplied.
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size_px: [f32; 2],
    scale: f32,
    zoom: f32,
    pan: [f32; 2],
    offset: [f32; 2],
}

/// GPU pipelines exist for the blend modes fixed-function blending can express.
fn blend_key(b: BlendMode) -> usize {
    match b {
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Darken => 3,
        BlendMode::Lighten => 4,
        _ => 0,
    }
}

fn blend_state(key: usize) -> wgpu::BlendState {
    use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
    let alpha = C {
        src_factor: F::One,
        dst_factor: F::OneMinusSrcAlpha,
        operation: O::Add,
    };
    let color = match key {
        1 => C {
            src_factor: F::Dst,
            dst_factor: F::OneMinusSrcAlpha,
            operation: O::Add,
        },
        2 => C {
            src_factor: F::One,
            dst_factor: F::OneMinusSrc,
            operation: O::Add,
        },
        3 => C {
            src_factor: F::One,
            dst_factor: F::One,
            operation: O::Min,
        },
        4 => C {
            src_factor: F::One,
            dst_factor: F::One,
            operation: O::Max,
        },
        _ => alpha,
    };
    wgpu::BlendState { color, alpha }
}

/// (index range, world clip, blend key, image) of one cached world draw.
type WorldDraw = (std::ops::Range<u32>, Option<Rect>, usize, Option<String>);

struct Draw {
    range: std::ops::Range<u32>,
    /// Screen-space scissor (logical px), already intersected with the viewport.
    clip: Option<Rect>,
    blend: usize,
    image: Option<String>,
    world: bool,
}

/// Cached world-space geometry: reused while the item list is unchanged (panning, idle).
struct WorldBuffers {
    signature: Vec<(usize, Option<[u64; 4]>, usize)>,
    vb: Option<wgpu::Buffer>,
    ib: Option<wgpu::Buffer>,
    /// (index range, world clip, blend key, image) per draw.
    draws: Vec<WorldDraw>,
}

pub struct CanvasRenderer {
    pipelines: Vec<wgpu::RenderPipeline>,
    world_globals: wgpu::Buffer,
    screen_globals: wgpu::Buffer,
    world_bg: wgpu::BindGroup,
    screen_bg: wgpu::BindGroup,
    ramp_tex: wgpu::Texture,
    ramp_bg: wgpu::BindGroup,
    ramp_version: u64,
    img_layout: wgpu::BindGroupLayout,
    img_sampler: wgpu::Sampler,
    blank_img: wgpu::BindGroup,
    images: HashMap<String, wgpu::BindGroup>,
    world: Option<WorldBuffers>,
    msaa: Option<(wgpu::TextureView, [u32; 2])>,
    format: wgpu::TextureFormat,
    /// World geometry uploads (for tests: panning must not re-upload).
    pub uploads: u64,
}

fn texture_2d(device: &wgpu::Device, label: &str, w: u32, h: u32, mips: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_texture(queue: &wgpu::Queue, tex: &wgpu::Texture, mip: u32, img: &image::RgbaImage) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        img.as_raw(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width() * 4),
            rows_per_image: Some(img.height()),
        },
        wgpu::Extent3d {
            width: img.width(),
            height: img.height(),
            depth_or_array_layers: 1,
        },
    );
}

impl CanvasRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> CanvasRenderer {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas globals"),
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
        let tex_layout = |label| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
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
            })
        };
        let ramp_layout = tex_layout("ramp");
        let img_layout = tex_layout("image");
        let mk_globals = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let world_globals = mk_globals("world globals");
        let screen_globals = mk_globals("screen globals");
        let mk_bg = |buf: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("globals"),
                layout: &globals_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            })
        };
        let world_bg = mk_bg(&world_globals);
        let screen_bg = mk_bg(&screen_globals);
        let ramp_tex = texture_2d(device, "gradient ramps", tess::ATLAS_W, tess::ATLAS_H, 1);
        let ramp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ramp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let ramp_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ramp"),
            layout: &ramp_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &ramp_tex.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&ramp_sampler),
                },
            ],
        });
        let img_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let blank = texture_2d(device, "blank", 1, 1, 1);
        let blank_img = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blank image"),
            layout: &img_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &blank.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&img_sampler),
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas"),
            bind_group_layouts: &[Some(&globals_layout), Some(&ramp_layout), Some(&img_layout)],
            immediate_size: 0,
        });
        let pipelines = (0..5)
            .map(|key| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("canvas"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<Vertex>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &wgpu::vertex_attr_array![
                                0 => Float32x2, 1 => Float32x4, 2 => Float32x2, 3 => Uint32
                            ],
                        })],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState {
                        count: MSAA,
                        mask: !0,
                        alpha_to_coverage_enabled: false,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: Some(blend_state(key)),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        CanvasRenderer {
            pipelines,
            world_globals,
            screen_globals,
            world_bg,
            screen_bg,
            ramp_tex,
            ramp_bg,
            ramp_version: 0,
            img_layout,
            img_sampler,
            blank_img,
            images: HashMap::new(),
            world: None,
            msaa: None,
            format,
            uploads: 0,
        }
    }

    fn msaa_view(&mut self, device: &wgpu::Device, size: [u32; 2]) -> &wgpu::TextureView {
        if self.msaa.as_ref().is_none_or(|(_, s)| *s != size) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas msaa"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: MSAA,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.msaa = Some((tex.create_view(&Default::default()), size));
        }
        &self.msaa.as_ref().unwrap().0
    }

    fn sync_ramps(&mut self, queue: &wgpu::Queue) {
        let a = tess::atlas().lock().unwrap();
        if a.version == self.ramp_version {
            return;
        }
        self.ramp_version = a.version;
        let img = image::RgbaImage::from_raw(tess::ATLAS_W, tess::ATLAS_H, a.pixels.clone())
            .expect("atlas size");
        write_texture(queue, &self.ramp_tex, 0, &img);
    }

    fn image_bg(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: &str) {
        if self.images.contains_key(id) {
            return;
        }
        let Some(a) = assets::get(id) else { return };
        // Mip chain so minified images stay smooth.
        let mut levels = vec![a.rgba.clone()];
        while levels.last().unwrap().width() > 1 || levels.last().unwrap().height() > 1 {
            let l = levels.last().unwrap();
            let (w, h) = ((l.width() / 2).max(1), (l.height() / 2).max(1));
            levels.push(image::imageops::resize(
                l,
                w,
                h,
                image::imageops::FilterType::Triangle,
            ));
            if levels.len() > 14 {
                break;
            }
        }
        let tex = texture_2d(device, "image", a.width, a.height, levels.len() as u32);
        for (i, l) in levels.iter().enumerate() {
            write_texture(queue, &tex, i as u32, l);
        }
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.img_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &tex.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.img_sampler),
                },
            ],
        });
        self.images.insert(id.to_string(), bg);
    }

    /// Rebuild the world vertex/index buffers when the item list changed.
    fn sync_world(&mut self, device: &wgpu::Device, items: &[Item]) {
        let sig: Vec<(usize, Option<[u64; 4]>, usize)> = items
            .iter()
            .map(|it| {
                (
                    Arc::as_ptr(&it.pieces) as usize,
                    it.clip
                        .map(|c| [c.x.to_bits(), c.y.to_bits(), c.w.to_bits(), c.h.to_bits()]),
                    blend_key(it.blend),
                )
            })
            .collect();
        if self.world.as_ref().is_some_and(|w| w.signature == sig) {
            return;
        }
        self.uploads += 1;
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut draws: Vec<WorldDraw> = Vec::new();
        for it in items {
            let bk = blend_key(it.blend);
            for p in it.pieces.iter() {
                let base = vertices.len() as u32;
                let start = indices.len() as u32;
                vertices.extend_from_slice(&p.mesh.vertices);
                indices.extend(p.mesh.indices.iter().map(|i| i + base));
                let end = indices.len() as u32;
                match draws.last_mut() {
                    Some(d)
                        if d.1 == it.clip && d.2 == bk && d.3 == p.image && d.0.end == start =>
                    {
                        d.0.end = end
                    }
                    _ => draws.push((start..end, it.clip, bk, p.image.clone())),
                }
            }
        }
        let mk = |data: &[u8], usage, label| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage,
            })
        };
        let (vb, ib) = if indices.is_empty() {
            (None, None)
        } else {
            (
                Some(mk(
                    bytemuck::cast_slice(&vertices),
                    wgpu::BufferUsages::VERTEX,
                    "world vertices",
                )),
                Some(mk(
                    bytemuck::cast_slice(&indices),
                    wgpu::BufferUsages::INDEX,
                    "world indices",
                )),
            )
        };
        self.world = Some(WorldBuffers {
            signature: sig,
            vb,
            ib,
            draws,
        });
    }

    /// Record a pass that clears `target` and draws a canvas frame.
    /// Logical px × `scale` = physical px.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: [u32; 2],
        scale: f32,
        frame: &CanvasFrame,
        clear: [f64; 4],
    ) {
        let cam = frame.camera;
        let world = Globals {
            size_px: [size[0] as f32, size[1] as f32],
            scale,
            zoom: cam.zoom as f32,
            pan: [cam.pan.x as f32, cam.pan.y as f32],
            offset: [frame.offset.x as f32, frame.offset.y as f32],
        };
        let screen = Globals {
            zoom: 1.0,
            pan: [0.0; 2],
            offset: [0.0; 2],
            ..world
        };
        queue.write_buffer(&self.world_globals, 0, bytemuck::bytes_of(&world));
        queue.write_buffer(&self.screen_globals, 0, bytemuck::bytes_of(&screen));
        self.sync_ramps(queue);
        for it in &frame.items {
            for p in it.pieces.iter() {
                if let Some(id) = &p.image {
                    self.image_bg(device, queue, id);
                }
            }
        }
        self.sync_world(device, &frame.items);

        // Screen-space batches (underlay + overlay) are small: rebuilt every frame.
        let mut sverts: Vec<Vertex> = Vec::new();
        let mut sidx: Vec<u32> = Vec::new();
        let mut under = Vec::new();
        let mut over = Vec::new();
        for (list, out) in [(&frame.under, &mut under), (&frame.over, &mut over)] {
            for b in list.iter() {
                let base = sverts.len() as u32;
                let start = sidx.len() as u32;
                sverts.extend_from_slice(&b.mesh.vertices);
                sidx.extend(b.mesh.indices.iter().map(|i| i + base));
                out.push(Draw {
                    range: start..sidx.len() as u32,
                    clip: b.clip,
                    blend: 0,
                    image: None,
                    world: false,
                });
            }
        }
        let sbuf = (!sidx.is_empty()).then(|| {
            (
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("screen vertices"),
                    contents: bytemuck::cast_slice(&sverts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("screen indices"),
                    contents: bytemuck::cast_slice(&sidx),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            )
        });
        let vp = frame.viewport;
        let to_screen = |r: &Rect| {
            Rect::new(
                (r.x - cam.pan.x) * cam.zoom + frame.offset.x,
                (r.y - cam.pan.y) * cam.zoom + frame.offset.y,
                r.w * cam.zoom,
                r.h * cam.zoom,
            )
        };
        let world_draws: Vec<Draw> = self
            .world
            .as_ref()
            .map(|w| {
                w.draws
                    .iter()
                    .filter_map(|(range, clip, blend, image)| {
                        let sc = match clip {
                            Some(c) => vp.intersection(&to_screen(c))?,
                            None => vp,
                        };
                        Some(Draw {
                            range: range.clone(),
                            clip: Some(sc),
                            blend: *blend,
                            image: image.clone(),
                            world: true,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let msaa = self.msaa_view(device, size).clone();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                depth_slice: None,
                resolve_target: Some(target),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0],
                        g: clear[1],
                        b: clear[2],
                        a: clear[3],
                    }),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(1, &self.ramp_bg, &[]);
        let mut current_pipeline = usize::MAX;
        for d in under.iter().chain(&world_draws).chain(&over) {
            let (vb, ib) = if d.world {
                match self
                    .world
                    .as_ref()
                    .and_then(|w| w.vb.as_ref().zip(w.ib.as_ref()))
                {
                    Some(b) => b,
                    None => continue,
                }
            } else {
                match &sbuf {
                    Some((v, i)) => (v, i),
                    None => continue,
                }
            };
            let (x, y, w, h) = match d.clip {
                Some(c) => {
                    let x0 = (c.x * scale as f64).floor().clamp(0.0, size[0] as f64);
                    let y0 = (c.y * scale as f64).floor().clamp(0.0, size[1] as f64);
                    let x1 = (c.right() * scale as f64).ceil().clamp(0.0, size[0] as f64);
                    let y1 = (c.bottom() * scale as f64)
                        .ceil()
                        .clamp(0.0, size[1] as f64);
                    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
                }
                None => (0, 0, size[0], size[1]),
            };
            if w == 0 || h == 0 || d.range.is_empty() {
                continue;
            }
            if current_pipeline != d.blend {
                pass.set_pipeline(&self.pipelines[d.blend]);
                current_pipeline = d.blend;
            }
            pass.set_bind_group(
                0,
                if d.world {
                    &self.world_bg
                } else {
                    &self.screen_bg
                },
                &[],
            );
            let img = d
                .image
                .as_ref()
                .and_then(|i| self.images.get(i))
                .unwrap_or(&self.blank_img);
            pass.set_bind_group(2, img, &[]);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_scissor_rect(x, y, w, h);
            pass.draw_indexed(d.range.clone(), 0, 0..1);
        }
    }
}

/// A device without a window, for tests, screenshots and PNG export.
pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
}

impl Headless {
    pub fn new() -> Result<Headless, String> {
        let instance = wgpu::Instance::new(instance_descriptor());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter available: {e}"))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sketch-rs headless"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("failed to create device: {e}"))?;
        Ok(Headless {
            device,
            queue,
            adapter_info,
        })
    }

    /// Create a render target texture of `size` physical px.
    pub fn target(&self, size: [u32; 2]) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Copy a texture back to the CPU as RGBA8.
    pub fn read(&self, tex: &wgpu::Texture) -> image::RgbaImage {
        let (w, h) = (tex.width(), tex.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = slice.get_mapped_range().expect("mapped readback buffer");
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            let src = &data[(y * row) as usize..(y * row + w * 4) as usize];
            img.as_mut()[(y * w * 4) as usize..((y + 1) * w * 4) as usize].copy_from_slice(src);
        }
        img
    }

    /// Render a canvas frame into a new image of `size` physical px.
    pub fn render_frame(
        &self,
        frame: &CanvasFrame,
        size: [u32; 2],
        scale: f32,
        clear: [f64; 4],
    ) -> image::RgbaImage {
        let tex = self.target(size);
        let view = tex.create_view(&Default::default());
        let mut r = CanvasRenderer::new(&self.device, OFFSCREEN_FORMAT);
        let mut enc = self.device.create_command_encoder(&Default::default());
        r.render(
            &self.device,
            &self.queue,
            &mut enc,
            &view,
            size,
            scale,
            frame,
            clear,
        );
        self.queue.submit([enc.finish()]);
        self.read(&tex)
    }
}

#[cfg(test)]
mod backend_tests {
    use super::default_backends;
    use wgpu::Backends;

    #[test]
    fn windows_leaves_gl_out_by_default() {
        let b = default_backends(Backends::all(), true);
        assert!(!b.contains(Backends::GL));
        assert!(b.contains(Backends::VULKAN | Backends::DX12));
    }

    #[test]
    fn other_platforms_keep_every_backend() {
        assert_eq!(default_backends(Backends::all(), false), Backends::all());
    }
}
