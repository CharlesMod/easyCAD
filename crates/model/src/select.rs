//! Topological naming by geometry: features store *selectors*, never raw indices.
//! Selectors are resolved against the shape at regeneration time by best match.

use glam::DVec3;
use occt::{CurveKind, EdgeInfo, FaceInfo, Shape, SurfaceKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Extremum {
    /// Farthest along the normal (e.g. top-most for +Z).
    Max,
    /// Nearest along the normal.
    Min,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FaceSelector {
    /// The face best matching a picked centroid and outward normal.
    Hint { centroid: DVec3, normal: DVec3 },
    /// The planar face with this outward normal that is extreme along it,
    /// e.g. `{normal: +Z, which: Max}` = "the top face".
    Extreme { normal: DVec3, which: Extremum },
    /// Every planar face whose outward normal is parallel to `normal`.
    AllWithNormal { normal: DVec3 },
    /// Cylindrical faces (optionally of one radius, ±0.01 mm, and axis direction).
    Cylinders { radius: Option<f64>, axis: Option<DVec3> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EdgeSelector {
    /// The edge best matching a picked midpoint and tangent.
    Hint { midpoint: DVec3, tangent: DVec3 },
    /// All edges bounding the selected face(s).
    OfFace(FaceSelector),
    /// Straight edges parallel to `dir` (e.g. "the vertical edges").
    Parallel { dir: DVec3 },
    /// Edges shared by a face from each selector.
    Between(FaceSelector, FaceSelector),
    /// Circular edges of a radius (±0.01 mm), optionally within `near.1` of `near.0`.
    Circles { radius: f64, near: Option<(DVec3, f64)> },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SelectError {
    #[error("selector matched nothing: {0}")]
    NoMatch(String),
    #[error(transparent)]
    Kernel(#[from] occt::Error),
}

const PARALLEL_COS: f64 = 0.9998; // ~1.1°

impl FaceSelector {
    /// Selector for a face picked in the viewport.
    pub fn from_pick(f: &FaceInfo) -> Self {
        FaceSelector::Hint { centroid: f.centroid, normal: f.normal }
    }
    pub fn top() -> Self {
        FaceSelector::Extreme { normal: DVec3::Z, which: Extremum::Max }
    }
    pub fn bottom() -> Self {
        FaceSelector::Extreme { normal: DVec3::NEG_Z, which: Extremum::Max }
    }

    pub fn resolve_infos(&self, faces: &[FaceInfo], scale: f64) -> Result<Vec<u32>, SelectError> {
        let planar_with = |n: DVec3| {
            let n = n.normalize_or_zero();
            faces.iter().filter(move |f| f.kind == SurfaceKind::Plane && f.normal.dot(n) > PARALLEL_COS)
        };
        let out: Vec<u32> = match self {
            FaceSelector::Hint { centroid, normal } => {
                let n = normal.normalize_or_zero();
                faces
                    .iter()
                    .map(|f| (f.index, f.centroid.distance(*centroid) + scale * (1.0 - f.normal.dot(n))))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| vec![i])
                    .unwrap_or_default()
            }
            FaceSelector::Extreme { normal, which } => {
                let n = normal.normalize_or_zero();
                let key = |f: &&FaceInfo| f.centroid.dot(n);
                let it = planar_with(n);
                let best = match which {
                    Extremum::Max => it.max_by(|a, b| key(a).total_cmp(&key(b))),
                    Extremum::Min => it.min_by(|a, b| key(a).total_cmp(&key(b))),
                };
                best.map(|f| vec![f.index]).unwrap_or_default()
            }
            FaceSelector::AllWithNormal { normal } => planar_with(*normal).map(|f| f.index).collect(),
            FaceSelector::Cylinders { radius, axis } => faces
                .iter()
                .filter(|f| f.kind == SurfaceKind::Cylinder)
                .filter(|f| radius.is_none_or(|r| (f.radius - r).abs() < 0.01))
                .filter(|f| match (axis, f.axis) {
                    (Some(a), Some((_, d))) => d.normalize().dot(a.normalize()).abs() > PARALLEL_COS,
                    (Some(_), None) => false,
                    _ => true,
                })
                .map(|f| f.index)
                .collect(),
        };
        if out.is_empty() {
            return Err(SelectError::NoMatch(format!("{self:?}")));
        }
        Ok(out)
    }

    pub fn resolve(&self, shape: &Shape) -> Result<Vec<u32>, SelectError> {
        let scale = shape.bbox()?.size().length().max(1.0);
        self.resolve_infos(&shape.faces()?, scale)
    }
}

impl EdgeSelector {
    /// Selector for an edge picked in the viewport.
    pub fn from_pick(e: &EdgeInfo) -> Self {
        EdgeSelector::Hint { midpoint: e.midpoint, tangent: e.tangent }
    }

    pub fn resolve(&self, shape: &Shape) -> Result<Vec<u32>, SelectError> {
        let edges = shape.edges()?;
        let scale = shape.bbox()?.size().length().max(1.0);
        let mut out: Vec<u32> = match self {
            EdgeSelector::Hint { midpoint, tangent } => {
                let t = tangent.normalize_or_zero();
                edges
                    .iter()
                    .filter(|e| e.kind != CurveKind::Other || e.length > 0.0)
                    .map(|e| (e.index, e.midpoint.distance(*midpoint) + scale * (1.0 - e.tangent.dot(t).abs())))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| vec![i])
                    .unwrap_or_default()
            }
            EdgeSelector::OfFace(fs) => {
                let mut v = Vec::new();
                for f in fs.resolve(shape)? {
                    v.extend(shape.face_edges(f)?);
                }
                v
            }
            EdgeSelector::Parallel { dir } => {
                let d = dir.normalize_or_zero();
                edges.iter().filter(|e| e.kind == CurveKind::Line && e.tangent.dot(d).abs() > PARALLEL_COS).map(|e| e.index).collect()
            }
            EdgeSelector::Between(a, b) => {
                let fa = a.resolve(shape)?;
                let fb = b.resolve(shape)?;
                edges
                    .iter()
                    .filter(|e| {
                        e.faces.len() == 2
                            && ((fa.contains(&e.faces[0]) && fb.contains(&e.faces[1]))
                                || (fa.contains(&e.faces[1]) && fb.contains(&e.faces[0])))
                    })
                    .map(|e| e.index)
                    .collect()
            }
            EdgeSelector::Circles { radius, near } => edges
                .iter()
                .filter(|e| match e.circle {
                    Some((c, _, r)) => (r - radius).abs() < 0.01 && near.is_none_or(|(p, tol)| c.distance(p) <= tol),
                    None => false,
                })
                .map(|e| e.index)
                .collect(),
        };
        out.sort_unstable();
        out.dedup();
        if out.is_empty() {
            return Err(SelectError::NoMatch(format!("{self:?}")));
        }
        Ok(out)
    }
}

/// Resolve a union of selectors (deduplicated, sorted).
pub fn resolve_edges(shape: &Shape, sels: &[EdgeSelector]) -> Result<Vec<u32>, SelectError> {
    let mut v = Vec::new();
    for s in sels {
        v.extend(s.resolve(shape)?);
    }
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

pub fn resolve_faces(shape: &Shape, sels: &[FaceSelector]) -> Result<Vec<u32>, SelectError> {
    let mut v = Vec::new();
    for s in sels {
        v.extend(s.resolve(shape)?);
    }
    v.sort_unstable();
    v.dedup();
    Ok(v)
}
