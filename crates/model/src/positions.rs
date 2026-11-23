//! Positions (animation states): named poses of the model, e.g. a lid open and a
//! carriage raised. Each one moves some bodies (root component) or occurrences
//! rigidly from where the history put them: a rotation about an axis through a
//! pivot, then a translation. Display only: the feature history never sees them.

use glam::{DAffine3, DMat3, DVec3};
use serde::{Deserialize, Serialize};

use crate::assembly::OccurrenceId;
use crate::feature::BodyId;
use crate::params::{Expr, Values};
use crate::regen::Body;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PositionId(pub u32);

/// What a move applies to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoseTarget {
    /// A body of the root component.
    Body(BodyId),
    /// An occurrence (path from the root) with everything under it.
    Occurrence(Vec<OccurrenceId>),
}

/// One rigid move: rotate `angle_deg` about `axis` through `pivot`, then translate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoseMove {
    pub target: PoseTarget,
    #[serde(default = "zero3")]
    pub translate: [Expr; 3],
    #[serde(default)]
    pub pivot: DVec3,
    #[serde(default = "z_axis")]
    pub axis: DVec3,
    #[serde(default = "zero")]
    pub angle_deg: Expr,
}

impl PoseMove {
    pub fn new(target: PoseTarget) -> Self {
        PoseMove { target, translate: zero3(), pivot: DVec3::ZERO, axis: DVec3::Z, angle_deg: zero() }
    }
}

/// A named position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub id: PositionId,
    pub name: String,
    #[serde(default)]
    pub moves: Vec<PoseMove>,
}

fn zero() -> Expr {
    Expr::new("0")
}
fn zero3() -> [Expr; 3] {
    [zero(), zero(), zero()]
}
fn z_axis() -> DVec3 {
    DVec3::Z
}

/// A move with its expressions evaluated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rigid {
    pub translate: DVec3,
    pub pivot: DVec3,
    pub axis: DVec3,
    pub angle_deg: f64,
}

impl Rigid {
    pub const IDENTITY: Rigid = Rigid { translate: DVec3::ZERO, pivot: DVec3::ZERO, axis: DVec3::Z, angle_deg: 0.0 };

    pub fn is_identity(&self) -> bool {
        self.translate.length() < 1e-12 && self.angle_deg.abs() < 1e-12
    }
    /// Rotation about the origin and the translation that make up this move.
    pub fn parts(&self) -> (DVec3, f64, DVec3) {
        let axis = self.axis.try_normalize().unwrap_or(DVec3::Z);
        let r = DMat3::from_axis_angle(axis, self.angle_deg.to_radians());
        (axis, self.angle_deg, self.translate + self.pivot - r * self.pivot)
    }
    pub fn affine(&self) -> DAffine3 {
        let (axis, a, t) = self.parts();
        DAffine3 { matrix3: DMat3::from_axis_angle(axis, a.to_radians()), translation: t }
    }
    /// `t` of the way from `a` to `b` (0 = a, 1 = b). Translations and angles are
    /// interpolated; the axis and pivot are taken from whichever side rotates.
    pub fn lerp(a: &Rigid, b: &Rigid, t: f64) -> Rigid {
        let rot = if b.angle_deg.abs() > 1e-12 { b } else { a };
        let same_axis = a.angle_deg.abs() < 1e-12
            || b.angle_deg.abs() < 1e-12
            || (a.axis.normalize_or_zero().dot(b.axis.normalize_or_zero()) > 0.9999 && a.pivot.distance(b.pivot) < 1e-9);
        if !same_axis {
            return if t < 0.5 { *a } else { *b };
        }
        Rigid {
            translate: a.translate.lerp(b.translate, t),
            pivot: rot.pivot,
            axis: rot.axis,
            angle_deg: a.angle_deg + (b.angle_deg - a.angle_deg) * t,
        }
    }
}

/// An evaluated position: what moves and how.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pose {
    pub moves: Vec<(PoseTarget, Rigid)>,
}

