//! wgpu viewport renderer drawn inside egui's render pass via a paint callback.
//!
//! The scene is a flat list of triangles (shaded, optionally translucent), thick
//! screen-space line segments and round points. Geometry is uploaded only when
//! the scene's `version` changes; the camera uniform is written every frame.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};

pub const MSAA: u32 = 4;
pub const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct MeshVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct LineInst {
    pub a: [f32; 3],
    pub width: f32,
    pub b: [f32; 3],
    /// Depth pull towards the camera in NDC units (lines over faces).
    pub bias: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct PointInst {
    pub p: [f32; 3],
    pub size: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    eye_dir: [f32; 4],
    viewport: [f32; 4],
    /// xyz normal, w offset; w = 0 and normal 0 disables clipping
    clip: [f32; 4],
}

#[derive(Default, Clone)]
pub struct SceneData {
    pub version: u64,
    pub opaque: Vec<MeshVertex>,
    pub transparent: Vec<MeshVertex>,
    pub lines: Vec<LineInst>,
    /// Lines drawn without depth testing (overlays such as the sketch being edited).
    pub lines_top: Vec<LineInst>,
    pub points: Vec<PointInst>,
}

impl SceneData {
    pub fn tri(&mut self, transparent: bool, p: [Vec3; 3], color: [f32; 4]) {
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        let list = if transparent { &mut self.transparent } else { &mut self.opaque };
        for v in p {
            list.push(MeshVertex { pos: v.into(), normal: n.into(), color });
        }
    }

    pub fn quad(&mut self, transparent: bool, p: [Vec3; 4], color: [f32; 4]) {
        self.tri(transparent, [p[0], p[1], p[2]], color);
        self.tri(transparent, [p[0], p[2], p[3]], color);
    }

    pub fn line(&mut self, a: Vec3, b: Vec3, width: f32, color: [f32; 4]) {
        self.lines.push(LineInst { a: a.into(), b: b.into(), width, bias: 0.0, color });
    }

    pub fn polyline(&mut self, pts: &[Vec3], width: f32, color: [f32; 4], top: bool) {
        for w in pts.windows(2) {
            let l = LineInst { a: w[0].into(), b: w[1].into(), width, bias: 0.0, color };
            if top {
                self.lines_top.push(l)
            } else {
                self.lines.push(l)
            }
        }
    }

    pub fn point(&mut self, p: Vec3, size: f32, color: [f32; 4]) {
        self.points.push(PointInst { p: p.into(), size, color });
    }
}

pub struct ViewportCallback {
    pub scene: Arc<SceneData>,
    pub view_proj: Mat4,
    pub eye_dir: Vec3,
    pub clip: [f32; 4],
    /// Viewport size in physical pixels.
    pub size_px: [f32; 2],
    pub pixels_per_point: f32,
}

struct Buf {
    buf: Option<wgpu::Buffer>,
    count: u32,
}

impl Buf {
    fn upload<T: Pod>(&mut self, device: &wgpu::Device, data: &[T], usage: wgpu::BufferUsages) {
        use wgpu::util::DeviceExt;
        self.count = data.len() as u32;
        self.buf = (!data.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(data),
                usage,
            })
        });
    }
}

pub struct Renderer {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    mesh_opaque: wgpu::RenderPipeline,
    mesh_transparent: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    lines_top: wgpu::RenderPipeline,
    points: wgpu::RenderPipeline,
    version: u64,
    b_opaque: Buf,
    b_transparent: Buf,
    b_lines: Buf,
    b_lines_top: Buf,
    b_points: Buf,
}

const SHADER: &str = r#"
struct U { view_proj: mat4x4<f32>, eye_dir: vec4<f32>, viewport: vec4<f32>, clip: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

fn clip_dist(p: vec3<f32>) -> f32 {
    if (dot(u.clip.xyz, u.clip.xyz) == 0.0) { return 1.0; }
    return u.clip.w - dot(u.clip.xyz, p);
}

struct MOut { @builtin(position) pos: vec4<f32>, @location(0) n: vec3<f32>, @location(1) c: vec4<f32>, @location(2) w: vec3<f32> };

@vertex fn vs_mesh(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) c: vec4<f32>) -> MOut {
    var o: MOut;
    o.pos = u.view_proj * vec4<f32>(p, 1.0);
    o.n = n; o.c = c; o.w = p;
    return o;
}

