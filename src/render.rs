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
    /// Surface finish (see `Finish`): 0 is the plain CAD shading.
    pub finish: f32,
}

/// Surface finishes for part appearance.
pub const FINISHES: &[(&str, &str, f32)] = &[("", "Default", 0.0), ("matte", "Matte", 1.0), ("satin", "Satin", 2.0), ("glossy", "Glossy", 3.0), ("metal", "Metal", 4.0), ("glass", "Glass", 5.0)];

pub fn finish_code(name: Option<&str>) -> f32 {
    FINISHES.iter().find(|f| Some(f.0) == name).map(|f| f.2).unwrap_or(0.0)
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

/// An image laid on the model (a decal): RGBA pixels and the quad's corners
/// (bottom-left, bottom-right, top-right, top-left).
#[derive(Clone)]
pub struct DecalDraw {
    /// Identifies the image, so its texture is uploaded once.
    pub key: u64,
    pub rgba: Arc<Vec<u8>>,
    pub size: [u32; 2],
    pub corners: [[f32; 3]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
struct DecalVertex {
    pos: [f32; 3],
    uv: [f32; 2],
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

/// A part mesh in its own coordinates, uploaded to the GPU once and drawn wherever it is
/// placed (assembly instances): moving, hovering or selecting a part then only changes its
/// small per-object uniform, not the geometry.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct ObjVertex {
    pub pos: [f32; 3],
    /// Face index, so the hovered face can be lit without touching the vertices.
    pub face: u32,
    pub normal: [f32; 3],
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct ObjEdge {
    pub a: [f32; 3],
    pub _pad0: f32,
    pub b: [f32; 3],
    pub _pad1: f32,
}

pub struct ObjMesh {
    /// Identifies the geometry (unique for each mesh made), so it is uploaded once.
    pub key: u64,
    pub verts: Vec<ObjVertex>,
    pub indices: Vec<u32>,
    pub edges: Vec<ObjEdge>,
}

impl ObjMesh {
    /// A fresh key for a new mesh.
    pub fn next_key() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn from_body(m: &crate::kernel::BodyMesh) -> ObjMesh {
        let nv = m.positions.len() / 3;
        let mut face = vec![u32::MAX; nv];
        for (t, f) in m.indices.chunks(3).zip(&m.tri_face) {
            for &i in t {
                if let Some(x) = face.get_mut(i as usize) {
                    *x = *f;
                }
            }
        }
        let verts = (0..nv)
            .map(|i| ObjVertex {
                pos: [m.positions[3 * i], m.positions[3 * i + 1], m.positions[3 * i + 2]],
                face: face[i],
                normal: m.normals.get(3 * i..3 * i + 3).map(|n| [n[0], n[1], n[2]]).unwrap_or_default(),
                _pad: 0,
            })
            .collect();
        let edges = m.edges.iter().flat_map(|e| e.pts.windows(2).map(|w| ObjEdge { a: w[0], _pad0: 0.0, b: w[1], _pad1: 0.0 })).collect();
        ObjMesh { key: Self::next_key(), verts, indices: m.indices.clone(), edges }
    }
}

/// One placed copy of an `ObjMesh`.
#[derive(Clone)]
pub struct ObjDraw {
    pub mesh: Arc<ObjMesh>,
    pub model: Mat4,
    pub color: [f32; 4],
    /// Face drawn in `hover_color` (-1: none).
    pub hover_face: i32,
    pub hover_color: [f32; 4],
    pub finish: f32,
    pub edge_color: [f32; 4],
    pub edge_width: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    hover_color: [f32; 4],
    edge_color: [f32; 4],
    /// hover face, finish, edge width
    params: [f32; 4],
}

/// Spacing of the per-object uniforms (the usual minimum uniform offset alignment).
const OBJ_STRIDE: u64 = 256;

#[derive(Default, Clone)]
pub struct SceneData {
    pub version: u64,
    pub opaque: Vec<MeshVertex>,
    pub transparent: Vec<MeshVertex>,
    pub lines: Vec<LineInst>,
    /// Lines drawn without depth testing (overlays such as the sketch being edited).
    pub lines_top: Vec<LineInst>,
    pub points: Vec<PointInst>,
    pub decals: Vec<DecalDraw>,
    /// Placed part meshes (drawn opaque, with their edges).
    pub objects: Vec<ObjDraw>,
}

impl SceneData {
    pub fn tri(&mut self, transparent: bool, p: [Vec3; 3], color: [f32; 4]) {
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        let list = if transparent { &mut self.transparent } else { &mut self.opaque };
        for v in p {
            list.push(MeshVertex { pos: v.into(), normal: n.into(), color, finish: 0.0 });
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
            if top { self.lines_top.push(l) } else { self.lines.push(l) }
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
    /// Studio lighting and reflections.
    pub realistic: bool,
}

struct GpuMesh {
    verts: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    n_indices: u32,
    edges: Option<wgpu::Buffer>,
    n_edges: u32,
}

struct Buf {
    buf: Option<wgpu::Buffer>,
    count: u32,
}

impl Buf {
    fn upload<T: Pod>(&mut self, device: &wgpu::Device, data: &[T], usage: wgpu::BufferUsages) {
        use wgpu::util::DeviceExt;
        self.count = data.len() as u32;
        self.buf = (!data.is_empty()).then(|| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(data), usage }));
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
    decal_pipeline: wgpu::RenderPipeline,
    decal_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: std::collections::HashMap<u64, wgpu::BindGroup>,
    b_decals: Buf,
    /// Texture key of each decal quad in `b_decals`, in order.
    decal_keys: Vec<u64>,
    version: u64,
    b_opaque: Buf,
    b_transparent: Buf,
    b_lines: Buf,
    b_lines_top: Buf,
    b_points: Buf,
    obj_mesh: wgpu::RenderPipeline,
    obj_lines: wgpu::RenderPipeline,
    obj_bgl: wgpu::BindGroupLayout,
    obj_buf: Option<(wgpu::Buffer, wgpu::BindGroup, u64)>,
    /// GPU copies of the `ObjMesh`es in use, by key.
    meshes: std::collections::HashMap<u64, GpuMesh>,
    /// Mesh key of each object of the scene, in order.
    obj_keys: Vec<u64>,
}

const SHADER: &str = r#"
struct U { view_proj: mat4x4<f32>, eye_dir: vec4<f32>, viewport: vec4<f32>, clip: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

fn clip_dist(p: vec3<f32>) -> f32 {
    if (dot(u.clip.xyz, u.clip.xyz) == 0.0) { return 1.0; }
    return u.clip.w - dot(u.clip.xyz, p);
}

struct MOut { @builtin(position) pos: vec4<f32>, @location(0) n: vec3<f32>, @location(1) c: vec4<f32>, @location(2) w: vec3<f32>, @location(3) f: f32 };

@vertex fn vs_mesh(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) c: vec4<f32>, @location(3) f: f32) -> MOut {
    var o: MOut;
    o.pos = u.view_proj * vec4<f32>(p, 1.0);
    o.n = n; o.c = c; o.w = p; o.f = f;
    return o;
}

@fragment fn fs_mesh(i: MOut, @builtin(front_facing) ff: bool) -> @location(0) vec4<f32> {
    if (clip_dist(i.w) < 0.0) { discard; }
    var n = normalize(i.n);
    if (dot(n, n) < 0.5) { return i.c; }
    let v = u.eye_dir.xyz;
    if (dot(n, v) < 0.0) { n = -n; }
    let key = normalize(v + vec3<f32>(0.25, 0.15, 0.45));
    let d = max(dot(n, key), 0.0);
    let h = max(dot(n, v), 0.0);
    let realistic = u.eye_dir.w > 0.5;
    let finish = i3(i.f);
    if (finish == 0 && !realistic) {
        let l = 0.42 + 0.38 * d + 0.25 * pow(h, 6.0);
        return vec4<f32>(i.c.rgb * l, i.c.a);
    }
    // finishes: roughness and metalness
    var rough = 0.5;
    var metal = 0.0;
    if (finish == 1) { rough = 0.9; }
    if (finish == 3) { rough = 0.15; }
    if (finish == 4) { rough = 0.3; metal = 1.0; }
    if (finish == 5) { rough = 0.05; }
    let up = vec3<f32>(0.0, 0.0, 1.0);
    // sky above, darker ground below
    let ambient = mix(vec3<f32>(0.28, 0.27, 0.26), vec3<f32>(0.62, 0.66, 0.72), 0.5 + 0.5 * dot(n, up));
    let fill = normalize(vec3<f32>(-v.y, v.x, 0.3));
    let diffuse = ambient * 0.75 + vec3<f32>(0.62 * d + 0.18 * max(dot(n, fill), 0.0));
    let hv = normalize(key + v);
    let shin = mix(8.0, 300.0, 1.0 - rough);
    let spec = pow(max(dot(n, hv), 0.0), shin) * (1.0 - rough) * 1.4;
    let fres = pow(1.0 - h, 5.0);
    let r = reflect(-v, n);
    // a studio: bright ceiling, a softbox band just above the horizon, dark floor
    let band = exp(-pow((r.z - 0.18) * 5.0, 2.0)) * 0.55;
    let env = mix(vec3<f32>(0.32, 0.31, 0.30), vec3<f32>(0.95, 0.96, 0.98), smoothstep(-0.35, 0.45, r.z)) + vec3<f32>(band);
    let spec_col = mix(vec3<f32>(1.0), i.c.rgb, metal);
    let env_amt = mix(0.08, 0.85, metal) * (1.0 - 0.6 * rough) + fres * 0.25 * (1.0 - rough);
    let base = i.c.rgb * diffuse * (1.0 - 0.55 * metal);
    var col = base + spec_col * (spec + env * env_amt);
    var a = i.c.a;
    if (finish == 5) { a = min(a, 0.35 + 0.5 * fres); }
    return vec4<f32>(col, a);
}

fn i3(f: f32) -> i32 { return i32(f + 0.5); }

struct O { model: mat4x4<f32>, color: vec4<f32>, hover_color: vec4<f32>, edge_color: vec4<f32>, params: vec4<f32> };
@group(2) @binding(0) var<uniform> obj: O;

@vertex fn vs_obj(@location(0) p: vec3<f32>, @location(1) face: u32, @location(2) n: vec3<f32>) -> MOut {
    var r: MOut;
    let w = (obj.model * vec4<f32>(p, 1.0)).xyz;
    r.pos = u.view_proj * vec4<f32>(w, 1.0);
    r.n = (obj.model * vec4<f32>(n, 0.0)).xyz;
    r.c = obj.color;
    if (f32(face) == obj.params.x) { r.c = obj.hover_color; }
    r.w = w; r.f = obj.params.y;
    return r;
}

@vertex fn vs_obj_line(@builtin(vertex_index) vi: u32, @location(0) a: vec3<f32>, @location(1) b: vec3<f32>) -> LOut {
    let wa = (obj.model * vec4<f32>(a, 1.0)).xyz;
    let wb = (obj.model * vec4<f32>(b, 1.0)).xyz;
    return line_corner(vi, wa, wb, obj.params.z, 0.0, obj.edge_color);
}

struct LOut { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32>, @location(1) w: vec3<f32> };

@group(1) @binding(0) var dtex: texture_2d<f32>;
@group(1) @binding(1) var dsamp: sampler;
struct DOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) w: vec3<f32> };
@vertex fn vs_decal(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>) -> DOut {
    var o: DOut;
    o.pos = u.view_proj * vec4<f32>(p, 1.0);
    o.uv = uv; o.w = p;
    return o;
}
@fragment fn fs_decal(i: DOut) -> @location(0) vec4<f32> {
    if (clip_dist(i.w) < 0.0) { discard; }
    let c = textureSample(dtex, dsamp, i.uv);
    if (c.a < 0.02) { discard; }
    return c;
}

@vertex fn vs_line(@builtin(vertex_index) vi: u32,
                   @location(0) a: vec3<f32>, @location(1) width: f32,
                   @location(2) b: vec3<f32>, @location(3) bias: f32,
                   @location(4) c: vec4<f32>) -> LOut {
    return line_corner(vi, a, b, width, bias, c);
}

fn line_corner(vi: u32, a: vec3<f32>, b: vec3<f32>, width: f32, bias: f32, c: vec4<f32>) -> LOut {
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
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("viewport"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
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
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let target = [Some(wgpu::ColorTargetState { format: rs.target_format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })];
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

        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32];
        let mesh_layout = wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<MeshVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &mesh_attrs };
        let mesh = |write: bool, bias: i32, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_mesh"), buffers: &[Some(mesh_layout.clone())], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_mesh"), targets: &target, compilation_options: Default::default() }),
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
        let line_layout = wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<LineInst>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &line_attrs };
        let line = |cmp: wgpu::CompareFunction, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_line"), buffers: &[Some(line_layout.clone())], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_line"), targets: &target, compilation_options: Default::default() }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth(false, cmp, 0),
                multisample: ms,
                multiview_mask: None,
                cache: None,
            })
        };
        let lines = line(wgpu::CompareFunction::LessEqual, "lines");
        let lines_top = line(wgpu::CompareFunction::Always, "lines top");

        // placed part meshes: their pose, colours and hover in a per-object uniform (group 2)
        let obj_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("objects"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<ObjUniform>() as u64) },
                count: None,
            }],
        });
        let obj_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl), None, Some(&obj_bgl)], immediate_size: 0 });
        let obj_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32, 2 => Float32x3];
        let obj_mesh = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("object mesh"),
            layout: Some(&obj_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_obj"),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<ObjVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &obj_attrs })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_mesh"), targets: &target, compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth(true, wgpu::CompareFunction::LessEqual, 2),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        let obj_lines = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("object edges"),
            layout: Some(&obj_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_obj_line"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ObjEdge>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 16, shader_location: 1 },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_line"), targets: &target, compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth(false, wgpu::CompareFunction::LessEqual, 0),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });

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
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_point"), targets: &target, compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth(false, wgpu::CompareFunction::LessEqual, 0),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });

        let decal_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("decal"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let decal_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl), Some(&decal_bgl)], immediate_size: 0 });
        let decal_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];
        let decal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("decals"),
            layout: Some(&decal_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_decal"),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<DecalVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &decal_attrs })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_decal"), targets: &target, compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(),
            // pulled towards the camera so it sits on the face it covers
            depth_stencil: depth(false, wgpu::CompareFunction::LessEqual, -4),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("decal"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let r = Renderer {
            uniform,
            bind,
            decal_pipeline,
            decal_bgl,
            sampler,
            textures: Default::default(),
            b_decals: Buf { buf: None, count: 0 },
            decal_keys: vec![],
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
            obj_mesh,
            obj_lines,
            obj_bgl,
            obj_buf: None,
            meshes: Default::default(),
            obj_keys: vec![],
        };
        rs.renderer.write().callback_resources.insert(r);
    }
}

