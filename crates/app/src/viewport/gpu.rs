//! wgpu renderer for the 3D viewport, hooked into egui with an `egui_wgpu` paint
//! callback. `prepare` renders the scene into an offscreen 4×MSAA colour + depth
//! target (so egui's own pass needs no depth buffer), `paint` blits the resolved
//! image into egui's render pass.
//!
//! Meshes are uploaded once per shape: the cache is keyed by the `Arc<Mesh>`
//! (which it keeps alive), so a regeneration that reuses a body's geometry
//! uploads nothing and orbiting only rewrites a few uniforms. Triangles are
//! sorted by face so a face highlight or x-ray is one ranged draw.

use std::collections::HashMap;
use std::sync::Arc;

use casework_model::occt::Mesh;
use eframe::egui_wgpu::{self, wgpu, CallbackResources, CallbackTrait, ScreenDescriptor};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SLOT: u64 = 256;
const MAX_DRAWS: u64 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DisplayMode {
    Shaded,
    #[default]
    ShadedEdges,
    Wireframe,
}

#[derive(Clone)]
pub struct DrawBody {
    pub mesh: Arc<Mesh>,
    /// sRGB-encoded RGBA, 0..1.
    pub color: [f32; 4],
    /// Drawn translucent after the opaque bodies (references, ghosts).
    pub translucent: bool,
}

#[derive(Clone)]
pub struct FaceHighlight {
    pub mesh: Arc<Mesh>,
    /// Face id, or `u32::MAX` for the whole body.
    pub face: u32,
    pub color: [f32; 4],
    /// Draw through everything in front (hidden pick candidate).
    pub xray: bool,
}

#[derive(Clone, Copy)]
pub struct Grid {
    pub z: f32,
    pub spacing: f32,
    pub extent: f32,
    pub center: [f32; 2],
}

/// Everything one frame of the viewport needs.
#[derive(Clone)]
pub struct Frame3D {
    pub size_px: [u32; 2],
    pub view_proj: Mat4,
    pub eye: Vec3,
    pub forward: Vec3,
    pub ortho: bool,
    pub light: Vec3,
    pub background: [f32; 4],
    pub mode: DisplayMode,
    pub edge_color: [f32; 4],
    pub bodies: Vec<DrawBody>,
    pub highlights: Vec<FaceHighlight>,
    pub grid: Option<Grid>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    light: [f32; 4],
    grid: [f32; 4],
    grid_center: [f32; 4],
    view_dir: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    color: [f32; 4],
    params: [f32; 4],
}

struct GpuMesh {
    _keep: Arc<Mesh>,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    faces: HashMap<u32, (u32, u32)>,
    ebuf: Option<wgpu::Buffer>,
    edge_verts: u32,
    last_used: u64,
}

struct Target {
    size: [u32; 2],
    msaa_color: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
    resolve: wgpu::TextureView,
    blit_bind: wgpu::BindGroup,
}

/// GPU state, stored in egui_wgpu's callback resources.
pub struct ViewportGpu {
    msaa: u32,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    draws: wgpu::Buffer,
    draws_bind: wgpu::BindGroup,
    opaque: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    xray: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    meshes: HashMap<usize, GpuMesh>,
    target: Option<Target>,
    frame: u64,
    /// Bytes uploaded for meshes so far (for the status bar / tests).
    pub uploaded_meshes: u64,
}

fn key(m: &Arc<Mesh>) -> usize {
    Arc::as_ptr(m) as usize
}

impl ViewportGpu {
    /// Create the pipelines and register them with egui's renderer.
    pub fn install(rs: &egui_wgpu::RenderState) {
        let gpu = ViewportGpu::new(&rs.device, &rs.adapter, rs.target_format);
        rs.renderer.write().callback_resources.insert(gpu);
    }

