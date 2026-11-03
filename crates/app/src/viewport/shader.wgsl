// Casework viewport shaders. Colours arrive sRGB-encoded (like egui's); lighting
// runs in linear space and the result is re-encoded, so the offscreen target
// (Rgba8Unorm) holds the same gamma-space values egui itself would write.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,      // xyz eye position, w = 1 for ortho
    light: vec4<f32>,    // xyz key light direction (towards the light)
    grid: vec4<f32>,     // x = plane z, y = minor spacing, z = extent, w = fade radius
    grid_center: vec4<f32>,
    view_dir: vec4<f32>, // xyz camera forward
};
@group(0) @binding(0) var<uniform> g: Globals;

struct Draw {
    color: vec4<f32>,
    // x = lighting amount (0 flat .. 1 lit), y = use vertex colours
    params: vec4<f32>,
    // model matrix: component frame -> view frame (instances, explode, gizmo drags)
    model: mat4x4<f32>,
};
@group(1) @binding(0) var<uniform> d: Draw;

fn to_lin(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

struct MeshOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world: vec3<f32>,
    @location(2) vcolor: vec3<f32>,
};

@vertex
fn vs_mesh(@location(0) p0: vec3<f32>, @location(1) n: vec3<f32>, @location(2) c: vec3<f32>) -> MeshOut {
    var o: MeshOut;
    let p = (d.model * vec4<f32>(p0, 1.0)).xyz;
    o.pos = g.view_proj * vec4<f32>(p, 1.0);
    o.normal = (d.model * vec4<f32>(n, 0.0)).xyz;
    o.world = p;
    o.vcolor = c;
    return o;
}

@fragment
fn fs_mesh(i: MeshOut) -> @location(0) vec4<f32> {
    var n = normalize(i.normal);
    var v = normalize(g.eye.xyz - i.world);
    if (g.eye.w > 0.5) {
        v = -g.view_dir.xyz;
    }
    if (dot(n, v) < 0.0) {
        n = -n;
    }
    let base = to_lin(mix(d.color.rgb, i.vcolor, d.params.y));
    let l = normalize(g.light.xyz);
    let diff = max(dot(n, l), 0.0);
    let fill = max(dot(n, v), 0.0);
    let hemi = 0.5 + 0.5 * n.z;
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.12;
    let lit = base * (0.10 + 0.12 * hemi + 0.48 * diff + 0.32 * fill) + vec3<f32>(spec);
    let c = mix(base, lit, d.params.x);
    return vec4<f32>(to_srgb(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))), d.color.a);
}

@vertex
fn vs_line(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    return g.view_proj * (d.model * vec4<f32>(p, 1.0));
}

@fragment
fn fs_flat() -> @location(0) vec4<f32> {
    return d.color;
}

// ---------------------------------------------------------------- ground grid

struct GridOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec2<f32>,
};

@vertex
fn vs_grid(@builtin(vertex_index) vi: u32) -> GridOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let c = corners[vi] * g.grid.z + g.grid_center.xy;
    var o: GridOut;
    o.pos = g.view_proj * vec4<f32>(c, g.grid.x, 1.0);
    o.world = c;
    return o;
}

fn grid_line(p: vec2<f32>, spacing: f32) -> f32 {
    let q = p / spacing;
    let w = fwidth(q);
    let gd = abs(fract(q - 0.5) - 0.5) / max(w, vec2<f32>(1e-5));
    return 1.0 - min(min(gd.x, gd.y), 1.0);
}

@fragment
fn fs_grid(i: GridOut) -> @location(0) vec4<f32> {
    let minor = grid_line(i.world, g.grid.y);
    let major = grid_line(i.world, g.grid.y * 10.0);
    let r = length(i.world - g.grid_center.xy);
    let fade = clamp(1.0 - r / g.grid.w, 0.0, 1.0);
    var a = max(minor * 0.35, major * 0.75) * fade;
    // X axis red, Y axis green
    let w = fwidth(i.world);
    let on_x = 1.0 - min(abs(i.world.y) / max(w.y, 1e-5), 1.0);
    let on_y = 1.0 - min(abs(i.world.x) / max(w.x, 1e-5), 1.0);
    var col = vec3<f32>(0.20, 0.22, 0.25);
    if (on_x > 0.01) {
        col = mix(col, vec3<f32>(0.70, 0.30, 0.28), on_x);
        a = max(a, on_x * 0.8 * fade);
    }
    if (on_y > 0.01) {
        col = mix(col, vec3<f32>(0.34, 0.62, 0.43), on_y);
        a = max(a, on_y * 0.8 * fade);
    }
    return vec4<f32>(col, a);
}

