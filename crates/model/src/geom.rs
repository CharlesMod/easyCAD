//! Shared geometric primitives (millimetres, right-handed, Z up).

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

/// A placed 2D coordinate system in 3D. `x_dir` and `normal` are unit and orthogonal.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plane {
    pub origin: DVec3,
    pub x_dir: DVec3,
    pub normal: DVec3,
}

impl Plane {
    pub const XY: Plane = Plane { origin: DVec3::ZERO, x_dir: DVec3::X, normal: DVec3::Z };
    pub const XZ: Plane = Plane { origin: DVec3::ZERO, x_dir: DVec3::X, normal: DVec3::NEG_Y };
    pub const YZ: Plane = Plane { origin: DVec3::ZERO, x_dir: DVec3::Y, normal: DVec3::X };

    pub fn new(origin: DVec3, x_dir: DVec3, normal: DVec3) -> Self {
        let n = normal.normalize();
        let x = (x_dir - n * x_dir.dot(n)).normalize();
        Plane { origin, x_dir: x, normal: n }
    }

    pub fn y_dir(&self) -> DVec3 {
        self.normal.cross(self.x_dir)
    }

    pub fn to_world(&self, p: DVec2) -> DVec3 {
        self.origin + self.x_dir * p.x + self.y_dir() * p.y
    }

    pub fn to_local(&self, p: DVec3) -> DVec2 {
        let d = p - self.origin;
        DVec2::new(d.dot(self.x_dir), d.dot(self.y_dir()))
    }

    pub fn offset(&self, dist: f64) -> Plane {
        Plane { origin: self.origin + self.normal * dist, ..*self }
    }
}

/// An infinite axis (for revolve, circular pattern).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    pub origin: DVec3,
    pub dir: DVec3,
}

impl Axis {
    pub const X: Axis = Axis { origin: DVec3::ZERO, dir: DVec3::X };
    pub const Y: Axis = Axis { origin: DVec3::ZERO, dir: DVec3::Y };
    pub const Z: Axis = Axis { origin: DVec3::ZERO, dir: DVec3::Z };
}

/// One segment of a closed 2D profile loop, in sketch-plane coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Segment2 {
    Line { a: DVec2, b: DVec2 },
    /// Arc from `start` to `end` around `center`; `ccw` gives direction of travel.
    Arc { center: DVec2, start: DVec2, end: DVec2, ccw: bool },
    /// Full circle (always forms its own loop).
    Circle { center: DVec2, radius: f64 },
}

/// A closed loop of segments, chained end-to-start.
pub type Loop2 = Vec<Segment2>;

/// A region to extrude/revolve: one outer boundary and zero or more holes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub outer: Loop2,
    pub holes: Vec<Loop2>,
}
