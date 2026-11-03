//! Overhang analysis for printing: how far each face of a body leans out over
//! the layers below it, against a build (up) direction.
//!
//! The overhang angle of a surface point is measured from vertical: 0° for walls
//! and surfaces facing up, 90° for a ceiling facing straight down. Against the
//! process's critical angle ([`ProcessProfile::critical_overhang_deg`], FDM 45°):
//!
//! | angle | class |
//! |---|---|
//! | facing up, vertical (within [`WALL_TOL_DEG`]) or resting on the build plate | `Ok` |
//! | up to the critical angle | `Overhang` (prints, worse surface) |
//! | beyond it | `Support` (needs support) |
//!
//! The analysis runs on the tessellation. Each triangle is classified by its own
//! normal and counts its area, so a curved face (a horizontal hole, a fillet under
//! a ledge) reports how much of it needs support instead of one verdict; a face's
//! class is the worst class any of its area has. Triangles lying in the lowest
//! plane of the body, facing down, rest on the build plate and are `Ok`.

use glam::DVec3;
use occt::Mesh;
use serde::Serialize;

use crate::process::{ProcessKind, ProcessProfile};
use crate::regen::{Body, MeshQuality};

/// Faces leaning less than this past vertical are walls, not overhangs.
pub const WALL_TOL_DEG: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum OverhangClass {
    Ok,
    Overhang,
    Support,
}

impl OverhangClass {
    pub fn of(angle_deg: f64, critical_deg: f64) -> OverhangClass {
        if angle_deg <= WALL_TOL_DEG {
            OverhangClass::Ok
        } else if angle_deg <= critical_deg {
            OverhangClass::Overhang
        } else {
            OverhangClass::Support
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            OverhangClass::Ok => "ok",
            OverhangClass::Overhang => "overhang",
            OverhangClass::Support => "needs support",
        }
    }
}

impl ProcessKind {
    /// The steepest overhang (degrees from vertical) the process prints without
    /// support. SLS never needs support (the powder holds the part); molded and
    /// machined parts get the FDM rule of thumb for display.
    pub fn critical_overhang_deg(self) -> f64 {
        match self {
            ProcessKind::Fdm | ProcessKind::Molded | ProcessKind::Cnc => 45.0,
            ProcessKind::Sla => 60.0,
            ProcessKind::Sls => 90.0,
        }
    }
}

impl ProcessProfile {
    /// See [`ProcessKind::critical_overhang_deg`].
    pub fn critical_overhang_deg(&self) -> f64 {
        self.kind.critical_overhang_deg()
    }
}

