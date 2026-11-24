//! The viewport renderer on a 200k-triangle mesh: meshes upload once, and an
//! orbit frame only rewrites uniforms. Timings are printed (software Vulkan in CI
//! is far slower than a real GPU, so only the CPU encode time is asserted).

use std::sync::Arc;
use std::time::Instant;

use easycad_app::camera::Camera;
use easycad_app::viewport::gpu::{DisplayMode, DrawBody, Frame3D, ViewportGpu, COLOR_FORMAT};
use easycad_model::occt::{EdgePolyline, Mesh};
use eframe::egui_wgpu::wgpu;
use glam::{Vec2, Vec3};

fn grid_mesh(n: u32) -> Mesh {
    // n×n quads on a wavy sheet = 2n² triangles
    let mut m = Mesh::default();
    for j in 0..=n {
        for i in 0..=n {
            let (x, y) = (i as f32 / n as f32 * 100.0, j as f32 / n as f32 * 100.0);
            m.positions.push([x, y, (x * 0.2).sin() * 3.0]);
            m.normals.push([0.0, 0.0, 1.0]);
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = j * (n + 1) + i;
            m.indices.push([a, a + 1, a + n + 2]);
            m.indices.push([a, a + n + 2, a + n + 1]);
            m.tri_face.push((j / 16) * 64 + i / 16);
            m.tri_face.push((j / 16) * 64 + i / 16);
        }
    }
    m.edges.push(EdgePolyline {
        edge: 0,
        points: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]],
    });
    m
}

#[test]
fn orbit_frames_reuse_uploaded_mesh() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("no wgpu adapter: skipped");
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut gpu = ViewportGpu::new(&device, &adapter, COLOR_FORMAT);
    let mesh = Arc::new(grid_mesh(317)); // ≈ 201k triangles
    assert!(mesh.indices.len() > 200_000);
    let mut cam = Camera::default();
    cam.fit_sphere(Vec3::new(50.0, 50.0, 0.0), 75.0, 1.6);
    let size = Vec2::new(1280.0, 800.0);
    let mut encode = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    let frames = 30;
    for i in 0..frames {
        cam.orbit(Vec2::new(4.0, 1.0));
        let f = Frame3D {
            size_px: [1280, 800],
            view_proj: cam.view_proj(size),
            eye: cam.eye(),
            forward: cam.forward(),
            ortho: false,
            light: Vec3::Z,
            background: [0.1, 0.1, 0.1, 1.0],
            mode: DisplayMode::ShadedEdges,
            edge_color: [0.1, 0.1, 0.1, 1.0],
            bodies: vec![DrawBody {
                mesh: mesh.clone(),
                color: [0.7, 0.7, 0.7, 1.0],
                translucent: false,
                colors: None,
                model: glam::Mat4::IDENTITY,
                top: false,
                edges_only: false,
            }],
            highlights: Vec::new(),
            sketches: vec![],
            grid: None,
        };
        let t0 = Instant::now();
        let mut enc = device.create_command_encoder(&Default::default());
        gpu.render(&device, &queue, &mut enc, &f);
        let cb = enc.finish();
        if i > 0 {
            encode += t0.elapsed();
        }
        queue.submit([cb]);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        if i > 0 {
            total += t0.elapsed();
        }
    }
    let n = (frames - 1) as u32;
    eprintln!(
        "200k tris on {:?}: encode {:.2} ms/frame, encode+GPU {:.1} ms/frame (msaa {})",
        adapter.get_info().name,
        (encode / n).as_secs_f64() * 1000.0,
        (total / n).as_secs_f64() * 1000.0,
        gpu.msaa()
    );
    assert_eq!(
        gpu.uploaded_meshes, 1,
        "the mesh is uploaded once, not per frame"
    );
    assert!(
        (encode / n).as_secs_f64() < 0.004,
        "CPU encode per orbit frame must stay well under a 60 fps budget"
    );
}

/// A frame that draws the same scene as the last one is not rendered again:
/// UI-only repaints (hover, tooltips, status) cost a blit, not a 3D pass.
#[test]
fn unchanged_frames_reuse_the_image() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("no wgpu adapter: skipped");
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut gpu = ViewportGpu::new(&device, &adapter, COLOR_FORMAT);
    let mesh = Arc::new(grid_mesh(8));
    let mut cam = Camera::default();
    cam.fit_sphere(Vec3::new(50.0, 50.0, 0.0), 75.0, 1.6);
    let size = Vec2::new(640.0, 400.0);
    let frame = |cam: &Camera| {
        Arc::new(Frame3D {
            size_px: [640, 400],
            view_proj: cam.view_proj(size),
            eye: cam.eye(),
            forward: cam.forward(),
            ortho: false,
            light: Vec3::Z,
            background: [0.1, 0.1, 0.1, 1.0],
            mode: DisplayMode::ShadedEdges,
            edge_color: [0.1, 0.1, 0.1, 1.0],
            bodies: vec![DrawBody {
                mesh: mesh.clone(),
                color: [0.7, 0.7, 0.7, 1.0],
                translucent: false,
                colors: None,
                model: glam::Mat4::IDENTITY,
                top: false,
                edges_only: false,
            }],
            highlights: Vec::new(),
            sketches: vec![],
            grid: None,
        })
    };
    let draw = |gpu: &mut ViewportGpu, f: &Arc<Frame3D>| {
        let mut enc = device.create_command_encoder(&Default::default());
        gpu.render_cached(&device, &queue, &mut enc, f);
        queue.submit([enc.finish()]);
    };
    draw(&mut gpu, &frame(&cam));
    draw(&mut gpu, &frame(&cam));
    draw(&mut gpu, &frame(&cam));
    assert_eq!(gpu.renders, 1, "same scene in new frames: one render");
    cam.orbit(Vec2::new(4.0, 1.0));
    draw(&mut gpu, &frame(&cam));
    assert_eq!(gpu.renders, 2, "a camera move renders");
}

