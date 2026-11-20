//! Construction geometry ("datums"): planes, axes and points defined from other
//! geometry, parametric in the feature history.
//!
//! A datum feature stores [`GeomRef`]s (never raw numbers) and is re-resolved at every
//! regeneration against the model state *at that point of the history*: the bodies
//! built so far, earlier datums, sketches and the origin. The result is a
//! [`DatumGeom`] recorded in `RegenResult::datums`.
//!
//! # Conventions
//! * **Plane frame.** Derived planes get their `x_dir` from the normal alone, so a
//!   plane does not spin when its inputs change: world X projected onto the plane,
//!   or world Y projected when the normal is within ~26° of the X axis
//!   (`|n.x| > 0.9`). This reproduces `Plane::XY/XZ/YZ`. `Offset` keeps the base
//!   plane's own frame.
//! * **Flip** reverses the normal of a datum plane (origin and `x_dir` stay).
//! * **Face normals** are the outward normals of the face (as the kernel reports).
//! * **Angle plane.** Through the axis `(p, d)`; its zero-angle plane is the plane
//!   containing the axis whose normal is the base plane's normal with the axis
//!   component removed (explicit `base`), or, by default, the plane containing the
//!   axis and the first of world Z, X, Y that is not within ~26° of the axis. The
//!   result is that zero-angle plane rotated about the axis by `angle` degrees
//!   (right-hand rule about the axis direction).
//! * **Midplane** of parallel planes is halfway between them (normal of `a`). Of
//!   non-parallel planes it is the bisector that contains their intersection line,
//!   with normal `n_a - n_b`; for two faces of a solid (outward normals) that is
//!   the bisector through the material wedge.
//! * **Tangent plane.** To a cylinder, touching it at the point whose radial
//!   direction is the zero direction rotated about the cylinder axis by `angle`
//!   (zero direction: the base plane's normal with the axis component removed, or
//!   the same default rule as the angle plane). Normal points away from the axis.
//!   Cylinder axes are direction-canonical (first non-zero component of x, y, z
//!   positive), so angles are right-handed about that direction, not the kernel's.
//! * **NormalAt.** Plane through the path point at fraction `t` (0..1 of the edge,
//!   start to end), normal = path tangent. Infinite axes (datum axes, origin axes)
//!   have the reference segment `origin..origin+dir` so `t` is millimetres along
//!   the axis. Closed circles start at their seam.
//! * **Vertex refs** match the nearest body vertex to `hint` within
//!   `max(0.5 mm, 5 % of the model diagonal)`.
//! * Line intersections (`EdgePlane`) use the infinite carrier line of the edge.

use glam::{DMat3, DQuat, DVec2, DVec3};
use occt::SurfaceKind;
use serde::{Deserialize, Serialize};

use crate::document::DocData;
use crate::feature::{FeatureId, SketchId};
use crate::geom::Plane;
use crate::params::{Expr, Values};
use crate::regen::Body;
use crate::select::{EdgeSelector, FaceSelector, SelectError, Selector};
use crate::sketch::{segment_point, Geom, Sketch};

/// Fixed world datums.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OriginRef {
    XY,
    XZ,
    YZ,
    XAxis,
    YAxis,
    ZAxis,
    Origin,
}

