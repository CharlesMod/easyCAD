//! Face provenance (GUI_NEEDS #8): which feature made each face of a body and in
//! what role ("Bosses · boss top", "Shell · inner wall"), for pick labels.
//!
//! After every feature, each face of a changed body is matched against the faces
//! of the previous state by *surface identity* (same plane, same cylinder axis and
//! radius, ...). A match inherits the earlier origin, so a floor that a later cut
//! trims is still the shell's; a face with no match was made by this feature and
//! gets a role from the feature kind and the face's geometry.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use glam::DVec3;
use occt::{FaceInfo, SurfaceKind};
use serde::Serialize;

use crate::document::DocData;
use crate::feature::{BodyId, Feature, FeatureId, FeatureKind};
use crate::regen::{Body, BodyGeom};

/// What a face is to the feature that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FaceRole {
    /// Extrude: the face in the sketch plane.
    StartCap,
    /// Extrude: the far face (or a pocket's bottom).
    EndCap,
    /// Extrude/revolve side wall.
    Side,
    Fillet,
    Chamfer,
    /// Shell: the faces of the hollow (walls, floor, ceiling).
    Inner,
    Drafted,
    Rib,
    BossTop,
    BossSide,
    BossHole,
    /// A hole cut by an inserted part (auto-hole) or lid holes.
    Hole,
    /// Tongue, groove, lip, gland, snap faces of a parting joint.
    Joint,
    /// The cut face of a split.
    SplitFace,
    /// From an imported STEP or an in-context reference.
    Imported,
    /// A pattern / mirror copy.
    Copy,
    /// Made by the teardrop process stage.
    Teardrop,
    Other,
}

impl FaceRole {
    pub fn label(self) -> &'static str {
        match self {
            FaceRole::StartCap => "start face",
            FaceRole::EndCap => "end face",
            FaceRole::Side => "side",
            FaceRole::Fillet => "fillet",
            FaceRole::Chamfer => "chamfer",
            FaceRole::Inner => "inner wall",
            FaceRole::Drafted => "drafted face",
            FaceRole::Rib => "rib",
            FaceRole::BossTop => "boss top",
            FaceRole::BossSide => "boss side",
            FaceRole::BossHole => "boss hole",
            FaceRole::Hole => "hole",
            FaceRole::Joint => "joint",
            FaceRole::SplitFace => "split face",
            FaceRole::Imported => "imported",
            FaceRole::Copy => "pattern copy",
            FaceRole::Teardrop => "teardrop",
            FaceRole::Other => "face",
        }
    }
}

/// The feature (`None` = the process stage) and role that made a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FaceOrigin {
    pub feature: Option<FeatureId>,
    pub role: FaceRole,
}

impl FaceOrigin {
    /// "Bosses · boss top"; the shell's upward inner face reads "floor", its
    /// downward one "ceiling".
    pub fn label(&self, doc: &DocData, face: Option<&FaceInfo>) -> String {
        let name = self.feature.and_then(|f| doc.feature(f)).map(|f| f.name.clone()).unwrap_or_else(|| "Process".into());
        let role = match (self.role, face) {
            (FaceRole::Inner, Some(f)) if f.kind == SurfaceKind::Plane && f.normal.z > 0.99 => "floor",
            (FaceRole::Inner, Some(f)) if f.kind == SurfaceKind::Plane && f.normal.z < -0.99 => "ceiling",
            (r, _) => r.label(),
        };
        format!("{name} · {role}")
    }
}

/// Face origins of one body geometry, computed on first use from the previous
/// state's tables (regeneration only links them, so it costs nothing).
pub(crate) struct FaceTable {
    geom: Arc<BodyGeom>,
    id: BodyId,
    prev: FaceTables,
    /// The feature that produced this geometry (`None` = the process stage), and
    /// its sketch plane for extrudes.
    made_by: Option<(Arc<Feature>, Option<crate::geom::Plane>)>,
    cell: OnceLock<(Vec<FaceInfo>, Arc<Vec<FaceOrigin>>)>,
}

pub(crate) type FaceTables = BTreeMap<BodyId, Arc<FaceTable>>;

/// Lazily computed face provenance of a regeneration's bodies.
#[derive(Clone, Default)]
pub struct FaceOrigins(pub(crate) FaceTables);

impl FaceOrigins {
    /// Origins of every face of `body` (indexed like `body.shape().faces()`).
    /// Computed on first call (a few ms per changed feature), then cached.
    pub fn of(&self, body: BodyId) -> Option<Arc<Vec<FaceOrigin>>> {
        self.0.get(&body).map(|t| t.get().1.clone())
    }
    pub fn face(&self, body: BodyId, face: u32) -> Option<FaceOrigin> {
        self.of(body).and_then(|v| v.get(face as usize).copied())
    }
}

impl FaceTable {
    fn get(&self) -> &(Vec<FaceInfo>, Arc<Vec<FaceOrigin>>) {
        self.cell.get_or_init(|| {
            let infos = self.geom.shape.faces().unwrap_or_default();
            // the body's own previous faces first, then every other body's (a
            // split's new half, a joined tool body)
            let own = self.prev.get(&self.id);
            let others = self.prev.iter().filter(|(id, _)| **id != self.id).map(|(_, t)| t);
            let cands: Vec<&(Vec<FaceInfo>, Arc<Vec<FaceOrigin>>)> = own.into_iter().chain(others).map(|t| t.get()).collect();
            let origins = infos
                .iter()
                .map(|fi| {
                    cands
                        .iter()
                        .flat_map(|t| t.0.iter().zip(t.1.iter()))
                        .filter(|(old, _)| same_surface(fi, old))
                        .min_by(|a, b| a.0.centroid.distance(fi.centroid).total_cmp(&b.0.centroid.distance(fi.centroid)))
                        .map(|(_, o)| *o)
                        .unwrap_or_else(|| match &self.made_by {
                            Some((f, plane)) => FaceOrigin { feature: Some(f.id), role: role_of(&f.kind, fi, plane.as_ref()) },
                            None => FaceOrigin { feature: None, role: FaceRole::Teardrop },
                        })
                })
                .collect();
            (infos, Arc::new(origins))
        })
    }
}

