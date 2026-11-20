//! Sketch editing helpers shared by every front end (GUI_NEEDS #12–14): chain
//! walking and a native offset, entity removal with id remapping, split, trim at
//! intersections, corner fillet, and exact projection of model edges.
//!
//! Every helper keeps the sketch solvable: new points are tied to what they came
//! from with constraints (a trimmed end stays on the curve that cut it, a filleted
//! corner keeps its virtual sharp point on both lines, projected geometry is fixed).

use glam::{DVec2, DVec3};

use crate::params::{Expr, Values};
use crate::sketch::{Constraint, EntityId, Geom, PointId, Sketch};

/// Old id → new id after a removal (`None` = removed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remap {
    pub points: Vec<Option<PointId>>,
    pub entities: Vec<Option<EntityId>>,
}

impl Remap {
    pub fn point(&self, p: PointId) -> Option<PointId> {
        self.points.get(p.0 as usize).copied().flatten()
    }
    pub fn entity(&self, e: EntityId) -> Option<EntityId> {
        self.entities.get(e.0 as usize).copied().flatten()
    }
}

impl Geom {
    pub fn points(&self) -> Vec<PointId> {
        match *self {
            Geom::Line { a, b } => vec![a, b],
            Geom::Arc { center, start, end } => vec![center, start, end],
            Geom::Circle { center, .. } => vec![center],
        }
    }
    /// The chain endpoints (none for a circle).
    pub fn ends(&self) -> Option<(PointId, PointId)> {
        match *self {
            Geom::Line { a, b } => Some((a, b)),
            Geom::Arc { start, end, .. } => Some((start, end)),
            Geom::Circle { .. } => None,
        }
    }
    fn remap(&self, p: &dyn Fn(PointId) -> Option<PointId>) -> Option<Geom> {
        Some(match *self {
            Geom::Line { a, b } => Geom::Line { a: p(a)?, b: p(b)? },
            Geom::Arc { center, start, end } => Geom::Arc { center: p(center)?, start: p(start)?, end: p(end)? },
            Geom::Circle { center, radius } => Geom::Circle { center: p(center)?, radius },
        })
    }
}

impl Constraint {
    /// Points the constraint refers to.
    pub fn points(&self) -> Vec<PointId> {
        use Constraint::*;
        match self {
            Coincident(a, b) | Distance(a, b, _) | DistanceX(a, b, _) | DistanceY(a, b, _) | Symmetric(a, b, _) => vec![*a, *b],
            PointOnLine(q, _) | PointOnCircle(q, _) | Midpoint(q, _) | Fix(q, _) | PointLineDistance(q, _, _) => vec![*q],
            _ => vec![],
        }
    }
    /// Entities the constraint refers to.
    pub fn entities(&self) -> Vec<EntityId> {
        use Constraint::*;
        match self {
            Horizontal(e) | Vertical(e) | Length(e, _) | Radius(e, _) | Diameter(e, _) => vec![*e],
            Parallel(a, b) | Perpendicular(a, b) | Tangent(a, b) | Equal(a, b) | Angle(a, b, _) => vec![*a, *b],
            PointOnLine(_, e) | PointOnCircle(_, e) | Midpoint(_, e) | Symmetric(_, _, e) | PointLineDistance(_, e, _) => vec![*e],
            Offset { source, target, .. } => source.iter().chain(target).copied().collect(),
            Coincident(..) | Fix(..) | Distance(..) | DistanceX(..) | DistanceY(..) => vec![],
        }
    }
    /// The constraint with ids mapped (`None` if anything it needs is gone). An
    /// offset keeps the pairs whose source and target both survive.
    pub fn remap(&self, p: &dyn Fn(PointId) -> Option<PointId>, e: &dyn Fn(EntityId) -> Option<EntityId>) -> Option<Constraint> {
        use Constraint::*;
        Some(match self {
            Coincident(a, b) => Coincident(p(*a)?, p(*b)?),
            Horizontal(x) => Horizontal(e(*x)?),
            Vertical(x) => Vertical(e(*x)?),
            Parallel(a, b) => Parallel(e(*a)?, e(*b)?),
            Perpendicular(a, b) => Perpendicular(e(*a)?, e(*b)?),
            Tangent(a, b) => Tangent(e(*a)?, e(*b)?),
            Equal(a, b) => Equal(e(*a)?, e(*b)?),
            PointOnLine(q, x) => PointOnLine(p(*q)?, e(*x)?),
            PointOnCircle(q, x) => PointOnCircle(p(*q)?, e(*x)?),
            Midpoint(q, x) => Midpoint(p(*q)?, e(*x)?),
            Symmetric(a, b, x) => Symmetric(p(*a)?, p(*b)?, e(*x)?),
            Fix(q, v) => Fix(p(*q)?, *v),
            Distance(a, b, d) => Distance(p(*a)?, p(*b)?, d.clone()),
            DistanceX(a, b, d) => DistanceX(p(*a)?, p(*b)?, d.clone()),
            DistanceY(a, b, d) => DistanceY(p(*a)?, p(*b)?, d.clone()),
            PointLineDistance(q, x, d) => PointLineDistance(p(*q)?, e(*x)?, d.clone()),
            Length(x, d) => Length(e(*x)?, d.clone()),
            Radius(x, d) => Radius(e(*x)?, d.clone()),
            Diameter(x, d) => Diameter(e(*x)?, d.clone()),
            Angle(a, b, d) => Angle(e(*a)?, e(*b)?, d.clone()),
            Offset { source, target, distance } => {
                let pairs: Vec<(EntityId, EntityId)> = source.iter().zip(target).filter_map(|(s, t)| Some((e(*s)?, e(*t)?))).collect();
                if pairs.is_empty() {
                    return None;
                }
                Offset { source: pairs.iter().map(|x| x.0).collect(), target: pairs.iter().map(|x| x.1).collect(), distance: distance.clone() }
            }
        })
    }
}