/// A reference to existing geometry, re-resolved at every regen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GeomRef {
    Origin(OriginRef),
    /// A construction plane/axis/point feature.
    Datum(FeatureId),
    Face(FaceSelector),
    Edge(EdgeSelector),
    /// Nearest body vertex to `hint` (tolerance-matched).
    Vertex { hint: DVec3 },
    /// A sketch line/arc/circle (construction or not): `entity` indexes `Sketch::entities`.
    SketchLine { sketch: SketchId, entity: u32 },
    /// A sketch point: `point` indexes `Sketch::points`.
    SketchPoint { sketch: SketchId, point: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlaneDef {
    /// Parallel to a planar ref, `distance` along its normal.
    Offset { base: GeomRef, distance: Expr },
    /// Through a line, `angle` degrees from the base plane (see module docs).
    Angle {
        axis: GeomRef,
        #[serde(default)]
        base: Option<GeomRef>,
        angle: Expr,
    },
    Midplane { a: GeomRef, b: GeomRef },
    /// Contains two coplanar lines.
    TwoLines { a: GeomRef, b: GeomRef },
    ThreePoints { a: GeomRef, b: GeomRef, c: GeomRef },
    /// Tangent to a cylinder at `angle` degrees around its axis from the base plane.
    Tangent {
        face: GeomRef,
        #[serde(default)]
        base: Option<GeomRef>,
        angle: Expr,
    },
    /// Perpendicular to an edge/line at fraction `t` (0..1).
    NormalAt { path: GeomRef, t: Expr },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AxisDef {
    /// Axis of a cylindrical/conical face, or of a circular edge / sketch circle.
    Cylinder { face: GeomRef },
    /// Along an edge / sketch line / axis.
    Line { line: GeomRef },
    TwoPoints { a: GeomRef, b: GeomRef },
    TwoPlanes { a: GeomRef, b: GeomRef },
    /// Normal to a planar or cylindrical face through `point`.
    NormalAt { face: GeomRef, point: GeomRef },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PointDef {
    /// A vertex / sketch point / existing point.
    At { point: GeomRef },
    /// Centre of a circular edge / sketch circle, cylinder axis point, sphere centre.
    Center { circle: GeomRef },
    /// Where an edge's line pierces a plane.
    EdgePlane { edge: GeomRef, plane: GeomRef },
    ThreePlanes { a: GeomRef, b: GeomRef, c: GeomRef },
    /// Intersection of two lines (midpoint of the closest approach when skew).
    TwoLines { a: GeomRef, b: GeomRef },
}

/// Resolved construction geometry.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DatumGeom {
    Plane(Plane),
    Axis { origin: DVec3, dir: DVec3 },
    Point(DVec3),
}

/// Why a datum could not be computed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DatumError {
    /// A face/edge selector no longer matches confidently (the existing flow).
    #[error(transparent)]
    Select(#[from] SelectError),
    #[error("{0}")]
    Msg(String),
}

fn err<T>(m: impl Into<String>) -> Result<T, DatumError> {
    Err(DatumError::Msg(m.into()))
}

/// What a [`GeomRef`] points at, in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resolved {
    Plane(Plane),
    /// A line through `a` and `b` (a finite reference segment; `b - a` is the direction).
    Line { a: DVec3, b: DVec3 },
    /// A circle or arc about `axis` through `start`, via `mid`, to `end`.
    Arc { center: DVec3, axis: DVec3, radius: f64, start: DVec3, mid: DVec3, end: DVec3, closed: bool },
    /// A cylindrical or conical face (axis through `origin`, nearest the face centroid).
    Cylinder { origin: DVec3, dir: DVec3, radius: f64 },
    Sphere { center: DVec3, radius: f64 },
    Point(DVec3),
}

impl Resolved {
    fn what(&self) -> &'static str {
        match self {
            Resolved::Plane(_) => "a plane",
            Resolved::Line { .. } => "a line",
            Resolved::Arc { .. } => "a circle/arc",
            Resolved::Cylinder { .. } => "a cylinder",
            Resolved::Sphere { .. } => "a sphere",
            Resolved::Point(_) => "a point",
        }
    }
    fn plane(&self) -> Result<Plane, DatumError> {
        match self {
            Resolved::Plane(p) => Ok(*p),
            o => err(format!("a plane is needed, but this is {}", o.what())),
        }
    }
    fn point(&self) -> Result<DVec3, DatumError> {
        match self {
            Resolved::Point(p) => Ok(*p),
            o => err(format!("a point is needed, but this is {}", o.what())),
        }
    }
    /// A straight line `(point, unit direction)`.
    fn line(&self) -> Result<(DVec3, DVec3), DatumError> {
        match self {
            Resolved::Line { a, b } => {
                let d = (*b - *a).normalize_or_zero();
                if d == DVec3::ZERO {
                    return err("a line has zero length");
                }
                Ok((*a, d))
            }
            o => err(format!("a straight line is needed, but this is {}", o.what())),
        }
    }
}