impl Renderer {
    /// Upload the meshes not on the GPU yet and write every object's uniform.
    fn upload_objects(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, objects: &[ObjDraw]) {
        use wgpu::util::DeviceExt;
        self.obj_keys.clear();
        if objects.is_empty() {
            // other viewports (a Part Studio) draw no objects: keep the meshes for coming back
            return;
        }
        let mut used = std::collections::HashSet::new();
        for o in objects {
            let m = &o.mesh;
            used.insert(m.key);
            self.obj_keys.push(m.key);
            if self.meshes.contains_key(&m.key) || m.verts.is_empty() {
                continue;
            }
            let verts = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("object verts"), contents: bytemuck::cast_slice(&m.verts), usage: wgpu::BufferUsages::VERTEX });
            let indices = (!m.indices.is_empty()).then(|| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("object indices"), contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX }));
            let edges = (!m.edges.is_empty()).then(|| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("object edges"), contents: bytemuck::cast_slice(&m.edges), usage: wgpu::BufferUsages::VERTEX }));
            self.meshes.insert(m.key, GpuMesh { verts, indices, n_indices: m.indices.len() as u32, edges, n_edges: m.edges.len() as u32 });
        }
        self.meshes.retain(|k, _| used.contains(k));
        let need = objects.len() as u64 * OBJ_STRIDE;
        if self.obj_buf.as_ref().map(|b| b.2 < need).unwrap_or(true) {
            let cap = need.next_power_of_two().max(OBJ_STRIDE * 64);
            let buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("object uniforms"), size: cap, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("objects"),
                layout: &self.obj_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &buf, offset: 0, size: wgpu::BufferSize::new(std::mem::size_of::<ObjUniform>() as u64) }) }],
            });
            self.obj_buf = Some((buf, bind, cap));
        }
        let mut bytes = vec![0u8; need as usize];
        for (i, o) in objects.iter().enumerate() {
            let u = ObjUniform {
                model: o.model.to_cols_array_2d(),
                color: o.color,
                hover_color: o.hover_color,
                edge_color: o.edge_color,
                params: [o.hover_face as f32, o.finish, o.edge_width, 0.0],
            };
            let at = i * OBJ_STRIDE as usize;
            bytes[at..at + std::mem::size_of::<ObjUniform>()].copy_from_slice(bytemuck::bytes_of(&u));
        }
        if let Some((buf, _, _)) = &self.obj_buf {
            queue.write_buffer(buf, 0, &bytes);
        }
    }

    /// Draw the objects' faces, or (`edges`) their edges.
    fn paint_objects(&self, rp: &mut wgpu::RenderPass<'static>, edges: bool) {
        let Some((_, bind, _)) = &self.obj_buf else { return };
        if self.obj_keys.is_empty() {
            return;
        }
        rp.set_pipeline(if edges { &self.obj_lines } else { &self.obj_mesh });
        for (i, k) in self.obj_keys.iter().enumerate() {
            let Some(m) = self.meshes.get(k) else { continue };
            rp.set_bind_group(2, bind, &[(i as u64 * OBJ_STRIDE) as u32]);
            if edges {
                if let Some(e) = &m.edges {
                    rp.set_vertex_buffer(0, e.slice(..));
                    rp.draw(0..6, 0..m.n_edges);
                }
            } else if let Some(ix) = &m.indices {
                rp.set_vertex_buffer(0, m.verts.slice(..));
                rp.set_index_buffer(ix.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..m.n_indices, 0, 0..1);
            }
        }
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
            // decals: one textured quad each, textures uploaded once per image
            let mut verts: Vec<DecalVertex> = vec![];
            r.decal_keys.clear();
            for d in &self.scene.decals {
                if !r.textures.contains_key(&d.key) {
                    let tex = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("decal"),
                        size: wgpu::Extent3d { width: d.size[0].max(1), height: d.size[1].max(1), depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        &d.rgba,
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * d.size[0]), rows_per_image: Some(d.size[1]) },
                        wgpu::Extent3d { width: d.size[0], height: d.size[1], depth_or_array_layers: 1 },
                    );
                    let view = tex.create_view(&Default::default());
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("decal"),
                        layout: &r.decal_bgl,
                        entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&r.sampler) }],
                    });
                    r.textures.insert(d.key, bg);
                }
                let c = d.corners;
                let uv = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
                for k in [0, 1, 2, 0, 2, 3] {
                    verts.push(DecalVertex { pos: c[k], uv: uv[k] });
                }
                r.decal_keys.push(d.key);
            }
            r.b_decals.upload(device, &verts, v);
            r.upload_objects(device, queue, &self.scene.objects);
            r.version = self.scene.version;
        }
        let _ = sd;
        let u = Uniforms {
            view_proj: self.view_proj.to_cols_array_2d(),
            eye_dir: self.eye_dir.extend(if self.realistic { 1.0 } else { 0.0 }).into(),
            viewport: [self.size_px[0], self.size_px[1], self.pixels_per_point, 0.0],
            clip: self.clip,
        };
        queue.write_buffer(&r.uniform, 0, bytemuck::bytes_of(&u));
        vec![]
    }

    fn paint(&self, info: egui::PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>, res: &egui_wgpu::CallbackResources) {
        let Some(r) = res.get::<Renderer>() else { return };
        let _ = info;
        rp.set_bind_group(0, &r.bind, &[]);
        if let Some(b) = &r.b_opaque.buf {
            rp.set_pipeline(&r.mesh_opaque);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..r.b_opaque.count, 0..1);
        }
        r.paint_objects(rp, false);
        if let Some(b) = &r.b_decals.buf {
            if !r.decal_keys.is_empty() {
                rp.set_pipeline(&r.decal_pipeline);
                rp.set_vertex_buffer(0, b.slice(..));
                for (i, k) in r.decal_keys.iter().enumerate() {
                    if let Some(bg) = r.textures.get(k) {
                        rp.set_bind_group(1, bg, &[]);
                        rp.draw(i as u32 * 6..i as u32 * 6 + 6, 0..1);
                    }
                }
                rp.set_bind_group(0, &r.bind, &[]);
            }
        }
        if let Some(b) = &r.b_lines.buf {
            rp.set_pipeline(&r.lines);
            rp.set_vertex_buffer(0, b.slice(..));
            rp.draw(0..6, 0..r.b_lines.count);
        }
        r.paint_objects(rp, true);
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