@fragment fn fs_mesh(i: MOut, @builtin(front_facing) ff: bool) -> @location(0) vec4<f32> {
    if (clip_dist(i.w) < 0.0) { discard; }
    var n = normalize(i.n);
    if (dot(n, n) < 0.5) { return i.c; }
    if (dot(n, u.eye_dir.xyz) < 0.0) { n = -n; }
    let key = normalize(u.eye_dir.xyz + vec3<f32>(0.25, 0.15, 0.45));
    let d = max(dot(n, key), 0.0);
    let h = max(dot(n, u.eye_dir.xyz), 0.0);
    let l = 0.42 + 0.38 * d + 0.25 * pow(h, 6.0);
    return vec4<f32>(i.c.rgb * l, i.c.a);
}

struct LOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32>, @location(1) w: vec3<f32> };

@vertex fn vs_line(@builtin(vertex_index) vi: u32,
                   @location(0) a: vec3<f32>, @location(1) width: f32,
                   @location(2) b: vec3<f32>, @location(3) bias: f32,
                   @location(4) c: vec4<f32>) -> LOut {
    let ca = u.view_proj * vec4<f32>(a, 1.0);
    let cb = u.view_proj * vec4<f32>(b, 1.0);
    let sa = ca.xy / ca.w * u.viewport.xy * 0.5;
    let sb = cb.xy / cb.w * u.viewport.xy * 0.5;
    var dir = sb - sa;
    if (length(dir) < 1e-6) { dir = vec2<f32>(1.0, 0.0); }
    dir = normalize(dir);
    let nrm = vec2<f32>(-dir.y, dir.x);
    // corners: 0..5 -> (end, side)
    var ends = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    var sides = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    let e = ends[vi];
    let s = sides[vi];
    var cp = mix(ca, cb, e);
    let off = (nrm * s * width * 0.5 + dir * (e * 2.0 - 1.0) * width * 0.5) * u.viewport.z;
    cp = vec4<f32>(cp.xy + off / (u.viewport.xy * 0.5) * cp.w, cp.z - bias * cp.w, cp.w);
    var o: LOut;
    o.pos = cp; o.c = c; o.w = mix(a, b, e);
    return o;
}

@fragment fn fs_line(i: LOut) -> @location(0) vec4<f32> {
    if (clip_dist(i.w) < -1e-3) { discard; }
    return i.c;
}

struct POut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32>, @location(1) uv: vec2<f32> };

@vertex fn vs_point(@builtin(vertex_index) vi: u32, @location(0) p: vec3<f32>, @location(1) size: f32, @location(2) c: vec4<f32>) -> POut {
    var xs = array<f32, 6>(-1.0, 1.0, 1.0, -1.0, 1.0, -1.0);
    var ys = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    var cp = u.view_proj * vec4<f32>(p, 1.0);
    let uv = vec2<f32>(xs[vi], ys[vi]);
    let off = uv * size * 0.5 * u.viewport.z / (u.viewport.xy * 0.5) * cp.w;
    var o: POut;
    o.pos = vec4<f32>(cp.xy + off, cp.z - 0.000002 * cp.w, cp.w);
    o.c = c; o.uv = uv;
    return o;
}

@fragment fn fs_point(i: POut) -> @location(0) vec4<f32> {
    if (dot(i.uv, i.uv) > 1.0) { discard; }
    return i.c;
}
"#;

