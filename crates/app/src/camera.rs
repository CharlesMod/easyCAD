//! Orbit camera with Fusion-style navigation, view-cube orientations, 15° steps,
//! a saved home view and short eased transitions.
//!
//! Conventions: world is right-handed, Z up, millimetres. The camera orientation
//! `rot` maps camera space to world space; the camera looks along camera −Z with
//! camera +Y up and +X right, so the eye sits at `target + rot * (0, 0, distance)`.
//! Screen coordinates are in points with +y down.

use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

/// One of the 26 view-cube regions: each component is −1, 0 or +1 (not all zero).
/// One non-zero component is a face, two an edge, three a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CubeRegion(pub [i8; 3]);

impl CubeRegion {
    pub const FRONT: CubeRegion = CubeRegion([0, -1, 0]);
    pub const BACK: CubeRegion = CubeRegion([0, 1, 0]);
    pub const RIGHT: CubeRegion = CubeRegion([1, 0, 0]);
    pub const LEFT: CubeRegion = CubeRegion([-1, 0, 0]);
    pub const TOP: CubeRegion = CubeRegion([0, 0, 1]);
    pub const BOTTOM: CubeRegion = CubeRegion([0, 0, -1]);

    /// All 26 regions (6 faces, 12 edges, 8 corners).
    pub fn all() -> Vec<CubeRegion> {
        let mut v = Vec::with_capacity(26);
        for x in -1..=1i8 {
            for y in -1..=1i8 {
                for z in -1..=1i8 {
                    if (x, y, z) != (0, 0, 0) {
                        v.push(CubeRegion([x, y, z]));
                    }
                }
            }
        }
        v
    }
    pub fn nonzero(&self) -> usize {
        self.0.iter().filter(|c| **c != 0).count()
    }
    pub fn is_face(&self) -> bool {
        self.nonzero() == 1
    }
    pub fn is_edge(&self) -> bool {
        self.nonzero() == 2
    }
    pub fn is_corner(&self) -> bool {
        self.nonzero() == 3
    }
    /// Unit direction from the target to the eye for this view.
    pub fn eye_dir(&self) -> Vec3 {
        Vec3::new(self.0[0] as f32, self.0[1] as f32, self.0[2] as f32).normalize()
    }
    pub fn label(&self) -> String {
        let names = [("Left", "Right"), ("Front", "Back"), ("Bottom", "Top")];
        let mut parts = Vec::new();
        for (i, c) in [2usize, 1, 0].iter().map(|i| (*i, self.0[*i])) {
            if c != 0 {
                parts.push(if c < 0 { names[i].0 } else { names[i].1 });
            }
        }
        parts.join(" ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAxis {
    X,
    Y,
    Z,
}

/// Which frame the 15° step buttons rotate about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StepFrame {
    /// Model X/Y/Z (default).
    #[default]
    World,
    /// Screen right / up / out-of-screen.
    Screen,
}

pub const STEP_DEG: f32 = 15.0;
pub const ANIM_SECS: f64 = 0.25;

/// A camera pose (what is animated and what "home" stores).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub rot: Quat,
    pub target: Vec3,
    pub distance: f32,
}

#[derive(Clone, Copy, Debug)]
struct Anim {
    from: Pose,
    to: Pose,
    start: f64,
}

/// Saved home view. `pose: None` means the default (iso, fit all).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Home {
    pub pose: Option<Pose>,
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f32,
    pub rot: Quat,
    /// Vertical field of view (radians), also used to size the ortho frustum.
    pub fov_y: f32,
    pub ortho: bool,
    /// Bounding sphere of the scene, for clip planes and fitting.
    pub scene_center: Vec3,
    pub scene_radius: f32,
    pub home: Home,
    /// Fit the model whenever a view-cube view is chosen.
    pub fit_on_change: bool,
    pub step_frame: StepFrame,
    anim: Option<Anim>,
}

impl Default for Camera {
    fn default() -> Self {
        let mut c = Camera {
            target: Vec3::ZERO,
            distance: 300.0,
            rot: iso_rot(),
            fov_y: 30f32.to_radians(),
            ortho: false,
            scene_center: Vec3::ZERO,
            scene_radius: 100.0,
            home: Home::default(),
            fit_on_change: false,
            step_frame: StepFrame::World,
            anim: None,
        };
        c.fit_sphere(Vec3::ZERO, 60.0, 1.0);
        c
    }
}