/// Overhang angle (degrees from vertical) of a surface with outward normal `n`.
pub fn overhang_angle(n: DVec3, up: DVec3) -> f64 {
    let n = n.normalize_or_zero();
    (-n.dot(up.normalize_or_zero())).clamp(0.0, 1.0).asin().to_degrees()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FaceOverhang {
    pub face: u32,
    /// Steepest overhang on the face (degrees from vertical; 0 = wall or facing up).
    pub max_angle: f64,
    /// The worst class any of the face's area has.
    pub class: OverhangClass,
    pub area: f64,
    /// Area leaning out up to the critical angle.
    pub overhang_area: f64,
    /// Area leaning out beyond it.
    pub support_area: f64,
    /// The face lies on the build plate (facing down at the lowest level).
    pub on_plate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OverhangReport {
    /// Build (up) direction, unit, in the mesh's frame.
    pub up: DVec3,
    pub critical_deg: f64,
    /// Height of the build plate along `up`.
    pub plate: f64,
    /// One entry per face id of the mesh, in face order.
    pub faces: Vec<FaceOverhang>,
    /// Overhang angle per mesh vertex (from the surface normal there), for a heat
    /// map; negative on faces resting on the plate.
    pub vertex_angles: Vec<f32>,
}

impl OverhangReport {
    pub fn face(&self, face: u32) -> Option<&FaceOverhang> {
        self.faces.iter().find(|f| f.face == face)
    }
    pub fn overhang_area(&self) -> f64 {
        self.faces.iter().map(|f| f.overhang_area).sum()
    }
    pub fn support_area(&self) -> f64 {
        self.faces.iter().map(|f| f.support_area).sum()
    }
    /// Faces of a class.
    pub fn of_class(&self, c: OverhangClass) -> impl Iterator<Item = &FaceOverhang> {
        self.faces.iter().filter(move |f| f.class == c)
    }
}

fn v3(p: [f32; 3]) -> DVec3 {
    DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
}

/// Analyse a tessellated body against the build direction `up`. `plate` is the
/// build plate's height along `up` (`None`: the body's lowest point, i.e. the body
/// sits on the plate).
pub fn analyze(mesh: &Mesh, up: DVec3, critical_deg: f64, plate: Option<f64>) -> OverhangReport {
    let up = up.normalize_or_zero();
    let heights: Vec<f64> = mesh.positions.iter().map(|p| v3(*p).dot(up)).collect();
    let (lo, hi) = heights.iter().fold((f64::MAX, f64::MIN), |(a, b), h| (a.min(*h), b.max(*h)));
    let plate = plate.unwrap_or(if heights.is_empty() { 0.0 } else { lo });
    // the tessellation is within its deflection of the surface; a flat face is exact
    let tol = 1e-4 * (hi - lo).abs().max(1.0) + 1e-6;
    let nfaces = mesh.tri_face.iter().map(|f| *f as usize + 1).max().unwrap_or(0);
    let mut faces: Vec<FaceOverhang> = (0..nfaces as u32)
        .map(|face| FaceOverhang { face, max_angle: 0.0, class: OverhangClass::Ok, area: 0.0, overhang_area: 0.0, support_area: 0.0, on_plate: true })
        .collect();
    let has_normals = mesh.normals.len() == mesh.positions.len();
    let vangle: Vec<f64> = if has_normals { mesh.normals.iter().map(|n| overhang_angle(v3(*n), up)).collect() } else { vec![0.0; mesh.positions.len()] };
    let mut seen = vec![false; nfaces];
    for (t, f) in mesh.indices.iter().zip(&mesh.tri_face) {
        let fi = *f as usize;
        let [a, b, c] = t.map(|i| v3(mesh.positions[i as usize]));
        let cross = (b - a).cross(c - a);
        let area = cross.length() * 0.5;
        if area <= 0.0 {
            continue;
        }
        // the winding's normal, turned to agree with the surface normals
        let mut n = cross / (2.0 * area);
        if has_normals {
            let s: DVec3 = t.iter().map(|i| v3(mesh.normals[*i as usize])).sum();
            if s.dot(n) < 0.0 {
                n = -n;
            }
        }
        let fo = &mut faces[fi];
        seen[fi] = true;
        fo.area += area;
        let on_plate = t.iter().all(|i| (heights[*i as usize] - plate).abs() <= tol) && n.dot(up) < 0.0;
        if on_plate {
            continue;
        }
        fo.on_plate = false;
        let ang = overhang_angle(n, up);
        let vmax = t.iter().map(|i| vangle[*i as usize]).fold(0.0, f64::max);
        fo.max_angle = fo.max_angle.max(ang).max(vmax);
        match OverhangClass::of(ang, critical_deg) {
            OverhangClass::Ok => {}
            OverhangClass::Overhang => fo.overhang_area += area,
            OverhangClass::Support => fo.support_area += area,
        }
    }
    for (f, s) in faces.iter_mut().zip(&seen) {
        if !s {
            f.on_plate = false;
        }
        f.class = if f.on_plate {
            OverhangClass::Ok
        } else if f.support_area > 0.0 {
            OverhangClass::Support
        } else if f.overhang_area > 0.0 {
            OverhangClass::Overhang
        } else {
            OverhangClass::Ok
        };
    }
    let mut vertex_angles: Vec<f32> = vangle.iter().map(|a| *a as f32).collect();
    for (t, f) in mesh.indices.iter().zip(&mesh.tri_face) {
        if faces[*f as usize].on_plate {
            for i in t {
                vertex_angles[*i as usize] = -1.0;
            }
        }
    }
    OverhangReport { up, critical_deg, plate, faces, vertex_angles }
}

impl Body {
    /// Overhang analysis of this body standing on the build plate, `up` in the
    /// body's frame (see [`analyze`]).
    pub fn overhang(&self, up: DVec3, critical_deg: f64, q: MeshQuality) -> Result<OverhangReport, String> {
        Ok(analyze(&*self.mesh_quality(q)?, up, critical_deg, None))
    }
}
