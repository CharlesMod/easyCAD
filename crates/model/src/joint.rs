//! Parting joints: split a shelled enclosure into two halves with mating joint
//! geometry (tongue & groove, lip, O-ring gland, snap fits).
//!
//! Every joint is built from the **wall section ring** `R` at the split plane, using
//! uniform 2D offsets of that ring (`BRepOffsetAPI_MakeOffset`) and prisms, not by
//! sweeping a profile around the perimeter. Shrinking the ring by `m` on every
//! boundary gives a band of width `t − 2m` centred in the wall, whatever the outline
//! (fillets, concave corners, arcs, bosses or ribs merged into the wall). The band
//! ends cleanly where a cutout interrupts the wall.
//!
//! * `E` (envelope) = morphological closing of `R` with holes filled: the outline
//!   including the cavity, with cutouts (e.g. a USB slot) bridged.
//! * `G = closing(R) − R`: cutout gaps and concave-corner slivers (never filled).
//! * Rim thickening: `R' = (R ⊕ k) ∩ E − G` grows the wall inward only, by
//!   `k = rim_width − t`.

use std::sync::OnceLock;

use glam::DVec3;
use occt::{CurveKind, Edge, Frame, Shape};
use serde::{Deserialize, Serialize};

use crate::geom::Plane;
use crate::params::{Expr, Values};
use crate::process::{Fit, FitClass, ProcessProfile};