impl Renderer {
    pub fn install(rs: &egui_wgpu::RenderState) {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let target = [Some(wgpu::ColorTargetState {
            format: rs.target_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let depth = |write: bool, cmp: wgpu::CompareFunction, bias: i32| {
            Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(write),
                depth_compare: Some(cmp),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState { constant: bias, slope_scale: if bias != 0 { 1.5 } else { 0.0 }, clamp: 0.0 },
            })
        };
        let ms = wgpu::MultisampleState { count: MSAA, mask: !0, alpha_to_coverage_enabled: false };

        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
        let mesh_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &mesh_attrs,
        };
        let mesh = |write: bool, bias: i32, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_mesh"),
                    buffers: &[Some(mesh_layout.clone())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_mesh"),
                    targets: &target,
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth(write, wgpu::CompareFunction::LessEqual, bias),
                multisample: ms,
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh_opaque = mesh(true, 2, "mesh opaque");
        let mesh_transparent = mesh(false, 2, "mesh transparent");

        let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x3, 3 => Float32, 4 => Float32x4];
        let line_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineInst>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &line_attrs,
        };
        let line = |cmp: wgpu::CompareFunction, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_line"),
                    buffers: &[Some(line_layout.clone())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_line"),
                    targets: &target,
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth(false, cmp, 0),
                multisample: ms,
                multiview_mask: None,
                cache: None,
            })
        };
        let lines = line(wgpu::CompareFunction::LessEqual, "lines");
        let lines_top = line(wgpu::CompareFunction::Always, "lines top");

        let point_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x4];
        let points = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("points"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_point"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PointInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &point_attrs,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_point"),
                targets: &target,
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth(false, wgpu::CompareFunction::LessEqual, 0),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });

        let r = Renderer {
            uniform,
            bind,
            mesh_opaque,
            mesh_transparent,
            lines,
            lines_top,
            points,
            version: u64::MAX,
            b_opaque: Buf { buf: None, count: 0 },
            b_transparent: Buf { buf: None, count: 0 },
            b_lines: Buf { buf: None, count: 0 },
            b_lines_top: Buf { buf: None, count: 0 },
            b_points: Buf { buf: None, count: 0 },
        };
        rs.renderer.write().callback_resources.insert(r);
    }
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sd: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = res.get_mut::<Renderer>() else { return vec![] };
        if r.version != self.scene.version {
            let v = wgpu::BufferUsages::VERTEX;
            r.b_opaque.upload(device, &self.scene.opaque, v);
            r.b_transparent.upload(device, &self.scene.transparent, v);
            r.b_lines.upload(device, &self.scene.lines, v);
            r.b_lines_top.upload(device, &self.scene.lines_top, v);
            r.b_points.upload(device, &self.scene.points, v);
            r.version = self.scene.version;
        }
        let _ = sd;
        let u = Uniforms {
            view_proj: self.view_proj.to_cols_array_2d(),
            eye_dir: self.eye_dir.extend(0.0).into(),
            viewport: [self.size_px[0], self.size_px[1], self.pixels_per_point, 0.0],
            clip: self.clip,
        };
        queue.write_buffer(&r.uniform, 0, bytemuck::bytes_of(&u));
        vec![]
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        rp: &mut wgpu::RenderPass<'static>,
        res: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = res.get::<Renderer>() else { return };
        let _ = info;
        rp.set_bind_group(0, &r.bind, &[]);
        if let Some(b) = &r.b_opaque.buf {
            rp.set_pipeline(&r.mesh_opaque);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..r.b_opaque.count, 0..1);
        }
        if let Some(b) = &r.b_lines.buf {
            rp.set_pipeline(&r.lines);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..6, 0..r.b_lines.count);
        }
        if let Some(b) = &r.b_transparent.buf {
            rp.set_pipeline(&r.mesh_transparent);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..r.b_transparent.count, 0..1);
        }
        if let Some(b) = &r.b_lines_top.buf {
            rp.set_pipeline(&r.lines_top);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..6, 0..r.b_lines_top.count);
        }
        if let Some(b) = &r.b_points.buf {
            rp.set_pipeline(&r.points);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..6, 0..r.b_points.count);
        }
    }
}
