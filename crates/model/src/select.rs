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

/// Distance from a hint point to an edge, and the edge tangent there. A closed
/// circle's midpoint depends on where its seam happens to lie (a rebuilt or mirrored
/// copy puts it elsewhere), so circles are measured to the nearest point of the curve.
fn hint_distance(e: &EdgeInfo, p: DVec3) -> (f64, DVec3) {
    if let (true, Some((c, axis, r))) = (e.closed, e.circle) {
        let a = axis.normalize_or_zero();
        let h = (p - c).dot(a);
        let radial = p - c - a * h;
        if radial.length() > 1e-9 && a != DVec3::ZERO {
            return ((radial.length() - r).hypot(h), a.cross(radial).normalize());
        }
    }
    (e.midpoint.distance(p), e.tangent)
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

/// What an edge is regardless of where it sits: kind, intrinsic size (circle radius
/// or line length), axis (circle axis or line tangent) and an anchor (circle
/// centre or line midpoint). Lets a projection find its edge again after a move
/// too large for the hint to accept.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeSig {
    pub kind: EdgeKind,
    pub size: f64,
    pub axis: DVec3,
    pub anchor: DVec3,
}

impl EdgeSig {
    /// `None` for edges with no simple intrinsic size (splines, ellipses).
    pub fn of(e: &EdgeInfo) -> Option<Self> {
        match (e.kind, e.circle) {
            (CurveKind::Line, _) => Some(EdgeSig { kind: EdgeKind::Line, size: e.length, axis: e.tangent, anchor: e.midpoint }),
            (CurveKind::Circle, Some((c, ax, r))) => Some(EdgeSig { kind: EdgeKind::Circle, size: r, axis: ax, anchor: c }),
            _ => None,
        }
    }

    /// The edge with this signature (size within 0.1 %, axis parallel) after the
    /// model moved. Edges that left the plane (normal `n`) of the original are
    /// dropped when any stays in it. One match is taken; several are told apart by
    /// distance from the old anchor (across the edge for lines, so sliding along
    /// it costs nothing) and the nearest wins only if clearly nearer (1.5x).
    pub fn find_like(&self, edges: &[EdgeInfo], n: DVec3) -> Option<u32> {
        let mut hits: Vec<(u32, DVec3)> = edges
            .iter()
            .filter_map(|e| {
                let o = EdgeSig::of(e)?;
                let same = o.kind == self.kind
                    && (o.size - self.size).abs() <= 1e-3 * self.size.abs().max(1e-6)
                    && o.axis.normalize_or_zero().dot(self.axis.normalize_or_zero()).abs() > PARALLEL_COS;
                same.then_some((e.index, o.anchor))
            })
            .collect();
        let n = n.normalize_or_zero();
        let coplanar: Vec<_> = hits.iter().copied().filter(|(_, a)| (*a - self.anchor).dot(n).abs() < 1e-4).collect();
        if !coplanar.is_empty() {
            hits = coplanar;
        }
        if hits.len() == 1 {
            return Some(hits[0].0);
        }
        let t = self.axis.normalize_or_zero();
        let dist = |a: DVec3| {
            let d = a - self.anchor;
            if self.kind == EdgeKind::Line {
                (d - t * d.dot(t)).length()
            } else {
                d.length()
            }
        };
        hits.sort_by(|x, y| dist(x.1).total_cmp(&dist(y.1)));
        match hits.as_slice() {
            [a, b, ..] if dist(b.1) > 1.5 * dist(a.1) => Some(a.0),
            _ => None,
        }
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
                        let (d, tan) = hint_distance(e, *midpoint);
                        (e, hint_score(d, scale, tan.dot(t).abs(), type_ok))
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

/// A rigid placement (pattern instance or mirror) applied to selectors: `point` maps
/// positions, `dir` maps directions.
pub struct Placement<'a> {
    pub point: &'a dyn Fn(DVec3) -> DVec3,
    pub dir: &'a dyn Fn(DVec3) -> DVec3,
}

impl FaceSelector {
    /// The selector for the same face of a placed copy. Selectors that pick by
    /// direction or size over the whole body (`Extreme`, `AllWithNormal`,
    /// `Cylinders`) have no copy of their own and give `Err` with their name.
    pub fn placed(&self, p: &Placement) -> Result<Self, &'static str> {
        match self {
            FaceSelector::Hint { centroid, normal, kind } => Ok(FaceSelector::Hint { centroid: (p.point)(*centroid), normal: (p.dir)(*normal), kind: *kind }),
            FaceSelector::Extreme { .. } => Err("an extreme face"),
            FaceSelector::AllWithNormal { .. } => Err("all faces with a normal"),
            FaceSelector::Cylinders { .. } => Err("all cylinders of a radius"),
        }
    }
}

impl EdgeSelector {
    /// The selector for the same edges of a placed copy; see [`FaceSelector::placed`].
    /// `Circles` with a `near` point is local and moves with it.
    pub fn placed(&self, p: &Placement) -> Result<Self, &'static str> {
        Ok(match self {
            EdgeSelector::Hint { midpoint, tangent, kind } => EdgeSelector::Hint { midpoint: (p.point)(*midpoint), tangent: (p.dir)(*tangent), kind: *kind },
            EdgeSelector::OfFace(f) => EdgeSelector::OfFace(f.placed(p)?),
            EdgeSelector::Between(a, b) => EdgeSelector::Between(a.placed(p)?, b.placed(p)?),
            EdgeSelector::Circles { radius, near: Some((c, tol)) } => EdgeSelector::Circles { radius: *radius, near: Some(((p.point)(*c), *tol)) },
            EdgeSelector::Circles { near: None, .. } => return Err("all circles of a radius"),
            EdgeSelector::Parallel { .. } => return Err("all edges parallel to a direction"),
        })
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