    pub fn new(
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let ok4 = |f: wgpu::TextureFormat| {
            adapter
                .get_texture_format_features(f)
                .flags
                .sample_count_supported(4)
        };
        let msaa = if ok4(COLOR_FORMAT) && ok4(DEPTH_FORMAT) {
            4
        } else {
            1
        };

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("casework viewport"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("casework blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });

        let uniform_layout = |dynamic: bool, size: u64| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: dynamic,
                        min_binding_size: wgpu::BufferSize::new(size),
                    },
                    count: None,
                }],
            })
        };
        let globals_layout = uniform_layout(false, std::mem::size_of::<Globals>() as u64);
        let draw_layout = uniform_layout(true, std::mem::size_of::<DrawUniform>() as u64);

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draws = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("draws"),
            size: SLOT * MAX_DRAWS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let draws_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &draw_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &draws,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<DrawUniform>() as u64),
                }),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_layout), Some(&draw_layout)],
            immediate_size: 0,
        });

        let mesh_vb = [Some(wgpu::VertexBufferLayout {
            array_stride: 24,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        })];
        let line_vb = [Some(wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
        })];
        let blend_state = Some(wgpu::BlendState::ALPHA_BLENDING);
        let bias = wgpu::DepthBiasState {
            constant: 2,
            slope_scale: 1.0,
            clamp: 0.0,
        };
        let make = |label: &str,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout>],
                    topology: wgpu::PrimitiveTopology,
                    write: bool,
                    compare: wgpu::CompareFunction,
                    bias: wgpu::DepthBiasState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(write),
                    depth_compare: Some(compare),
                    stencil: Default::default(),
                    bias,
                }),
                multisample: wgpu::MultisampleState {
                    count: msaa,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: blend_state,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::CompareFunction as C;
        use wgpu::PrimitiveTopology as T;
        let none = wgpu::DepthBiasState::default();
        let opaque = make(
            "mesh opaque",
            "vs_mesh",
            "fs_mesh",
            &mesh_vb,
            T::TriangleList,
            true,
            C::Less,
            bias,
        );
        let blend = make(
            "mesh blend",
            "vs_mesh",
            "fs_mesh",
            &mesh_vb,
            T::TriangleList,
            false,
            C::LessEqual,
            bias,
        );
        let xray = make(
            "mesh xray",
            "vs_mesh",
            "fs_mesh",
            &mesh_vb,
            T::TriangleList,
            false,
            C::Always,
            none,
        );
        let lines = make(
            "edges",
            "vs_line",
            "fs_flat",
            &line_vb,
            T::LineList,
            true,
            C::LessEqual,
            none,
        );
        let grid = make(
            "grid",
            "vs_grid",
            "fs_grid",
            &[],
            T::TriangleList,
            false,
            C::Less,
            none,
        );

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
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
        });
        let blit_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport blit"),
            layout: Some(&blit_pl),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some(if target_format.is_srgb() {
                    "fs_blit_srgb"
                } else {
                    "fs_blit"
                }),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("viewport"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        ViewportGpu {
            msaa,
            globals,
            globals_bind,
            draws,
            draws_bind,
            opaque,
            blend,
            xray,
            lines,
            grid,
            blit,
            blit_layout,
            sampler,
            meshes: HashMap::new(),
            target: None,
            frame: 0,
            uploaded_meshes: 0,
        }
    }

    pub fn msaa(&self) -> u32 {
        self.msaa
    }
    pub fn cached_meshes(&self) -> usize {
        self.meshes.len()
    }

    fn upload(&mut self, device: &wgpu::Device, m: &Arc<Mesh>) {
        let k = key(m);
        if let Some(g) = self.meshes.get_mut(&k) {
            g.last_used = self.frame;
            return;
        }
        let mut verts: Vec<f32> = Vec::with_capacity(m.positions.len() * 6);
        for (i, p) in m.positions.iter().enumerate() {
            let n = m.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]);
            verts.extend_from_slice(p);
            verts.extend_from_slice(&n);
        }
        // triangles sorted by face → contiguous ranges
        let mut order: Vec<u32> = (0..m.indices.len() as u32).collect();
        order.sort_by_key(|t| m.tri_face.get(*t as usize).copied().unwrap_or(0));
        let mut idx: Vec<u32> = Vec::with_capacity(m.indices.len() * 3);
        let mut faces: HashMap<u32, (u32, u32)> = HashMap::new();
        for t in order {
            let f = m.tri_face.get(t as usize).copied().unwrap_or(0);
            let e = faces.entry(f).or_insert((idx.len() as u32, 0));
            e.1 += 3;
            idx.extend_from_slice(&m.indices[t as usize]);
        }
        let mut lines: Vec<f32> = Vec::new();
        for e in &m.edges {
            for w in e.points.windows(2) {
                lines.extend_from_slice(&w[0]);
                lines.extend_from_slice(&w[1]);
            }
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh vertices"),
            contents: bytemuck::cast_slice(if verts.is_empty() {
                &[0.0f32; 6][..]
            } else {
                &verts
            }),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh indices"),
            contents: bytemuck::cast_slice(if idx.is_empty() { &[0u32; 3][..] } else { &idx }),
            usage: wgpu::BufferUsages::INDEX,
        });
        let ebuf = (!lines.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh edges"),
                contents: bytemuck::cast_slice(&lines),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.uploaded_meshes += 1;
        self.meshes.insert(
            k,
            GpuMesh {
                _keep: m.clone(),
                vbuf,
                ibuf,
                index_count: idx.len() as u32,
                faces,
                ebuf,
                edge_verts: (lines.len() / 3) as u32,
                last_used: self.frame,
            },
        );
    }

    fn ensure_target(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        let size = [size[0].max(1), size[1].max(1)];
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let ext = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let tex =
            |label: &str, format: wgpu::TextureFormat, samples: u32, usage: wgpu::TextureUsages| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: ext,
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let resolve = tex(
            "viewport resolve",
            COLOR_FORMAT,
            1,
            rt | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let msaa_color = (self.msaa > 1).then(|| tex("viewport msaa", COLOR_FORMAT, self.msaa, rt));
        let depth = tex("viewport depth", DEPTH_FORMAT, self.msaa, rt);
        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolve),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.target = Some(Target {
            size,
            msaa_color,
            depth,
            resolve,
            blit_bind,
        });
    }

    /// Render one frame into the offscreen target.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        f: &Frame3D,
    ) {
        self.frame += 1;
        self.ensure_target(device, f.size_px);
        for b in &f.bodies {
            self.upload(device, &b.mesh);
        }
        for h in &f.highlights {
            self.upload(device, &h.mesh);
        }
        // evict meshes unused for a while (keeps undo/redo flips cheap)
        let frame = self.frame;
        self.meshes.retain(|_, m| frame - m.last_used < 600);

        let gr = f.grid.unwrap_or(Grid {
            z: 0.0,
            spacing: 10.0,
            extent: 1.0,
            center: [0.0, 0.0],
        });
        let globals = Globals {
            view_proj: f.view_proj.to_cols_array_2d(),
            eye: [f.eye.x, f.eye.y, f.eye.z, if f.ortho { 1.0 } else { 0.0 }],
            light: [f.light.x, f.light.y, f.light.z, 0.0],
            grid: [gr.z, gr.spacing, gr.extent, gr.extent * 0.95],
            grid_center: [gr.center[0], gr.center[1], 0.0, 0.0],
            view_dir: [f.forward.x, f.forward.y, f.forward.z, 0.0],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        // Draw list: (pipeline, mesh key, what, uniform)
        enum What {
            All,
            Face(u32, u32),
            Edges,
            Grid,
        }
        #[derive(Clone, Copy)]
        enum P {
            Opaque,
            Blend,
            Xray,
            Lines,
            Grid,
        }
        let mut list: Vec<(P, usize, What, DrawUniform)> = Vec::new();
        let u = |c: [f32; 4], lit: f32| DrawUniform {
            color: c,
            params: [lit, 0.0, 0.0, 0.0],
        };
        let wire = f.mode == DisplayMode::Wireframe;
        for b in f.bodies.iter().filter(|b| !b.translucent) {
            if wire {
                continue;
            }
            list.push((P::Opaque, key(&b.mesh), What::All, u(b.color, 1.0)));
        }
        if f.mode != DisplayMode::Shaded {
            let ec = if wire {
                [0.78, 0.80, 0.83, 1.0]
            } else {
                f.edge_color
            };
            for b in &f.bodies {
                let c = if b.translucent {
                    [b.color[0], b.color[1], b.color[2], 0.9]
                } else {
                    ec
                };
                list.push((P::Lines, key(&b.mesh), What::Edges, u(c, 0.0)));
            }
        }
        if f.grid.is_some() {
            list.push((P::Grid, 0, What::Grid, u([0.0; 4], 0.0)));
        }
        for b in &f.bodies {
            if b.translucent {
                list.push((P::Blend, key(&b.mesh), What::All, u(b.color, 0.6)));
            } else if wire {
                let mut c = b.color;
                c[3] = 0.06;
                list.push((P::Blend, key(&b.mesh), What::All, u(c, 0.0)));
            }
        }
        for h in &f.highlights {
            if let Some(g) = self.meshes.get(&key(&h.mesh)) {
                let range = if h.face == u32::MAX {
                    Some((0, g.index_count))
                } else {
                    g.faces.get(&h.face).copied()
                };
                if let Some((s, n)) = range {
                    let p = if h.xray { P::Xray } else { P::Blend };
                    list.push((p, key(&h.mesh), What::Face(s, n), u(h.color, 0.0)));
                }
            }
        }
        list.truncate(MAX_DRAWS as usize);
        let mut bytes = vec![0u8; list.len().max(1) * SLOT as usize];
        for (i, (_, _, _, du)) in list.iter().enumerate() {
            let o = i * SLOT as usize;
            bytes[o..o + std::mem::size_of::<DrawUniform>()]
                .copy_from_slice(bytemuck::bytes_of(du));
        }
        queue.write_buffer(&self.draws, 0, &bytes);

        let t = self.target.as_ref().expect("target");
        let bg = f.background;
        let (view, resolve) = match &t.msaa_color {
            Some(m) => (m, Some(&t.resolve)),
            None => (&t.resolve, None),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("casework viewport"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: bg[0] as f64,
                        g: bg[1] as f64,
                        b: bg[2] as f64,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.globals_bind, &[]);
        for (i, (p, k, what, _)) in list.iter().enumerate() {
            let pipe = match p {
                P::Opaque => &self.opaque,
                P::Blend => &self.blend,
                P::Xray => &self.xray,
                P::Lines => &self.lines,
                P::Grid => &self.grid,
            };
            pass.set_pipeline(pipe);
            pass.set_bind_group(1, &self.draws_bind, &[(i as u64 * SLOT) as u32]);
            match what {
                What::Grid => pass.draw(0..6, 0..1),
                What::Edges => {
                    let Some(g) = self.meshes.get(k) else {
                        continue;
                    };
                    if let Some(e) = &g.ebuf {
                        pass.set_vertex_buffer(0, e.slice(..));
                        pass.draw(0..g.edge_verts, 0..1);
                    }
                }
                What::All | What::Face(..) => {
                    let Some(g) = self.meshes.get(k) else {
                        continue;
                    };
                    let (s, n) = match what {
                        What::Face(s, n) => (*s, *n),
                        _ => (0, g.index_count),
                    };
                    if n == 0 {
                        continue;
                    }
                    pass.set_vertex_buffer(0, g.vbuf.slice(..));
                    pass.set_index_buffer(g.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(s..s + n, 0, 0..1);
                }
            }
        }
    }

    fn paint(&self, pass: &mut wgpu::RenderPass<'static>) {
        if let Some(t) = &self.target {
            pass.set_pipeline(&self.blit);
            pass.set_bind_group(0, &t.blit_bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// The per-frame paint callback.
pub struct ViewportCallback {
    pub frame: Arc<Frame3D>,
}

impl CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<ViewportGpu>() {
            gpu.render(device, queue, encoder, &self.frame);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<ViewportGpu>() {
            gpu.paint(pass);
        }
    }
}

pub fn srgba(c: egui::Color32) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}