impl Pose {
    /// The move of a root body (identity when not listed).
    pub fn body(&self, id: BodyId) -> Rigid {
        self.moves.iter().find(|(t, _)| *t == PoseTarget::Body(id)).map(|m| m.1).unwrap_or(Rigid::IDENTITY)
    }
    /// The move of an occurrence path: the closest listed ancestor (or itself).
    pub fn occurrence(&self, path: &[OccurrenceId]) -> Option<Rigid> {
        self.moves
            .iter()
            .filter_map(|(t, r)| match t {
                PoseTarget::Occurrence(p) if !p.is_empty() && path.starts_with(p) => Some((p.len(), *r)),
                _ => None,
            })
            .max_by_key(|x| x.0)
            .map(|x| x.1)
    }
    pub fn is_empty(&self) -> bool {
        self.moves.iter().all(|m| m.1.is_identity())
    }
    /// Root bodies moved to this pose (unlisted bodies unchanged).
    pub fn apply(&self, bodies: &[Body]) -> Result<Vec<Body>, String> {
        bodies
            .iter()
            .map(|b| {
                let r = self.body(b.id);
                if r.is_identity() {
                    Ok(b.clone())
                } else {
                    let (axis, a, t) = r.parts();
                    b.moved(axis, a, t).map_err(|e| format!("{}: {e}", b.name))
                }
            })
            .collect()
    }
    /// `t` of the way from `a` to `b`, per target.
    pub fn lerp(a: &Pose, b: &Pose, t: f64) -> Pose {
        let mut targets: Vec<PoseTarget> = a.moves.iter().map(|m| m.0.clone()).collect();
        for m in &b.moves {
            if !targets.contains(&m.0) {
                targets.push(m.0.clone());
            }
        }
        let get = |p: &Pose, t: &PoseTarget| p.moves.iter().find(|m| m.0 == *t).map(|m| m.1).unwrap_or(Rigid::IDENTITY);
        Pose { moves: targets.into_iter().map(|x| (x.clone(), Rigid::lerp(&get(a, &x), &get(b, &x), t))).collect() }
    }
    /// Place assembly instances: occurrence moves pre-multiply the instance
    /// transforms, root body moves move the body.
    pub fn apply_assembly(&self, r: &crate::assembly::AssemblyResult) -> Result<crate::assembly::AssemblyResult, String> {
        let mut out = r.clone();
        for i in out.instances.iter_mut() {
            if i.path.is_empty() {
                let m = self.body(i.body.id);
                if !m.is_identity() {
                    let (axis, a, t) = m.parts();
                    i.body = i.body.moved(axis, a, t)?;
                }
            } else if let Some(m) = self.occurrence(&i.path) {
                let w = crate::assembly::Transform::from_affine(&m.affine());
                i.transform = w.mul(&i.transform);
                i.display = w.mul(&i.display);
            }
        }
        Ok(out)
    }
}

impl Position {
    pub fn evaluate(&self, vals: &Values) -> Result<Pose, String> {
        let ev = |e: &Expr| e.eval(vals).map_err(|err| format!("position `{}`: `{}`: {err}", self.name, e.0));
        let mut moves = Vec::new();
        for m in &self.moves {
            moves.push((
                m.target.clone(),
                Rigid { translate: DVec3::new(ev(&m.translate[0])?, ev(&m.translate[1])?, ev(&m.translate[2])?), pivot: m.pivot, axis: m.axis, angle_deg: ev(&m.angle_deg)? },
            ));
        }
        Ok(Pose { moves })
    }
}

impl crate::document::DocData {
    pub fn position(&self, id: PositionId) -> Option<&Position> {
        self.positions.iter().find(|p| p.id == id)
    }
    pub fn position_named(&self, name: &str) -> Option<&Position> {
        self.positions.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
    /// The evaluated pose of a position.
    pub fn pose(&self, id: PositionId) -> Result<Pose, String> {
        let p = self.position(id).ok_or_else(|| format!("position {} does not exist", id.0))?;
        let vals = self.values().map_err(|e| e.to_string())?;
        p.evaluate(&vals)
    }
    /// Add a position (no moves) and return its id.
    pub fn add_position(&mut self, name: &str) -> PositionId {
        let id = PositionId(self.alloc());
        self.positions.push(Position { id, name: name.to_string(), moves: Vec::new() });
        id
    }
}