/// Everything a datum can refer to at its place in the history.
pub struct RefCtx<'a> {
    pub doc: &'a DocData,
    /// Bodies built by the features before this one.
    pub bodies: &'a [Body],
    /// Earlier datums (live ones only), in history order.
    pub datums: &'a [(FeatureId, DatumGeom)],
    pub vals: &'a Values,
}

/// The plane a sketch lies on right now: its datum plane if `plane_src` is set and
/// that datum is available, else the stored plane.
pub fn effective_sketch_plane(sk: &Sketch, datums: &[(FeatureId, DatumGeom)]) -> Plane {
    match sk.plane_src.and_then(|id| datums.iter().find(|(f, _)| *f == id)) {
        Some((_, DatumGeom::Plane(p))) => *p,
        _ => sk.plane,
    }
}

/// World X projected onto the plane (world Y when the normal is near X).
pub fn stable_x(normal: DVec3) -> DVec3 {
    let n = normal.normalize_or_zero();
    let base = if n.x.abs() > 0.9 { DVec3::Y } else { DVec3::X };
    (base - n * base.dot(n)).normalize_or_zero()
}

/// The sign of an undirected axis (a cylinder's or circle's) made canonical: the
/// first non-negligible component of x, y, z is positive. Kernel axis directions are
/// arbitrary; angles around them must not depend on that.
fn canon(d: DVec3) -> DVec3 {
    let d = d.normalize_or_zero();
    for c in [d.x, d.y, d.z] {
        if c.abs() > 1e-9 {
            return if c < 0.0 { -d } else { d };
        }
    }
    d
}

fn plane_at(origin: DVec3, normal: DVec3) -> Plane {
    let n = normal.normalize();
    Plane { origin, x_dir: stable_x(n), normal: n }
}

const PAR: f64 = 1e-6; // |sin| below this counts as parallel
fn near_zero_len(v: DVec3) -> bool {
    v.length() < 1e-9
}

impl<'a> RefCtx<'a> {
    fn scale(&self) -> f64 {
        let mut d = 0.0f64;
        for b in self.bodies {
            if let Ok(bb) = b.shape().bbox() {
                d = d.max(bb.size().length());
            }
        }
        d.max(1.0)
    }

    fn sketch(&self, id: SketchId) -> Result<(Sketch, Plane), DatumError> {
        let mut sk = match self.doc.sketch(id) {
            Some(s) => s.clone(),
            None => return err(format!("sketch {id:?} does not exist")),
        };
        if !sk.constraints.is_empty() {
            let _ = sk.solve(self.vals);
        }
        let plane = effective_sketch_plane(&sk, self.datums);
        Ok((sk, plane))
    }

    /// Resolve a reference against the current state.
    pub fn resolve(&self, r: &GeomRef) -> Result<Resolved, DatumError> {
        resolve_ref(r, self)
    }
}