/// Which half of the split gets the male part of a joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Half {
    /// Below the plane (against its normal): keeps the body id.
    Lower,
    /// Above the plane: the new body.
    Upper,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rim {
    /// Total wall width at the joint (mm); the wall is thickened inward to this.
    pub width: Expr,
    /// Height of the thickened rim on each side of the plane.
    pub height: Expr,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SnapPositions {
    /// Evenly spaced along the outline.
    Count(u32),
    /// Arc-length fractions (0..1) along the outline.
    At(Vec<f64>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JointKind {
    TongueGroove {
        /// Tongue width (Pin role).
        width: Expr,
        height: Expr,
        /// Nominal clearance per side between tongue and groove walls.
        clearance: Expr,
        /// Extra groove depth beyond the tongue height (Gap role).
        extra_depth: Expr,
        tongue_on: Half,
        /// Chamfer on the tongue's top edges (0 = none).
        #[serde(default = "zero")]
        lead_in: Expr,
    },
    /// Step/overlap: an inner lip on one half sits in a recess of the other.
    Lip { width: Expr, height: Expr, clearance: Expr, lip_on: Half },
    /// Face-seal O-ring gland.
    ORingGland {
        /// Cord id from `data/joints/orings.json` (e.g. `AS568-1xx`, `M2.0`).
        cord: String,
        #[serde(default)]
        squeeze: Option<Expr>,
        /// Override the computed gland depth / width.
        #[serde(default)]
        depth: Option<Expr>,
        #[serde(default)]
        width: Option<Expr>,
        gland_on: Half,
        /// Optional compression rib on the mating half: (width, height).
        #[serde(default)]
        compression_rib: Option<(Expr, Expr)>,
    },
    SnapFit {
        positions: SnapPositions,
        beam_length: Expr,
        thickness: Expr,
        width: Expr,
        undercut: Expr,
        #[serde(default = "deg30")]
        insertion_deg: Expr,
        #[serde(default = "deg90")]
        retention_deg: Expr,
        /// Material id from `data/joints/snap_materials.json`.
        material: String,
        beam_on: Half,
        /// Clearance around hook and beam (Gap role).
        #[serde(default = "c02")]
        clearance: Expr,
        /// Keep hooks this far from corners and other features.
        #[serde(default = "two")]
        margin: Expr,
    },
}

fn zero() -> Expr {
    Expr::new("0")
}
fn deg30() -> Expr {
    Expr::new("30")
}
fn deg90() -> Expr {
    Expr::new("90")
}
fn c02() -> Expr {
    Expr::new("0.2")
}
fn two() -> Expr {
    Expr::new("2")
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JointCheck {
    pub name: String,
    pub value: f64,
    pub limit: f64,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ORingReport {
    pub cord: String,
    pub cord_dia: f64,
    pub gland_depth: f64,
    pub gland_width: f64,
    pub squeeze: f64,
    pub fill: f64,
    /// Smallest corner radius of the gland (0 = sharp corner).
    pub min_corner_radius: f64,
    pub suggested_depth: f64,
    pub suggested_width: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SnapCalc {
    /// Cantilever strain ε = 1.5·t·y / L².
    pub strain: f64,
    pub allowable: f64,
    pub pass: bool,
    /// Deflection force P = b·t²·E·ε / (6·L), N.
    pub deflection_force_n: f64,
    /// Mating force W = P·(μ + tan α)/(1 − μ·tan α), N.
    pub mating_force_n: f64,
    /// Shortest beam that keeps strain within the allowable.
    pub suggested_length: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SnapPlacement {
    pub requested: f64,
    /// Fraction actually used (moved away from corners / features), or `None`.
    pub placed: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct JointReport {
    pub kind: String,
    /// Measured wall thickness at the plane.
    pub wall_thickness: f64,
    /// Wall width used by the joint (after rim thickening).
    pub effective_wall: f64,
    pub rim_added: bool,
    /// Resulting horizontal clearance per side (tongue/lip/rib), after fits.
    pub clearance_per_side: Option<f64>,
    pub checks: Vec<JointCheck>,
    pub warnings: Vec<String>,
    pub oring: Option<ORingReport>,
    pub snap: Option<SnapCalc>,
    pub snaps: Vec<SnapPlacement>,
    /// Changes the engine made so the joint could be built (e.g. "rim added: 3.20 ×
    /// 3.00 mm so the groove fits"); shown to the user like warnings.
    pub adjustments: Vec<String>,
}

/// Why a joint was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum RefusalKind {
    /// The request cannot be built as asked (plane misses the cavity, a band that
    /// does not fit even with a rim, unknown cord...). The message says what to change.
    Infeasible,
    /// The algorithm or the kernel gave up on a request that should be buildable
    /// (2D offset failure, invalid result, no snap position found).
    Bailed,
}

/// Classify a joint error message (from `FeatureStatus::Error` of a `PartingJoint`).
pub fn refusal_kind(msg: &str) -> RefusalKind {
    const BAILED: [&str; 7] = ["offset failed", "offset region vanished", "came out invalid", "panicked", "OSD_", "no snap-fit position", "kernel"];
    const KERNEL_STEPS: [&str; 18] = [
        "section:", "section face:", "area:", "edges:", "envelope:", "cavity:", "closed ring:", "rim offset:", "rim clip:", "rim gaps:", "tongue:", "groove:", "lip:", "recess:",
        "gland:", "rib:", "outline:", "split:",
    ];
    if BAILED.iter().any(|k| msg.contains(k)) || KERNEL_STEPS.iter().any(|k| msg.starts_with(k)) {
        RefusalKind::Bailed
    } else {
        RefusalKind::Infeasible
    }
}

impl JointReport {
    fn check(&mut self, name: &str, value: f64, limit: f64, ok: bool) {
        self.checks.push(JointCheck { name: name.into(), value, limit, ok });
    }
}

/// Minimum skin left by joint cuts (floor, lid, groove walls).
pub const MIN_WALL: f64 = 0.4;

#[derive(Deserialize)]
struct CordRow {
    id: String,
    #[serde(default)]
    dia_in: Option<f64>,
    #[serde(default)]
    dia_mm: Option<f64>,
}

#[derive(Deserialize)]
struct ORingTable {
    squeeze_min: f64,
    squeeze_max: f64,
    squeeze_target: f64,
    fill_max: f64,
    fill_target: f64,
    min_corner_radius_factor: f64,
    cords: Vec<CordRow>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SnapMaterial {
    pub id: String,
    pub strain: f64,
    pub modulus_mpa: f64,
    pub friction: f64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize)]
struct SnapTable {
    materials: Vec<SnapMaterial>,
}

fn orings() -> &'static ORingTable {
    static T: OnceLock<ORingTable> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(include_str!("../data/joints/orings.json")).expect("orings.json"))
}

fn snap_materials() -> &'static SnapTable {
    static T: OnceLock<SnapTable> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(include_str!("../data/joints/snap_materials.json")).expect("snap_materials.json"))
}

/// Cord ids available for O-ring glands, with their diameters in mm.
pub fn cords() -> Vec<(String, f64)> {
    orings().cords.iter().filter_map(|c| Some((c.id.clone(), c.dia_mm.or(c.dia_in.map(|d| d * 25.4))?))).collect()
}

pub fn snap_material(id: &str) -> Option<SnapMaterial> {
    snap_materials().materials.iter().find(|m| m.id.eq_ignore_ascii_case(id)).cloned()
}

/// Cantilever snap-fit calculation (constant rectangular section).
pub fn snap_calc(l: f64, t: f64, b: f64, y: f64, insertion_deg: f64, m: &SnapMaterial) -> SnapCalc {
    let strain = 1.5 * t * y / (l * l);
    let p = b * t * t * m.modulus_mpa * strain / (6.0 * l);
    let ta = insertion_deg.to_radians().tan();
    let w = p * (m.friction + ta) / (1.0 - m.friction * ta).max(1e-6);
    SnapCalc {
        strain,
        allowable: m.strain,
        pass: strain <= m.strain,
        deflection_force_n: p,
        mating_force_n: w,
        suggested_length: (1.5 * t * y / m.strain).sqrt(),
    }
}

/// Suggested O-ring gland (depth, width) for a cord, squeeze and fill.
pub fn gland_for(cord: f64, squeeze: f64, fill: f64, rib_h: f64, rib_w: f64) -> (f64, f64) {
    let depth = cord * (1.0 - squeeze) + rib_h;
    let width = (std::f64::consts::PI * cord * cord / 4.0 / fill + rib_w * rib_h) / depth;
    (depth, width)
}

type JR<T> = Result<T, String>;

fn k<T>(r: Result<T, occt::Error>, what: &str) -> JR<T> {
    r.map_err(|e| format!("{what}: {}", e.0))
}

/// Translate planar region `face` along `n` to `z0` and extrude to `z1`.
fn prism(face: &Shape, n: DVec3, z0: f64, z1: f64) -> Result<Shape, occt::Error> {
    let (a, b) = if z0 <= z1 { (z0, z1) } else { (z1, z0) };
    face.translated(n * a)?.extrude(n * (b - a))
}

fn tidy(s: Shape) -> Shape {
    match s.unify() {
        Ok(u) if u.is_valid() => u,
        _ => s,
    }
}

fn solid_volume(s: &Shape) -> f64 {
    s.volume().unwrap_or(0.0)
}

/// Overlap volume of two solids (0 for touching or disjoint).
pub fn interference(a: &Shape, b: &Shape) -> f64 {
    a.common(b).map(|c| solid_volume(&c)).unwrap_or(0.0).max(0.0)
}

struct Ring {
    r: Shape,
    env: Shape,
    cavity: Shape,
    gaps: Option<Shape>,
    t: f64,
}

fn wall_ring(body: &Shape, plane: &Plane, closing: f64) -> JR<Ring> {
    let n = plane.normal.normalize();
    let all = k(body.section(plane.origin, n), "section")?;
    // keep the face holding the outer contour (largest filled area)
    let nf = all.count(occt::ShapeKind::Face);
    let mut best: Option<(f64, Shape)> = None;
    for i in 0..nf {
        let f = k(all.face_shape(i), "section face")?;
        let a = f.fill_holes().and_then(|x| x.area()).unwrap_or(0.0);
        if best.as_ref().is_none_or(|(ba, _)| a > *ba) {
            best = Some((a, f));
        }
    }
    let (_, r) = best.ok_or("the plane does not cut the wall")?;
    let area = k(r.area(), "area")?;
    let perim: f64 = k(r.edges(), "edges")?.iter().map(|e| e.length).sum();
    let t = 2.0 * area / perim.max(1e-9);
    let filled = k(r.fill_holes(), "envelope")?;
    let filled_a = filled.area().unwrap_or(0.0);
    let (env, cavity, gaps) = if filled_a > area * 1.2 {
        // an unbroken wall: the ring's holes are the cavity, nothing to bridge
        let cavity = k(filled.cut(&r), "cavity")?;
        (filled, cavity, None)
    } else {
        // a broken ring (wall cutout) or a plane through the floor/lid: bridge
        // cutouts with a morphological closing
        let rho = closing.max(12.0);
        let none = || "the plane passes through the floor or lid (no cavity at this height)".to_string();
        // closing done with single-boundary offsets (robust): outline and cavity separately
        let g = k(r.offset_region(rho), "closing (2D offset failed)")?;
        let g_filled = k(g.fill_holes(), "envelope")?;
        let env = k(g_filled.offset_region(-rho), "closing (2D offset failed)")?;
        let hole = g_filled.cut(&g).map_err(|_| none())?;
        if hole.area().unwrap_or(0.0) < 1e-6 || env.area().unwrap_or(0.0) < area * 1.2 {
            return Err(none());
        }
        let cavity = k(hole.offset_region(rho), "cavity")?;
        let c = k(env.cut(&cavity), "closed ring")?;
        // keep only gap pieces that reach the outside (cutouts, outer concave
        // corners); slivers in the cavity corners are not gaps
        let skin = env.offset_region(-0.05).ok().and_then(|inner| env.cut(&inner).ok());
        let mut keep = Vec::new();
        if let (Ok(g), Some(skin)) = (c.cut(&r), skin) {
            for i in 0..g.count(occt::ShapeKind::Face) {
                if let Ok(piece) = g.face_shape(i) {
                    let touches = piece.common(&skin).and_then(|x| x.area()).unwrap_or(0.0) > 1e-9;
                    if touches && piece.area().unwrap_or(0.0) > 1e-6 {
                        keep.push(piece);
                    }
                }
            }
        }
        let gaps = match keep.len() {
            0 => None,
            1 => keep.pop(),
            _ => Shape::compound(&keep.iter().collect::<Vec<_>>()).ok(),
        };
        // the true cavity (sharp where the walls are sharp): outline minus wall minus gaps
        let mut true_cav = k(env.cut(&r), "cavity")?;
        if let Some(gp) = &gaps {
            true_cav = k(true_cav.cut(gp), "cavity")?;
        }
        let cavity = if true_cav.area().unwrap_or(0.0) > 1e-6 { true_cav } else { cavity };
        (env, cavity, gaps)
    };
    if cavity.area().unwrap_or(0.0) < 1e-6 {
        return Err("the plane passes through the floor or lid (no cavity at this height)".into());
    }
    Ok(Ring { r, env, cavity, gaps, t })
}

/// Evaluated joint inputs.
pub struct JointInput<'a> {
    pub body: &'a Shape,
    pub plane: Plane,
    pub kind: &'a JointKind,
    pub rim: Option<(f64, f64)>,
    pub class: FitClass,
    pub closing: f64,
    pub profile: &'a ProcessProfile,
    pub vals: &'a Values,
}

/// Build the two halves. Returns `(lower, upper, report)`; errors carry a reason.
///
/// When the only blocker is a band that does not fit the wall (and the message
/// names the rim width it needs), the joint is retried once with that rim
/// (added or widened) and the change is reported in `JointReport::adjustments`.
pub fn build(inp: &JointInput) -> JR<(Shape, Shape, JointReport)> {
    let first = build_once(inp);
    let Err(e) = &first else { return first };
    let Some(need) = e.split("rim width ≥ ").nth(1).and_then(|r| r.split(|c: char| !(c.is_ascii_digit() || c == '.')).next()).and_then(|v| v.parse::<f64>().ok()) else {
        return first;
    };
    let ev = |x: &Expr| x.eval(inp.vals).unwrap_or(0.0);
    // the rim must be taller than the deepest cut into it
    let depth = match inp.kind {
        JointKind::TongueGroove { height, extra_depth, .. } => ev(height) + ev(extra_depth),
        JointKind::Lip { height, clearance, .. } => ev(height) + ev(clearance),
        JointKind::ORingGland { cord, depth, .. } => depth.as_ref().map(ev).unwrap_or_else(|| cords().into_iter().find(|(id, _)| id == cord).map(|(_, d)| d).unwrap_or(2.0)),
        JointKind::SnapFit { .. } => return first,
    };
    let w = need + 0.2;
    let h = inp.rim.map(|r| r.1).unwrap_or(0.0).max(depth + 1.0);
    let retry = JointInput { rim: Some((w, h)), ..*inp };
    match build_once(&retry) {
        Ok((lo, up, mut rep)) => {
            rep.adjustments.push(match inp.rim {
                Some((w0, _)) if w0 > rep.wall_thickness => format!("rim widened from {w0:.2} to {w:.2} mm (height {h:.2}) so the joint fits ({e})"),
                _ => format!("rim added: {w:.2} mm wide, {h:.2} mm tall, so the joint fits the {:.2} mm wall ({e})", rep.wall_thickness),
            });
            Ok((lo, up, rep))
        }
        Err(e2) => Err(format!("{e} (an automatic {w:.2} mm rim did not help either: {e2})")),
    }
}

fn build_once(inp: &JointInput) -> JR<(Shape, Shape, JointReport)> {
    let plane = inp.plane;
    let n = plane.normal.normalize();
    let ev = |e: &Expr| e.eval(inp.vals).map_err(|err| format!("`{}`: {err}", e.0));
    let evf = |e: &Expr, f: Fit| e.eval_or(inp.vals, f).map_err(|err| format!("`{}`: {err}", e.0));
    let res = |v: f64, f: Fit| inp.profile.resolve(v, f);

    let prof = std::env::var("CW_PROF").is_ok();
    let t0 = std::time::Instant::now();
    let lap = |what: &str| {
        if prof {
            eprintln!("  joint {what}: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
        }
    };
    // the split and the wall-ring analysis are independent: run them concurrently
    let (split, ring) = std::thread::scope(|sc| {
        let h = sc.spawn(|| inp.body.split(plane.origin, n));
        let ring = wall_ring(inp.body, &plane, inp.closing);
        (h.join().unwrap_or_else(|_| Err(occt::Error("split worker panicked".into()))), ring)
    });
    let (lower, upper) = k(split, "split")?;
    lap("split");
    let (mut lower, mut upper) = match (lower, upper) {
        (Some(l), Some(u)) => (l, u),
        _ => return Err("the joint plane does not cut the body".into()),
    };
    let ring = ring?;
    lap("ring");
    let mut rep = JointReport { wall_thickness: ring.t, effective_wall: ring.t, ..Default::default() };

    // available material on each side of the plane (floor / lid limits)
    // (cheap: extremes along the normal from vertices and edges; the exact box of
    // shelled halves with offset surfaces costs ~100 ms each)
    let extent = |s: &Shape, sign: f64| -> f64 {
        match s.extent_along(plane.origin, n) {
            Ok((lo, hi)) => (if sign > 0.0 { hi } else { -lo }).max(0.0),
            Err(_) => 0.0,
        }
    };
    // booleans are deferred: one fuse + one cut per half at the end
    let mut pend = Pending::default();
    let up_room = extent(&upper, 1.0);
    let down_room = extent(&lower, -1.0);
    let room = |h: Half| if h == Half::Upper { up_room } else { down_room };
    // side sign: +1 = above the plane (upper), -1 = below (lower)
    let side = |h: Half| if h == Half::Upper { 1.0 } else { -1.0 };
    let other = |h: Half| if h == Half::Upper { Half::Lower } else { Half::Upper };

    // rim thickening
    let mut rr = ring.r.clone();
    if let Some((w, h)) = inp.rim {
        if w > ring.t + 1e-6 {
            let grown = k(ring.r.offset_region(w - ring.t), "rim offset")?;
            let mut r2 = k(grown.common(&ring.env), "rim clip")?;
            if let Some(g) = &ring.gaps {
                r2 = k(r2.cut(g), "rim gaps")?;
            }
            // a snap hook must engage rim material: extend the window half's rim
            let snap_need = match inp.kind {
                JointKind::SnapFit { beam_length, clearance, beam_on, .. } => {
                    Some((other(*beam_on), ev(beam_length)? + evf(clearance, Fit::gap(inp.class))? + 1.0))
                }
                _ => None,
            };
            for half in [Half::Lower, Half::Upper] {
                let want = match snap_need {
                    Some((hf, need)) if hf == half && need > h => {
                        rep.warnings.push(format!("rim extended to {need:.2} mm on the window half so the snap hooks engage"));
                        need
                    }
                    _ => h,
                };
                let hh = want.min(room(half) - MIN_WALL).max(0.0);
                if hh > 1e-6 {
                    let p = k(prism(&r2, n, 0.0, side(half) * hh), "rim prism")?;
                    pend.adds[half as usize].push(p);
                }
            }
            rr = r2;
            rep.rim_added = true;
            rep.effective_wall = w;
        } else {
            rep.warnings.push(format!("rim width {w:.2} ≤ wall {:.2}: no thickening needed", ring.t));
        }
    }
    lap("rim");
    let t = rep.effective_wall;
    let band = |width: f64, what: &str| -> JR<Shape> {
        if width <= 0.05 {
            return Err(format!("{what} width is {width:.2} mm after process fits; widen it or use a tighter fit class"));
        }
        let m = (t - width) / 2.0;
        if m <= 0.0 {
            return Err(format!("{what} ({width:.2} mm) does not fit in the {t:.2} mm wall; add rim thickening (rim width ≥ {:.2})", width + 2.0 * MIN_WALL));
        }
        rr.offset_region(-m).map_err(|e| format!("{what} ({width:.2} mm in the {t:.2} mm wall): 2D offset failed ({})", e.0))
    };
    let add = |half: Half, solid: Shape, pend: &mut Pending| -> JR<()> {
        pend.adds[half as usize].push(solid);
        Ok(())
    };
    let cut = |half: Half, tool: &Shape, pend: &mut Pending, what: &str, depth: f64| -> JR<()> {
        let avail = room(half);
        if depth + MIN_WALL > avail {
            return Err(format!("{what} depth {depth:.2} mm needs {:.2} mm of material but only {avail:.2} mm is available on that side of the plane (too close to the floor/lid)", depth + MIN_WALL));
        }
        pend.cuts[half as usize].push(tool.clone());
        Ok(())
    };

    match inp.kind {
        JointKind::TongueGroove { width, height, clearance, extra_depth, tongue_on, lead_in } => {
            rep.kind = "tongue & groove".into();
            let tw = evf(width, Fit::pin(inp.class))?;
            let cl = ev(clearance)?;
            let th = ev(height)?;
            let gw = res(ev(width)? + 2.0 * cl, Fit::hole(inp.class));
            let depth = th + evf(extra_depth, Fit::gap(inp.class))?;
            let margin = (t - gw) / 2.0;
            rep.check("groove wall", margin, MIN_WALL, margin >= MIN_WALL);
            if margin < MIN_WALL {
                return Err(format!(
                    "groove ({gw:.2} mm) leaves {margin:.2} mm walls in the {t:.2} mm wall (min {MIN_WALL}); add rim thickening (rim width ≥ {:.2}) or narrow the tongue",
                    gw + 2.0 * MIN_WALL
                ));
            }
            let tband = band(tw, "tongue")?;
            lap("tongue band");
            let li = ev(lead_in)?.min(tw * 0.45).min(th * 0.9);
            // the tongue as one or two solids, fused into the half with everything else
            let tongue: Vec<Shape> = if li > 1e-3 {
                // lead-in: the top `li` of the tongue tapers 45° on both sides. Built
                // directly as a ruled taper on top of the straight part (both go to the
                // half's one fuse: no boolean here); a raw chamfer is the fallback, a
                // step next, and no lead-in (with a warning) the last resort.
                let dirv = n * -side(*tongue_on);
                let body = k(prism(&tband, n, 0.0, -side(*tongue_on) * (th - li)), "tongue")?;
                let band_a = tband.area().unwrap_or(0.0);
                let full_v = band_a * th;
                let plausible = |s: &Shape| s.is_valid() && s.count(occt::ShapeKind::Solid) == 1 && s.volume().is_ok_and(|v| v < full_v && v > full_v * (th - li) / th * 0.999);
                // the taper alone (a ruled loft to the band inset by `li`; OCCT's draft
                // algorithm hangs on some sections): smaller than its straight prism
                let drafted = tband
                    .translated(dirv * (th - li))
                    .and_then(|f| f.taper(dirv * li, li))
                    .ok()
                    .filter(|d| d.volume().is_ok_and(|v| v > 1e-9 && v < band_a * li));
                lap(if drafted.is_some() { "tongue taper" } else { "tongue taper failed" });
                match drafted {
                    Some(top) => vec![body, top],
                    None => {
                        let full = k(prism(&tband, n, 0.0, -side(*tongue_on) * th), "tongue")?;
                        let top = if *tongue_on == Half::Lower { n } else { -n };
                        let sel = crate::select::EdgeSelector::OfFace(crate::select::FaceSelector::Extreme { normal: top, which: crate::select::Extremum::Max });
                        let chamfered = sel.resolve(&full).ok().and_then(|ids| {
                            let sizes = vec![li; ids.len()];
                            full.chamfer_raw(&ids, &sizes).ok().filter(|s| plausible(s))
                        });
                        // a step instead of a taper: the top `li` is `li` narrower
                        let stepped = || -> Option<Shape> {
                            let narrow = band(tw - li, "tongue lead-in").ok()?;
                            let top = prism(&narrow, n, -side(*tongue_on) * (th - li), -side(*tongue_on) * th).ok()?;
                            body.fuse(&top).ok().map(tidy).filter(|s| plausible(s))
                        };
                        vec![match chamfered {
                            Some(t) => t,
                            None => match stepped() {
                                Some(t) => {
                                    rep.adjustments.push(format!("lead-in built as a {li:.2} mm step (a corner of the tongue is tighter than the {li:.2} mm taper)"));
                                    t
                                }
                                None => {
                                    rep.warnings.push(format!("lead-in {li:.2} mm could not be built on this tongue; the tongue has square edges"));
                                    full
                                }
                            },
                        }]
                    }
                }
            } else {
                vec![k(prism(&tband, n, 0.0, -side(*tongue_on) * th), "tongue")?]
            };
            let groove = k(prism(&band(gw, "groove")?, n, 0.0, -side(*tongue_on) * depth), "groove")?;
            let g_half = other(*tongue_on);
            for t in tongue {
                add(*tongue_on, t, &mut pend)?;
            }
            cut(g_half, &groove, &mut pend, "groove", depth)?;
            rep.clearance_per_side = Some((gw - tw) / 2.0);
        }
        JointKind::Lip { width, height, clearance, lip_on } => {
            rep.kind = "lip".into();
            let lw = evf(width, Fit::pin(inp.class))?;
            let cl = ev(clearance)?;
            let lh = ev(height)?;
            let rw = res(ev(width)? + cl, Fit::hole(inp.class));
            let depth = lh + res(cl, Fit::gap(inp.class));
            let outer = t - rw;
            rep.check("outer wall at recess", outer, MIN_WALL, outer >= MIN_WALL);
            if outer < MIN_WALL {
                return Err(format!("lip recess ({rw:.2} mm) leaves {outer:.2} mm of outer wall (min {MIN_WALL}); add rim thickening (rim width ≥ {:.2}) or narrow the lip", rw + MIN_WALL + 0.05));
            }
            // the part of the (rimmed) wall within `w` of the cavity
            let cav = if rep.rim_added { k(ring.cavity.cut(&rr), "cavity")? } else { ring.cavity.clone() };
            if cav.area().unwrap_or(0.0) < 1e-6 {
                return Err("the rim fills the whole cavity at the plane; use a narrower rim".into());
            }
            let inner = |w: f64, what: &str| -> JR<Shape> {
                let e = cav.offset_region(w).map_err(|e| format!("{what}: 2D offset of the cavity failed ({})", e.0))?;
                k(rr.common(&e), what)
            };
            let lip = k(prism(&inner(lw, "lip")?, n, 0.0, -side(*lip_on) * lh), "lip")?;
            let recess = k(prism(&inner(rw, "recess")?, n, 0.0, -side(*lip_on) * depth), "recess")?;
            add(*lip_on, lip, &mut pend)?;
            cut(other(*lip_on), &recess, &mut pend, "lip recess", depth)?;
            rep.clearance_per_side = Some(rw - lw);
        }
        JointKind::ORingGland { cord, squeeze, depth, width, gland_on, compression_rib } => {
            rep.kind = "O-ring gland".into();
            let tab = orings();
            let d = cords().into_iter().find(|(id, _)| id == cord).map(|(_, d)| d).ok_or_else(|| format!("unknown O-ring cord `{cord}`"))?;
            let sq_t = squeeze.as_ref().map(ev).transpose()?.unwrap_or(tab.squeeze_target);
            let (rib_w, rib_h) = match compression_rib {
                Some((w, h)) => (evf(w, Fit::pin(inp.class))?, ev(h)?),
                None => (0.0, 0.0),
            };
            let (sd, sw) = gland_for(d, sq_t, tab.fill_target, rib_h, rib_w);
            let gd = depth.as_ref().map(ev).transpose()?.unwrap_or(sd);
            let gw_nom = width.as_ref().map(ev).transpose()?.unwrap_or(sw);
            let gw = res(gw_nom, Fit::hole(inp.class));
            let squeeze_v = (d - (gd - rib_h)) / d;
            let fill = std::f64::consts::PI * d * d / 4.0 / (gw * gd - rib_w * rib_h);
            let margin = (t - gw) / 2.0;
            rep.check("gland wall", margin, MIN_WALL, margin >= MIN_WALL);
            if margin < MIN_WALL {
                return Err(format!("gland ({gw:.2} mm) does not fit in the {t:.2} mm wall; add rim thickening (rim width ≥ {:.2})", gw + 2.0 * MIN_WALL));
            }
            let gland_face = band(gw, "gland")?;
            let min_r = min_corner_radius(&gland_face);
            rep.check("squeeze", squeeze_v, tab.squeeze_max, (tab.squeeze_min..=tab.squeeze_max).contains(&squeeze_v));
            rep.check("gland fill", fill, tab.fill_max, fill <= tab.fill_max);
            if !(tab.squeeze_min..=tab.squeeze_max).contains(&squeeze_v) {
                rep.warnings.push(format!(
                    "squeeze {:.0} % outside {:.0}–{:.0} %: suggest gland depth {sd:.2} × width {sw:.2} mm for {:.0} % squeeze",
                    squeeze_v * 100.0,
                    tab.squeeze_min * 100.0,
                    tab.squeeze_max * 100.0,
                    sq_t.clamp(tab.squeeze_min, tab.squeeze_max) * 100.0
                ));
            }
            if fill > tab.fill_max {
                rep.warnings.push(format!("gland fill {:.0} % above {:.0} %: suggest width {sw:.2} mm", fill * 100.0, tab.fill_max * 100.0));
            }
            let min_bend = tab.min_corner_radius_factor * d;
            rep.check("min corner radius", min_r, min_bend, min_r >= min_bend);
            if min_r < min_bend {
                rep.warnings.push(format!("gland corner radius {min_r:.2} mm is below the {min_bend:.2} mm bend guidance for a {d:.2} mm cord (round the outline corners)"));
            }
            let gland = k(prism(&gland_face, n, 0.0, side(*gland_on) * gd), "gland")?;
            cut(*gland_on, &gland, &mut pend, "gland", gd)?;
            if rib_h > 0.0 {
                let rib = k(prism(&band(rib_w, "compression rib")?, n, 0.0, side(*gland_on) * rib_h), "rib")?;
                add(other(*gland_on), rib, &mut pend)?;
                rep.clearance_per_side = Some((gw - rib_w) / 2.0);
            }
            rep.oring = Some(ORingReport {
                cord: cord.clone(),
                cord_dia: d,
                gland_depth: gd,
                gland_width: gw,
                squeeze: squeeze_v,
                fill,
                min_corner_radius: min_r,
                suggested_depth: sd,
                suggested_width: sw,
            });
        }
        JointKind::SnapFit { positions, beam_length, thickness, width, undercut, insertion_deg, retention_deg, material, beam_on, clearance, margin } => {
            rep.kind = "snap fit".into();
            let mat = snap_material(material).ok_or_else(|| format!("unknown snap-fit material `{material}`"))?;
            let l = ev(beam_length)?;
            let tb = ev(thickness)?;
            let b = ev(width)?;
            let y_nom = ev(undercut)?;
            let y = res(y_nom, Fit::pin(inp.class)).max(0.05);
            let ins = ev(insertion_deg)?.clamp(5.0, 80.0);
            let ret = ev(retention_deg)?.clamp(20.0, 90.0);
            let c = evf(clearance, Fit::gap(inp.class))?;
            let mg = ev(margin)?;
            let calc = snap_calc(l, tb, b, y_nom, ins, &mat);
            rep.check("beam strain", calc.strain, calc.allowable, calc.pass);
            if !calc.pass {
                rep.warnings.push(format!(
                    "strain {:.2} % exceeds the {:.2} % allowed for {}: lengthen the beam to ≥ {:.2} mm",
                    calc.strain * 100.0,
                    calc.allowable * 100.0,
                    mat.id,
                    calc.suggested_length
                ));
            }
            rep.snap = Some(calc);
            let s_side = side(*beam_on);
            let nb = n * -s_side; // beams stick out of their half towards the other
            let zr = (ins.to_radians().tan(), ret.to_radians().tan());
            let z2 = l - y / zr.0;
            let z1 = z2 - y / zr.1;
            if z1 < 0.5 {
                return Err(format!("beam length {l:.2} mm is too short for a {y:.2} mm hook at {ins:.0}°/{ret:.0}°"));
            }
            if l + c + MIN_WALL > room(other(*beam_on)) {
                return Err(format!(
                    "snap beam ({l:.2} mm + {c:.2} clearance + {MIN_WALL} skin) does not fit in the other half ({:.2} mm tall)",
                    room(other(*beam_on))
                ));
            }
            let root = (l * 0.5).min(room(*beam_on) - MIN_WALL).max(1.0);
            let env_outer = &ring.env;
            let perim = k(env_outer.outer_wire_length(), "outline")?;
            let req: Vec<f64> = match positions {
                SnapPositions::Count(cnt) => (0..*cnt).map(|i| (i as f64 + 0.5) / *cnt as f64).collect(),
                SnapPositions::At(v) => v.clone(),
            };
            let half_span = b / 2.0 + c + mg;
            let mut placed_frac: Vec<f64> = Vec::new();
            let mut beams = Vec::new();
            let mut windows = Vec::new();
            for s0 in req {
                let mut found = None;
                let mut why = String::from("no straight wall section free of features nearby");
                let step = (half_span / perim).max(1e-4);
                // walk the whole outline if needed (both directions)
                let max_k = ((0.5 / step).ceil() as usize + 1).clamp(40, 400);
                'search: for kk in 0..max_k {
                    for sgn in [1.0, -1.0] {
                        if kk == 0 && sgn < 0.0 {
                            continue;
                        }
                        let s = (s0 + sgn * kk as f64 * step).rem_euclid(1.0);
                        if placed_frac.iter().any(|q| {
                            let d = (q - s).abs();
                            d.min(1.0 - d) * perim < 2.0 * half_span
                        }) {
                            continue;
                        }
                        let dims = SnapDims { t_eff: t, b, tb, l, y, z1, z2, c, mg, root };
                        match snap_geometry(&ring, &rr, env_outer, perim, s, n, nb, &dims) {
                            Ok((beam, window, keepout_lo, keepout_up, fr)) => {
                                let (lo_s, up_s) = if *beam_on == Half::Lower { (&lower, &upper) } else { (&upper, &lower) };
                                let blocked = |ko_lo: &Shape, ko_up: &Shape| interference(ko_lo, lo_s) > 1e-6 || interference(ko_up, up_s) > 1e-6;
                                if !blocked(&keepout_lo, &keepout_up) {
                                    found = Some((s, beam, window, None));
                                    break 'search;
                                }
                                // fit the beam to the cavity: the lid/floor (or a boss
                                // under the beam) is closer than the beam or its root
                                let free_up = fr.free(up_s, nb, room(other(*beam_on)));
                                let free_lo = fr.free(lo_s, -nb, room(*beam_on));
                                let l2 = l.min(free_up - c - 0.1);
                                let root2 = root.min(free_lo - 0.1);
                                let l_min = 0.5 + y / zr.0 + y / zr.1 + 0.2;
                                why = format!("blocked by a boss, rib, the lid/floor or another feature (free height {free_up:.2} mm above, {free_lo:.2} mm below)");
                                if l2 >= l_min && l2 >= 0.5 * l && root2 >= 1.0 && (l2 < l - 1e-6 || root2 < root - 1e-6) {
                                    let z2b = l2 - y / zr.0;
                                    let d2 = SnapDims { l: l2, z2: z2b, z1: z2b - y / zr.1, root: root2, ..dims };
                                    if let Ok((beam, window, ko_lo, ko_up, _)) = snap_geometry(&ring, &rr, env_outer, perim, s, n, nb, &d2) {
                                        if !blocked(&ko_lo, &ko_up) {
                                            found = Some((s, beam, window, Some((l2, root2))));
                                            break 'search;
                                        }
                                    }
                                }
                                continue;
                            }
                            Err(e) => why = e,
                        }
                    }
                }
                match found {
                    Some((s, beam, window, fit)) => {
                        if let Some((l2, root2)) = fit {
                            let mut msg = format!("snap at {:.0} %: ", s * 100.0);
                            if l2 < l - 1e-6 {
                                msg += &format!("beam shortened to {l2:.2} mm (the far side is that close)");
                                let c2 = snap_calc(l2, tb, b, y_nom, ins, &mat);
                                if !c2.pass {
                                    rep.warnings.push(format!("shortened snap beam strain {:.2} % exceeds {:.2} %", c2.strain * 100.0, c2.allowable * 100.0));
                                }
                            }
                            if root2 < root - 1e-6 {
                                if l2 < l - 1e-6 {
                                    msg += ", ";
                                }
                                msg += &format!("root shortened to {root2:.2} mm");
                            }
                            rep.adjustments.push(msg);
                        }
                        placed_frac.push(s);
                        beams.push(beam);
                        windows.push(window);
                        rep.snaps.push(SnapPlacement { requested: s0, placed: Some(s), reason: None });
                    }
                    None => rep.snaps.push(SnapPlacement { requested: s0, placed: None, reason: Some(why) }),
                }
            }
            if beams.is_empty() {
                let mut why: Vec<String> = rep.snaps.iter().filter_map(|s| s.reason.clone()).collect();
                why.dedup();
                // the whole outline was searched: without a blocker it is the outline
                // itself (no straight run long enough, wall too thin for the hook)
                if why.iter().all(|w| !w.starts_with("blocked")) {
                    return Err(format!("snap fits do not fit this outline ({:.1} mm of straight wall needed per beam): {}", 2.0 * half_span, why.join("; ")));
                }
                return Err(format!("no snap-fit position could be placed: {}", why.join("; ")));
            }
            for (bm, w) in beams.into_iter().zip(windows) {
                add(*beam_on, bm, &mut pend)?;
                pend.cuts[other(*beam_on) as usize].push(w);
            }
            rep.clearance_per_side = Some(c);
        }
    }
    lap("kind geometry");
    let [lo_ops, up_ops] = pend.take();
    // the halves are independent: finish them concurrently
    let (lo, up) = std::thread::scope(|sc| {
        let h = sc.spawn(|| apply_pending(upper, up_ops, "upper"));
        let lo = apply_pending(lower, lo_ops, "lower");
        (lo, h.join().unwrap_or_else(|_| Err("joint worker panicked".to_string())))
    });
    lower = lo?;
    upper = up?;
    lap("validate");
    Ok((lower, upper, rep))
}

#[derive(Default)]
struct Pending {
    /// Indexed by `Half as usize` (Lower = 0, Upper = 1).
    adds: [Vec<Shape>; 2],
    cuts: [Vec<Shape>; 2],
}

impl Pending {
    fn take(self) -> [(Vec<Shape>, Vec<Shape>); 2] {
        let [al, au] = self.adds;
        let [cl, cu] = self.cuts;
        [(al, cl), (au, cu)]
    }
}

/// Fuse `adds` into `s`, cut `cuts`, unify; the result is checked valid (one
/// `BRepCheck` per half, done here so both halves check concurrently).
fn apply_pending(mut s: Shape, (adds, cuts): (Vec<Shape>, Vec<Shape>), name: &str) -> JR<Shape> {
    let prof = std::env::var("CW_PROF").is_ok();
    let t = std::time::Instant::now();
    let lap = |w: &str| {
        if prof {
            eprintln!("    apply {name} {w}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0)
        }
    };
    if !adds.is_empty() {
        let mut all = vec![&s];
        all.extend(adds.iter());
        s = k(Shape::fuse_all(&all), "joint fuse")?;
        lap("fuse");
    }
    if !cuts.is_empty() {
        let tool = if cuts.len() == 1 { cuts[0].clone() } else { k(Shape::fuse_all(&cuts.iter().collect::<Vec<_>>()), "joint tools")? };
        s = k(s.cut(&tool), "joint cut")?;
        lap("cut");
    }
    let u = s.unify();
    lap("unify");
    let r = match u {
        Ok(u) if u.is_valid() => u,
        _ if s.is_valid() => s,
        _ => return Err(format!("the {name} half came out invalid; joint not applied")),
    };
    lap("tidy");
    if r.count(occt::ShapeKind::Solid) == 0 {
        return Err(format!("the {name} half came out invalid; joint not applied"));
    }
    Ok(r)
}

struct SnapDims {
    t_eff: f64,
    b: f64,
    tb: f64,
    l: f64,
    y: f64,
    z1: f64,
    z2: f64,
    c: f64,
    mg: f64,
    root: f64,
}

/// Box in the local snap frame: `a` along the wall tangent, `u` inward from the
/// outer surface, `z` along `nb` from the plane.
fn local_box(p: DVec3, tan: DVec3, inw: DVec3, nb: DVec3, a: (f64, f64), u: (f64, f64), z: (f64, f64)) -> Result<Shape, occt::Error> {
    let q = |aa: f64, uu: f64| p + tan * aa + inw * uu + nb * z.0;
    let pts = [q(a.0, u.0), q(a.1, u.0), q(a.1, u.1), q(a.0, u.1)];
    let e: Vec<Edge> = (0..4).map(|i| Edge::Line(pts[i], pts[(i + 1) % 4])).collect();
    Shape::face(&Frame { origin: pts[0], x_dir: tan, normal: nb }, &e, &[])?.extrude(nb * (z.1 - z.0))
}

#[allow(clippy::too_many_arguments)]
fn snap_geometry(ring: &Ring, rr: &Shape, env: &Shape, perim: f64, s: f64, n: DVec3, nb: DVec3, d: &SnapDims) -> JR<(Shape, Shape, Shape, Shape, SnapFrame)> {
    let (p, tan) = k(env.outer_wire_point(s), "outline point")?;
    let tan = (tan - n * tan.dot(n)).normalize();
    // straight-wall check: neighbours along the outline stay on the tangent line
    let span = d.b / 2.0 + d.c + d.mg;
    for sg in [-1.0, 1.0] {
        let (q, tq) = k(env.outer_wire_point(s + sg * span / perim), "outline point")?;
        let off = (q - p) - tan * (q - p).dot(tan);
        if off.length() > 0.05 || tq.normalize().dot(tan).abs() < 0.999 {
            return Err("too close to a corner".into());
        }
    }
    let mut inw = n.cross(tan);
    let probe = p + inw * (ring.t * 0.5).max(0.2);
    if !env.region_contains(probe).unwrap_or(false) {
        inw = -inw;
    }
    // local wall thickness of the (rimmed) ring along the inward direction, at the
    // centre and both ends of the beam span: a boss or rib merged into the wall
    // shows up as a locally thicker wall
    let thickness_at = |q: DVec3| -> Option<f64> {
        let step = 0.05;
        let mut uu = step / 2.0;
        let mut last = None;
        while uu < 50.0 {
            if rr.region_contains(q + inw * uu).unwrap_or(false) {
                last = Some(uu + step / 2.0);
            } else if last.is_some() {
                break;
            }
            uu += step;
        }
        last
    };
    let Some(tw) = thickness_at(p) else { return Err("no wall at this position (cutout?)".into()) };
    for sg in [-1.0, 1.0] {
        match thickness_at(p + tan * sg * span) {
            Some(t2) if (t2 - tw).abs() <= 0.3 => {}
            Some(_) => return Err("a boss or rib merges with the wall here".into()),
            None => return Err("too close to a wall cutout".into()),
        }
    }
    if tw > d.t_eff + 0.3 {
        return Err("a boss or rib merges with the wall here".into());
    }
    let gap = d.c;
    let ub = tw + gap; // beam outer face
    if d.y > ub + 0.5 {
        return Err("undercut deeper than the wall".into());
    }
    let ha = d.b / 2.0;
    let bx = |a: (f64, f64), u: (f64, f64), z: (f64, f64)| local_box(p, tan, inw, nb, a, u, z).map_err(|e| e.0);
    let root = bx((-ha, ha), (tw, ub + d.tb), (-d.root, 0.0))?;
    let beam = bx((-ha, ha), (ub, ub + d.tb), (0.0, d.l))?;
    // hook: triangle (ub, z1) → (ub − y, z2) → (ub, L), extruded along the tangent
    let hp = |uq: f64, zq: f64| p + tan * -ha + inw * uq + nb * zq;
    let tri = [hp(ub, d.z1), hp(ub - d.y, d.z2), hp(ub, d.l)];
    let e: Vec<Edge> = (0..3).map(|i| Edge::Line(tri[i], tri[(i + 1) % 3])).collect();
    let hook = Shape::face(&Frame { origin: tri[0], x_dir: inw, normal: tan }, &e, &[]).and_then(|f| f.extrude(tan * d.b)).map_err(|e| e.0)?;
    let solid = Shape::fuse_all(&[&root, &beam, &hook]).map_err(|e| e.0)?;
    let solid = tidy(solid);
    // window through the other half's wall + pocket for the beam
    let wa = (-(ha + d.c), ha + d.c);
    let window = bx(wa, (-0.5, ub + d.tb + d.c), (d.z1 - d.c, d.l + d.c))?;
    let pocket = bx(wa, (tw - 0.001, ub + d.tb + d.c), (-0.001, d.l + d.c))?;
    let tool = window.fuse(&pocket).map_err(|e| e.0)?;
    // keep-outs: space the beam needs, away from the wall it bonds to
    let ko_lo = bx((-ha - d.mg, ha + d.mg), (tw + 0.05, ub + d.tb + d.mg), (-d.root, -0.05))?;
    let ko_up = bx((-ha - d.mg, ha + d.mg), (tw + 0.05, ub + d.tb + d.mg), (0.05, d.l + d.c))?;
    let frame = SnapFrame { p, tan, inw, u: (tw + 0.05, ub + d.tb + d.mg), a: (-ha - d.mg, ha + d.mg) };
    Ok((solid, tool, ko_lo, ko_up, frame))
}

/// Where a snap beam sits: outline point, tangent, inward direction, and the
/// keep-out footprint (inward `u` range, tangential `a` range).
struct SnapFrame {
    p: DVec3,
    tan: DVec3,
    inw: DVec3,
    u: (f64, f64),
    a: (f64, f64),
}

impl SnapFrame {
    /// Free height along `dir` above the footprint before `half` has material
    /// (up to `reach`).
    fn free(&self, half: &Shape, dir: DVec3, reach: f64) -> f64 {
        let Ok(tall) = local_box(self.p, self.tan, self.inw, dir, self.a, self.u, (0.05, reach.max(0.1))) else { return 0.0 };
        match half.common(&tall).and_then(|h| h.edges()) {
            Ok(es) => es.iter().flat_map(|e| [e.start, e.end]).map(|q| (q - self.p).dot(dir)).fold(reach, f64::min),
            Err(_) => 0.0,
        }
    }
}

/// Smallest corner radius of a planar region's boundary (0 if any sharp corner).
pub fn min_corner_radius(region: &Shape) -> f64 {
    let Ok(edges) = region.edges() else { return 0.0 };
    // free-form edges (B-spline / offset curves from shelled walls): the circle
    // through start, middle and end stands in for the curve at its ends
    let circ = |e: &occt::EdgeInfo| -> Option<(DVec3, DVec3, f64)> {
        if let Some(c) = e.circle {
            return Some(c);
        }
        if e.kind == CurveKind::Line {
            return None;
        }
        let (a, b, c) = (e.start, e.midpoint, e.end);
        let (ab, ac) = (b - a, c - a);
        let nrm = ab.cross(ac);
        if nrm.length_squared() < 1e-12 * ab.length_squared().max(1e-12) * ac.length_squared().max(1e-12) {
            return None; // straight
        }
        let center = a + (nrm.cross(ab) * ac.length_squared() + ac.cross(nrm) * ab.length_squared()) / (2.0 * nrm.length_squared());
        Some((center, nrm.normalize(), center.distance(a)))
    };
    let tangent_at = |e: &occt::EdgeInfo, p: DVec3| -> DVec3 {
        match circ(e) {
            Some((c, ax, _)) => ax.cross(p - c).normalize_or_zero(),
            None => e.tangent.normalize_or_zero(),
        }
    };
    let mut min_r = f64::INFINITY;
    for e in &edges {
        if let Some((_, _, r)) = circ(e) {
            min_r = min_r.min(r);
        }
    }
    for (i, a) in edges.iter().enumerate() {
        for b in edges.iter().skip(i + 1) {
            for pa in [a.start, a.end] {
                for pb in [b.start, b.end] {
                    if pa.distance(pb) < 1e-6 && tangent_at(a, pa).dot(tangent_at(b, pb)).abs() < 0.996 {
                        return 0.0;
                    }
                }
            }
        }
    }
    if min_r.is_finite() {
        min_r
    } else {
        f64::INFINITY
    }
}