/// A body wholly outside the view costs no draw call.
#[test]
fn bodies_outside_the_view_are_skipped() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("no wgpu adapter: skipped");
        return;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut gpu = ViewportGpu::new(&device, &adapter, COLOR_FORMAT);
    let mesh = Arc::new(grid_mesh(8));
    let mut cam = Camera::default();
    cam.fit_sphere(Vec3::new(50.0, 50.0, 0.0), 75.0, 1.6);
    let size = Vec2::new(640.0, 400.0);
    let mut draws = |model: glam::Mat4| {
        let f = Frame3D {
            size_px: [640, 400],
            view_proj: cam.view_proj(size),
            eye: cam.eye(),
            forward: cam.forward(),
            ortho: false,
            light: Vec3::Z,
            background: [0.1, 0.1, 0.1, 1.0],
            mode: DisplayMode::Shaded,
            edge_color: [0.1, 0.1, 0.1, 1.0],
            bodies: vec![DrawBody { mesh: mesh.clone(), color: [0.7, 0.7, 0.7, 1.0], translucent: false, colors: None, model, top: false, edges_only: false }],
            highlights: Vec::new(),
            sketches: vec![],
            grid: None,
        };
        let mut enc = device.create_command_encoder(&Default::default());
        gpu.render(&device, &queue, &mut enc, &f);
        queue.submit([enc.finish()]);
        gpu.last_draws
    };
    assert_eq!(draws(glam::Mat4::IDENTITY), 1);
    assert_eq!(draws(glam::Mat4::from_translation(Vec3::new(10_000.0, 0.0, 0.0))), 0);
}

/// A 100 mm square in the XY plane with its four sides as edges.
fn square() -> Mesh {
    let mut m = Mesh::default();
    for p in [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]] {
        m.positions.push([p[0], p[1], 0.0]);
        m.normals.push([0.0, 0.0, 1.0]);
    }
    m.indices = vec![[0, 1, 2], [0, 2, 3]];
    m.tri_face = vec![0, 0];
    let c = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [100.0, 100.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 0.0]];
    for (i, w) in c.windows(2).enumerate() {
        m.edges.push(EdgePolyline { edge: i as u32, points: w.to_vec() });
    }
    m
}

/// A section view's cut-away half (`edges_only`) draws as a wireframe in every
/// display mode: its edges in the wireframe colour, its faces all but see-through.
#[test]
fn edges_only_bodies_draw_as_wireframe() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("no wgpu adapter: skipped");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut gpu = ViewportGpu::new(&device, &adapter, COLOR_FORMAT);
    let mesh = Arc::new(square());
    let mut cam = Camera::default();
    cam.fit_sphere(Vec3::new(50.0, 50.0, 0.0), 75.0, 1.0);
    let (w, h) = (320u32, 320u32);
    let mut shot = |mode: DisplayMode, edges_only: bool| {
        let f = Frame3D {
            size_px: [w, h],
            view_proj: cam.view_proj(Vec2::new(w as f32, h as f32)),
            eye: cam.eye(),
            forward: cam.forward(),
            ortho: false,
            light: Vec3::Z,
            background: [0.0, 0.0, 0.0, 1.0],
            mode,
            edge_color: [0.0, 0.0, 1.0, 1.0],
            bodies: vec![DrawBody {
                mesh: mesh.clone(),
                color: [0.9, 0.2, 0.2, 1.0],
                translucent: false,
                colors: None,
                model: glam::Mat4::IDENTITY,
                top: false,
                edges_only,
            }],
            sketches: vec![],
            highlights: Vec::new(),
            grid: None,
        };
        image::RgbaImage::from_raw(w, h, gpu.render_to_image(&device, &queue, &f).unwrap()).unwrap()
    };
    let grey = |img: &image::RgbaImage| img.pixels().filter(|p| p.0[0] > 150 && p.0[0].abs_diff(p.0[2]) < 30 && p.0[1] > 150).count();
    let centre = |img: &image::RgbaImage| img.get_pixel(w / 2, h / 2).0;
    for mode in [DisplayMode::Shaded, DisplayMode::ShadedEdges, DisplayMode::Wireframe] {
        let solid = shot(mode, false);
        let ghost = shot(mode, true);
        assert!(centre(&ghost)[0] < 40, "{mode:?}: the cut-away half is not filled: {:?}", centre(&ghost));
        assert!(grey(&ghost) > 200, "{mode:?}: its edges in the wireframe grey ({})", grey(&ghost));
        if mode != DisplayMode::Wireframe {
            assert!(centre(&solid)[0] > 100, "{mode:?}: a normal body is filled: {:?}", centre(&solid));
        }
    }
}