/// Resolve a reference (usable by the UI for previews and validation).
pub fn resolve_ref(r: &GeomRef, ctx: &RefCtx) -> Result<Resolved, DatumError> {
    match r {
        GeomRef::Origin(o) => Ok(match o {
            OriginRef::XY => Resolved::Plane(Plane::XY),
            OriginRef::XZ => Resolved::Plane(Plane::XZ),
            OriginRef::YZ => Resolved::Plane(Plane::YZ),
            OriginRef::XAxis => Resolved::Line { a: DVec3::ZERO, b: DVec3::X },
            OriginRef::YAxis => Resolved::Line { a: DVec3::ZERO, b: DVec3::Y },
            OriginRef::ZAxis => Resolved::Line { a: DVec3::ZERO, b: DVec3::Z },
            OriginRef::Origin => Resolved::Point(DVec3::ZERO),
        }),
        GeomRef::Datum(id) => match ctx.datums.iter().find(|(f, _)| f == id) {
            Some((_, DatumGeom::Plane(p))) => Ok(Resolved::Plane(*p)),
            Some((_, DatumGeom::Axis { origin, dir })) => Ok(Resolved::Line { a: *origin, b: *origin + dir.normalize_or_zero() }),
            Some((_, DatumGeom::Point(p))) => Ok(Resolved::Point(*p)),
            None => err(format!("construction feature {id:?} is not available here (it comes later, is suppressed or failed)")),
        },
        GeomRef::Face(fs) => resolve_face(fs, ctx),
        GeomRef::Edge(es) => resolve_edge(es, ctx),
        GeomRef::Vertex { hint } => {
            let tol = (0.05 * ctx.scale()).max(0.5);
            let mut best: Option<(f64, DVec3)> = None;
            for b in ctx.bodies {
                for e in b.shape().edges().map_err(SelectError::Kernel)? {
                    for p in [e.start, e.end] {
                        let d = p.distance(*hint);
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, p));
                        }
                    }
                }
            }
            match best {
                Some((d, p)) if d <= tol => Ok(Resolved::Point(p)),
                Some((d, _)) => err(format!("no vertex near ({:.2}, {:.2}, {:.2}): the closest is {d:.2} mm away", hint.x, hint.y, hint.z)),
                None => err("there is no body to take a vertex from"),
            }
        }
        GeomRef::SketchLine { sketch, entity } => {
            let (sk, pl) = ctx.sketch(*sketch)?;
            let Some(e) = sk.entities.get(*entity as usize) else {
                return err(format!("sketch `{}` has no entity {entity}", sk.name));
            };
            Ok(match e.geom {
                Geom::Line { a, b } => Resolved::Line { a: pl.to_world(sk.point(a)), b: pl.to_world(sk.point(b)) },
                Geom::Circle { center, radius } => {
                    let c = sk.point(center);
                    let w = |p: DVec2| pl.to_world(p);
                    Resolved::Arc {
                        center: w(c),
                        axis: pl.normal,
                        radius,
                        start: w(c + DVec2::new(radius, 0.0)),
                        mid: w(c - DVec2::new(radius, 0.0)),
                        end: w(c + DVec2::new(radius, 0.0)),
                        closed: true,
                    }
                }
                Geom::Arc { center, start, end } => {
                    let (c, s, e2) = (sk.point(center), sk.point(start), sk.point(end));
                    let seg = crate::geom::Segment2::Arc { center: c, start: s, end: e2, ccw: true };
                    let w = |p: DVec2| pl.to_world(p);
                    Resolved::Arc { center: w(c), axis: pl.normal, radius: (s - c).length(), start: w(s), mid: w(segment_point(&seg, 0.5)), end: w(e2), closed: false }
                }
            })
        }
        GeomRef::SketchPoint { sketch, point } => {
            let (sk, pl) = ctx.sketch(*sketch)?;
            match sk.points.get(*point as usize) {
                Some(p) => Ok(Resolved::Point(pl.to_world(*p))),
                None => err(format!("sketch `{}` has no point {point}", sk.name)),
            }
        }
    }
}

fn no_match(sel: Selector) -> SelectError {
    SelectError::Unresolved { selector: Box::new(sel), best: None, best_index: None, score: f64::INFINITY }
}

/// Try `f` on every body; the first success wins, else the most confident failure.
fn over_bodies<T>(ctx: &RefCtx, sel: Selector, mut f: impl FnMut(&Body) -> Result<T, SelectError>) -> Result<T, DatumError> {
    let mut best: Option<SelectError> = None;
    let score = |e: &SelectError| match e {
        SelectError::Unresolved { score, .. } => *score,
        _ => f64::INFINITY,
    };
    // solid bodies first, reference bodies after
    let order = ctx.bodies.iter().filter(|b| !b.reference).chain(ctx.bodies.iter().filter(|b| b.reference));
    for b in order {
        match f(b) {
            Ok(v) => return Ok(v),
            Err(e) => {
                if best.as_ref().is_none_or(|c| score(&e) < score(c)) {
                    best = Some(e);
                }
            }
        }
    }
    Err(best.unwrap_or_else(|| no_match(sel)).into())
}