/// Orientation looking from `eye_dir` (target → eye) with `up_hint` kept upright.
pub fn look_rot(eye_dir: Vec3, up_hint: Vec3) -> Quat {
    let z = eye_dir.normalize();
    let mut x = up_hint.cross(z);
    if x.length_squared() < 1e-8 {
        x = Vec3::Y.cross(z);
        if x.length_squared() < 1e-8 {
            x = Vec3::X;
        }
    }
    let x = x.normalize();
    let y = z.cross(x);
    Quat::from_mat3(&Mat3::from_cols(x, y, z)).normalize()
}

/// The default home orientation: front-right-top isometric.
pub fn iso_rot() -> Quat {
    look_rot(Vec3::new(1.0, -1.0, 1.0), Vec3::Z)
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Snap a vector to the nearest principal axis (±X/±Y/±Z).
pub fn snap_axis(v: Vec3) -> Vec3 {
    let a = v.abs();
    if a.x >= a.y && a.x >= a.z {
        Vec3::X * v.x.signum()
    } else if a.y >= a.z {
        Vec3::Y * v.y.signum()
    } else {
        Vec3::Z * v.z.signum()
    }
}

/// Angle between two orientations (radians).
pub fn rot_angle(a: Quat, b: Quat) -> f32 {
    let d = a.dot(b).abs().min(1.0);
    2.0 * d.acos()
}

impl Camera {
    pub fn right(&self) -> Vec3 {
        self.rot * Vec3::X
    }
    pub fn up(&self) -> Vec3 {
        self.rot * Vec3::Y
    }
    /// View direction (from the eye into the scene).
    pub fn forward(&self) -> Vec3 {
        self.rot * Vec3::NEG_Z
    }
    pub fn eye(&self) -> Vec3 {
        self.target + self.rot * Vec3::new(0.0, 0.0, self.distance)
    }
    pub fn pose(&self) -> Pose {
        Pose {
            rot: self.rot,
            target: self.target,
            distance: self.distance,
        }
    }
    pub fn set_pose(&mut self, p: Pose) {
        self.rot = p.rot.normalize();
        self.target = p.target;
        self.distance = p.distance;
    }

    /// World units per screen point at the target depth.
    pub fn world_per_point(&self, viewport_h: f32) -> f32 {
        2.0 * self.distance * (self.fov_y / 2.0).tan() / viewport_h.max(1.0)
    }

    fn clip(&self) -> (f32, f32) {
        let r = self.scene_radius.max(1e-3);
        let d = (self.scene_center - self.eye()).dot(self.forward());
        if self.ortho {
            (d - 2.0 * r - 1.0, d + 2.0 * r + 1.0)
        } else {
            let near = (d - 1.5 * r).max(self.distance * 0.002).max(1e-3);
            let far = (d + 1.5 * r).max(near * 2.0) + 1.0;
            (near, far)
        }
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, self.up())
    }

    /// Projection with wgpu's 0..1 depth range.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        let (near, far) = self.clip();
        if self.ortho {
            let h = self.distance * (self.fov_y / 2.0).tan();
            let w = h * aspect;
            Mat4::orthographic_rh(-w, w, -h, h, near, far)
        } else {
            Mat4::perspective_rh(self.fov_y, aspect, near, far)
        }
    }

    pub fn view_proj(&self, size: Vec2) -> Mat4 {
        self.proj(size.x / size.y.max(1.0)) * self.view()
    }

    /// Ray through a viewport point (points, origin top-left).
    pub fn ray(&self, p: Vec2, size: Vec2) -> (Vec3, Vec3) {
        let x = 2.0 * p.x / size.x.max(1.0) - 1.0;
        let y = 1.0 - 2.0 * p.y / size.y.max(1.0);
        let t = (self.fov_y / 2.0).tan();
        let aspect = size.x / size.y.max(1.0);
        if self.ortho {
            let h = self.distance * t;
            let o = self.target + self.right() * (x * h * aspect) + self.up() * (y * h);
            let back = self.distance + 4.0 * self.scene_radius;
            (o - self.forward() * back, self.forward())
        } else {
            let d = self.forward() + self.right() * (x * t * aspect) + self.up() * (y * t);
            (self.eye(), d.normalize())
        }
    }

    /// Project a world point to viewport points; `None` behind the eye.
    pub fn project(&self, w: Vec3, size: Vec2) -> Option<Vec2> {
        let c = self.view_proj(size) * w.extend(1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let n = c.truncate() / c.w;
        Some(Vec2::new(
            (n.x + 1.0) * 0.5 * size.x,
            (1.0 - n.y) * 0.5 * size.y,
        ))
    }

    /// Shift+MMB orbit: horizontal drag turns about world Z, vertical about screen X.
    pub fn orbit(&mut self, delta: Vec2) {
        self.anim = None;
        let k = 0.008;
        let qz = Quat::from_axis_angle(Vec3::Z, -delta.x * k);
        let qx = Quat::from_axis_angle(self.right(), -delta.y * k);
        self.rot = (qz * qx * self.rot).normalize();
    }

    /// MMB pan: the point under the cursor (at target depth) follows the cursor.
    pub fn pan(&mut self, delta: Vec2, viewport_h: f32) {
        self.anim = None;
        let w = self.world_per_point(viewport_h);
        self.target += (-self.right() * delta.x + self.up() * delta.y) * w;
    }

    /// Wheel zoom about the cursor: `factor` < 1 zooms in. The world point under
    /// the cursor at target depth stays fixed on screen.
    pub fn zoom_at(&mut self, cursor: Vec2, size: Vec2, factor: f32) {
        self.anim = None;
        let min = (self.scene_radius * 1e-3).max(1e-3);
        let factor = factor.max(min / self.distance);
        let (o, d) = self.ray(cursor, size);
        // point on the cursor ray at the depth of the target plane
        let denom = d.dot(self.forward());
        let t = if denom.abs() > 1e-6 {
            (self.target - o).dot(self.forward()) / denom
        } else {
            self.distance
        };
        let p = o + d * t;
        self.target = p + (self.target - p) * factor;
        self.distance *= factor;
    }

    /// Fit a bounding sphere (keeps the orientation).
    pub fn fit_sphere(&mut self, center: Vec3, radius: f32, aspect: f32) {
        let r = radius.max(1e-3);
        let half = (self.fov_y / 2.0).min((aspect.min(1.0) * (self.fov_y / 2.0).tan()).atan());
        self.target = center;
        self.distance = r / half.sin() * 1.08;
        self.scene_center = center;
        self.scene_radius = r;
    }

    pub fn fitted_pose(&self, rot: Quat, aspect: f32) -> Pose {
        let mut c = self.clone();
        c.rot = rot;
        c.fit_sphere(self.scene_center, self.scene_radius, aspect);
        c.pose()
    }

    /// Target orientation for a view-cube region, kept upright: side, edge and
    /// corner views keep world Z up; top/bottom keep the current screen-up snapped
    /// to the nearest horizontal axis. Clicking the face already viewed straight-on
    /// rolls the view by 90°.
    pub fn region_rot(&self, region: CubeRegion) -> Quat {
        let eye = region.eye_dir();
        let vertical = region.0[0] == 0 && region.0[1] == 0;
        let up = if vertical {
            let mut u = self.up();
            u.z = 0.0;
            if u.length_squared() < 1e-6 {
                let mut f = self.forward();
                f.z = 0.0;
                u = if f.length_squared() < 1e-6 {
                    Vec3::Y
                } else {
                    f
                };
            }
            snap_axis(u)
        } else {
            Vec3::Z
        };
        let rot = look_rot(eye, up);
        if region.is_face() && rot_angle(rot, self.rot) < 1f32.to_radians() {
            // already looking straight at this face: roll 90° about the view axis
            return (self.rot * Quat::from_rotation_z(90f32.to_radians())).normalize();
        }
        rot
    }

    /// Orientation after a ±15° step about X/Y/Z (world or screen axes).
    pub fn step_rot(&self, axis: StepAxis, sign: f32) -> Quat {
        let a = STEP_DEG.to_radians() * sign.signum();
        let v = match axis {
            StepAxis::X => Vec3::X,
            StepAxis::Y => Vec3::Y,
            StepAxis::Z => Vec3::Z,
        };
        match self.step_frame {
            // Turning the model by +a about a world axis = turning the camera by −a.
            StepFrame::World => (Quat::from_axis_angle(v, -a) * self.rot).normalize(),
            StepFrame::Screen => (self.rot * Quat::from_axis_angle(v, -a)).normalize(),
        }
    }

    pub fn cancel_anim(&mut self) {
        self.anim = None;
    }

    pub fn animate_to(&mut self, to: Pose, now: f64) {
        self.anim = Some(Anim {
            from: self.pose(),
            to,
            start: now,
        });
    }

    /// Animate to a rotation, optionally fitting.
    pub fn animate_rot(&mut self, rot: Quat, now: f64, aspect: f32, fit: bool) {
        let to = if fit {
            self.fitted_pose(rot, aspect)
        } else {
            Pose { rot, ..self.pose() }
        };
        self.animate_to(to, now);
    }

    pub fn go_region(&mut self, region: CubeRegion, now: f64, aspect: f32) {
        let r = self.region_rot(region);
        self.animate_rot(r, now, aspect, self.fit_on_change);
    }

    pub fn go_step(&mut self, axis: StepAxis, sign: f32, now: f64) {
        // chain steps while an animation is running
        let base = self.anim.map(|a| a.to.rot);
        let mut c = self.clone();
        if let Some(b) = base {
            c.rot = b;
        }
        let r = c.step_rot(axis, sign);
        self.animate_rot(r, now, 1.0, false);
    }

    pub fn home_pose(&self, aspect: f32) -> Pose {
        match self.home.pose {
            Some(p) => p,
            None => self.fitted_pose(iso_rot(), aspect),
        }
    }
    pub fn go_home(&mut self, now: f64, aspect: f32) {
        let p = self.home_pose(aspect);
        self.animate_to(p, now);
    }
    pub fn set_home(&mut self) {
        self.home.pose = Some(self.pose());
    }
    pub fn reset_home(&mut self) {
        self.home.pose = None;
    }

    pub fn animating(&self) -> bool {
        self.anim.is_some()
    }

    /// Advance the transition; returns true while animating.
    pub fn tick(&mut self, now: f64) -> bool {
        let Some(a) = self.anim else { return false };
        let t = ((now - a.start) / ANIM_SECS) as f32;
        if t >= 1.0 {
            self.set_pose(a.to);
            self.anim = None;
            return false;
        }
        let e = ease(t);
        self.rot = a.from.rot.slerp(a.to.rot, e).normalize();
        self.target = a.from.target.lerp(a.to.target, e);
        self.distance = a.from.distance + (a.to.distance - a.from.distance) * e;
        true
    }

    /// Jump to the end of a running transition.
    pub fn finish(&mut self) {
        if let Some(a) = self.anim.take() {
            self.set_pose(a.to);
        }
    }

    /// The face of the view cube currently viewed most directly.
    pub fn facing_region(&self) -> CubeRegion {
        let e = snap_axis(self.rot * Vec3::Z);
        CubeRegion([e.x as i8, e.y as i8, e.z as i8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() < eps
    }

    fn cam() -> Camera {
        let mut c = Camera::default();
        c.fit_sphere(Vec3::new(10.0, -5.0, 3.0), 50.0, 1.5);
        c
    }

    const SIZE: Vec2 = Vec2::new(1200.0, 800.0);

    #[test]
    fn front_and_iso_basis() {
        let r = look_rot(Vec3::NEG_Y, Vec3::Z);
        assert!(close(r * Vec3::X, Vec3::X, 1e-5));
        assert!(close(r * Vec3::Y, Vec3::Z, 1e-5));
        let i = iso_rot();
        let up = i * Vec3::Y;
        assert!(up.z > 0.8, "iso keeps Z up: {up}");
        assert!(close(
            i * Vec3::Z,
            Vec3::new(1.0, -1.0, 1.0).normalize(),
            1e-5
        ));
    }

    #[test]
    fn ray_and_project_round_trip() {
        for ortho in [false, true] {
            let mut c = cam();
            c.ortho = ortho;
            let p = Vec3::new(20.0, 4.0, -7.0);
            let s = c.project(p, SIZE).unwrap();
            let (o, d) = c.ray(s, SIZE);
            // p lies on the ray
            let t = (p - o).dot(d);
            assert!(close(o + d * t, p, 1e-2), "ortho={ortho}");
            // centre of the screen looks at the target
            let (o, d) = c.ray(SIZE / 2.0, SIZE);
            let t = (c.target - o).dot(d);
            assert!(close(o + d * t, c.target, 1e-2));
        }
    }

    #[test]
    fn zoom_to_cursor_keeps_point_fixed() {
        for ortho in [false, true] {
            let mut c = cam();
            c.ortho = ortho;
            let cursor = Vec2::new(900.0, 250.0);
            // world point under the cursor at target depth
            let (o, d) = c.ray(cursor, SIZE);
            let t = (c.target - o).dot(c.forward()) / d.dot(c.forward());
            let p = o + d * t;
            let d0 = c.distance;
            c.zoom_at(cursor, SIZE, 0.8);
            assert!((c.distance - d0 * 0.8).abs() < 1e-3);
            let s = c.project(p, SIZE).unwrap();
            assert!(
                (s - cursor).length() < 0.05,
                "ortho={ortho}: {s} vs {cursor}"
            );
            c.zoom_at(cursor, SIZE, 1.5);
            let s = c.project(p, SIZE).unwrap();
            assert!((s - cursor).length() < 0.05);
        }
    }

    #[test]
    fn pan_follows_cursor() {
        let mut c = cam();
        let start = Vec2::new(400.0, 300.0);
        let (o, d) = c.ray(start, SIZE);
        let t = (c.target - o).dot(c.forward()) / d.dot(c.forward());
        let p = o + d * t;
        let delta = Vec2::new(37.0, -21.0);
        c.pan(delta, SIZE.y);
        let s = c.project(p, SIZE).unwrap();
        assert!((s - (start + delta)).length() < 0.05, "{s}");
    }

    #[test]
    fn orbit_turns_about_world_z_and_screen_x() {
        let mut c = cam();
        c.rot = look_rot(Vec3::NEG_Y, Vec3::Z);
        let target = c.target;
        c.orbit(Vec2::new(-(90f32.to_radians() / 0.008), 0.0));
        // turned 90° about Z: from the front (eye −Y) to the right (eye +X)
        assert!(close(c.rot * Vec3::Z, Vec3::X, 1e-4), "{}", c.rot * Vec3::Z);
        assert!(close(c.up(), Vec3::Z, 1e-4));
        assert_eq!(c.target, target);
        // vertical drag tilts towards the top while keeping the right vector
        let right = c.right();
        c.orbit(Vec2::new(0.0, 45f32.to_radians() / 0.008));
        assert!(close(c.right(), right, 1e-4));
        assert!((c.rot * Vec3::Z).z > 0.69);
    }

    #[test]
    fn twenty_six_regions_map_to_upright_views() {
        let regions = CubeRegion::all();
        assert_eq!(regions.len(), 26);
        assert_eq!(regions.iter().filter(|r| r.is_face()).count(), 6);
        assert_eq!(regions.iter().filter(|r| r.is_edge()).count(), 12);
        assert_eq!(regions.iter().filter(|r| r.is_corner()).count(), 8);
        let c = cam();
        for r in regions {
            let q = c.region_rot(r);
            // eye along the region direction (face / edge bisector / iso corner)
            assert!(close(q * Vec3::Z, r.eye_dir(), 1e-4), "{r:?}");
            let up = q * Vec3::Y;
            if r.0[0] == 0 && r.0[1] == 0 {
                // top/bottom: up is a horizontal principal axis
                assert!(
                    up.z.abs() < 1e-4 && (up.x.abs() > 0.999 || up.y.abs() > 0.999),
                    "{r:?} {up}"
                );
            } else {
                // upright: no roll, world Z points up on screen
                assert!(up.z > 0.0, "{r:?} {up}");
                assert!((q * Vec3::X).z.abs() < 1e-4, "{r:?} has roll");
            }
        }
        // named faces
        let c = cam();
        let top = c.region_rot(CubeRegion::TOP);
        assert!(close(top * Vec3::NEG_Z, Vec3::NEG_Z, 1e-4));
        let right = c.region_rot(CubeRegion::RIGHT);
        assert!(
            close(right * Vec3::X, Vec3::Y, 1e-4),
            "right view: +Y to the right"
        );
        assert_eq!(CubeRegion([1, -1, 1]).label(), "Top Front Right");
    }

    #[test]
    fn clicking_the_current_face_rolls_90() {
        let mut c = cam();
        c.rot = c.region_rot(CubeRegion::FRONT);
        let before_up = c.up();
        let rolled = c.region_rot(CubeRegion::FRONT);
        assert!(
            close(rolled * Vec3::Z, Vec3::NEG_Y, 1e-4),
            "still looking at the front"
        );
        let up = rolled * Vec3::Y;
        assert!(up.dot(before_up).abs() < 1e-4, "rolled by 90°");
        assert_eq!(c.facing_region(), CubeRegion::FRONT);
    }

    #[test]
    fn top_view_keeps_snapped_screen_up() {
        let mut c = cam();
        // looking from the right: screen-up is Z, forward is −X → top view gets up = −X
        c.rot = look_rot(Vec3::X, Vec3::Z);
        let q = c.region_rot(CubeRegion::TOP);
        assert!(close(q * Vec3::Y, Vec3::NEG_X, 1e-4), "{}", q * Vec3::Y);
        // from the front: top view has +Y up (front at the bottom of the screen)
        c.rot = look_rot(Vec3::NEG_Y, Vec3::Z);
        let q = c.region_rot(CubeRegion::TOP);
        assert!(close(q * Vec3::Y, Vec3::Y, 1e-4));
    }

    #[test]
    fn fifteen_degree_steps() {
        let mut c = cam();
        c.rot = look_rot(Vec3::NEG_Y, Vec3::Z);
        let start = c.rot;
        // world Z: six +15° steps turn the model by 90°, so the camera ends up
        // looking from the left (the model's +X now faces the viewer's right... )
        for _ in 0..6 {
            c.rot = c.step_rot(StepAxis::Z, 1.0);
        }
        assert!(
            close(c.rot * Vec3::Z, Vec3::NEG_X, 1e-4),
            "{}",
            c.rot * Vec3::Z
        );
        assert!(close(c.up(), Vec3::Z, 1e-4));
        // 24 steps = full turn
        for _ in 0..18 {
            c.rot = c.step_rot(StepAxis::Z, 1.0);
        }
        assert!(rot_angle(c.rot, start) < 1e-3);
        // a step and its inverse cancel, for every axis and both frames
        for frame in [StepFrame::World, StepFrame::Screen] {
            c.step_frame = frame;
            for axis in [StepAxis::X, StepAxis::Y, StepAxis::Z] {
                let r0 = c.rot;
                c.rot = c.step_rot(axis, 1.0);
                assert!((rot_angle(c.rot, r0) - 15f32.to_radians()).abs() < 1e-4);
                c.rot = c.step_rot(axis, -1.0);
                assert!(rot_angle(c.rot, r0) < 1e-4);
            }
        }
        // screen Z = roll about the view axis: the view direction is unchanged
        c.step_frame = StepFrame::Screen;
        let f = c.forward();
        c.rot = c.step_rot(StepAxis::Z, 1.0);
        assert!(close(c.forward(), f, 1e-5));
        // screen X tilts about the screen's right vector
        let r = c.right();
        c.rot = c.step_rot(StepAxis::X, -1.0);
        assert!(close(c.right(), r, 1e-5));
    }

    #[test]
    fn home_save_restore_and_animation() {
        let mut c = cam();
        // default home is iso, fitted
        let h = c.home_pose(1.5);
        assert!(rot_angle(h.rot, iso_rot()) < 1e-4);
        assert!((h.target - c.scene_center).length() < 1e-4);
        // save a custom home
        c.rot = c.region_rot(CubeRegion([1, 0, 1]));
        c.target = Vec3::new(1.0, 2.0, 3.0);
        c.distance = 123.0;
        c.set_home();
        let saved = c.pose();
        c.orbit(Vec2::new(100.0, 40.0));
        c.pan(Vec2::new(10.0, 10.0), 800.0);
        c.go_home(10.0, 1.5);
        assert!(c.animating());
        assert!(c.tick(10.1));
        // eased midpoint is between the two poses
        assert!(rot_angle(c.rot, saved.rot) > 1e-3);
        assert!(!c.tick(10.0 + ANIM_SECS + 0.01));
        assert_eq!(c.pose(), saved);
        // reset returns to the default iso
        c.reset_home();
        c.go_home(20.0, 1.5);
        c.finish();
        assert!(rot_angle(c.rot, iso_rot()) < 1e-4);
    }

    #[test]
    fn fit_contains_sphere() {
        let mut c = Camera {
            rot: iso_rot(),
            ..Camera::default()
        };
        c.fit_sphere(Vec3::new(5.0, 5.0, 5.0), 40.0, 0.5);
        // every point of the sphere's silhouette projects inside the viewport
        let size = Vec2::new(400.0, 800.0);
        for dir in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::NEG_X,
            Vec3::NEG_Y,
            Vec3::NEG_Z,
        ] {
            let s = c.project(Vec3::splat(5.0) + dir * 40.0, size).unwrap();
            assert!(
                s.x >= 0.0 && s.x <= size.x && s.y >= 0.0 && s.y <= size.y,
                "{s}"
            );
        }
    }
}
