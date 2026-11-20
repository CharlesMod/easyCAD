//! 2D constraint sketches: points, lines, arcs, circles, constraints, a damped
//! Gauss-Newton (Levenberg-Marquardt) solver with DOF analysis, and profile
//! extraction into [`Profile`]s / open chains.

use glam::DVec2;
use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};

use crate::geom::{Loop2, Plane, Profile, Segment2};
use crate::params::{Expr, ExprError, Values};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PointId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Geom {
    Line { a: PointId, b: PointId },
    /// Counter-clockwise from `start` to `end`; radius is |start - center|.
    Arc { center: PointId, start: PointId, end: PointId },
    Circle { center: PointId, radius: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub geom: Geom,
    #[serde(default)]
    pub construction: bool,
    /// Brought in from a model edge (Project tool): drawn in its own colour; it is a
    /// normal profile entity unless also marked construction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub projected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    Coincident(PointId, PointId),
    Horizontal(EntityId),
    Vertical(EntityId),
    Parallel(EntityId, EntityId),
    Perpendicular(EntityId, EntityId),
    /// Line–arc/circle or arc/circle–arc/circle tangency.
    Tangent(EntityId, EntityId),
    /// Equal length (lines) or equal radius (arcs/circles).
    Equal(EntityId, EntityId),
    PointOnLine(PointId, EntityId),
    PointOnCircle(PointId, EntityId),
    Midpoint(PointId, EntityId),
    /// The two points are mirror images across the line.
    Symmetric(PointId, PointId, EntityId),
    Fix(PointId, DVec2),
    Distance(PointId, PointId, Expr),
    /// Signed `b.x - a.x`.
    DistanceX(PointId, PointId, Expr),
    /// Signed `b.y - a.y`.
    DistanceY(PointId, PointId, Expr),
    PointLineDistance(PointId, EntityId, Expr),
    /// Two lines held parallel at this perpendicular distance.
    LineDistance(EntityId, EntityId, Expr),
    Length(EntityId, Expr),
    Radius(EntityId, Expr),
    Diameter(EntityId, Expr),
    /// Signed angle (degrees, CCW) from the first line's direction to the second's.
    Angle(EntityId, EntityId, Expr),
    /// `target[i]` is `source[i]` offset by `distance`: lines stay parallel at that
    /// distance, arcs/circles concentric with the radius changed by it (the side is
    /// the one the target is on). Joints between targets that share a point are
    /// pinned without redundancy: on both offset carriers, or, where the source
    /// is tangent, on one carrier and the normal through the source joint. Build
    /// both with [`Sketch::offset_chain`].
    Offset { source: Vec<EntityId>, target: Vec<EntityId>, distance: Expr },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sketch {
    pub name: String,
    pub plane: Plane,
    pub points: Vec<DVec2>,
    pub entities: Vec<Entity>,
    pub constraints: Vec<Constraint>,
    /// A construction plane this sketch follows: at regen the sketch lies on that
    /// datum's current plane, and `plane` is only a cache / fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane_src: Option<crate::feature::FeatureId>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SketchError {
    #[error("dimension: {0}")]
    Expr(#[from] ExprError),
    #[error("constraint {index}: {msg}")]
    BadConstraint { index: usize, msg: String },
    #[error("solver did not converge (max residual {0:.3e})")]
    NotConverged(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintStatus {
    UnderConstrained,
    FullyConstrained,
    /// Converged, but some constraints are redundant.
    OverConstrained,
    /// Constraints contradict each other (no solution found).
    Conflicting,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolveReport {
    pub converged: bool,
    pub iterations: usize,
    pub max_residual: f64,
    pub variables: usize,
    pub equations: usize,
    pub rank: usize,
    pub dof: usize,
    pub status: ConstraintStatus,
}

/// Closed regions and open chains of a sketch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SketchProfiles {
    pub profiles: Vec<Profile>,
    pub open: Vec<Vec<Segment2>>,
}

impl Sketch {
    pub fn new(name: impl Into<String>, plane: Plane) -> Self {
        Sketch { name: name.into(), plane, points: Vec::new(), entities: Vec::new(), constraints: Vec::new(), plane_src: None }
    }

    pub fn point(&self, p: PointId) -> DVec2 {
        self.points[p.0 as usize]
    }

    pub fn add_point(&mut self, p: DVec2) -> PointId {
        self.points.push(p);
        PointId(self.points.len() as u32 - 1)
    }

    pub fn add_entity(&mut self, geom: Geom, construction: bool) -> EntityId {
        self.entities.push(Entity { geom, construction, projected: false });
        EntityId(self.entities.len() as u32 - 1)
    }

    pub fn add_line(&mut self, a: PointId, b: PointId) -> EntityId {
        self.add_entity(Geom::Line { a, b }, false)
    }

    /// New line with fresh endpoints.
    pub fn line(&mut self, a: DVec2, b: DVec2) -> EntityId {
        let (pa, pb) = (self.add_point(a), self.add_point(b));
        self.add_line(pa, pb)
    }

    /// CCW arc between existing points.
    pub fn add_arc(&mut self, center: PointId, start: PointId, end: PointId) -> EntityId {
        self.add_entity(Geom::Arc { center, start, end }, false)
    }

    pub fn circle(&mut self, center: DVec2, radius: f64) -> EntityId {
        let c = self.add_point(center);
        self.add_entity(Geom::Circle { center: c, radius }, false)
    }

    pub fn constrain(&mut self, c: Constraint) {
        self.constraints.push(c);
    }

    pub fn set_construction(&mut self, e: EntityId, on: bool) {
        self.entities[e.0 as usize].construction = on;
    }

    /// Axis-aligned rectangle (shared corner points, H/V constraints).
    /// Returns `[bottom, right, top, left]`.
    pub fn rect(&mut self, min: DVec2, max: DVec2) -> [EntityId; 4] {
        let p0 = self.add_point(min);
        let p1 = self.add_point(DVec2::new(max.x, min.y));
        let p2 = self.add_point(max);
        let p3 = self.add_point(DVec2::new(min.x, max.y));
        let l = [self.add_line(p0, p1), self.add_line(p1, p2), self.add_line(p2, p3), self.add_line(p3, p0)];
        self.constrain(Constraint::Horizontal(l[0]));
        self.constrain(Constraint::Vertical(l[1]));
        self.constrain(Constraint::Horizontal(l[2]));
        self.constrain(Constraint::Vertical(l[3]));
        l
    }

    /// Fully constrained centred rounded rectangle; `w`, `h`, `r` may be expressions.
    /// The centre is a fixed construction point.
    pub fn rounded_rect(&mut self, center: DVec2, w: impl Into<Expr>, h: impl Into<Expr>, r: impl Into<Expr>, guess: (f64, f64, f64)) -> Vec<EntityId> {
        let (gw, gh, gr) = guess;
        let (x0, x1, y0, y1) = (center.x - gw / 2.0, center.x + gw / 2.0, center.y - gh / 2.0, center.y + gh / 2.0);
        let v = DVec2::new;
        // line endpoints (CCW): bottom, right, top, left
        let b0 = self.add_point(v(x0 + gr, y0));
        let b1 = self.add_point(v(x1 - gr, y0));
        let r0 = self.add_point(v(x1, y0 + gr));
        let r1 = self.add_point(v(x1, y1 - gr));
        let t0 = self.add_point(v(x1 - gr, y1));
        let t1 = self.add_point(v(x0 + gr, y1));
        let l0 = self.add_point(v(x0, y1 - gr));
        let l1 = self.add_point(v(x0, y0 + gr));
        let c_br = self.add_point(v(x1 - gr, y0 + gr));
        let c_tr = self.add_point(v(x1 - gr, y1 - gr));
        let c_tl = self.add_point(v(x0 + gr, y1 - gr));
        let c_bl = self.add_point(v(x0 + gr, y0 + gr));
        let bottom = self.add_line(b0, b1);
        let a_br = self.add_arc(c_br, b1, r0);
        let right = self.add_line(r0, r1);
        let a_tr = self.add_arc(c_tr, r1, t0);
        let top = self.add_line(t0, t1);
        let a_tl = self.add_arc(c_tl, t1, l0);
        let left = self.add_line(l0, l1);
        let a_bl = self.add_arc(c_bl, l1, b0);
        use Constraint::*;
        for c in [Horizontal(bottom), Vertical(right), Horizontal(top), Vertical(left)] {
            self.constrain(c);
        }
        for (a, l1_, l2_) in [(a_br, bottom, right), (a_tr, right, top), (a_tl, top, left), (a_bl, left, bottom)] {
            self.constrain(Tangent(l1_, a));
            self.constrain(Tangent(a, l2_));
        }
        for a in [a_tr, a_tl, a_bl] {
            self.constrain(Equal(a_br, a));
        }
        self.constrain(Radius(a_br, r.into()));
        self.constrain(DistanceX(l1, r0, w.into()));
        self.constrain(DistanceY(b0, t0, h.into()));
        let cp = self.add_point(center);
        let diag = self.add_entity(Geom::Line { a: c_bl, b: c_tr }, true);
        self.constrain(Midpoint(cp, diag));
        self.constrain(Fix(cp, center));
        vec![bottom, a_br, right, a_tr, top, a_tl, left, a_bl]
    }

    // ------------------------------------------------------------------ solver

    /// Solve in place from the current positions (minimal movement). Positions are
    /// updated even if the solve fails to converge, so callers may want to clone first.
    pub fn solve(&mut self, vars: &Values) -> Result<SolveReport, SketchError> {
        let sys = System::build(self, vars)?;
        let mut x = sys.pack(self);
        let rep = sys.solve(&mut x);
        sys.unpack(self, &x);
        Ok(rep)
    }

    /// Degrees of freedom analysis at the current positions (no solving).
    pub fn analyze(&self, vars: &Values) -> Result<SolveReport, SketchError> {
        let sys = System::build(self, vars)?;
        let x = sys.pack(self);
        let r = sys.residuals(&x);
        let max = r.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        Ok(sys.report(&x, max < SOLVE_TOL, 0, max))
    }

    // ------------------------------------------------------------------ profiles

    /// Non-construction geometry as 2D segments.
    pub fn segments(&self) -> Vec<Segment2> {
        self.entities
            .iter()
            .filter(|e| !e.construction)
            .map(|e| match e.geom {
                Geom::Line { a, b } => Segment2::Line { a: self.point(a), b: self.point(b) },
                Geom::Arc { center, start, end } => {
                    Segment2::Arc { center: self.point(center), start: self.point(start), end: self.point(end), ccw: true }
                }
                Geom::Circle { center, radius } => Segment2::Circle { center: self.point(center), radius },
            })
            .collect()
    }

    /// Chain segments into closed loops (nested into profiles) and open chains.
    pub fn profiles(&self) -> SketchProfiles {
        profiles_from_segments(&self.segments(), CHAIN_TOL)
    }

    /// The bounded faces of the arrangement of all non-construction curves.
    pub fn regions(&self) -> Vec<crate::regions::Region> {
        crate::regions::regions_from_segments(&self.segments(), CHAIN_TOL)
    }
}

pub const CHAIN_TOL: f64 = 1e-4;
const SOLVE_TOL: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Constraint system

/// A constraint lowered to equations over the variable vector.
enum Eq {
    Coincident(usize, usize),
    Horizontal(usize, usize),
    Vertical(usize, usize),
    Parallel([usize; 4]),
    Perpendicular([usize; 4]),
    /// (line a, line b, circle center, radius source)
    TangentLineCircle(usize, usize, usize, Rad),
    TangentCircles(usize, Rad, usize, Rad, bool),
    EqualLen([usize; 4]),
    EqualRad(Rad, Rad),
    PointOnLine(usize, usize, usize),
    PointOnCircle(usize, usize, Rad),
    Midpoint(usize, usize, usize),
    Symmetric(usize, usize, usize, usize),
    Fix(usize, DVec2),
    Distance(usize, usize, f64),
    Dx(usize, usize, f64),
    Dy(usize, usize, f64),
    /// Distance from a point to the infinite line through two points. The last field is the
    /// side the point started on (+1/-1), kept so the residual stays smooth through the
    /// solve; 0 means unsigned (`abs`).
    PointLine(usize, usize, usize, f64, f64),
    Length(usize, usize, f64),
    Radius(Rad, f64),
    /// `rad(target) - rad(source) = v`.
    RadiusOffset(Rad, Rad, f64),
    /// `|q - c| = rad(source) + v`: q on an offset of the source circle.
    OnOffsetCircle(usize, usize, Rad, f64),
    /// `(q - j) · (b - a) = 0`: q on the normal through j of the line a–b.
    Foot(usize, usize, usize, usize),
    Angle([usize; 4], f64),
    /// Implicit: arc end on the arc's circle.
    ArcEnd(usize, usize, usize),
}

/// Where a radius comes from: explicit variable (circle) or |start - center| (arc).
#[derive(Clone, Copy)]
enum Rad {
    Var(usize),
    Arc(usize, usize),
}

struct System {
    n_points: usize,
    circle_vars: Vec<(usize, usize)>, // (entity index, var index)
    eqs: Vec<Eq>,
}

fn p(x: &[f64], i: usize) -> DVec2 {
    DVec2::new(x[2 * i], x[2 * i + 1])
}

fn rad(x: &[f64], r: Rad) -> f64 {
    match r {
        Rad::Var(v) => x[v],
        Rad::Arc(c, s) => (p(x, s) - p(x, c)).length(),
    }
}

fn wrap_angle(a: f64) -> f64 {
    let t = std::f64::consts::TAU;
    let mut a = a % t;
    if a > std::f64::consts::PI {
        a -= t;
    } else if a < -std::f64::consts::PI {
        a += t;
    }
    a
}

/// Lower one `Constraint::Offset` (GUI_NEEDS #21). Every end point of the target
/// chain gets exactly two equations, so a chain of lines and arcs is fully
/// determined with no redundancy. At a transversal joint the point lies on both
/// offset carriers. At a tangent joint (and at an open end) it lies on one offset
/// carrier and on the normal through the source joint: there the two carriers
/// touch, and their equations would be dependent. A target end shared with no
/// other target is on its carrier only, unless the source chain ends there too
/// (then it is on the normal): a fillet or trim of the offset places such ends.
/// Target arcs share (or are tied to) the source centre and take their radius
/// from their ends; a target circle takes the source radius ± d.
fn lower_offset(
    sk: &Sketch,
    x0: &[f64],
    circle_vars: &[(usize, usize)],
    source: &[EntityId],
    target: &[EntityId],
    d: f64,
    eqs: &mut Vec<Eq>,
) -> Result<(), &'static str> {
    #[derive(Clone, Copy)]
    enum Src {
        Line(usize, usize),
        Circ(usize, Rad),
    }
    let geom = |e: EntityId| sk.entities.get(e.0 as usize).map(|x| x.geom).ok_or("no such entity");
    let circ = |e: EntityId| -> Result<(usize, Rad), &'static str> {
        match geom(e)? {
            Geom::Arc { center, start, .. } => Ok((center.0 as usize, Rad::Arc(center.0 as usize, start.0 as usize))),
            Geom::Circle { center, .. } => {
                let v = circle_vars.iter().find(|v| v.0 == e.0 as usize).ok_or("no such circle")?.1;
                Ok((center.0 as usize, Rad::Var(v)))
            }
            Geom::Line { .. } => Err("expected an arc or circle"),
        }
    };
    let pt = |i: usize| p(x0, i);
    let mut srcs = Vec::new();
    // (target end point, pair, the source end point it offsets)
    let mut ends: Vec<(usize, usize, usize)> = Vec::new();
    for (k, (&s, &t)) in source.iter().zip(target).enumerate() {
        let mut pair = |ta: PointId, tb: PointId, sa: PointId, sb: PointId| {
            let [ta, tb, sa, sb] = [ta, tb, sa, sb].map(|q| q.0 as usize);
            let straight = pt(ta).distance(pt(sa)) + pt(tb).distance(pt(sb)) <= pt(ta).distance(pt(sb)) + pt(tb).distance(pt(sa));
            ends.push((ta, k, if straight { sa } else { sb }));
            ends.push((tb, k, if straight { sb } else { sa }));
        };
        match (geom(s)?, geom(t)?) {
            (Geom::Line { a, b }, Geom::Line { a: c, b: e }) => {
                pair(c, e, a, b);
                srcs.push(Src::Line(a.0 as usize, b.0 as usize));
            }
            (Geom::Arc { start, end, .. }, Geom::Arc { start: ts, end: te, .. }) => {
                let ((c1, r1), (c2, _)) = (circ(s)?, circ(t)?);
                if c1 != c2 {
                    eqs.push(Eq::Coincident(c1, c2));
                }
                pair(ts, te, start, end);
                srcs.push(Src::Circ(c1, r1));
            }
            (Geom::Arc { .. } | Geom::Circle { .. }, Geom::Circle { .. }) => {
                let ((c1, r1), (c2, r2)) = (circ(s)?, circ(t)?);
                if c1 != c2 {
                    eqs.push(Eq::Coincident(c1, c2));
                }
                let grow = rad(x0, r2) >= rad(x0, r1);
                eqs.push(Eq::RadiusOffset(r1, r2, if grow { d } else { -d }));
                srcs.push(Src::Circ(c1, r1));
            }
            _ => return Err("offset pairs a line with a line, an arc with an arc and a circle with an arc or circle"),
        }
    }
    let on = |k: usize, q: usize| match srcs[k] {
        Src::Line(a, b) => Eq::PointLine(q, a, b, d, 0.0),
        Src::Circ(c, r) => Eq::OnOffsetCircle(q, c, r, if pt(q).distance(pt(c)) >= rad(x0, r) { d } else { -d }),
    };
    let normal = |k: usize, q: usize, j: usize| match srcs[k] {
        Src::Line(a, b) => Eq::Foot(q, j, a, b),
        Src::Circ(c, _) => Eq::PointOnLine(q, c, j),
    };
    let tangent = |k: usize, j: usize| match srcs[k] {
        Src::Line(a, b) => (pt(b) - pt(a)).normalize_or_zero(),
        Src::Circ(c, _) => (pt(j) - pt(c)).perp().normalize_or_zero(),
    };
    let mut done = Vec::new();
    for &(q, _, _) in &ends {
        if done.contains(&q) {
            continue;
        }
        done.push(q);
        let inc: Vec<(usize, usize)> = ends.iter().filter(|e| e.0 == q).map(|e| (e.1, e.2)).collect();
        match inc[..] {
            [(k, j)] => {
                eqs.push(on(k, q));
                // an end of the source chain: pinned on its normal. Otherwise the
                // target was cut here (a fillet, a trim) and what cut it places it.
                let shared = ends.iter().filter(|e| pt(e.2).distance(pt(j)) < 1e-6).count();
                if shared < 2 {
                    eqs.push(normal(k, q, j));
                }
            }
            [(k1, j1), (k2, j2), ..] => {
                eqs.push(on(k1, q));
                if tangent(k1, j1).perp_dot(tangent(k2, j2)).abs() < 1e-6 {
                    // the normal of an arc (its radial) when there is one
                    let (kn, jn) = if matches!(srcs[k1], Src::Circ(..)) || !matches!(srcs[k2], Src::Circ(..)) { (k1, j1) } else { (k2, j2) };
                    eqs.push(normal(kn, q, jn));
                } else {
                    eqs.push(on(k2, q));
                }
            }
            [] => {}
        }
    }
    Ok(())
}

impl System {
    fn build(sk: &Sketch, vals: &Values) -> Result<System, SketchError> {
        let n_points = sk.points.len();
        let mut circle_vars = Vec::new();
        for (i, e) in sk.entities.iter().enumerate() {
            if let Geom::Circle { .. } = e.geom {
                circle_vars.push((i, 2 * n_points + circle_vars.len()));
            }
        }
        let mut eqs = Vec::new();
        let bad = |index: usize, msg: &str| SketchError::BadConstraint { index, msg: msg.into() };
        let line = |i: usize, e: EntityId| -> Result<[usize; 2], SketchError> {
            match sk.entities.get(e.0 as usize).map(|e| e.geom) {
                Some(Geom::Line { a, b }) => Ok([a.0 as usize, b.0 as usize]),
                _ => Err(bad(i, "expected a line")),
            }
        };
        let circ = |i: usize, e: EntityId| -> Result<(usize, Rad), SketchError> {
            match sk.entities.get(e.0 as usize).map(|e| e.geom) {
                Some(Geom::Arc { center, start, .. }) => Ok((center.0 as usize, Rad::Arc(center.0 as usize, start.0 as usize))),
                Some(Geom::Circle { center, .. }) => {
                    let v = circle_vars.iter().find(|(ei, _)| *ei == e.0 as usize).unwrap().1;
                    Ok((center.0 as usize, Rad::Var(v)))
                }
                _ => Err(bad(i, "expected an arc or circle")),
            }
        };
        let is_line = |e: EntityId| matches!(sk.entities.get(e.0 as usize).map(|e| e.geom), Some(Geom::Line { .. }));
        let pt = |i: usize, q: PointId| -> Result<usize, SketchError> {
            if (q.0 as usize) < n_points {
                Ok(q.0 as usize)
            } else {
                Err(bad(i, "point out of range"))
            }
        };
        // an offset target arc gets both ends from its joints (`Offset` below), so
        // its implicit end-on-circle equation would be redundant
        let offset_arcs: Vec<usize> = sk
            .constraints
            .iter()
            .flat_map(|c| match c {
                Constraint::Offset { target, .. } => target.as_slice(),
                _ => &[],
            })
            .map(|e| e.0 as usize)
            .collect();
        for (i, e) in sk.entities.iter().enumerate() {
            if let Geom::Arc { center, start, end } = e.geom {
                if !offset_arcs.contains(&i) {
                    eqs.push(Eq::ArcEnd(center.0 as usize, start.0 as usize, end.0 as usize));
                }
            }
        }
        let x0: Vec<f64> = {
            let mut x: Vec<f64> = sk.points.iter().flat_map(|p| [p.x, p.y]).collect();
            for (ei, _) in &circle_vars {
                if let Geom::Circle { radius, .. } = sk.entities[*ei].geom {
                    x.push(radius)
                }
            }
            x
        };
        // side of the line a point is on at the starting positions (0 when on it)
        let side = |q: usize, a: usize, b: usize| {
            let d = p(&x0, b) - p(&x0, a);
            let s = d.perp_dot(p(&x0, q) - p(&x0, a)) / d.length().max(1e-12);
            if s.abs() > 1e-9 { s.signum() } else { 0.0 }
        };
        for (i, c) in sk.constraints.iter().enumerate() {
            use Constraint as C;
            let ev = |e: &Expr| e.eval(vals);
            eqs.push(match c {
                C::Coincident(a, b) => Eq::Coincident(pt(i, *a)?, pt(i, *b)?),
                C::Horizontal(l) => {
                    let [a, b] = line(i, *l)?;
                    Eq::Horizontal(a, b)
                }
                C::Vertical(l) => {
                    let [a, b] = line(i, *l)?;
                    Eq::Vertical(a, b)
                }
                C::Parallel(l1, l2) => {
                    let ([a, b], [c, d]) = (line(i, *l1)?, line(i, *l2)?);
                    Eq::Parallel([a, b, c, d])
                }
                C::Perpendicular(l1, l2) => {
                    let ([a, b], [c, d]) = (line(i, *l1)?, line(i, *l2)?);
                    Eq::Perpendicular([a, b, c, d])
                }
                C::Tangent(e1, e2) => match (is_line(*e1), is_line(*e2)) {
                    (true, true) => return Err(bad(i, "tangent needs an arc or circle")),
                    (true, false) | (false, true) => {
                        let (l, c) = if is_line(*e1) { (*e1, *e2) } else { (*e2, *e1) };
                        let [a, b] = line(i, l)?;
                        let (cc, r) = circ(i, c)?;
                        Eq::TangentLineCircle(a, b, cc, r)
                    }
                    (false, false) => {
                        let ((c1, r1), (c2, r2)) = (circ(i, *e1)?, circ(i, *e2)?);
                        let d = (p(&x0, c1) - p(&x0, c2)).length();
                        let internal = d < rad(&x0, r1).max(rad(&x0, r2));
                        Eq::TangentCircles(c1, r1, c2, r2, internal)
                    }
                },
                C::Equal(e1, e2) => match (is_line(*e1), is_line(*e2)) {
                    (true, true) => {
                        let ([a, b], [c, d]) = (line(i, *e1)?, line(i, *e2)?);
                        Eq::EqualLen([a, b, c, d])
                    }
                    (false, false) => Eq::EqualRad(circ(i, *e1)?.1, circ(i, *e2)?.1),
                    _ => return Err(bad(i, "equal needs two lines or two arcs/circles")),
                },
                C::PointOnLine(q, l) => {
                    let [a, b] = line(i, *l)?;
                    Eq::PointOnLine(pt(i, *q)?, a, b)
                }
                C::PointOnCircle(q, c) => {
                    let (cc, r) = circ(i, *c)?;
                    Eq::PointOnCircle(pt(i, *q)?, cc, r)
                }
                C::Midpoint(q, l) => {
                    let [a, b] = line(i, *l)?;
                    Eq::Midpoint(pt(i, *q)?, a, b)
                }
                C::Symmetric(q1, q2, l) => {
                    let [a, b] = line(i, *l)?;
                    Eq::Symmetric(pt(i, *q1)?, pt(i, *q2)?, a, b)
                }
                C::Fix(q, at) => Eq::Fix(pt(i, *q)?, *at),
                C::Distance(a, b, d) => Eq::Distance(pt(i, *a)?, pt(i, *b)?, ev(d)?),
                C::DistanceX(a, b, d) => Eq::Dx(pt(i, *a)?, pt(i, *b)?, ev(d)?),
                C::DistanceY(a, b, d) => Eq::Dy(pt(i, *a)?, pt(i, *b)?, ev(d)?),
                C::PointLineDistance(q, l, d) => {
                    let [a, b] = line(i, *l)?;
                    let q = pt(i, *q)?;
                    Eq::PointLine(q, a, b, ev(d)?, side(q, a, b))
                }
                C::LineDistance(l1, l2, d) => {
                    let ([a, b], [c, dd]) = (line(i, *l1)?, line(i, *l2)?);
                    eqs.push(Eq::Parallel([a, b, c, dd]));
                    eqs.push(Eq::PointLine(c, a, b, ev(d)?, side(c, a, b)));
                    continue;
                }
                C::Length(l, d) => {
                    let [a, b] = line(i, *l)?;
                    Eq::Length(a, b, ev(d)?)
                }
                C::Radius(c, d) => Eq::Radius(circ(i, *c)?.1, ev(d)?),
                C::Diameter(c, d) => Eq::Radius(circ(i, *c)?.1, ev(d)? / 2.0),
                C::Angle(l1, l2, d) => {
                    let ([a, b], [c, dd]) = (line(i, *l1)?, line(i, *l2)?);
                    Eq::Angle([a, b, c, dd], ev(d)?.to_radians())
                }
                C::Offset { source, target, distance } => {
                    if source.len() != target.len() {
                        return Err(bad(i, "offset needs as many targets as sources"));
                    }
                    lower_offset(sk, &x0, &circle_vars, source, target, ev(distance)?.abs(), &mut eqs).map_err(|m| bad(i, m))?;
                    continue;
                }
            });
        }
        Ok(System { n_points, circle_vars, eqs })
    }

    fn n_vars(&self) -> usize {
        2 * self.n_points + self.circle_vars.len()
    }

    fn pack(&self, sk: &Sketch) -> Vec<f64> {
        let mut x: Vec<f64> = sk.points.iter().flat_map(|p| [p.x, p.y]).collect();
        for (ei, _) in &self.circle_vars {
            if let Geom::Circle { radius, .. } = sk.entities[*ei].geom {
                x.push(radius);
            }
        }
        x
    }

    fn unpack(&self, sk: &mut Sketch, x: &[f64]) {
        for (i, pt) in sk.points.iter_mut().enumerate() {
            *pt = p(x, i);
        }
        for (ei, v) in &self.circle_vars {
            if let Geom::Circle { radius, .. } = &mut sk.entities[*ei].geom {
                *radius = x[*v].abs();
            }
        }
    }

    fn residuals(&self, x: &[f64]) -> Vec<f64> {
        let mut r = Vec::with_capacity(self.eqs.len() * 2);
        let dir = |a: usize, b: usize| p(x, b) - p(x, a);
        let unit_cross = |u: DVec2, v: DVec2| u.perp_dot(v) / (u.length() * v.length()).max(1e-12);
        let unit_dot = |u: DVec2, v: DVec2| u.dot(v) / (u.length() * v.length()).max(1e-12);
        let line_dist = |q: usize, a: usize, b: usize| {
            let d = dir(a, b);
            d.perp_dot(p(x, q) - p(x, a)) / d.length().max(1e-12)
        };
        for e in &self.eqs {
            match *e {
                Eq::Coincident(a, b) => {
                    let d = p(x, b) - p(x, a);
                    r.push(d.x);
                    r.push(d.y);
                }
                Eq::Fix(a, at) => {
                    let d = p(x, a) - at;
                    r.push(d.x);
                    r.push(d.y);
                }
                Eq::Horizontal(a, b) => r.push(p(x, b).y - p(x, a).y),
                Eq::Vertical(a, b) => r.push(p(x, b).x - p(x, a).x),
                Eq::Parallel([a, b, c, d]) => r.push(unit_cross(dir(a, b), dir(c, d))),
                Eq::Perpendicular([a, b, c, d]) => r.push(unit_dot(dir(a, b), dir(c, d))),
                Eq::TangentLineCircle(a, b, c, rr) => r.push(line_dist(c, a, b).abs() - rad(x, rr)),
                Eq::TangentCircles(c1, r1, c2, r2, internal) => {
                    let d = (p(x, c1) - p(x, c2)).length();
                    let (a, b) = (rad(x, r1), rad(x, r2));
                    r.push(if internal { d - (a - b).abs() } else { d - (a + b) });
                }
                Eq::EqualLen([a, b, c, d]) => r.push(dir(a, b).length() - dir(c, d).length()),
                Eq::EqualRad(r1, r2) => r.push(rad(x, r1) - rad(x, r2)),
                Eq::PointOnLine(q, a, b) => r.push(line_dist(q, a, b)),
                Eq::PointOnCircle(q, c, rr) => r.push((p(x, q) - p(x, c)).length() - rad(x, rr)),
                Eq::Midpoint(q, a, b) => {
                    let d = p(x, q) - (p(x, a) + p(x, b)) * 0.5;
                    r.push(d.x);
                    r.push(d.y);
                }
                Eq::Symmetric(q1, q2, a, b) => {
                    let m = (p(x, q1) + p(x, q2)) * 0.5;
                    let d = dir(a, b);
                    let n = d.length().max(1e-12);
                    r.push(d.perp_dot(m - p(x, a)) / n);
                    r.push(d.dot(p(x, q2) - p(x, q1)) / n);
                }
                Eq::Distance(a, b, v) => r.push(dir(a, b).length() - v),
                Eq::Dx(a, b, v) => r.push(p(x, b).x - p(x, a).x - v),
                Eq::Dy(a, b, v) => r.push(p(x, b).y - p(x, a).y - v),
                Eq::PointLine(q, a, b, v, sg) => {
                    let s = line_dist(q, a, b);
                    r.push(if sg == 0.0 { s.abs() } else { sg * s } - v);
                }
                Eq::Length(a, b, v) => r.push(dir(a, b).length() - v),
                Eq::Radius(rr, v) => r.push(rad(x, rr) - v),
                Eq::RadiusOffset(r1, r2, v) => r.push(rad(x, r2) - rad(x, r1) - v),
                Eq::OnOffsetCircle(q, c, rr, v) => r.push((p(x, q) - p(x, c)).length() - rad(x, rr) - v),
                Eq::Foot(q, j, a, b) => {
                    let d = dir(a, b);
                    r.push(d.dot(p(x, q) - p(x, j)) / d.length().max(1e-12))
                }
                Eq::Angle([a, b, c, d], v) => {
                    let (u, w) = (dir(a, b), dir(c, d));
                    r.push(wrap_angle(u.perp_dot(w).atan2(u.dot(w)) - v));
                }
                Eq::ArcEnd(c, s, e) => r.push((p(x, e) - p(x, c)).length() - (p(x, s) - p(x, c)).length()),
            }
        }
        r
    }

    fn jacobian(&self, x: &[f64], m: usize) -> DMatrix<f64> {
        let n = x.len();
        let mut j = DMatrix::zeros(m, n);
        let mut xp = x.to_vec();
        for k in 0..n {
            let h = 1e-7 * (1.0 + x[k].abs());
            xp[k] = x[k] + h;
            let rp = self.residuals(&xp);
            xp[k] = x[k] - h;
            let rm = self.residuals(&xp);
            xp[k] = x[k];
            for i in 0..m {
                j[(i, k)] = (rp[i] - rm[i]) / (2.0 * h);
            }
        }
        j
    }

    fn solve(&self, x: &mut [f64]) -> SolveReport {
        let mut r = self.residuals(x);
        let m = r.len();
        let maxabs = |r: &[f64]| r.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        let norm2 = |r: &[f64]| r.iter().map(|v| v * v).sum::<f64>();
        let mut lambda = 1e-3;
        let mut it = 0;
        let mut converged = maxabs(&r) < SOLVE_TOL;
        while !converged && it < 200 && m > 0 {
            it += 1;
            let j = self.jacobian(x, m);
            let jt = j.transpose();
            let a = &jt * &j;
            let g = &jt * DVector::from_column_slice(&r);
            let e0 = norm2(&r);
            let mut improved = false;
            while lambda < 1e12 {
                let mut damped = a.clone();
                for k in 0..x.len() {
                    damped[(k, k)] += lambda * (a[(k, k)] + 1.0);
                }
                let Some(dx) = damped.lu().solve(&(-&g)) else {
                    lambda *= 10.0;
                    continue;
                };
                let xn: Vec<f64> = x.iter().zip(dx.iter()).map(|(a, b)| a + b).collect();
                let rn = self.residuals(&xn);
                if norm2(&rn) < e0 && rn.iter().all(|v| v.is_finite()) {
                    x.copy_from_slice(&xn);
                    r = rn;
                    lambda = (lambda / 5.0).max(1e-12);
                    improved = true;
                    break;
                }
                lambda *= 4.0;
            }
            converged = maxabs(&r) < SOLVE_TOL;
            if !improved {
                break;
            }
        }
        self.report(x, converged, it, maxabs(&r))
    }

    fn report(&self, x: &[f64], converged: bool, iterations: usize, max_residual: f64) -> SolveReport {
        let n = self.n_vars();
        let m = self.residuals(x).len();
        let rank = if m == 0 || n == 0 {
            0
        } else {
            let j = self.jacobian(x, m);
            let sv = j.svd(false, false).singular_values;
            let smax = sv.iter().cloned().fold(0.0, f64::max);
            sv.iter().filter(|s| **s > 1e-7 * smax.max(1.0)).count()
        };
        let dof = n - rank;
        let status = if !converged {
            ConstraintStatus::Conflicting
        } else if m > rank {
            ConstraintStatus::OverConstrained
        } else if dof == 0 {
            ConstraintStatus::FullyConstrained
        } else {
            ConstraintStatus::UnderConstrained
        };
        SolveReport { converged, iterations, max_residual, variables: n, equations: m, rank, dof, status }
    }
}

// ---------------------------------------------------------------------------
// Profile extraction

pub(crate) fn seg_ends(s: &Segment2) -> Option<(DVec2, DVec2)> {
    match *s {
        Segment2::Line { a, b } => Some((a, b)),
        Segment2::Arc { start, end, .. } => Some((start, end)),
        Segment2::Circle { .. } => None,
    }
}

/// Reverse the direction of travel of a segment.
pub fn reverse_segment(s: &Segment2) -> Segment2 {
    match *s {
        Segment2::Line { a, b } => Segment2::Line { a: b, b: a },
        Segment2::Arc { center, start, end, ccw } => Segment2::Arc { center, start: end, end: start, ccw: !ccw },
        c @ Segment2::Circle { .. } => c,
    }
}

pub fn reverse_loop(l: &Loop2) -> Loop2 {
    l.iter().rev().map(reverse_segment).collect()
}

/// Sweep angle of an arc in its direction of travel (always in (0, 2π]).
pub fn arc_sweep(center: DVec2, start: DVec2, end: DVec2, ccw: bool) -> f64 {
    let a0 = (start - center).to_angle();
    let a1 = (end - center).to_angle();
    let mut s = if ccw { a1 - a0 } else { a0 - a1 };
    while s <= 1e-12 {
        s += std::f64::consts::TAU;
    }
    s
}

/// Point at parameter t in [0,1] along a segment.
pub fn segment_point(s: &Segment2, t: f64) -> DVec2 {
    match *s {
        Segment2::Line { a, b } => a.lerp(b, t),
        Segment2::Arc { center, start, end, ccw } => {
            let sw = arc_sweep(center, start, end, ccw);
            let a0 = (start - center).to_angle();
            let ang = if ccw { a0 + sw * t } else { a0 - sw * t };
            center + DVec2::from_angle(ang) * (start - center).length()
        }
        Segment2::Circle { center, radius } => center + DVec2::from_angle(std::f64::consts::TAU * t) * radius,
    }
}

/// Polyline approximation of a loop (no closing duplicate).
pub fn polygonize(l: &Loop2) -> Vec<DVec2> {
    let mut out = Vec::new();
    for s in l {
        let n = match s {
            Segment2::Line { .. } => 1,
            Segment2::Arc { center, start, end, ccw } => ((arc_sweep(*center, *start, *end, *ccw) / 0.1).ceil() as usize).max(2),
            Segment2::Circle { .. } => 64,
        };
        for i in 0..n {
            out.push(segment_point(s, i as f64 / n as f64));
        }
    }
    out
}

pub fn signed_area(poly: &[DVec2]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| poly[i].perp_dot(poly[(i + 1) % n])).sum::<f64>() * 0.5
}

pub fn point_in_polygon(q: DVec2, poly: &[DVec2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Signed area of a loop (positive = CCW), exact for arcs (Green's theorem).
pub fn loop_area(l: &Loop2) -> f64 {
    l.iter()
        .map(|s| match *s {
            Segment2::Line { a, b } => a.perp_dot(b) / 2.0,
            Segment2::Arc { center, start, end, ccw } => {
                let r2 = (start - center).length_squared();
                let sweep = arc_sweep(center, start, end, ccw) * if ccw { 1.0 } else { -1.0 };
                center.perp_dot(end - start) / 2.0 + r2 * sweep / 2.0
            }
            Segment2::Circle { radius, .. } => std::f64::consts::PI * radius * radius,
        })
        .sum()
}

/// Chain segments into closed loops / open chains, then nest loops by containment
/// (even depth = outer, odd depth = hole). Outer loops are CCW, holes CW.
pub fn profiles_from_segments(segs: &[Segment2], tol: f64) -> SketchProfiles {
    let mut loops: Vec<Loop2> = Vec::new();
    let mut open = Vec::new();
    let mut used = vec![false; segs.len()];
    for (i, s) in segs.iter().enumerate() {
        if let Segment2::Circle { radius, .. } = s {
            used[i] = true;
            if *radius > tol {
                loops.push(vec![*s]);
            }
        }
    }
    let close = |a: DVec2, b: DVec2| a.distance(b) <= tol;
    for i in 0..segs.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let mut chain: std::collections::VecDeque<Segment2> = std::collections::VecDeque::from([segs[i]]);
        // extend forward then backward
        for forward in [true, false] {
            loop {
                let (head, tail) = (seg_ends(chain.front().unwrap()).unwrap().0, seg_ends(chain.back().unwrap()).unwrap().1);
                if chain.len() > 1 && close(head, tail) {
                    break;
                }
                let end = if forward { tail } else { head };
                let next = (0..segs.len()).find(|&k| {
                    !used[k] && seg_ends(&segs[k]).is_some_and(|(a, b)| close(a, end) || close(b, end))
                });
                let Some(k) = next else { break };
                used[k] = true;
                let (a, _) = seg_ends(&segs[k]).unwrap();
                let s = segs[k];
                if forward {
                    chain.push_back(if close(a, end) { s } else { reverse_segment(&s) });
                } else {
                    chain.push_front(if close(a, end) { reverse_segment(&s) } else { s });
                }
            }
        }
        let chain: Vec<Segment2> = chain.into_iter().collect();
        let (head, tail) = (seg_ends(&chain[0]).unwrap().0, seg_ends(chain.last().unwrap()).unwrap().1);
        if close(head, tail) && (chain.len() > 1 || matches!(chain[0], Segment2::Arc { .. })) {
            loops.push(snap_loop(chain));
        } else {
            open.push(chain);
        }
    }
    // Nesting.
    let polys: Vec<Vec<DVec2>> = loops.iter().map(polygonize).collect();
    let areas: Vec<f64> = polys.iter().map(|p| signed_area(p).abs()).collect();
    let n = loops.len();
    let contains = |outer: usize, inner: usize| -> bool {
        outer != inner && areas[outer] > areas[inner] && point_in_polygon(sample_inside_point(&polys[inner]), &polys[outer])
    };
    let depth: Vec<usize> = (0..n).map(|i| (0..n).filter(|&j| contains(j, i)).count()).collect();
    let mut profiles = Vec::new();
    let mut outer_ids = Vec::new();
    for i in 0..n {
        if depth[i].is_multiple_of(2) {
            let mut l = loops[i].clone();
            if signed_area(&polys[i]) < 0.0 {
                l = reverse_loop(&l);
            }
            outer_ids.push(i);
            profiles.push(Profile { outer: l, holes: Vec::new() });
        }
    }
    for i in 0..n {
        if depth[i] % 2 == 1 {
            // parent = smallest containing loop with depth - 1
            let parent = (0..n)
                .filter(|&j| depth[j] + 1 == depth[i] && contains(j, i))
                .min_by(|a, b| areas[*a].total_cmp(&areas[*b]));
            if let Some(pj) = parent {
                let pi = outer_ids.iter().position(|o| *o == pj).unwrap();
                let mut l = loops[i].clone();
                if signed_area(&polys[i]) > 0.0 {
                    l = reverse_loop(&l);
                }
                profiles[pi].holes.push(l);
            }
        }
    }
    SketchProfiles { profiles, open }
}

/// Make consecutive endpoints exactly equal (removes chaining tolerance gaps).
fn snap_loop(mut l: Loop2) -> Loop2 {
    let n = l.len();
    for i in 0..n {
        let end = seg_ends(&l[i]).unwrap().1;
        let j = (i + 1) % n;
        match &mut l[j] {
            Segment2::Line { a, .. } => *a = end,
            Segment2::Arc { start, .. } => *start = end,
            Segment2::Circle { .. } => {}
        }
    }
    l
}

/// A point strictly inside a polygon (used for containment tests).
fn sample_inside_point(poly: &[DVec2]) -> DVec2 {
    // Try slightly inside a vertex along the angle bisector; fall back to centroid.
    let n = poly.len();
    let ccw = signed_area(poly) > 0.0;
    for i in 0..n {
        let (a, b, c) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
        let e1 = (b - a).normalize_or_zero();
        let e2 = (c - b).normalize_or_zero();
        let mut inward = (e1 + e2).perp().normalize_or_zero();
        if !ccw {
            inward = -inward;
        }
        let len = (b - a).length().min((c - b).length());
        let q = b + inward * (len * 1e-3).max(1e-7);
        if point_in_polygon(q, poly) {
            return q;
        }
    }
    poly.iter().copied().sum::<DVec2>() / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn v(x: f64, y: f64) -> DVec2 {
        DVec2::new(x, y)
    }

    #[test]
    fn rectangle_fully_constrained() {
        let mut s = Sketch::new("r", Plane::XY);
        // start from a sloppy quadrilateral
        let p0 = s.add_point(v(0.3, -0.2));
        let p1 = s.add_point(v(9.0, 0.4));
        let p2 = s.add_point(v(11.0, 4.0));
        let p3 = s.add_point(v(-1.0, 6.0));
        let l: Vec<EntityId> = [(p0, p1), (p1, p2), (p2, p3), (p3, p0)].iter().map(|(a, b)| s.add_line(*a, *b)).collect();
        let vals = Values::from([("w".to_string(), 40.0)]);
        let rep = s.analyze(&vals).unwrap();
        assert_eq!(rep.dof, 8);
        s.constrain(Constraint::Horizontal(l[0]));
        s.constrain(Constraint::Vertical(l[1]));
        s.constrain(Constraint::Parallel(l[2], l[0]));
        s.constrain(Constraint::Perpendicular(l[3], l[2]));
        s.constrain(Constraint::Fix(p0, v(0.0, 0.0)));
        s.constrain(Constraint::Length(l[0], "w".into()));
        s.constrain(Constraint::DistanceY(p0, p3, "w / 2".into()));
        let rep = s.solve(&vals).unwrap();
        assert!(rep.converged, "{rep:?}");
        assert_eq!(rep.dof, 0, "{rep:?}");
        assert_eq!(rep.status, ConstraintStatus::FullyConstrained);
        for (got, want) in [(p0, v(0.0, 0.0)), (p1, v(40.0, 0.0)), (p2, v(40.0, 20.0)), (p3, v(0.0, 20.0))] {
            assert!(s.point(got).distance(want) < 1e-7, "{:?} vs {want:?}", s.point(got));
        }
        // a redundant constraint is detected
        s.constrain(Constraint::Vertical(l[3]));
        let rep = s.solve(&vals).unwrap();
        assert_eq!(rep.status, ConstraintStatus::OverConstrained);
        // a conflicting one is reported
        s.constrain(Constraint::Length(l[1], 5.0.into()));
        let rep = s.solve(&vals).unwrap();
        assert!(!rep.converged);
        assert_eq!(rep.status, ConstraintStatus::Conflicting);
    }

    fn pld_setup(a: DVec2, b: DVec2) -> (Sketch, PointId, EntityId) {
        let mut s = Sketch::new("pld", Plane::XY);
        let o = s.add_point(v(0.0, 0.0));
        let l = s.line(a, b);
        s.constrain(Constraint::Fix(o, v(0.0, 0.0)));
        s.constrain(Constraint::Horizontal(l));
        (s, o, l)
    }

    fn line_y(s: &Sketch, l: EntityId) -> f64 {
        let Geom::Line { a, .. } = s.entities[l.0 as usize].geom else { panic!() };
        s.point(a).y
    }

    #[test]
    fn point_line_distance_any_side() {
        for (a, b) in [
            (v(10.0, 2.0), v(20.0, 2.0)),
            (v(-20.0, 2.0), v(-10.0, 2.0)),
            (v(10.0, -2.0), v(20.0, -2.0)),
            (v(-20.0, -2.0), v(-10.0, -2.0)),
            (v(-5.0, 2.0), v(5.0, 2.0)),
        ] {
            let side = a.y.signum();
            let (mut s, o, l) = pld_setup(a, b);
            s.constrain(Constraint::PointLineDistance(o, l, "d".into()));
            for d in [5.0, 3.0, 12.0, 0.5, 7.0] {
                let vals = Values::from([("d".into(), d)]);
                let rep = s.solve(&vals).unwrap();
                assert!(rep.converged, "{a:?} d={d}: {rep:?} {:?}", s.points);
                assert_ne!(rep.status, ConstraintStatus::OverConstrained, "{a:?} d={d}: {rep:?}");
                assert_ne!(rep.status, ConstraintStatus::Conflicting, "{a:?} d={d}: {rep:?}");
                assert!((line_y(&s, l) - side * d).abs() < 1e-6, "{a:?} d={d}: y={}", line_y(&s, l));
            }
        }
    }

    #[test]
    fn point_line_distance_slanted() {
        for (a, b) in [(v(10.0, 2.0), v(20.0, 9.0)), (v(10.0, -2.0), v(20.0, -9.0)), (v(-20.0, 3.0), v(-10.0, 8.0))] {
            let mut s = Sketch::new("pld", Plane::XY);
            let o = s.add_point(v(0.0, 0.0));
            let l = s.line(a, b);
            s.constrain(Constraint::Fix(o, v(0.0, 0.0)));
            s.constrain(Constraint::PointLineDistance(o, l, "d".into()));
            for d in [4.0, 9.0, 1.0] {
                let rep = s.solve(&Values::from([("d".into(), d)])).unwrap();
                assert!(rep.converged && rep.status != ConstraintStatus::OverConstrained, "{a:?} d={d}: {rep:?}");
                let Geom::Line { a: pa, b: pb } = s.entities[l.0 as usize].geom else { panic!() };
                let (pa, pb) = (s.point(pa), s.point(pb));
                let dist = (pb - pa).perp_dot(-pa) / (pb - pa).length();
                assert!((dist.abs() - d).abs() < 1e-6, "{a:?} d={d}: {dist}");
            }
        }
    }

    #[test]
    fn line_distance_any_side() {
        for (a, b, c, e) in [
            (v(0.0, 0.0), v(10.0, 0.0), v(30.0, 2.0), v(40.0, 2.0)),
            (v(0.0, 0.0), v(10.0, 0.0), v(30.0, -2.0), v(40.0, -2.0)),
            (v(0.0, 0.0), v(10.0, 0.0), v(-40.0, 2.0), v(-30.0, 2.0)),
        ] {
            let mut s = Sketch::new("ld", Plane::XY);
            let (l1, l2) = (s.line(a, b), s.line(c, e));
            s.constrain(Constraint::Fix(PointId(0), a));
            s.constrain(Constraint::Fix(PointId(1), b));
            s.constrain(Constraint::LineDistance(l1, l2, "d".into()));
            for d in [5.0, 3.0, 12.0, 0.5] {
                let rep = s.solve(&Values::from([("d".into(), d)])).unwrap();
                assert!(rep.converged && rep.status != ConstraintStatus::OverConstrained, "{c:?} d={d}: {rep:?}");
                assert!((line_y(&s, l2).abs() - d).abs() < 1e-6, "{c:?} d={d}");
                assert_eq!(line_y(&s, l2).signum(), c.y.signum());
            }
        }
    }

    #[test]
    fn rect_helper_dof() {
        let mut s = Sketch::new("r", Plane::XY);
        let l = s.rect(v(0.0, 0.0), v(10.0, 5.0));
        assert_eq!(s.analyze(&Values::new()).unwrap().dof, 4);
        s.constrain(Constraint::Fix(PointId(0), v(0.0, 0.0)));
        s.constrain(Constraint::Length(l[0], 30.0.into()));
        s.constrain(Constraint::Length(l[1], 12.0.into()));
        let rep = s.solve(&Values::new()).unwrap();
        assert_eq!((rep.dof, rep.converged), (0, true));
        assert!(s.point(PointId(2)).distance(v(30.0, 12.0)) < 1e-8);
    }

    #[test]
    fn rounded_rect_parametric() {
        let mut s = Sketch::new("rr", Plane::XY);
        s.rounded_rect(v(0.0, 0.0), "w", "h", "r", (50.0, 30.0, 3.0));
        let vals = Values::from([("w".into(), 80.0), ("h".into(), 60.0), ("r".into(), 6.0)]);
        let rep = s.solve(&vals).unwrap();
        assert!(rep.converged, "{rep:?}");
        assert_eq!(rep.dof, 0, "{rep:?}");
        let pr = s.profiles();
        assert_eq!(pr.profiles.len(), 1);
        assert!(pr.open.is_empty());
        let area = loop_area(&pr.profiles[0].outer);
        let want = 80.0 * 60.0 - (4.0 - PI) * 36.0;
        assert!((area - want).abs() < 0.5, "{area} vs {want}");
    }

    #[test]
    fn tangent_point_on_circle_symmetric_angle() {
        let mut s = Sketch::new("t", Plane::XY);
        let c = s.circle(v(0.0, 0.0), 4.0);
        let l = s.line(v(-10.0, 5.5), v(10.0, 4.5));
        s.constrain(Constraint::Fix(PointId(0), v(0.0, 0.0)));
        s.constrain(Constraint::Radius(c, 5.0.into()));
        s.constrain(Constraint::Tangent(l, c));
        s.constrain(Constraint::Horizontal(l));
        s.solve(&Values::new()).unwrap();
        let Geom::Line { a, b } = s.entities[l.0 as usize].geom else { panic!() };
        assert!((s.point(a).y - 5.0).abs() < 1e-7 && (s.point(b).y - 5.0).abs() < 1e-7);

        let mut s = Sketch::new("a", Plane::XY);
        let l1 = s.line(v(0.0, 0.0), v(10.0, 0.0));
        let l2 = s.line(v(0.0, 0.0), v(7.0, 8.0));
        s.constrain(Constraint::Fix(PointId(0), v(0.0, 0.0)));
        s.constrain(Constraint::Coincident(PointId(0), PointId(2)));
        s.constrain(Constraint::Horizontal(l1));
        s.constrain(Constraint::Angle(l1, l2, "30deg".into()));
        let q = s.add_point(v(3.0, 3.0));
        let q2 = s.add_point(v(5.0, -2.0));
        s.constrain(Constraint::Symmetric(q, q2, l1));
        s.constrain(Constraint::PointOnLine(q, l2));
        let rep = s.solve(&Values::new()).unwrap();
        assert!(rep.converged);
        let d = s.point(PointId(3)) - s.point(PointId(2));
        assert!((d.y.atan2(d.x) - PI / 6.0).abs() < 1e-7);
        assert!((s.point(q).y + s.point(q2).y).abs() < 1e-7);
        assert!((s.point(q).x - s.point(q2).x).abs() < 1e-7);
    }

    #[test]
    fn midpoint_equal_diameter_distance() {
        let mut s = Sketch::new("m", Plane::XY);
        let l1 = s.line(v(0.0, 0.0), v(10.0, 1.0));
        let l2 = s.line(v(0.0, 5.0), v(3.0, 6.0));
        let m = s.add_point(v(1.0, 1.0));
        let c = s.circle(v(20.0, 0.0), 1.0);
        let c2 = s.circle(v(30.0, 0.0), 2.0);
        s.constrain(Constraint::Midpoint(m, l1));
        s.constrain(Constraint::Equal(l1, l2));
        s.constrain(Constraint::Diameter(c, 6.0.into()));
        s.constrain(Constraint::Equal(c, c2));
        s.constrain(Constraint::Distance(PointId(5), PointId(6), 12.0.into()));
        s.constrain(Constraint::PointOnCircle(PointId(0), c));
        let rep = s.solve(&Values::new()).unwrap();
        assert!(rep.converged, "{rep:?}");
        let len = |e: EntityId| match s.entities[e.0 as usize].geom {
            Geom::Line { a, b } => s.point(a).distance(s.point(b)),
            _ => 0.0,
        };
        assert!((len(l1) - len(l2)).abs() < 1e-7);
        assert!(s.point(m).distance((s.point(PointId(0)) + s.point(PointId(1))) / 2.0) < 1e-7);
        assert!((s.point(PointId(5)).distance(s.point(PointId(6))) - 12.0).abs() < 1e-7);
        assert!((s.point(PointId(0)).distance(s.point(PointId(5))) - 3.0).abs() < 1e-7);
        assert!(matches!(s.entities[c2.0 as usize].geom, Geom::Circle { radius, .. } if (radius - 3.0).abs() < 1e-7));
    }

    #[test]
    fn line_distance_holds_parallel_and_serde_round_trips() {
        let mut s = Sketch::new("ld", Plane::XY);
        let l1 = s.line(v(0.0, 0.0), v(10.0, 0.0));
        let l2 = s.line(v(0.0, 3.0), v(10.0, 4.0));
        s.constrain(Constraint::Fix(PointId(0), v(0.0, 0.0)));
        s.constrain(Constraint::Fix(PointId(1), v(10.0, 0.0)));
        s.constrain(Constraint::LineDistance(l1, l2, "6".into()));
        let rep = s.solve(&Values::new()).unwrap();
        assert!(rep.converged, "{rep:?}");
        assert!((s.point(PointId(2)).y.abs() - 6.0).abs() < 1e-7);
        assert!((s.point(PointId(3)).y.abs() - 6.0).abs() < 1e-7);
        let back: Sketch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn profile_nesting_and_open_chains() {
        let mut s = Sketch::new("n", Plane::XY);
        s.rect(v(0.0, 0.0), v(100.0, 100.0));
        s.rect(v(10.0, 10.0), v(90.0, 90.0)); // hole
        s.rect(v(20.0, 20.0), v(30.0, 30.0)); // island inside the hole
        s.circle(v(70.0, 70.0), 5.0); // hole in the island? no: inside hole region -> island
        s.circle(v(200.0, 0.0), 5.0); // separate outer
        // shuffled, reversed open chain of two segments
        s.line(v(0.0, 200.0), v(10.0, 200.0));
        s.line(v(20.0, 210.0), v(10.0, 200.0));
        let pr = s.profiles();
        assert_eq!(pr.profiles.len(), 4, "{:#?}", pr.profiles.iter().map(|p| p.holes.len()).collect::<Vec<_>>());
        let big = pr.profiles.iter().find(|p| loop_area(&p.outer) > 9000.0).unwrap();
        assert_eq!(big.holes.len(), 1);
        assert!(loop_area(&big.outer) > 0.0 && loop_area(&big.holes[0]) < 0.0);
        assert_eq!(pr.open.len(), 1);
        assert_eq!(pr.open[0].len(), 2);
    }
}