fn resolve_face(fs: &FaceSelector, ctx: &RefCtx) -> Result<Resolved, DatumError> {
    over_bodies(ctx, Selector::Face(fs.clone()), |b| {
        let idx = *fs.resolve(b.shape())?.first().ok_or_else(|| no_match(Selector::Face(fs.clone())))?;
        let faces = b.shape().faces()?;
        let f = faces.iter().find(|f| f.index == idx).ok_or_else(|| no_match(Selector::Face(fs.clone())))?;
        Ok(match f.kind {
            SurfaceKind::Plane => Resolved::Plane(plane_at(f.centroid, f.normal)),
            SurfaceKind::Cylinder | SurfaceKind::Cone => {
                let (o, d) = f.axis.unwrap_or((f.centroid, DVec3::Z));
                let d = d.normalize_or_zero();
                Resolved::Cylinder { origin: o + d * (f.centroid - o).dot(d), dir: canon(d), radius: f.radius }
            }
            SurfaceKind::Sphere => Resolved::Sphere { center: f.point - f.normal.normalize_or_zero() * f.radius, radius: f.radius },
            _ => return Err(no_match(Selector::Face(fs.clone()))),
        })
    })
    .map_err(|e| match e {
        DatumError::Select(SelectError::Unresolved { score, .. }) if score.is_infinite() => {
            DatumError::Msg("the face has an unsupported surface type (not plane/cylinder/cone/sphere)".into())
        }
        e => e,
    })
}

fn resolve_edge(es: &EdgeSelector, ctx: &RefCtx) -> Result<Resolved, DatumError> {
    over_bodies(ctx, Selector::Edge(es.clone()), |b| {
        let idx = *es.resolve(b.shape())?.first().ok_or_else(|| no_match(Selector::Edge(es.clone())))?;
        let edges = b.shape().edges()?;
        let e = edges.iter().find(|e| e.index == idx).ok_or_else(|| no_match(Selector::Edge(es.clone())))?;
        Ok(match (e.kind, e.circle) {
            (occt::CurveKind::Line, _) => Resolved::Line { a: e.start, b: e.end },
            (_, Some((c, axis, r))) => Resolved::Arc { center: c, axis: axis.normalize_or_zero(), radius: r, start: e.start, mid: e.midpoint, end: e.end, closed: e.closed },
            _ => return Err(no_match(Selector::Edge(es.clone()))),
        })
    })
    .map_err(|e| match e {
        DatumError::Select(SelectError::Unresolved { score, .. }) if score.is_infinite() => {
            DatumError::Msg("the edge is neither straight nor circular".into())
        }
        e => e,
    })
}

fn rot(v: DVec3, axis: DVec3, deg: f64) -> DVec3 {
    DQuat::from_axis_angle(axis.normalize(), deg.to_radians()) * v
}

/// `v` with its component along unit `d` removed.
fn perp(v: DVec3, d: DVec3) -> DVec3 {
    v - d * v.dot(d)
}

/// Default zero direction for angle/tangent planes: the first of world Z, X, Y not
/// within ~26° of the axis, made perpendicular to it.
fn default_ref_dir(d: DVec3) -> DVec3 {
    for e in [DVec3::Z, DVec3::X, DVec3::Y] {
        if e.dot(d).abs() < 0.9 {
            return perp(e, d).normalize_or_zero();
        }
    }
    DVec3::ZERO
}