fn line_line(p: DVec2, u: DVec2, q: DVec2, v: DVec2) -> Option<(f64, f64)> {
    let den = u.perp_dot(v);
    if den.abs() < 1e-12 {
        return None;
    }
    let w = q - p;
    Some((w.perp_dot(v) / den, w.perp_dot(u) / den))
}

/// Parameters `t` along `p + u·t` where the line meets the circle.
fn line_circle(p: DVec2, u: DVec2, c: DVec2, r: f64) -> Vec<f64> {
    let (a, b, cc) = (u.dot(u), 2.0 * u.dot(p - c), (p - c).length_squared() - r * r);
    let disc = b * b - 4.0 * a * cc;
    if a < 1e-18 || disc < 0.0 {
        return vec![];
    }
    let s = disc.sqrt();
    vec![(-b - s) / (2.0 * a), (-b + s) / (2.0 * a)]
}

fn circle_circle(c1: DVec2, r1: f64, c2: DVec2, r2: f64) -> Vec<DVec2> {
    let d = c1.distance(c2);
    if d < 1e-12 || d > r1 + r2 + 1e-12 || d < (r1 - r2).abs() - 1e-12 {
        return vec![];
    }
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let m = c1 + (c2 - c1) * (a / d);
    let perp = (c2 - c1).perp() / d;
    vec![m + perp * h, m - perp * h]
}

/// CCW angle from `a` to `b` in [0, 2π).
fn ccw(a: f64, b: f64) -> f64 {
    (b - a).rem_euclid(std::f64::consts::TAU)
}

/// An entity's geometry in sketch coordinates.
#[derive(Clone, Copy, Debug)]
enum Carrier {
    Seg(DVec2, DVec2),
    /// center, radius, start angle, CCW sweep (2π for a circle)
    Arc(DVec2, f64, f64, f64),
}

impl Carrier {
    /// Intersection points of the full carriers that lie within both extents.
    fn meet(&self, o: &Carrier) -> Vec<DVec2> {
        let inside = |c: &Carrier, q: DVec2| match *c {
            Carrier::Seg(a, b) => {
                let t = (q - a).dot(b - a) / (b - a).length_squared().max(1e-18);
                (-1e-9..=1.0 + 1e-9).contains(&t)
            }
            Carrier::Arc(cc, _, a0, sw) => sw >= std::f64::consts::TAU - 1e-12 || ccw(a0, (q - cc).to_angle()) <= sw + 1e-9,
        };
        let pts: Vec<DVec2> = match (*self, *o) {
            (Carrier::Seg(a, b), Carrier::Seg(c, d)) => line_line(a, b - a, c, d - c).map(|(t, _)| vec![a + (b - a) * t]).unwrap_or_default(),
            (Carrier::Seg(a, b), Carrier::Arc(c, r, ..)) | (Carrier::Arc(c, r, ..), Carrier::Seg(a, b)) => {
                line_circle(a, b - a, c, r).into_iter().map(|t| a + (b - a) * t).collect()
            }
            (Carrier::Arc(c1, r1, ..), Carrier::Arc(c2, r2, ..)) => circle_circle(c1, r1, c2, r2),
        };
        pts.into_iter().filter(|q| inside(self, *q) && inside(o, *q)).collect()
    }
    /// Position of `q` along the entity: 0..1 for a segment, CCW angle from the
    /// start for an arc.
    fn param(&self, q: DVec2) -> f64 {
        match *self {
            Carrier::Seg(a, b) => (q - a).dot(b - a) / (b - a).length_squared().max(1e-18),
            Carrier::Arc(c, _, a0, _) => ccw(a0, (q - c).to_angle()),
        }
    }
    fn end_param(&self) -> f64 {
        match *self {
            Carrier::Seg(..) => 1.0,
            Carrier::Arc(.., sw) => sw,
        }
    }
}