const TOL: f64 = 1e-4;

fn radial(f: &FaceInfo) -> Option<DVec3> {
    let (p, d) = f.axis?;
    let d = d.normalize_or_zero();
    let v = f.point - p;
    Some(v - d * v.dot(d))
}

fn same_surface(a: &FaceInfo, b: &FaceInfo) -> bool {
    if a.kind != b.kind {
        return false;
    }
    match a.kind {
        SurfaceKind::Plane => a.normal.dot(b.normal) > 1.0 - 1e-6 && (a.point - b.point).dot(a.normal).abs() < TOL,
        SurfaceKind::Cylinder | SurfaceKind::Cone => match (a.axis, b.axis, radial(a), radial(b)) {
            (Some((pa, da)), Some((pb, db)), Some(ra), Some(rb)) => {
                let (da, db) = (da.normalize_or_zero(), db.normalize_or_zero());
                let off = pb - pa;
                da.dot(db).abs() > 1.0 - 1e-6
                    && (off - da * off.dot(da)).length() < TOL
                    && (a.radius - b.radius).abs() < TOL
                    && (a.normal.dot(ra) > 0.0) == (b.normal.dot(rb) > 0.0)
            }
            _ => false,
        },
        _ => a.centroid.distance(b.centroid) < TOL && (a.area - b.area).abs() < TOL * a.area.max(1.0),
    }
}

/// Role of a face made by `kind`.
fn role_of(kind: &FeatureKind, f: &FaceInfo, sketch_plane: Option<&crate::geom::Plane>) -> FaceRole {
    let convex = || radial(f).map(|r| f.normal.dot(r) > 0.0).unwrap_or(true);
    match kind {
        FeatureKind::Extrude { .. } => {
            let Some(pl) = sketch_plane else { return FaceRole::Side };
            let n = pl.normal.normalize_or_zero();
            if f.kind == SurfaceKind::Plane && f.normal.dot(n).abs() > 0.999 {
                if (f.point - pl.origin).dot(n).abs() < 1e-5 {
                    FaceRole::StartCap
                } else {
                    FaceRole::EndCap
                }
            } else {
                FaceRole::Side
            }
        }
        FeatureKind::ExtrudeFace { .. } | FeatureKind::Revolve { .. } => FaceRole::Side,
        FeatureKind::Fillet { .. } => FaceRole::Fillet,
        FeatureKind::Chamfer { .. } => FaceRole::Chamfer,
        FeatureKind::Shell { .. } => FaceRole::Inner,
        FeatureKind::Draft { .. } => FaceRole::Drafted,
        FeatureKind::Rib { .. } => FaceRole::Rib,
        FeatureKind::ScrewBoss { plane, .. } | FeatureKind::StandoffsFromPcb { plane, .. } => match f.kind {
            SurfaceKind::Plane if f.normal.dot(plane.normal.normalize_or_zero()).abs() > 0.999 => FaceRole::BossTop,
            SurfaceKind::Cylinder | SurfaceKind::Cone if convex() => FaceRole::BossSide,
            SurfaceKind::Cylinder | SurfaceKind::Cone => FaceRole::BossHole,
            SurfaceKind::Torus => FaceRole::Fillet,
            _ => FaceRole::Other,
        },
        FeatureKind::InsertPart { .. } => FaceRole::Hole,
        FeatureKind::Pattern { .. } | FeatureKind::Mirror { .. } => FaceRole::Copy,
        FeatureKind::Split { .. } => FaceRole::SplitFace,
        FeatureKind::PartingJoint { .. } => FaceRole::Joint,
        FeatureKind::ImportStep { .. } | FeatureKind::ContextRef { .. } => FaceRole::Imported,
        FeatureKind::DatumPlane { .. } | FeatureKind::DatumAxis { .. } | FeatureKind::DatumPoint { .. } => FaceRole::Other,
    }
}

/// Tables for `bodies` after `made_by` ran on a state whose tables are `prev`
/// (links only; nothing is computed here). Unchanged geometry keeps its table.
/// `made_by = None` is the process stage.
pub(crate) fn update(prev: &FaceTables, bodies: &[Body], made_by: Option<&Feature>, doc: &DocData, datums: &[(FeatureId, crate::datum::DatumGeom)]) -> FaceTables {
    let made = made_by.map(|f| {
        let plane = match &f.kind {
            FeatureKind::Extrude { sketch, .. } => doc.sketch(*sketch).map(|s| crate::datum::effective_sketch_plane(s, datums)),
            _ => None,
        };
        (Arc::new(f.clone()), plane)
    });
    let mut out = FaceTables::new();
    for b in bodies {
        if let Some(t) = prev.get(&b.id) {
            if Arc::ptr_eq(&t.geom, &b.geom) {
                out.insert(b.id, t.clone());
                continue;
            }
        }
        out.insert(b.id, Arc::new(FaceTable { geom: b.geom.clone(), id: b.id, prev: prev.clone(), made_by: made.clone(), cell: OnceLock::new() }));
    }
    out
}