/// Angle at `t` (0..1) along an arc/circle: the point and unit tangent.
fn arc_at(center: DVec3, axis: DVec3, start: DVec3, mid: DVec3, end: DVec3, closed: bool, t: f64) -> (DVec3, DVec3) {
    let ax = axis.normalize();
    let ang = |a: DVec3, b: DVec3| {
        let (a, b) = (perp(a, ax), perp(b, ax));
        ax.dot(a.cross(b)).atan2(a.dot(b)) // signed, (-pi, pi]
    };
    let rs = start - center;
    let sweep = if closed {
        // direction of travel from the midpoint
        let half = ang(rs, mid - center);
        if half >= 0.0 { std::f64::consts::TAU } else { -std::f64::consts::TAU }
    } else {
        let to_end = ang(rs, end - center);
        let to_mid = ang(rs, mid - center);
        // `mid` lies halfway along the travelled arc
        let ccw = if to_end >= 0.0 { to_end } else { to_end + std::f64::consts::TAU };
        if (to_mid.rem_euclid(std::f64::consts::TAU) - ccw / 2.0).abs() < 0.1 { ccw } else { ccw - std::f64::consts::TAU }
    };
    let a = sweep * t;
    let q = DQuat::from_axis_angle(ax, a);
    let p = center + q * rs;
    let tan = ax.cross(p - center).normalize_or_zero() * sweep.signum();
    (p, tan)
}

/// Evaluate a datum feature at its place in the history.
pub fn eval_datum(kind: &crate::feature::FeatureKind, ctx: &RefCtx) -> Result<DatumGeom, DatumError> {
    use crate::feature::FeatureKind as K;
    let ev = |e: &Expr| e.eval(ctx.vals).map_err(|x| DatumError::Msg(format!("`{}`: {x}", e.0)));
    match kind {
        K::DatumPlane { def, flip } => {
            let mut p = plane_def(def, ctx, &ev)?;
            if *flip {
                p.normal = -p.normal;
            }
            Ok(DatumGeom::Plane(p))
        }
        K::DatumAxis { def } => axis_def(def, ctx),
        K::DatumPoint { def } => point_def(def, ctx),
        _ => err("not a construction feature"),
    }
}

