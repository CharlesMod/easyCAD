//! Topological naming by geometry: features store *selectors*, never raw indices.
//! Selectors are resolved against the shape at regeneration time by best match.
//!
//! Hint selectors carry a confidence threshold: if the best candidate is too far
//! from the stored hint (distance relative to the part size, normal/tangent angle,
//! surface/curve type), resolution fails with [`SelectError::Unresolved`] carrying
//! the best candidate, instead of silently binding to the wrong face or edge. The UI
//! shows such features as "re-pick" and can accept the candidate with
//! [`replace_selector`].

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

/// Surface type recorded with a face hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Other,
}

impl From<SurfaceKind> for FaceKind {
    fn from(k: SurfaceKind) -> Self {
        match k {
            SurfaceKind::Plane => FaceKind::Plane,
            SurfaceKind::Cylinder => FaceKind::Cylinder,
            SurfaceKind::Cone => FaceKind::Cone,
            SurfaceKind::Sphere => FaceKind::Sphere,
            SurfaceKind::Torus => FaceKind::Torus,
            SurfaceKind::Other => FaceKind::Other,
        }
    }
}

/// Curve type recorded with an edge hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Line,
    Circle,
    Other,
}

impl From<CurveKind> for EdgeKind {
    fn from(k: CurveKind) -> Self {
        match k {
            CurveKind::Line => EdgeKind::Line,
            CurveKind::Circle => EdgeKind::Circle,
            _ => EdgeKind::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FaceSelector {
    /// The face best matching a picked centroid, outward normal and surface type.
    Hint {
        centroid: DVec3,
        normal: DVec3,
        #[serde(default)]
        kind: Option<FaceKind>,
    },
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
    /// The edge best matching a picked midpoint, tangent and curve type.
    Hint {
        midpoint: DVec3,
        tangent: DVec3,
        #[serde(default)]
        kind: Option<EdgeKind>,
    },
    /// All edges bounding the selected face(s).
    OfFace(FaceSelector),
    /// Straight edges parallel to `dir` (e.g. "the vertical edges").
    Parallel { dir: DVec3 },
    /// Edges shared by a face from each selector.
    Between(FaceSelector, FaceSelector),
    /// Circular edges of a radius (±0.01 mm), optionally within `near.1` of `near.0`.
    Circles { radius: f64, near: Option<(DVec3, f64)> },
}

/// Either kind of selector (for reporting).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Selector {
    Face(FaceSelector),
    Edge(EdgeSelector),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SelectError {
    /// Nothing matched confidently. `best` is a ready-to-use replacement selector
    /// for the closest candidate (if any); `score` is its mismatch (0 = perfect,
    /// > 1 = rejected).
    #[error("selector could not be resolved confidently (score {score:.2}): {selector:?}")]
    Unresolved { selector: Box<Selector>, best: Option<Box<Selector>>, best_index: Option<u32>, score: f64 },
    #[error(transparent)]
    Kernel(#[from] occt::Error),
}

const PARALLEL_COS: f64 = 0.9998; // ~1.1°

/// Hint acceptance limits: the best candidate must be within this fraction of the
/// part's bounding-box diagonal ...
pub const HINT_MAX_DIST_FRAC: f64 = 0.25;
/// ... and within this angle of the stored normal / tangent, and of the same type.
pub const HINT_MAX_ANGLE_DEG: f64 = 20.0;

/// Mismatch score of a candidate: ≤ 1 accepted, > 1 rejected. Each term is
/// normalised by its limit; the worst term decides (type mismatch is 2).
fn hint_score(dist: f64, scale: f64, cos: f64, type_ok: bool) -> f64 {
    let d = dist / (HINT_MAX_DIST_FRAC * scale);
    let ang = cos.clamp(-1.0, 1.0).acos().to_degrees() / HINT_MAX_ANGLE_DEG;
    let t = if type_ok { 0.0 } else { 2.0 };
    d.max(ang).max(t)
}

fn unresolved(selector: Selector, best: Option<(u32, Selector, f64)>) -> SelectError {
    match best {
        Some((i, s, score)) => SelectError::Unresolved { selector: Box::new(selector), best: Some(Box::new(s)), best_index: Some(i), score },
        None => SelectError::Unresolved { selector: Box::new(selector), best: None, best_index: None, score: f64::INFINITY },
    }
}

impl FaceSelector {
    /// Selector for a face picked in the viewport.
    pub fn from_pick(f: &FaceInfo) -> Self {
        FaceSelector::Hint { centroid: f.centroid, normal: f.normal, kind: Some(f.kind.into()) }
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
            FaceSelector::Hint { centroid, normal, kind } => {
                let n = normal.normalize_or_zero();
                let best = faces
                    .iter()
                    .map(|f| {
                        let type_ok = kind.is_none_or(|k| FaceKind::from(f.kind) == k);
                        (f, hint_score(f.centroid.distance(*centroid), scale, f.normal.dot(n), type_ok))
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                return match best {
                    Some((f, s)) if s <= 1.0 => Ok(vec![f.index]),
                    Some((f, s)) => Err(unresolved(
                        Selector::Face(self.clone()),
                        Some((f.index, Selector::Face(FaceSelector::from_pick(f)), s)),
                    )),
                    None => Err(unresolved(Selector::Face(self.clone()), None)),
                };
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
            return Err(unresolved(Selector::Face(self.clone()), None));
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
        EdgeSelector::Hint { midpoint: e.midpoint, tangent: e.tangent, kind: Some(e.kind.into()) }
    }

    pub fn resolve(&self, shape: &Shape) -> Result<Vec<u32>, SelectError> {
        let edges = shape.edges()?;
        let scale = shape.bbox()?.size().length().max(1.0);
        let mut out: Vec<u32> = match self {
            EdgeSelector::Hint { midpoint, tangent, kind } => {
                let t = tangent.normalize_or_zero();
                let best = edges
                    .iter()
                    .filter(|e| e.length > 0.0)
                    .map(|e| {
                        let type_ok = kind.is_none_or(|k| EdgeKind::from(e.kind) == k);
                        (e, hint_score(e.midpoint.distance(*midpoint), scale, e.tangent.dot(t).abs(), type_ok))
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                return match best {
                    Some((e, s)) if s <= 1.0 => Ok(vec![e.index]),
                    Some((e, s)) => Err(unresolved(
                        Selector::Edge(self.clone()),
                        Some((e.index, Selector::Edge(EdgeSelector::from_pick(e)), s)),
                    )),
                    None => Err(unresolved(Selector::Edge(self.clone()), None)),
                };
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
            return Err(unresolved(Selector::Edge(self.clone()), None));
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

/// Replace every occurrence of selector `old` inside any serializable value (e.g. a
/// `Feature`) with `new`. Returns how many were replaced. Used to accept an
/// `Unresolved` candidate.
pub fn replace_selector<T: Serialize + serde::de::DeserializeOwned>(value: &mut T, old: &Selector, new: &Selector) -> usize {
    let inner = |s: &Selector| match s {
        Selector::Face(f) => serde_json::to_value(f),
        Selector::Edge(e) => serde_json::to_value(e),
    };
    let (Ok(old), Ok(new), Ok(mut v)) = (inner(old), inner(new), serde_json::to_value(&*value)) else {
        return 0;
    };
    fn walk(v: &mut serde_json::Value, old: &serde_json::Value, new: &serde_json::Value) -> usize {
        if v == old {
            *v = new.clone();
            return 1;
        }
        match v {
            serde_json::Value::Array(a) => a.iter_mut().map(|x| walk(x, old, new)).sum(),
            serde_json::Value::Object(o) => o.values_mut().map(|x| walk(x, old, new)).sum(),
            _ => 0,
        }
    }
    let n = walk(&mut v, &old, &new);
    if n > 0 {
        if let Ok(t) = serde_json::from_value(v) {
            *value = t;
            return n;
        }
    }
    0
}