impl Sketch {
    fn carrier(&self, e: EntityId) -> Option<Carrier> {
        Some(match self.entities.get(e.0 as usize)?.geom {
            Geom::Line { a, b } => Carrier::Seg(self.point(a), self.point(b)),
            Geom::Arc { center, start, end } => {
                let c = self.point(center);
                let (s, t) = (self.point(start) - c, self.point(end) - c);
                let sw = ccw(s.to_angle(), t.to_angle());
                Carrier::Arc(c, s.length(), s.to_angle(), if sw < 1e-12 { std::f64::consts::TAU } else { sw })
            }
            Geom::Circle { center, radius } => Carrier::Arc(self.point(center), radius, 0.0, std::f64::consts::TAU),
        })
    }

    /// Point classes joined by shared ids, `Coincident` constraints, or entity end
    /// points on the same spot (DXF imports have separate, equal end points).
    fn same_point(&self) -> impl Fn(PointId, PointId) -> bool + use<> {
        let n = self.points.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], i: usize) -> usize {
            let mut i = i;
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for c in &self.constraints {
            if let Constraint::Coincident(a, b) = c {
                if (a.0 as usize) < n && (b.0 as usize) < n {
                    let (x, y) = (find(&mut parent, a.0 as usize), find(&mut parent, b.0 as usize));
                    parent[x] = y;
                }
            }
        }
        let ends: Vec<usize> = self.entities.iter().filter_map(|e| e.geom.ends()).flat_map(|(a, b)| [a.0 as usize, b.0 as usize]).filter(|i| *i < n).collect();
        for (k, &i) in ends.iter().enumerate() {
            for &j in &ends[k + 1..] {
                if self.points[i].distance(self.points[j]) < 1e-6 {
                    let (x, y) = (find(&mut parent, i), find(&mut parent, j));
                    parent[x] = y;
                }
            }
        }
        let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
        move |a: PointId, b: PointId| a == b || roots.get(a.0 as usize).is_some_and(|r| Some(r) == roots.get(b.0 as usize))
    }

    /// Remove entities, the points no entity uses any more, and the constraints
    /// that referred to anything removed; the remaining ids are renumbered.
    pub fn remove_entities(&mut self, ids: &[EntityId]) -> Remap {
        let keep_e: Vec<bool> = (0..self.entities.len()).map(|i| !ids.contains(&EntityId(i as u32))).collect();
        let mut used = vec![false; self.points.len()];
        for (i, e) in self.entities.iter().enumerate() {
            if keep_e[i] {
                for p in e.geom.points() {
                    if let Some(u) = used.get_mut(p.0 as usize) {
                        *u = true;
                    }
                }
            }
        }
        // points that only constraints use (virtual corners, construction points
        // a dimension refers to) stay unless every entity they belonged to went
        let mut owned = vec![false; self.points.len()];
        for e in &self.entities {
            for p in e.geom.points() {
                if let Some(o) = owned.get_mut(p.0 as usize) {
                    *o = true;
                }
            }
        }
        let keep_p: Vec<bool> = (0..self.points.len()).map(|i| used[i] || !owned[i]).collect();
        let mut map = Remap::default();
        let mut n = 0;
        for k in &keep_p {
            map.points.push(if *k {
                n += 1;
                Some(PointId(n - 1))
            } else {
                None
            });
        }
        let mut n = 0;
        for k in &keep_e {
            map.entities.push(if *k {
                n += 1;
                Some(EntityId(n - 1))
            } else {
                None
            });
        }
        let (pf, ef) = (|p: PointId| map.point(p), |e: EntityId| map.entity(e));
        self.points = self.points.iter().enumerate().filter(|(i, _)| keep_p[*i]).map(|(_, p)| *p).collect();
        let ents = std::mem::take(&mut self.entities);
        self.entities = ents
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep_e[*i])
            .filter_map(|(_, mut e)| {
                e.geom = e.geom.remap(&pf)?;
                Some(e)
            })
            .collect();
        let cs = std::mem::take(&mut self.constraints);
        self.constraints = cs.iter().filter_map(|c| c.remap(&pf, &ef)).collect();
        map
    }

    /// The chain through `start`: entities joined end to end (shared, coincident or
    /// equal points), as `(entity, reversed)` in travel order, and whether it closes.
    /// Construction entities are skipped unless `start` is one. A circle is a
    /// closed chain of itself.
    pub fn chain_from(&self, start: EntityId) -> (Vec<(EntityId, bool)>, bool) {
        let Some(e0) = self.entities.get(start.0 as usize) else { return (vec![], false) };
        let Some((s0, t0)) = e0.geom.ends() else { return (vec![(start, false)], true) };
        let same = self.same_point();
        let cons = e0.construction;
        let next = |at: PointId, used: &[EntityId]| -> Option<(EntityId, bool, PointId)> {
            self.entities.iter().enumerate().find_map(|(i, e)| {
                let id = EntityId(i as u32);
                if used.contains(&id) || e.construction != cons {
                    return None;
                }
                let (a, b) = e.geom.ends()?;
                if same(a, at) {
                    Some((id, false, b))
                } else if same(b, at) {
                    Some((id, true, a))
                } else {
                    None
                }
            })
        };
        let mut chain = vec![(start, false)];
        let mut used = vec![start];
        let mut at = t0;
        while let Some((id, rev, far)) = next(at, &used) {
            if chain.len() > self.entities.len() {
                break;
            }
            chain.push((id, rev));
            used.push(id);
            at = far;
            if same(at, s0) {
                return (chain, true);
            }
        }
        // open: extend backwards from the start
        let mut at = s0;
        while let Some((id, rev, far)) = next(at, &used) {
            chain.insert(0, (id, !rev));
            used.push(id);
            at = far;
        }
        (chain, false)
    }

    fn travel(&self, e: EntityId, rev: bool) -> (PointId, PointId) {
        let (a, b) = self.entities[e.0 as usize].geom.ends().expect("chain entity with ends");
        if rev {
            (b, a)
        } else {
            (a, b)
        }
    }

    /// Offset the chain through `start` by `distance` (GUI_NEEDS #12): positive
    /// grows a closed loop (outward), and goes to the left of an open chain's
    /// travel direction. Makes new entities joined at the offset carriers'
    /// intersections (sharp corners stay sharp), arcs concentric with their source
    /// (sharing its centre point), and one `Constraint::Offset` tying them to the
    /// source; it leaves them fully determined, tangent joints included. The new
    /// entities are profile geometry even when the source is construction (a DXF
    /// reference). Returns them in chain order.
    pub fn offset_chain(&mut self, start: EntityId, distance: impl Into<Expr>, vals: &Values) -> Result<Vec<EntityId>, String> {
        let distance = distance.into();
        let d = distance.eval(vals).map_err(|e| e.to_string())?;
        if d.abs() < 1e-9 {
            return Err("offset distance is zero".into());
        }
        let (chain, closed) = self.chain_from(start);
        if chain.is_empty() {
            return Err("no such entity".into());
        }
        // a lone circle
        if let [(e, _)] = chain[..] {
            if let Geom::Circle { center, radius } = self.entities[e.0 as usize].geom {
                if radius + d <= 1e-9 {
                    return Err(format!("the offset circle would have radius {:.3}", radius + d));
                }
                let id = self.add_entity(Geom::Circle { center, radius: radius + d }, false);
                self.constrain(Constraint::Offset { source: vec![e], target: vec![id], distance });
                return Ok(vec![id]);
            }
        }
        // orientation: a CCW loop grows to the right of travel
        let poly: Vec<DVec2> = chain
            .iter()
            .flat_map(|&(e, rev)| {
                let (a, b) = self.travel(e, rev);
                let mut v = vec![self.point(a)];
                if let Geom::Arc { center, .. } = self.entities[e.0 as usize].geom {
                    let c = self.point(center);
                    let (s, t) = (self.point(a) - c, self.point(b) - c);
                    let sw = if rev { -ccw(t.to_angle(), s.to_angle()) } else { ccw(s.to_angle(), t.to_angle()) };
                    v.extend((1..8).map(|k| c + DVec2::from_angle(s.to_angle() + sw * k as f64 / 8.0) * s.length()));
                }
                v
            })
            .collect();
        let area = crate::sketch::signed_area(&poly);
        let right = if closed { if area > 0.0 { d } else { -d } } else { -d };
        // offset carriers in travel order: (line point, dir) or (centre, radius)
        enum Off {
            Line(DVec2, DVec2),
            Circ(PointId, DVec2, f64),
        }
        let mut offs = Vec::new();
        for &(e, rev) in &chain {
            let (a, b) = self.travel(e, rev);
            let (pa, pb) = (self.point(a), self.point(b));
            offs.push(match self.entities[e.0 as usize].geom {
                Geom::Line { .. } => {
                    let u = (pb - pa).normalize_or_zero();
                    let nr = DVec2::new(u.y, -u.x);
                    Off::Line(pa + nr * right, u)
                }
                Geom::Arc { center, .. } => {
                    let r = (pa - self.point(center)).length() + if rev { -right } else { right };
                    if r <= 1e-9 {
                        return Err("an arc of the chain is smaller than the offset".into());
                    }
                    Off::Circ(center, self.point(center), r)
                }
                Geom::Circle { .. } => return Err("a circle cannot be part of a chain".into()),
            });
        }
        // joint between offset k and k+1, near the source joint `j`
        let joint = |k1: &Off, k2: &Off, j: DVec2| -> Option<DVec2> {
            let cands: Vec<DVec2> = match (k1, k2) {
                (Off::Line(p, u), Off::Line(q, v)) => match line_line(*p, *u, *q, *v) {
                    Some((t, _)) => vec![*p + *u * t],
                    None => vec![*p + *u * (j - *p).dot(*u)], // collinear continuation
                },
                (Off::Line(p, u), Off::Circ(_, c, r)) | (Off::Circ(_, c, r), Off::Line(p, u)) => line_circle(*p, *u, *c, *r).into_iter().map(|t| *p + *u * t).collect(),
                (Off::Circ(_, c1, r1), Off::Circ(_, c2, r2)) => {
                    if c1.distance(*c2) < 1e-9 {
                        vec![*c1 + (j - *c1).normalize_or_zero() * *r1]
                    } else {
                        circle_circle(*c1, *r1, *c2, *r2)
                    }
                }
            };
            cands.into_iter().min_by(|a, b| a.distance(j).total_cmp(&b.distance(j)))
        };
        let n = chain.len();
        let mut jpts: Vec<DVec2> = Vec::new();
        let nj = if closed { n } else { n + 1 };
        for k in 0..nj {
            let src_j = |kk: usize| {
                let (e, rev) = chain[kk % n];
                self.point(self.travel(e, rev).0)
            };
            let q = if !closed && k == 0 {
                // open start: project the chain start onto the first offset
                let (e, rev) = chain[0];
                let s = self.point(self.travel(e, rev).0);
                match &offs[0] {
                    Off::Line(p, u) => *p + *u * (s - *p).dot(*u),
                    Off::Circ(_, c, r) => *c + (s - *c).normalize_or_zero() * *r,
                }
            } else if !closed && k == n {
                let (e, rev) = chain[n - 1];
                let s = self.point(self.travel(e, rev).1);
                match &offs[n - 1] {
                    Off::Line(p, u) => *p + *u * (s - *p).dot(*u),
                    Off::Circ(_, c, r) => *c + (s - *c).normalize_or_zero() * *r,
                }
            } else {
                let (prev, cur) = (&offs[(k + n - 1) % n], &offs[k % n]);
                joint(prev, cur, src_j(k)).ok_or("the offset needs a corner arc here (the offset carriers do not meet); not supported")?
            };
            jpts.push(q);
        }
        let ids: Vec<PointId> = jpts.iter().map(|q| self.add_point(*q)).collect();
        let pid = |k: usize| ids[k % ids.len()];
        let mut out = Vec::new();
        for (k, &(_, rev)) in chain.iter().enumerate() {
            let (a, b) = (pid(k), pid(k + 1));
            let id = match &offs[k] {
                Off::Line(..) => self.add_entity(Geom::Line { a, b }, false),
                Off::Circ(center, ..) => {
                    let (s, t) = if rev { (b, a) } else { (a, b) };
                    self.add_entity(Geom::Arc { center: *center, start: s, end: t }, false)
                }
            };
            out.push(id);
        }
        self.constrain(Constraint::Offset { source: chain.iter().map(|c| c.0).collect(), target: out.clone(), distance });
        Ok(out)
    }

    /// Split a line or arc at the point nearest `at`; `e` keeps the first part and
    /// the new entity (returned) is the rest. The two stay joined and collinear /
    /// concentric; direction constraints (H/V, parallel, perpendicular) are copied.
    pub fn split_at(&mut self, e: EntityId, at: DVec2) -> Result<EntityId, String> {
        let ent = self.entities.get(e.0 as usize).cloned().ok_or("no such entity")?;
        let car = self.carrier(e).ok_or("no such entity")?;
        let t = car.param(at);
        if !(1e-9..car.end_param() - 1e-9).contains(&t) {
            return Err("the split point is not inside the entity".into());
        }
        let new = match ent.geom {
            Geom::Line { a, b } => {
                let (pa, pb) = (self.point(a), self.point(b));
                let m = self.add_point(pa + (pb - pa) * t);
                self.entities[e.0 as usize].geom = Geom::Line { a, b: m };
                let n = self.add_entity(Geom::Line { a: m, b }, ent.construction);
                self.constrain(Constraint::PointOnLine(b, e));
                n
            }
            Geom::Arc { center, start, end } => {
                let c = self.point(center);
                let r = (self.point(start) - c).length();
                let m = self.add_point(c + DVec2::from_angle((self.point(start) - c).to_angle() + t) * r);
                self.entities[e.0 as usize].geom = Geom::Arc { center, start, end: m };
                self.add_entity(Geom::Arc { center, start: m, end }, ent.construction)
            }
            Geom::Circle { .. } => return Err("a circle has no ends to split between; trim it instead".into()),
        };
        let copies: Vec<Constraint> = self
            .constraints
            .iter()
            .filter_map(|c| match c {
                Constraint::Horizontal(x) if *x == e => Some(Constraint::Horizontal(new)),
                Constraint::Vertical(x) if *x == e => Some(Constraint::Vertical(new)),
                Constraint::Parallel(x, y) if *x == e => Some(Constraint::Parallel(new, *y)),
                Constraint::Parallel(x, y) if *y == e => Some(Constraint::Parallel(*x, new)),
                Constraint::Perpendicular(x, y) if *x == e => Some(Constraint::Perpendicular(new, *y)),
                Constraint::Perpendicular(x, y) if *y == e => Some(Constraint::Perpendicular(*x, new)),
                _ => None,
            })
            .collect();
        self.constraints.extend(copies);
        Ok(new)
    }

    /// Trim the piece of `e` around `pick` between its nearest intersections with
    /// the other entities (Fusion's trim). With no intersection the whole entity
    /// goes; a circle needs two and becomes an arc. New ends stay on the curve that
    /// cut them. Returns the remap of the removal (identity when nothing was removed).
    pub fn trim(&mut self, e: EntityId, pick: DVec2) -> Result<Remap, String> {
        let car = self.carrier(e).ok_or("no such entity")?;
        let mut cuts: Vec<(f64, DVec2, EntityId)> = Vec::new();
        for i in 0..self.entities.len() {
            let o = EntityId(i as u32);
            if o == e {
                continue;
            }
            if let Some(oc) = self.carrier(o) {
                for q in car.meet(&oc) {
                    cuts.push((car.param(q), q, o));
                }
            }
        }
        let end = car.end_param();
        cuts.retain(|c| c.0 > 1e-7 && c.0 < end - 1e-7);
        let tp = car.param(pick);
        let full_circle = matches!(self.entities[e.0 as usize].geom, Geom::Circle { .. });
        if full_circle {
            if cuts.len() < 2 {
                return Ok(self.remove_entities(&[e]));
            }
            // keep the arc from the cut after the pick round to the cut before it
            let after = cuts.iter().filter(|c| c.0 > tp).min_by(|a, b| a.0.total_cmp(&b.0)).or_else(|| cuts.iter().min_by(|a, b| a.0.total_cmp(&b.0))).cloned().unwrap();
            let before = cuts.iter().filter(|c| c.0 < tp).max_by(|a, b| a.0.total_cmp(&b.0)).or_else(|| cuts.iter().max_by(|a, b| a.0.total_cmp(&b.0))).cloned().unwrap();
            let Geom::Circle { center, .. } = self.entities[e.0 as usize].geom else { unreachable!() };
            let (s, t) = (self.add_point(after.1), self.add_point(before.1));
            self.entities[e.0 as usize].geom = Geom::Arc { center, start: s, end: t };
            self.on_curve(s, after.2);
            self.on_curve(t, before.2);
            return Ok(self.identity());
        }
        let lo = cuts.iter().filter(|c| c.0 < tp).max_by(|a, b| a.0.total_cmp(&b.0)).cloned();
        let hi = cuts.iter().filter(|c| c.0 > tp).min_by(|a, b| a.0.total_cmp(&b.0)).cloned();
        match (lo, hi) {
            (None, None) => Ok(self.remove_entities(&[e])),
            (Some(l), None) => {
                let rest = self.split_at(e, l.1)?;
                let m = self.travel(e, false).1;
                self.on_curve(m, l.2);
                Ok(self.remove_entities(&[rest]))
            }
            (None, Some(h)) => {
                let rest = self.split_at(e, h.1)?;
                let m = self.travel(rest, false).0;
                self.on_curve(m, h.2);
                Ok(self.remove_entities(&[e]))
            }
            (Some(l), Some(h)) => {
                let tail = self.split_at(e, h.1)?;
                let mh = self.travel(tail, false).0;
                self.on_curve(mh, h.2);
                let mid = self.split_at(e, l.1)?;
                let ml = self.travel(e, false).1;
                self.on_curve(ml, l.2);
                Ok(self.remove_entities(&[mid]))
            }
        }
    }

    fn identity(&self) -> Remap {
        Remap { points: (0..self.points.len() as u32).map(|i| Some(PointId(i))).collect(), entities: (0..self.entities.len() as u32).map(|i| Some(EntityId(i))).collect() }
    }

    fn on_curve(&mut self, p: PointId, curve: EntityId) {
        match self.entities[curve.0 as usize].geom {
            Geom::Line { .. } => self.constrain(Constraint::PointOnLine(p, curve)),
            _ => self.constrain(Constraint::PointOnCircle(p, curve)),
        }
    }

    /// Round the corner where two lines meet at `corner` (shared or coincident
    /// endpoints) with a tangent arc of `radius`. The lines are cut back to the
    /// tangent points; the old corner stays as a virtual sharp point on both lines
    /// (dimensions to it keep working). Returns the arc.
    pub fn fillet_corner(&mut self, corner: PointId, radius: impl Into<Expr>, vals: &Values) -> Result<EntityId, String> {
        let radius = radius.into();
        let r = radius.eval(vals).map_err(|e| e.to_string())?;
        if r <= 0.0 {
            return Err("fillet radius must be positive".into());
        }
        let same = self.same_point();
        let lines: Vec<(EntityId, PointId, PointId)> = self
            .entities
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e.geom {
                Geom::Line { a, b } if same(a, corner) => Some((EntityId(i as u32), a, b)),
                Geom::Line { a, b } if same(b, corner) => Some((EntityId(i as u32), b, a)),
                _ => None,
            })
            .collect();
        let [(l1, q1, f1), (l2, q2, f2)] = lines[..] else {
            return Err(format!("a corner fillet needs exactly two lines at the point ({} found)", lines.len()));
        };
        let pc = self.point(q1);
        let (v1, v2) = (self.point(f1) - pc, self.point(f2) - pc);
        let (u1, u2) = (v1.normalize_or_zero(), v2.normalize_or_zero());
        let th = u1.dot(u2).clamp(-1.0, 1.0).acos();
        if !(1e-6..=std::f64::consts::PI - 1e-6).contains(&th) {
            return Err("the lines are parallel at the corner".into());
        }
        let dt = r / (th / 2.0).tan();
        if dt >= v1.length() - 1e-9 || dt >= v2.length() - 1e-9 {
            return Err(format!("radius {r:.3} is too large for these lines (needs {dt:.3} of each)"));
        }
        let (t1, t2) = (pc + u1 * dt, pc + u2 * dt);
        let c = pc + (u1 + u2).normalize() * (r / (th / 2.0).sin());
        let (p1, p2, pcen) = (self.add_point(t1), self.add_point(t2), self.add_point(c));
        let set_end = |g: &mut Geom, old: PointId, new: PointId| {
            if let Geom::Line { a, b } = g {
                if *a == old {
                    *a = new;
                } else if *b == old {
                    *b = new;
                }
            }
        };
        set_end(&mut self.entities[l1.0 as usize].geom, q1, p1);
        set_end(&mut self.entities[l2.0 as usize].geom, q2, p2);
        let (s, t) = if (t1 - c).perp_dot(t2 - c) > 0.0 { (p1, p2) } else { (p2, p1) };
        let arc = self.add_entity(Geom::Arc { center: pcen, start: s, end: t }, self.entities[l1.0 as usize].construction);
        self.constrain(Constraint::Tangent(l1, arc));
        self.constrain(Constraint::Tangent(l2, arc));
        self.constrain(Constraint::Radius(arc, radius));
        // the virtual corner stays where the lines meet
        self.constrain(Constraint::PointOnLine(q1, l1));
        self.constrain(Constraint::PointOnLine(q1, l2));
        Ok(arc)
    }

    /// Project a model edge (in the same frame as the sketch plane) into the sketch
    /// as fixed geometry (GUI_NEEDS #14): lines stay lines, circles/arcs facing the
    /// plane stay exact circles/arcs, anything else (tilted circles, B-splines) is
    /// a fixed polyline through `samples` (e.g. the mesh edge polyline) or start,
    /// middle and end. Edges seen end-on add nothing.
    pub fn project_edge(&mut self, e: &occt::EdgeInfo, samples: Option<&[DVec3]>, construction: bool) -> Vec<EntityId> {
        let n0 = self.entities.len();
        let made = self.project_edge_inner(e, samples, construction);
        for i in n0..self.entities.len() {
            self.entities[i].projected = true;
        }
        made
    }

    fn project_edge_inner(&mut self, e: &occt::EdgeInfo, samples: Option<&[DVec3]>, construction: bool) -> Vec<EntityId> {
        let pl = self.plane;
        let n = pl.normal.normalize_or_zero();
        let to2 = |p: DVec3| pl.to_local(p);
        let fixed = |sk: &mut Sketch, q: DVec2| {
            // reuse an already fixed point at the same spot so projected loops share corners
            if let Some(i) = (0..sk.points.len()).find(|&i| {
                sk.points[i].distance(q) < 1e-9
                    && sk.constraints.iter().any(|c| matches!(c, Constraint::Fix(p, _) if p.0 as usize == i))
            }) {
                return PointId(i as u32);
            }
            let id = sk.add_point(q);
            sk.constrain(Constraint::Fix(id, q));
            id
        };
        match (e.kind, e.circle) {
            (occt::CurveKind::Line, _) => {
                let (a, b) = (to2(e.start), to2(e.end));
                if a.distance(b) < 1e-9 {
                    return vec![];
                }
                let (pa, pb) = (fixed(self, a), fixed(self, b));
                vec![self.add_entity(Geom::Line { a: pa, b: pb }, construction)]
            }
            (occt::CurveKind::Circle, Some((c, ax, r))) if ax.normalize_or_zero().dot(n).abs() > 1.0 - 1e-9 => {
                let cc = fixed(self, to2(c));
                if e.closed {
                    let id = self.add_entity(Geom::Circle { center: cc, radius: r }, construction);
                    self.constrain(Constraint::Radius(id, Expr::from(r)));
                    return vec![id];
                }
                let (s, m, t) = (to2(e.start), to2(e.midpoint), to2(e.end));
                let c2 = to2(c);
                let ang = |q: DVec2| (q - c2).to_angle();
                // CCW from s to t through m, else the other way round
                let fwd = ccw(ang(s), ang(m)) < ccw(ang(s), ang(t));
                let (a, b) = if fwd { (s, t) } else { (t, s) };
                let (pa, pb) = (fixed(self, a), fixed(self, b));
                vec![self.add_entity(Geom::Arc { center: cc, start: pa, end: pb }, construction)]
            }
            (occt::CurveKind::Circle, Some((c, ax, r))) if ax.normalize_or_zero().dot(n).abs() < 1e-9 => {
                // seen edge-on: a line over the curve's extent along the plane
                let dir = n.cross(ax.normalize_or_zero()).normalize_or_zero();
                let u = ax.normalize_or_zero().cross(dir);
                let pts: Vec<DVec3> = if e.closed {
                    vec![c - dir * r, c + dir * r]
                } else {
                    let (s0, m0) = (e.start - c, e.midpoint - c);
                    let a0 = s0.dot(u).atan2(s0.dot(dir));
                    let sweep = {
                        let am = m0.dot(u).atan2(m0.dot(dir)) - a0;
                        let am = am.rem_euclid(std::f64::consts::TAU);
                        2.0 * if am > std::f64::consts::PI { am - std::f64::consts::TAU } else { am }
                    };
                    (0..=32).map(|k| c + (dir * (a0 + sweep * k as f64 / 32.0).cos() + u * (a0 + sweep * k as f64 / 32.0).sin()) * r).collect()
                };
                let d2 = to2(c + dir) - to2(c);
                let ts: Vec<f64> = pts.iter().map(|p| (to2(*p) - to2(c)).dot(d2)).collect();
                let (lo, hi) = ts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| (a.min(*t), b.max(*t)));
                if hi - lo < 1e-9 {
                    return vec![];
                }
                let (pa, pb) = (fixed(self, to2(c) + d2 * lo), fixed(self, to2(c) + d2 * hi));
                vec![self.add_entity(Geom::Line { a: pa, b: pb }, construction)]
            }
            _ => {
                let pts3: Vec<DVec3> = match samples {
                    Some(s) if s.len() >= 2 => s.to_vec(),
                    _ => vec![e.start, e.midpoint, e.end],
                };
                let mut pts: Vec<DVec2> = Vec::new();
                for q in pts3.iter().map(|p| to2(*p)) {
                    if pts.last().is_none_or(|l: &DVec2| l.distance(q) > 1e-9) {
                        pts.push(q);
                    }
                }
                if pts.len() < 2 {
                    return vec![];
                }
                let ids: Vec<PointId> = pts.iter().map(|q| fixed(self, *q)).collect();
                ids.windows(2).map(|w| self.add_entity(Geom::Line { a: w[0], b: w[1] }, construction)).collect()
            }
        }
    }
}