fn plane_def(def: &PlaneDef, ctx: &RefCtx, ev: &dyn Fn(&Expr) -> Result<f64, DatumError>) -> Result<Plane, DatumError> {
    match def {
        PlaneDef::Offset { base, distance } => {
            let b = ctx.resolve(base)?.plane()?;
            Ok(b.offset(ev(distance)?))
        }
        PlaneDef::Angle { axis, base, angle } => {
            let (p, d) = ctx.resolve(axis)?.line()?;
            let a = ev(angle)?;
            let r = match base {
                Some(b) => perp(ctx.resolve(b)?.plane()?.normal, d).normalize_or_zero(),
                None => d.cross(default_ref_dir(d)).normalize_or_zero(),
            };
            if r == DVec3::ZERO {
                return err("the base plane is perpendicular to the axis: its angle is undefined");
            }
            // `r` is the zero-angle plane's normal for an explicit base; for the
            // default it is (axis x ref direction), whose plane contains the ref direction.
            Ok(plane_at(p, rot(r, d, a)))
        }
        PlaneDef::Midplane { a, b } => {
            let (pa, pb) = (ctx.resolve(a)?.plane()?, ctx.resolve(b)?.plane()?);
            let cross = pa.normal.cross(pb.normal);
            let mid = (pa.origin + pb.origin) * 0.5;
            if cross.length() < PAR {
                let dist = (pb.origin - pa.origin).dot(pa.normal);
                if dist.abs() < 1e-9 {
                    return err("the two planes coincide: there is no plane between them");
                }
                return Ok(plane_at(pa.origin + pa.normal * dist * 0.5, pa.normal));
            }
            let n = (pa.normal - pb.normal).normalize_or_zero();
            let n = if n == DVec3::ZERO { pa.normal.cross(cross).normalize() } else { n };
            // a point on the intersection line, then the line point nearest `mid`
            let (da, db) = (pa.normal.dot(pa.origin), pb.normal.dot(pb.origin));
            let l2 = cross.length_squared();
            let on_line = (pb.normal.cross(cross) * da + cross.cross(pa.normal) * db) / l2;
            let l = cross / l2.sqrt();
            let o = on_line + l * (mid - on_line).dot(l);
            Ok(plane_at(o, n))
        }
        PlaneDef::TwoLines { a, b } => {
            let ((p1, d1), (p2, d2)) = (ctx.resolve(a)?.line()?, ctx.resolve(b)?.line()?);
            let w = p2 - p1;
            let n = d1.cross(d2);
            let tol = 1e-3 + 1e-6 * w.length();
            if n.length() < PAR {
                let n = d1.cross(w);
                if n.length() < 1e-9 * w.length().max(1.0) || near_zero_len(n) {
                    return err("the two lines are the same line: they do not define a plane");
                }
                return Ok(plane_at(p1, n));
            }
            if w.dot(n.normalize()).abs() > tol {
                return err(format!("the two lines are not coplanar (skew by {:.3} mm)", w.dot(n.normalize()).abs()));
            }
            Ok(plane_at(p1, n))
        }
        PlaneDef::ThreePoints { a, b, c } => {
            let (pa, pb, pc) = (ctx.resolve(a)?.point()?, ctx.resolve(b)?.point()?, ctx.resolve(c)?.point()?);
            let n = (pb - pa).cross(pc - pa);
            let scale = (pb - pa).length().max((pc - pa).length());
            if n.length() <= 1e-7 * scale * scale || n.length() < 1e-12 {
                return err("the three points are in a line: they do not define a plane");
            }
            Ok(plane_at(pa, n))
        }
        PlaneDef::Tangent { face, base, angle } => {
            let a = ev(angle)?;
            let Resolved::Cylinder { origin, dir, radius } = ctx.resolve(face)? else {
                return err("a tangent plane needs a cylindrical face");
            };
            let u0 = match base {
                Some(b) => perp(ctx.resolve(b)?.plane()?.normal, dir).normalize_or_zero(),
                None => default_ref_dir(dir),
            };
            if u0 == DVec3::ZERO {
                return err("the base plane is perpendicular to the cylinder axis: its angle is undefined");
            }
            let u = rot(u0, dir, a);
            Ok(plane_at(origin + u * radius, u))
        }
        PlaneDef::NormalAt { path, t } => {
            let t = ev(t)?;
            if !(0.0..=1.0).contains(&t) && !matches!(path, GeomRef::Origin(_) | GeomRef::Datum(_)) {
                return err(format!("the position along the path must be between 0 and 1 (got {t})"));
            }
            match ctx.resolve(path)? {
                Resolved::Line { a, b } => {
                    let d = (b - a).normalize_or_zero();
                    if d == DVec3::ZERO {
                        return err("the path has zero length");
                    }
                    let p = if matches!(path, GeomRef::Origin(_) | GeomRef::Datum(_)) { a + d * t } else { a + (b - a) * t };
                    Ok(plane_at(p, d))
                }
                Resolved::Arc { center, axis, start, mid, end, closed, .. } => {
                    let (p, tan) = arc_at(center, axis, start, mid, end, closed, t);
                    Ok(plane_at(p, tan))
                }
                o => err(format!("a path (edge or line) is needed, but this is {}", o.what())),
            }
        }
    }
}

fn axis_def(def: &AxisDef, ctx: &RefCtx) -> Result<DatumGeom, DatumError> {
    let axis = |o: DVec3, d: DVec3| Ok(DatumGeom::Axis { origin: o, dir: d.normalize() });
    match def {
        AxisDef::Cylinder { face } => match ctx.resolve(face)? {
            Resolved::Cylinder { origin, dir, .. } => axis(origin, dir),
            Resolved::Arc { center, axis: a, .. } => axis(center, canon(a)),
            o => err(format!("a cylinder or circle is needed, but this is {}", o.what())),
        },
        AxisDef::Line { line } => {
            let (p, d) = ctx.resolve(line)?.line()?;
            axis(p, d)
        }
        AxisDef::TwoPoints { a, b } => {
            let (pa, pb) = (ctx.resolve(a)?.point()?, ctx.resolve(b)?.point()?);
            if near_zero_len(pb - pa) {
                return err("the two points coincide: they do not define an axis");
            }
            axis(pa, pb - pa)
        }
        AxisDef::TwoPlanes { a, b } => {
            let (pa, pb) = (ctx.resolve(a)?.plane()?, ctx.resolve(b)?.plane()?);
            let cross = pa.normal.cross(pb.normal);
            if cross.length() < PAR {
                return err("the two planes are parallel: they do not meet in a line");
            }
            let (da, db) = (pa.normal.dot(pa.origin), pb.normal.dot(pb.origin));
            let l2 = cross.length_squared();
            let on_line = (pb.normal.cross(cross) * da + cross.cross(pa.normal) * db) / l2;
            let l = cross / l2.sqrt();
            let mid = (pa.origin + pb.origin) * 0.5;
            axis(on_line + l * (mid - on_line).dot(l), l)
        }
        AxisDef::NormalAt { face, point } => {
            let p = ctx.resolve(point)?.point()?;
            match ctx.resolve(face)? {
                Resolved::Plane(pl) => axis(p - pl.normal * (p - pl.origin).dot(pl.normal), pl.normal),
                Resolved::Cylinder { origin, dir, radius } => {
                    let radial = perp(p - origin, dir);
                    if near_zero_len(radial) {
                        return err("the point is on the cylinder axis: there is no surface normal there");
                    }
                    let u = radial.normalize();
                    let foot = origin + dir * (p - origin).dot(dir);
                    axis(foot + u * radius, u)
                }
                Resolved::Sphere { center, radius } => {
                    let u = (p - center).normalize_or_zero();
                    if u == DVec3::ZERO {
                        return err("the point is at the sphere centre");
                    }
                    axis(center + u * radius, u)
                }
                o => err(format!("a face is needed, but this is {}", o.what())),
            }
        }
    }
}

fn point_def(def: &PointDef, ctx: &RefCtx) -> Result<DatumGeom, DatumError> {
    let pt = |p: DVec3| Ok(DatumGeom::Point(p));
    match def {
        PointDef::At { point } => pt(ctx.resolve(point)?.point()?),
        PointDef::Center { circle } => match ctx.resolve(circle)? {
            Resolved::Arc { center, .. } => pt(center),
            Resolved::Cylinder { origin, .. } => pt(origin),
            Resolved::Sphere { center, .. } => pt(center),
            o => err(format!("a circle, cylinder or sphere is needed, but this is {}", o.what())),
        },
        PointDef::EdgePlane { edge, plane } => {
            let (p, d) = ctx.resolve(edge)?.line()?;
            let pl = ctx.resolve(plane)?.plane()?;
            let den = pl.normal.dot(d);
            if den.abs() < PAR {
                return err("the edge is parallel to the plane: they do not meet");
            }
            pt(p + d * (pl.normal.dot(pl.origin - p) / den))
        }
        PointDef::ThreePlanes { a, b, c } => {
            let (pa, pb, pc) = (ctx.resolve(a)?.plane()?, ctx.resolve(b)?.plane()?, ctx.resolve(c)?.plane()?);
            let m = DMat3::from_cols(pa.normal, pb.normal, pc.normal).transpose();
            let det = m.determinant();
            if det.abs() < PAR {
                return err("the three planes do not meet in a single point (two are parallel or they share a line)");
            }
            let rhs = DVec3::new(pa.normal.dot(pa.origin), pb.normal.dot(pb.origin), pc.normal.dot(pc.origin));
            pt(m.inverse() * rhs)
        }
        PointDef::TwoLines { a, b } => {
            let ((p1, d1), (p2, d2)) = (ctx.resolve(a)?.line()?, ctx.resolve(b)?.line()?);
            let w = p1 - p2;
            let (bb, dd, ee) = (d1.dot(d2), d1.dot(w), d2.dot(w));
            let den = 1.0 - bb * bb;
            if den.abs() < PAR {
                return err("the two lines are parallel: they do not meet");
            }
            let s = (bb * ee - dd) / den;
            let t = (ee - bb * dd) / den;
            pt(((p1 + d1 * s) + (p2 + d2 * t)) * 0.5)
        }
    }
}
