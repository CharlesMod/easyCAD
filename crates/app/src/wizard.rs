//! "New enclosure from PCB": pick an EasyEDA STEP or DXF, set clearance, wall,
//! height, corner radius, joint and process, and get an ordinary editable history:
//!
//! 1. sketch "Enclosure profile": the board outline (construction, fixed) offset by
//!    `clearance + wall`, sharp corners rounded with `corner_r`;
//! 2. extrude `height` → body "Base"; 3. fillet the top edges (`edge_r`);
//! 4. closed shell `wall`; 5. the board as a locked reference (STEP) and standoffs
//!    from its holes (`standoff_h`, `boss_od`, `boss_hole`);
//! 6. a parting joint at `lid_split` → "Base" + "Lid".
//!
//! Everything is driven by named parameters, so the result edits like any model.

use std::path::{Path, PathBuf};

use easycad_model::document::Document;
use easycad_model::dxf::DxfDrawing;
use easycad_model::feature::*;
use easycad_model::geom::{Loop2, Plane, Segment2};
use easycad_model::occt::{DVec3, Shape, SurfaceKind};
use easycad_model::process::ProcessKind;
use easycad_model::select::{EdgeSelector, Extremum, FaceSelector};
use easycad_model::sketch::{Geom, Sketch};
use glam::DVec2;

use crate::dialogs::joint;
use crate::sketch_mode::SketchSession;

/// Mounting holes: plated M2–M3.5 sized holes.
const HOLE_MIN: f64 = 2.0;
const HOLE_MAX: f64 = 4.0;

#[derive(Clone, Debug)]
pub enum BoardKind {
    Step,
    Dxf {
        /// Layers holding the outline (the closed loop with the largest area).
        outline: String,
        /// Layers with mounting-hole circles.
        holes: Vec<String>,
    },
}

/// What the wizard read from the board file.
#[derive(Clone, Debug)]
pub struct Board {
    pub path: PathBuf,
    pub name: String,
    pub bytes: Vec<u8>,
    pub kind: BoardKind,
    /// Board size and centre (file coordinates, mm).
    pub size: DVec2,
    pub center: DVec2,
    /// Mounting holes (file coordinates) and their diameters.
    pub holes: Vec<(DVec2, f64)>,
    /// Outline polyline (file coordinates), for the preview.
    pub outline: Vec<DVec2>,
    /// STEP: bottom of the board and its thickness.
    pub bottom_z: f64,
    pub thickness: f64,
    /// STEP: overall bounding box height (board + parts).
    pub parts_top: f64,
    /// STEP: how far parts reach below the board's underside.
    pub below: f64,
    /// STEP: the board's true outline (file coordinates) when it is not an
    /// axis-aligned rectangle: one `Circle`, or a chained loop of lines and arcs.
    pub shape_loop: Option<Loop2>,
    /// STEP: the parts standing on the board (file coordinates).
    pub components: Vec<Component>,
}

/// A part on the board, from the STEP product structure.
#[derive(Clone, Debug)]
pub struct Component {
    /// The product name's first field (EasyEDA: designator or footprint).
    pub name: String,
    pub min: DVec3,
    pub max: DVec3,
}

impl Board {
    pub fn read(path: &Path) -> Result<Board, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "board".into());
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "dxf" => Board::from_dxf(path, name, bytes),
            "step" | "stp" => Board::from_step(path, name, bytes),
            _ => Err("pick a .step / .stp or .dxf board file".into()),
        }
    }

    fn from_dxf(path: &Path, name: String, bytes: Vec<u8>) -> Result<Board, String> {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let d = DxfDrawing::parse(&text).map_err(|e| format!("DXF: {e}"))?;
        // the outline: the layer whose largest closed loop is biggest
        let mut best: Option<(String, f64, Vec<DVec2>)> = None;
        for layer in d.layer_names() {
            let sk = d.to_sketch("probe", Plane::XY, &[layer]);
            for p in sk.profiles().profiles {
                let poly = polyline(&p.outer);
                let a = easycad_model::sketch::loop_area(&p.outer).abs();
                if best.as_ref().is_none_or(|b| a > b.1) {
                    best = Some((layer.to_string(), a, poly));
                }
            }
        }
        let (outline_layer, _, outline) = best.ok_or("no closed board outline in the DXF")?;
        let (lo, hi) = bounds(&outline);
        let mut holes = Vec::new();
        let mut hole_layers = Vec::new();
        for layer in d.layer_names() {
            for (c, r) in d.circles(&[layer]) {
                let dia = 2.0 * r;
                let inside = c.x > lo.x && c.x < hi.x && c.y > lo.y && c.y < hi.y;
                if (HOLE_MIN..=HOLE_MAX).contains(&dia) && inside {
                    holes.push((c, dia));
                    if !hole_layers.iter().any(|l| l == layer) {
                        hole_layers.push(layer.to_string());
                    }
                }
            }
        }
        Ok(Board {
            path: path.to_path_buf(),
            name,
            bytes,
            kind: BoardKind::Dxf {
                outline: outline_layer,
                holes: hole_layers,
            },
            size: hi - lo,
            center: (lo + hi) / 2.0,
            holes,
            outline,
            bottom_z: 0.0,
            thickness: 1.6,
            parts_top: 1.6,
            below: 0.0,
            shape_loop: None,
            components: Vec::new(),
        })
    }

    fn from_step(path: &Path, name: String, bytes: Vec<u8>) -> Result<Board, String> {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // with the product structure when the file has one (part names)
        let (shape, parts) = match Shape::read_step_styled(&text) {
            Ok(st) => (st.shape, st.parts),
            Err(_) => (
                Shape::read_step_str(&text).map_err(|e| format!("STEP: {}", e.0))?,
                Vec::new(),
            ),
        };
        let faces = shape.faces().map_err(|e| format!("STEP: {}", e.0))?;
        // the board: the largest horizontal planar face
        let top = faces
            .iter()
            .filter(|f| f.kind == SurfaceKind::Plane && f.normal.z.abs() > 0.99)
            .max_by(|a, b| a.area.total_cmp(&b.area))
            .ok_or("no flat board face in the STEP (is the board lying in XY?)")?;
        let bb = shape
            .face_shape(top.index)
            .and_then(|f| f.bbox())
            .map_err(|e| format!("STEP: {}", e.0))?;
        let all = shape.bbox().map_err(|e| format!("STEP: {}", e.0))?;
        let lo = DVec2::new(bb.min.x, bb.min.y);
        let hi = DVec2::new(bb.max.x, bb.max.y);
        // the opposite face of the board gives its thickness
        let z = top.point.z;
        let other = faces
            .iter()
            .filter(|f| {
                f.kind == SurfaceKind::Plane
                    && f.normal.z * top.normal.z < -0.99
                    && (f.area - top.area).abs() < 0.2 * top.area
            })
            .map(|f| f.point.z)
            .min_by(|a, b| (a - z).abs().total_cmp(&(b - z).abs()));
        let (bottom_z, thickness) = match other {
            Some(o) => (o.min(z), (o - z).abs()),
            None => (z - 1.6, 1.6),
        };
        let top_z = bottom_z + thickness;
        // the parts: children that stand off the board (not copper, silkscreen or
        // a stiffener sheet); a part's extra solids ("name (2)") join the part
        let mut components: Vec<Component> = Vec::new();
        let board_area = (hi.x - lo.x) * (hi.y - lo.y);
        let mut last: Option<(String, bool)> = None;
        for (i, k) in shape.children().unwrap_or_default().iter().enumerate() {
            let Some(p) = parts.get(i) else { break };
            let Ok(b) = k.bbox() else { continue };
            let base = base_name(&p.name);
            if let Some((l, kept)) = &last {
                if l == base && p.name != base {
                    if *kept {
                        if let Some(c) = components.last_mut() {
                            c.min = c.min.min(b.min);
                            c.max = c.max.max(b.max);
                        }
                    }
                    continue;
                }
            }
            let off = b.max.z > top_z + 0.1 || b.min.z < bottom_z - 0.1;
            let sz = b.max - b.min;
            let sheet = sz.x * sz.y > 0.5 * board_area
                && b.max.z - top_z < 0.5
                && bottom_z - b.min.z < 0.5;
            let keep = off && !sheet;
            last = Some((base.to_string(), keep));
            if keep {
                components.push(Component {
                    name: base.split('~').next().unwrap_or(base).to_string(),
                    min: b.min,
                    max: b.max,
                });
            }
        }
        // the outline and the holes: the loops of the board face. A hole with a
        // part standing in it (a through-hole pin) is not a mounting hole.
        let mut holes: Vec<(DVec2, f64)> = Vec::new();
        let mut shape_loop = None;
        let mut outline = vec![lo, DVec2::new(hi.x, lo.y), hi, DVec2::new(lo.x, hi.y)];
        let hole_ok = |c: DVec2, dia: f64, holes: &[(DVec2, f64)]| {
            let occupied = components.iter().any(|k| {
                c.x > k.min.x && c.x < k.max.x && c.y > k.min.y && c.y < k.max.y
                    && k.min.z < top_z - 0.1
                    && k.max.z > bottom_z + 0.1
            });
            (HOLE_MIN..=HOLE_MAX).contains(&dia)
                && c.x > lo.x && c.x < hi.x && c.y > lo.y && c.y < hi.y
                && !occupied
                && !holes.iter().any(|h| h.0.distance(c) < 0.05)
        };
        match face_loops(&shape, top.index) {
            Some(mut loops) if !loops.is_empty() => {
                let area = |l: &Loop2| {
                    let (a, b) = bounds(&polyline(l));
                    (b.x - a.x) * (b.y - a.y)
                };
                let outer = (0..loops.len())
                    .max_by(|a, b| area(&loops[*a]).total_cmp(&area(&loops[*b])))
                    .unwrap_or(0);
                let outer = loops.swap_remove(outer);
                for l in &loops {
                    if let Some((c, r)) = as_circle(l) {
                        if hole_ok(c, 2.0 * r, &holes) {
                            holes.push((c, 2.0 * r));
                        }
                    }
                }
                let rect = outer.len() == 4
                    && outer.iter().all(|s| match *s {
                        Segment2::Line { a, b } => (a.x - b.x).abs() < 1e-6 || (a.y - b.y).abs() < 1e-6,
                        _ => false,
                    });
                if !rect {
                    let l = match as_circle(&outer) {
                        Some((center, radius)) => vec![Segment2::Circle { center, radius }],
                        None => outer,
                    };
                    outline = polyline(&l);
                    shape_loop = Some(l);
                }
            }
            _ => {
                // an outline we cannot chain (splines): the bounding box, and
                // vertical cylinders as holes
                for f in &faces {
                    if f.kind != SurfaceKind::Cylinder {
                        continue;
                    }
                    let Some((o, axis)) = f.axis else { continue };
                    let c = DVec2::new(o.x, o.y);
                    if axis.z.abs() > 0.99 && hole_ok(c, 2.0 * f.radius, &holes) {
                        holes.push((c, 2.0 * f.radius));
                    }
                }
            }
        }
        Ok(Board {
            path: path.to_path_buf(),
            name,
            bytes,
            kind: BoardKind::Step,
            size: hi - lo,
            center: (lo + hi) / 2.0,
            holes,
            outline,
            bottom_z,
            thickness,
            parts_top: all.max.z - bottom_z,
            below: (bottom_z - all.min.z).max(0.0),
            shape_loop,
            components,
        })
    }

    /// The parts that should poke through the enclosure's top: connectors (pogo
    /// pins, headers) standing well above everything else on the board.
    pub fn guess_exposed(&self) -> Vec<usize> {
        let conn = |n: &str| {
            let u = n.to_uppercase();
            ["CONN", "POGO", "HDR", "HEADER", "PIN"].iter().any(|k| u.contains(k))
        };
        let rest = self
            .components
            .iter()
            .filter(|c| !conn(&c.name))
            .map(|c| c.max.z)
            .fold(self.bottom_z + self.thickness, f64::max);
        (0..self.components.len())
            .filter(|i| conn(&self.components[*i].name) && self.components[*i].max.z > rest + 1.5)
            .collect()
    }

    /// Top of the tallest part that stays inside (not in `exposed`), above the
    /// board's underside.
    pub fn enclosed_top(&self, exposed: &[usize]) -> f64 {
        if self.components.is_empty() {
            return self.parts_top;
        }
        self.components
            .iter()
            .enumerate()
            .filter(|(i, _)| !exposed.contains(i))
            .map(|(_, c)| c.max.z - self.bottom_z)
            .fold(self.thickness, f64::max)
    }

    pub fn source_label(&self) -> &'static str {
        match self.kind {
            BoardKind::Step => "STEP",
            BoardKind::Dxf { .. } => "DXF",
        }
    }
}

/// "name (2)" → "name": the extra solids of one STEP part.
fn base_name(n: &str) -> &str {
    if let Some(i) = n.rfind(" (") {
        let t = &n[i + 2..];
        if t.ends_with(')') && t[..t.len() - 1].chars().all(|c| c.is_ascii_digit()) && t.len() > 1 {
            return &n[..i];
        }
    }
    n
}

fn seg_ends(s: &Segment2) -> (DVec2, DVec2) {
    match *s {
        Segment2::Line { a, b } => (a, b),
        Segment2::Arc { start, end, .. } => (start, end),
        Segment2::Circle { center, .. } => (center, center),
    }
}

/// The closed loops of a planar face (XY), chained from its lines and circular
/// edges. `None` when an edge is neither (a spline outline) or a chain stays open.
fn face_loops(shape: &Shape, face: u32) -> Option<Vec<Loop2>> {
    use easycad_model::occt::CurveKind;
    const TOL: f64 = 1e-3;
    let edges = shape.edges().ok()?;
    let mut loops: Vec<Loop2> = Vec::new();
    let mut rest: Vec<Segment2> = Vec::new();
    let xy = |p: DVec3| DVec2::new(p.x, p.y);
    for i in shape.face_edges(face).ok()? {
        let e = edges.get(i as usize)?;
        match e.kind {
            CurveKind::Line => rest.push(Segment2::Line { a: xy(e.start), b: xy(e.end) }),
            CurveKind::Circle => {
                let (c, _, r) = e.circle?;
                let center = xy(c);
                if e.closed || xy(e.start).distance(xy(e.end)) < TOL {
                    loops.push(vec![Segment2::Circle { center, radius: r }]);
                } else {
                    let ccw = (xy(e.start) - center).perp_dot(xy(e.midpoint) - center) > 0.0;
                    rest.push(Segment2::Arc { center, start: xy(e.start), end: xy(e.end), ccw });
                }
            }
            _ => return None,
        }
    }
    while let Some(first) = rest.pop() {
        let (s0, mut end) = seg_ends(&first);
        let mut l = vec![first];
        while end.distance(s0) > TOL {
            let i = rest.iter().position(|s| {
                let (a, b) = seg_ends(s);
                a.distance(end) < TOL || b.distance(end) < TOL
            })?;
            let mut s = rest.swap_remove(i);
            if seg_ends(&s).0.distance(end) >= TOL {
                s = easycad_model::sketch::reverse_segment(&s);
            }
            end = seg_ends(&s).1;
            l.push(s);
        }
        loops.push(l);
    }
    Some(loops)
}

/// A loop that is one full circle: a `Circle`, or arcs of one centre and radius.
fn as_circle(l: &Loop2) -> Option<(DVec2, f64)> {
    let (c0, r0) = match *l.first()? {
        Segment2::Circle { center, radius } => return (l.len() == 1).then_some((center, radius)),
        Segment2::Arc { center, start, .. } => (center, (start - center).length()),
        Segment2::Line { .. } => return None,
    };
    let mut sweep = 0.0;
    for s in l {
        let Segment2::Arc { center, start, end, ccw } = *s else { return None };
        if center.distance(c0) > 1e-3 || ((start - center).length() - r0).abs() > 1e-3 {
            return None;
        }
        sweep += easycad_model::sketch::arc_sweep(center, start, end, ccw).abs();
    }
    ((sweep - std::f64::consts::TAU).abs() < 1e-3).then_some((c0, r0))
}

/// A loop as a polyline (arcs sampled every ~10°).
fn polyline(l: &easycad_model::geom::Loop2) -> Vec<DVec2> {
    use easycad_model::geom::Segment2;
    let mut out = Vec::new();
    for seg in l {
        match *seg {
            Segment2::Line { a, .. } => out.push(a),
            Segment2::Arc { center, start, end, ccw } => {
                let a0 = (start - center).to_angle();
                let mut a1 = (end - center).to_angle();
                if ccw && a1 <= a0 {
                    a1 += std::f64::consts::TAU;
                } else if !ccw && a1 >= a0 {
                    a1 -= std::f64::consts::TAU;
                }
                let r = (start - center).length();
                let n = (((a1 - a0).abs() / 10f64.to_radians()).ceil() as usize).max(1);
                for i in 0..n {
                    out.push(center + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / n as f64) * r);
                }
            }
            Segment2::Circle { center, radius } => {
                for i in 0..36 {
                    out.push(center + DVec2::from_angle(i as f64 * 10f64.to_radians()) * radius);
                }
            }
        }
    }
    out
}

fn bounds(p: &[DVec2]) -> (DVec2, DVec2) {
    p.iter().fold(
        (DVec2::splat(f64::MAX), DVec2::splat(f64::MIN)),
        |(lo, hi), q| (lo.min(*q), hi.max(*q)),
    )
}

/// The wizard's settings (expressions, so "wall + 1" works).
#[derive(Clone, Debug)]
pub struct Settings {
    pub name: String,
    pub clearance: String,
    pub wall: String,
    pub height: String,
    pub corner_r: String,
    pub standoff_h: String,
    pub joint: joint::Kind,
    /// Process profile name (one of the document's built-in profiles).
    pub profile: String,
    /// Make an assembly: the enclosure becomes a hidden master component, split
    /// into Base and Lid components (in context), plus the board as a locked
    /// reference component (STEP boards).
    pub components: bool,
    /// Parts (indices into `Board::components`) that poke through the top: each
    /// gets an opening, and the top sits `proud` below the tallest of them.
    pub exposed: Vec<usize>,
    /// How far the exposed parts stand above the top (mm).
    pub proud: String,
    /// The lid follows the parts: it is solid down to the board (holding the
    /// board down), with pockets over the parts, instead of an even shell.
    pub fill_top: bool,
}

impl Settings {
    /// Settings that suit this board: its exposed parts, and with them a height
    /// that leaves them standing `proud` of the top.
    pub fn fit_board(&mut self, b: &Board) {
        self.exposed = b.guess_exposed();
        // a board nothing screws down, under a top its parts come through: the
        // lid holds it
        self.fill_top = matches!(b.kind, BoardKind::Step) && b.holes.is_empty() && !self.exposed.is_empty();
        if self.fill_top {
            // snapped shut around the board; the hooks stand beside it
            self.joint = joint::Kind::Snap;
            self.clearance = "1.3".into();
        }
        self.fit_height(b);
    }

    /// With exposed parts, the height follows from them.
    pub fn fit_height(&mut self, b: &Board) {
        let vals = easycad_model::params::Values::new();
        let ev = |e: &str, d: f64| crate::widgets::eval(e, &vals).unwrap_or(d);
        let top = self
            .exposed
            .iter()
            .filter_map(|i| b.components.get(*i))
            .map(|c| c.max.z - b.bottom_z)
            .fold(f64::MIN, f64::max);
        if top > 0.0 {
            let h = ev(&self.wall, 2.0) + b.below + top - ev(&self.proud, 1.0);
            self.height = fmt(h);
        }
    }

    pub fn new(profile: &str) -> Settings {
        // wall thickness is a property of the process, so it comes from the
        // profile's default `wall`; clearance (PCB to wall) is specific to this
        // design and stays the wizard's own
        let wall = easycad_model::process::ProcessProfile::builtins()
            .into_iter()
            .find(|p| p.name == profile)
            .and_then(|p| p.params.into_iter().find(|q| q.name == "wall"))
            .map_or_else(|| "2".to_string(), |q| q.expr);
        Settings {
            name: "enclosure".into(),
            clearance: "0.5".into(),
            wall,
            height: "30".into(),
            corner_r: "6".into(),
            standoff_h: "5".into(),
            joint: joint::Kind::Tongue,
            profile: profile.into(),
            components: true,
            exposed: Vec::new(),
            proud: "1".into(),
            fill_top: false,
        }
    }
}

fn param(doc: &mut Document, name: &str, expr: &str, comment: &str) -> Result<(), String> {
    doc.set_param(name, expr)
        .map_err(|e| format!("{name} = {expr}: {e}"))?;
    if !comment.is_empty() {
        let _ = doc.edit("Comment", |d| {
            if let Some(p) = d.params.list.iter_mut().find(|p| p.name == name) {
                p.comment = comment.into();
            }
            Ok::<_, ()>(())
        });
    }
    Ok(())
}

/// Build the document. Pure (no GUI), so it is tested directly.
pub fn build(board: &Board, s: &Settings) -> Result<Document, String> {
    let mut doc = Document::new_with_profile(if s.name.trim().is_empty() { "enclosure" } else { s.name.trim() }, &s.profile);
    let molded = doc.data.profile().kind == ProcessKind::Molded;
    let src = board.source_label();
    let step = matches!(board.kind, BoardKind::Step);
    if step && board.shape_loop.is_none() {
        param(&mut doc, "pcb_w", &fmt(board.size.x), &format!("from {src}"))?;
        param(&mut doc, "pcb_h", &fmt(board.size.y), &format!("from {src}"))?;
    } else if let Some([Segment2::Circle { radius, .. }]) = board.shape_loop.as_deref() {
        param(&mut doc, "pcb_d", &format!("{:.4}", 2.0 * radius), &format!("from {src}"))?;
    }
    param(&mut doc, "clearance", &s.clearance, "PCB to wall")?;
    param(&mut doc, "wall", &s.wall, "wall thickness")?;
    param(&mut doc, "height", &s.height, "outside, floor to top")?;
    param(&mut doc, "corner_r", &s.corner_r, "outline corners")?;
    let exposed: Vec<usize> = s
        .exposed
        .iter()
        .copied()
        .filter(|i| *i < board.components.len())
        .collect();
    // a small round keeps the top flat where parts come through
    param(&mut doc, "edge_r", if exposed.is_empty() { "wall + 1" } else { "1" }, "top edge round")?;
    param(&mut doc, "standoff_h", &s.standoff_h, "board above the floor")?;
    param(&mut doc, "boss_od", "7", "")?;
    param(&mut doc, "boss_hole", "2.5", "M3 self-tap")?;
    if molded {
        param(&mut doc, "draft", "1 deg", "molding")?;
    }
    let vals = crate::adapters::values(&doc.data);
    let ev = |e: &str| crate::widgets::eval(e, &vals);
    let wall = ev("wall")?;
    let height = ev("height")?;
    // no mounting holes: no standoffs, so the board rests on the floor (on its
    // underside parts, when it has any) instead of hanging in the air
    let standoff = if step && board.holes.is_empty() { board.below } else { ev("standoff_h")? };
    let board_z = wall + standoff;
    // with exposed parts the top only has to cover the parts that stay inside
    let need = if exposed.is_empty() {
        2.0 * wall + standoff + board.parts_top + 1.0
    } else {
        board_z + board.enclosed_top(&exposed) + 1.5
    };
    if height < need {
        return Err(format!(
            "height {height:.1} leaves no room: floor + standoffs + board{} need {need:.1}",
            if step { " and parts" } else { "" },
        ));
    }
    // snap hooks around a board the lid holds down: they rise from the base beside
    // the board, into a ring left free around the lid's fill
    let hooked = step && board.holes.is_empty() && s.fill_top && s.joint == joint::Kind::Snap;
    if hooked && ev("clearance")? < 1.2 {
        return Err("snap hooks stand between the board and the wall: the clearance must be at least 1.2 mm".into());
    }
    if step && board.holes.is_empty() {
        // nothing screws the board down: the lid parts at the board's top face,
        // so the board drops into the base and the lid closes over it
        param(&mut doc, "lid_split", &fmt(board_z + board.thickness), "parting plane: the board's top")?;
    } else {
        param(&mut doc, "lid_split", "height - 10", "parting plane")?;
    }

    // 1. profile: the board outline offset by clearance + wall
    let (profile, rounded) = enclosure_profile(&doc, board)?;
    if rounded == 0 {
        // the board's corners are already round: the offset follows them, so the
        // corner radius would be an unused parameter
        let _ = doc.edit("Remove corner_r", |d| {
            d.params.list.retain(|p| p.name != "corner_r");
            Ok::<_, ()>(())
        });
    }
    let sk = doc.add_sketch(profile);
    // 2. extrude
    let ext = doc.add_feature(
        "Base",
        FeatureKind::Extrude {
            sketch: sk,
            profiles: ProfileSel::All,
            distance: "height".into(),
            symmetric: false,
            reverse: false,
            draft_deg: if molded { "draft".into() } else { "0".into() },
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let base = BodyId::of(ext);
    // 3. round the top edges
    let top_edges = |doc: &mut Document| doc.add_feature(
        "Top edges",
        FeatureKind::Fillet {
            body: Some(base),
            edges: vec![EdgeSelector::OfFace(FaceSelector::Extreme {
                normal: DVec3::Z,
                which: Extremum::Max,
            })],
            radius: "edge_r".into(),
            auto_reduce: true,
            sets: Vec::new(),
            skipped: Vec::new(),
        },
    );
    // a round smaller than the wall cannot be hollowed: with exposed parts (a
    // small round, a flat top) it goes on after the shell
    if exposed.is_empty() {
        top_edges(&mut doc);
    }
    // 4. closed shell
    doc.add_feature(
        "Shell",
        FeatureKind::Shell {
            body: Some(base),
            open_faces: Vec::new(),
            thickness: "wall".into(),
            outward: false,
        },
    );
    if !exposed.is_empty() {
        top_edges(&mut doc);
    }
    // 5. the board and its standoffs
    let mut board_place = DVec3::ZERO;
    let source = match &board.kind {
        BoardKind::Step => {
            let (imp, f) = doc.import_step(&board.name, board.bytes.clone(), true);
            // centre the board over the origin, resting on the standoffs
            let t = DVec3::new(-board.center.x, -board.center.y, board_z - board.bottom_z);
            board_place = t;
            let _ = doc.edit("Place board", |d| {
                if let Some(FeatureKind::ImportStep { translate, .. }) =
                    d.feature_mut(f).map(|f| &mut f.kind)
                {
                    *translate = t;
                }
                Ok::<_, ()>(())
            });
            PcbSource::Step {
                import: imp,
                min_dia: HOLE_MIN,
                max_dia: HOLE_MAX,
            }
        }
        BoardKind::Dxf { holes, .. } => {
            let file = board
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.dxf", board.name));
            let imp = doc.import_dxf(&file, board.bytes.clone());
            PcbSource::Dxf {
                import: imp,
                layers: holes.clone(),
                min_dia: HOLE_MIN,
                max_dia: HOLE_MAX,
                offset: -board.center,
            }
        }
    };
    if !board.holes.is_empty() {
        doc.add_feature(
            "Standoffs",
            FeatureKind::StandoffsFromPcb {
                body: Some(base),
                source,
                plane: Plane::XY,
                od: "boss_od".into(),
                hole_dia: "boss_hole".into(),
                height: "standoff_h".into(),
                base_fillet: "0.3".into(),
                draft_deg: if molded { "draft".into() } else { "0".into() },
                // standoffs start on the floor, whatever the wall thickness
                offset: "wall".into(),
                exclude: Vec::new(),
                gussets: None,
                lid_holes: None,
            },
        );
    }
    // the lid follows the parts: solid from just above the board up, with pockets
    if s.fill_top && step {
        param(&mut doc, "part_gap", "0.4", "around parts under the lid")?;
        let z0 = board_z + board.thickness;
        // 0.1 above the board (a sketch plane is a number, not an expression)
        const BOARD_GAP: f64 = 0.1;
        if hooked {
            // the top over the hooks is thin, so the beams are long enough to bend
            param(&mut doc, "top_wall", "1", "top over the snap hooks")?;
            let (mut recess, _) = outline_profile(&doc, board, "Top recess", "clearance", "corner_r - wall")?;
            recess.plane = Plane::XY.offset(z0);
            let recess = doc.add_sketch(recess);
            doc.add_feature(
                "Top recess",
                FeatureKind::Extrude {
                    sketch: recess,
                    profiles: ProfileSel::All,
                    distance: format!("height - top_wall - {}", fmt(z0)).as_str().into(),
                    symmetric: false,
                    reverse: false,
                    draft_deg: "0".into(),
                    op: BoolOp::Cut,
                    target: Some(base),
                },
            );
        }
        // with hooks the fill is an island just inside the board's edge; else it
        // runs into the walls
        let (off, top) = if hooked { ("-0.5", "top_wall/2") } else { ("clearance + wall/2", "wall/2") };
        let (mut fill, _) = outline_profile(&doc, board, "Lid fill", off, "corner_r - wall/2")?;
        fill.plane = Plane::XY.offset(z0 + BOARD_GAP);
        let fill = doc.add_sketch(fill);
        doc.add_feature(
            "Lid fill",
            FeatureKind::Extrude {
                sketch: fill,
                profiles: ProfileSel::All,
                distance: format!("height - {top} - {}", fmt(z0 + BOARD_GAP)).as_str().into(),
                symmetric: false,
                reverse: false,
                draft_deg: "0".into(),
                op: BoolOp::Join,
                target: Some(base),
            },
        );
        // pockets: the parts on the board's top that stay inside, neighbours merged
        let top_z = board.bottom_z + board.thickness;
        let mut boxes: Vec<(DVec2, DVec2, f64)> = board
            .components
            .iter()
            .enumerate()
            .filter(|(i, c)| !exposed.contains(i) && c.max.z > top_z + 0.05)
            .map(|(_, c)| {
                (
                    DVec2::new(c.min.x, c.min.y) - board.center,
                    DVec2::new(c.max.x, c.max.y) - board.center,
                    c.max.z - top_z,
                )
            })
            .collect();
        let touch = |a: &(DVec2, DVec2, f64), b: &(DVec2, DVec2, f64)| {
            let g = 2.0 * 0.4 + 0.6; // two gaps and a printable wall between
            a.0.x < b.1.x + g && b.0.x < a.1.x + g && a.0.y < b.1.y + g && b.0.y < a.1.y + g
        };
        'merge: loop {
            for i in 0..boxes.len() {
                for j in i + 1..boxes.len() {
                    if touch(&boxes[i], &boxes[j]) {
                        let b = boxes.swap_remove(j);
                        let a = &mut boxes[i];
                        *a = (a.0.min(b.0), a.1.max(b.1), a.2.max(b.2));
                        continue 'merge;
                    }
                }
            }
            break;
        }
        // two depths: over the low parts (passives) and over the tall ones
        for (label, tier) in [
            ("Part pockets · low", boxes.iter().filter(|b| b.2 <= 1.0).collect::<Vec<_>>()),
            ("Part pockets · tall", boxes.iter().filter(|b| b.2 > 1.0).collect::<Vec<_>>()),
        ] {
            if tier.is_empty() {
                continue;
            }
            let h = tier.iter().map(|b| b.2).fold(0.0, f64::max);
            let mut sk = Sketch::new(label, Plane::XY.offset(z0));
            for b in &tier {
                let size = b.1 - b.0;
                sk.rounded_rect(
                    (b.0 + b.1) / 2.0,
                    format!("{} + 2*part_gap", fmt(size.x)).as_str(),
                    format!("{} + 2*part_gap", fmt(size.y)).as_str(),
                    "0.3",
                    (size.x + 0.8, size.y + 0.8, 0.3),
                );
            }
            let sk = doc.add_sketch(sk);
            doc.add_feature(
                label,
                FeatureKind::Extrude {
                    sketch: sk,
                    profiles: ProfileSel::All,
                    distance: format!("{} + part_gap + {BOARD_GAP}", fmt(h)).as_str().into(),
                    symmetric: false,
                    reverse: false,
                    draft_deg: "0".into(),
                    op: BoolOp::Cut,
                    target: Some(base),
                },
            );
        }
    }
    // openings for the parts that poke through the top
    if !exposed.is_empty() {
        param(&mut doc, "opening_gap", "0.3", "around exposed parts")?;
        let plane = Plane::XY.offset(board_z + board.thickness);
        for i in &exposed {
            let c = &board.components[*i];
            let size = DVec2::new(c.max.x - c.min.x, c.max.y - c.min.y);
            let center = DVec2::new(c.min.x + c.max.x, c.min.y + c.max.y) / 2.0 - board.center;
            let name = format!("Opening · {}", c.name);
            let mut sk = Sketch::new(name.as_str(), plane);
            sk.rounded_rect(
                center,
                format!("{} + 2*opening_gap", fmt(size.x)).as_str(),
                format!("{} + 2*opening_gap", fmt(size.y)).as_str(),
                "0.5",
                (size.x + 0.6, size.y + 0.6, 0.5),
            );
            let sk = doc.add_sketch(sk);
            doc.add_feature(
                &name,
                FeatureKind::Extrude {
                    sketch: sk,
                    profiles: ProfileSel::All,
                    distance: "height".into(),
                    symmetric: false,
                    reverse: false,
                    draft_deg: "0".into(),
                    op: BoolOp::Cut,
                    target: Some(base),
                },
            );
        }
    }
    // 6. parting joint → Base + Lid
    let mut j = joint::JointD::new(&doc.data, None);
    j.kind = s.joint;
    j.body = Some(base);
    j.z = crate::dialogs::Field::new("lid_split");
    j.upper_name = "Lid".into();
    let mut jk = j.feature_kind();
    if hooked {
        // short, thin beams from the base, sized to the ring under the thin top
        let top = crate::widgets::eval("top_wall", &crate::adapters::values(&doc.data))?;
        let free = height - top - (board_z + board.thickness);
        j.male_lower = true;
        j.rim_on = false;
        j.s_len = crate::dialogs::Field::new(&fmt((free - 0.5).min(8.0)));
        j.s_thick = crate::dialogs::Field::new("0.7");
        j.s_width = crate::dialogs::Field::new("4");
        j.s_under = crate::dialogs::Field::new("0.3");
        jk = j.feature_kind();
        if let FeatureKind::PartingJoint { kind: easycad_model::joint::JointKind::SnapFit { margin, .. }, .. } = &mut jk {
            *margin = "0.6".into();
        }
    }
    let joint_id = doc.add_feature(&format!("Joint · {}", s.joint.label()), jk);
    for (k, v) in [("title", format!("{} enclosure", doc.data.name)), ("revision", "A".into())] {
        doc.data.meta.insert(k.into(), v);
    }
    if s.components {
        make_components(&mut doc, board, joint_id, board_place)?;
    }
    Ok(doc)
}

/// The flat enclosure as an assembly: `Enclosure` (the design master, hidden and
/// out of the BOM) split by its parting joint into `Base` (grounded) and `Lid`
/// components that follow every edit, plus the board as a locked, purchased
/// reference component (STEP boards).
fn make_components(
    doc: &mut Document,
    board: &Board,
    joint: easycad_model::feature::FeatureId,
    board_place: DVec3,
) -> Result<(), String> {
    use easycad_model::assembly::{ComponentId, Transform};
    let (_, master) = doc
        .move_root_into_component("Enclosure")
        .map_err(|e| format!("components: {e}"))?;
    let [(_, base), _] = doc
        .split_to_components(&[master], joint, Some(("Base", "Lid")))
        .map_err(|e| format!("components: {e}"))?;
    doc.edit_occurrence(master, "Master", |o| o.grounded = true)
        .map_err(|e| e.to_string())?;
    doc.edit_occurrence(base, "Ground Base", |o| o.grounded = true)
        .map_err(|e| e.to_string())?;
    if matches!(board.kind, BoardKind::Step) {
        let name = if board.name.is_empty() { "PCB" } else { board.name.as_str() };
        let (_, occ) = doc
            .import_step_assembly(name, board.bytes.clone(), ComponentId::ROOT, true)
            .map_err(|e| format!("board: {e}"))?;
        doc.edit_occurrence(occ, "Place board", |o| {
            o.transform = Transform::translation(board_place);
            o.grounded = true;
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The board outline (construction, fixed) and its offset by `clearance + wall`
/// with the offset's sharp corners rounded by `corner_r`.
/// Also returns how many sharp corners were rounded with `corner_r`.
fn enclosure_profile(doc: &Document, board: &Board) -> Result<(Sketch, usize), String> {
    outline_profile(doc, board, "Enclosure profile", "clearance + wall", "corner_r")
}

/// The board outline (construction, fixed) offset by `offset`, the offset's sharp
/// corners rounded by `corner`; also how many corners were rounded.
fn outline_profile(
    doc: &Document,
    board: &Board,
    name: &str,
    offset: &str,
    corner: &str,
) -> Result<(Sketch, usize), String> {
    use easycad_model::sketch::Constraint;
    let mut sess = SketchSession::new_on(Plane::XY, &doc.data);
    sess.sketch.name = "Enclosure profile".into();
    use easycad_model::sketch::{EntityId, PointId};
    let traced = match (&board.kind, &board.shape_loop) {
        (BoardKind::Step, None) => None,
        (BoardKind::Step, Some(l)) => {
            // the board's own outline, centred on the origin
            let mut sk = Sketch::new("Enclosure profile", Plane::XY);
            let mut pts: Vec<(DVec2, PointId)> = Vec::new();
            let mut pt = |sk: &mut Sketch, p: DVec2| match pts.iter().find(|q| q.0.distance(p) < 1e-3) {
                Some(q) => q.1,
                None => {
                    let id = sk.add_point(p);
                    pts.push((p, id));
                    id
                }
            };
            let c = board.center;
            for seg in l {
                match *seg {
                    Segment2::Line { a, b } => {
                        let (a, b) = (pt(&mut sk, a - c), pt(&mut sk, b - c));
                        sk.add_line(a, b);
                    }
                    Segment2::Arc { center, start, end, ccw } => {
                        let (s, e) = if ccw { (start, end) } else { (end, start) };
                        let o = sk.add_point(center - c);
                        let (s, e) = (pt(&mut sk, s - c), pt(&mut sk, e - c));
                        sk.add_arc(o, s, e);
                    }
                    Segment2::Circle { center, radius } => {
                        let e = sk.circle(center - c, radius);
                        sk.constrain(Constraint::Diameter(e, "pcb_d".into()));
                    }
                }
            }
            Some(sk)
        }
        (BoardKind::Dxf { outline, .. }, _) => {
            let text = String::from_utf8_lossy(&board.bytes).into_owned();
            let d = DxfDrawing::parse(&text).map_err(|e| format!("DXF: {e}"))?;
            let mut sk = d.to_sketch("Enclosure profile", Plane::XY, &[outline.as_str()]);
            for p in &mut sk.points {
                *p -= board.center;
            }
            Some(sk)
        }
    };
    let start = match traced {
        None => {
            // a rectangle sized by pcb_w × pcb_h, centred on the origin
            let r = crate::sample::centered_rect(
                "Enclosure profile",
                Plane::XY,
                "pcb_w",
                "pcb_h",
                board.size,
                DVec2::ZERO,
            );
            sess.sketch = r;
            sess.entity_layer.resize(sess.sketch.entities.len(), None);
            sess.resolve();
            sess.sketch
                .entities
                .iter()
                .position(|e| !e.construction)
                .map(|i| EntityId(i as u32))
                .ok_or("empty board outline")?
        }
        Some(sk) => {
            sess.sketch = sk;
            sess.entity_layer.resize(sess.sketch.entities.len(), None);
            for i in 0..sess.sketch.points.len() {
                let p = sess.sketch.points[i];
                sess.sketch.constrain(Constraint::Fix(PointId(i as u32), p));
            }
            sess.resolve();
            // the outer loop runs through the left-most end point; a round board
            // is its largest circle
            let s = &sess.sketch;
            let ends = |g: &Geom| match *g {
                Geom::Line { a, b } => vec![a, b],
                Geom::Arc { start, end, .. } => vec![start, end],
                Geom::Circle { .. } => Vec::new(),
            };
            let chained = s
                .entities
                .iter()
                .enumerate()
                .flat_map(|(i, e)| ends(&e.geom).into_iter().map(move |p| (i, p)))
                .min_by(|a, b| s.point(a.1).x.total_cmp(&s.point(b.1).x))
                .map(|(i, _)| i);
            let round = s
                .entities
                .iter()
                .enumerate()
                .filter_map(|(i, e)| match e.geom {
                    Geom::Circle { radius, .. } => Some((i, radius)),
                    _ => None,
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let i = match (chained, round) {
                (Some(i), None) => i,
                (None, Some((i, _))) => i,
                // both: the circle is the outline only when it holds the chain
                (Some(i), Some((c, r))) => {
                    if r > 0.5 * board.size.max_element() - 1e-3 { c } else { i }
                }
                (None, None) => return Err("board outline has no lines, arcs or circle".into()),
            };
            EntityId(i as u32)
        }
    };
    let n_before = sess.sketch.entities.len();
    let source_points = sess.sketch.points.len();
    sess.offset_loop(start, offset)?;
    // round the offset's sharp corners (round board corners stay concentric)
    let corners: Vec<easycad_model::sketch::PointId> = {
        let s = &sess.sketch;
        let mut count = std::collections::HashMap::new();
        for e in &s.entities[n_before..] {
            if let Geom::Line { a, b } = e.geom {
                *count.entry(a).or_insert(0) += 1;
                *count.entry(b).or_insert(0) += 1;
            }
        }
        let mut c: Vec<_> = count
            .into_iter()
            .filter(|(p, n)| *n == 2 && (p.0 as usize) >= source_points)
            .map(|(p, _)| p)
            .collect();
        c.sort_by_key(|p| p.0);
        c
    };
    let rounded = corners.len();
    for p in corners {
        sess.fillet_corner(p, corner)?;
    }
    for e in &mut sess.sketch.entities[..n_before] {
        e.construction = true;
    }
    sess.resolve();
    if let Some(e) = &sess.error {
        return Err(format!("enclosure profile: {e}"));
    }
    sess.sketch.name = name.into();
    if sess.sketch.profiles().profiles.len() != 1 {
        return Err("the offset outline is not one closed profile".into());
    }
    Ok((sess.sketch, rounded))
}

fn fmt(v: f64) -> String {
    crate::dialogs::fmt((v * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, data: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn reads_the_sample_dxf_and_step() {
        let dir = std::env::temp_dir().join(format!("cw-wizard-{}", std::process::id()));
        let d = Board::read(&write(&dir, "board.dxf", &crate::sample::board_dxf())).unwrap();
        assert!((d.size - DVec2::new(86.0, 50.0)).length() < 1e-6, "{:?}", d.size);
        assert_eq!(d.holes.len(), 4);
        assert!(matches!(&d.kind, BoardKind::Dxf { outline, .. } if outline == "BoardOutLine"));
        let s = Board::read(&write(&dir, "board.step", &crate::sample::pcb_step())).unwrap();
        assert!((s.size - DVec2::new(86.0, 50.0)).length() < 0.05, "{:?}", s.size);
        assert_eq!(s.holes.len(), 4, "{:?}", s.holes);
        assert!((s.thickness - 1.6).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Snap beams on a round wall curve with it.
    #[test]
    fn round_board_snap_fit() {
        let Ok(p) = std::env::var("EASYCAD_REAL_BOARD_STEP") else { return };
        let b = Board::read(Path::new(&p)).unwrap();
        let mut s = Settings { joint: joint::Kind::Snap, components: false, ..Settings::new("FDM 0.4 generic") };
        s.clearance = "1.6".into();
        s.height = "14".into();
        let mut doc = build(&b, &s).unwrap();
        let r = doc.regenerate().clone();
        println!("errors {:?}", r.errors());
        for f in &doc.data.features {
            if let Some(rep) = r.report(f.id) {
                println!("{}: {:?}", f.name, rep);
            }
        }
        for body in r.solids() {
            let bb = body.shape().bbox().unwrap();
            println!("{}: {:.2?} .. {:.2?} vol {:.0}", body.name, bb.min.to_array(), bb.max.to_array(), body.shape().volume().unwrap_or(0.0));
        }
        assert!(r.errors().is_empty());
    }

    /// A real EasyEDA export kept outside the repo: a round charger board with
    /// three pogo pins and no mounting holes. `EASYCAD_REAL_BOARD_STEP=<file>`.
    #[test]
    fn round_board_without_holes() {
        let Ok(p) = std::env::var("EASYCAD_REAL_BOARD_STEP") else { return };
        let b = Board::read(Path::new(&p)).unwrap();
        assert!(b.holes.is_empty(), "pin holes are not mounting holes: {:?}", b.holes);
        let Some([Segment2::Circle { radius, .. }]) = b.shape_loop.as_deref() else {
            panic!("round outline: {:?}", b.shape_loop)
        };
        assert!((radius - 12.862).abs() < 0.01, "{radius}");
        let pins = b.components.iter().find(|c| c.name.starts_with("CONN-TH")).expect("pogo pins");
        assert!((pins.max.z - 7.9).abs() < 0.05, "{pins:?}");
        println!("{} components", b.components.len());
        for c in &b.components {
            println!("  {} {:.2?} .. {:.2?}", c.name, c.min.to_array(), c.max.to_array());
        }
        for k in [joint::Kind::Snap, joint::Kind::Tongue, joint::Kind::Lip] {
            let mut s = Settings { components: false, ..Settings::new("FDM 0.4 generic") };
            s.fit_board(&b);
            assert!(s.joint == joint::Kind::Snap && s.clearance == "1.3");
            s.joint = k;
            if k != joint::Kind::Snap {
                s.clearance = "0.5".into();
            }
            assert_eq!(s.exposed.len(), 1, "the pogo pins poke through");
            assert_eq!(s.height, "8.9", "floor 2 + pins 7.9 - proud 1");
            let mut doc = build(&b, &s).unwrap();
            assert!(doc.data.features.iter().any(|f| f.name.starts_with("Opening")));
            assert!(s.fill_top && doc.data.features.iter().any(|f| f.name == "Lid fill"));
            println!("features: {:?}", doc.data.features.iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
            assert!(doc.data.params.list.iter().any(|p| p.name == "pcb_d"));
            let r = doc.regenerate().clone();
            assert!(r.errors().is_empty(), "{k:?}: {:?}", r.errors());
            if let Some(j) = doc.data.features.last().and_then(|f| r.report(f.id)).and_then(|r| r.joint.clone()) {
                println!("{k:?} joint: snaps {:?}\n  warnings {:?}\n  adjustments {:?}\n  checks {:?}", j.snaps.iter().map(|s| s.placed).collect::<Vec<_>>(), j.warnings, j.adjustments, j.checks);
                if k == joint::Kind::Snap {
                    assert!(j.snaps.iter().filter(|s| s.placed.is_some()).count() >= 3, "{:?}", j.snaps);
                }
            }
            for body in r.solids() {
                let bb = body.shape().bbox().unwrap();
                println!("{k:?} {}: {:.2?} .. {:.2?}", body.name, bb.min.to_array(), bb.max.to_array());
                if body.name == "Lid" {
                    // the fill: a downward face just above the board (z 3.7), most of
                    // the board's 520 mm² less the pockets
                    let under = body
                        .shape()
                        .faces()
                        .unwrap()
                        .into_iter()
                        .filter(|f| f.normal.z < -0.99 && (f.point.z - 3.7).abs() < 1e-3)
                        .map(|f| f.area)
                        .sum::<f64>();
                    println!("lid fill underside {under:.1} mm², volume {:.0} mm³", body.shape().volume().unwrap_or(0.0));
                    assert!(under > 120.0 && under < 520.0, "{under}");
                    // closed top 30.72² π/4 ≈ 741 mm² less the 8.3 × 3.1 opening: the
                    // lid's top face has a hole in it
                    let top = body
                        .shape()
                        .faces()
                        .unwrap()
                        .into_iter()
                        .filter(|f| f.normal.z > 0.99 && (f.point.z - 8.9).abs() < 1e-3)
                        .map(|f| f.area)
                        .sum::<f64>();
                    let full = std::f64::consts::PI * (bb.max.x - 1.0).powi(2);
                    assert!(top < full - 20.0 && top > full - 30.0, "top face {top:.1} of {full:.1}");
                }
                if body.name == "Base" {
                    let d = bb.max.x - bb.min.x;
                    // at least board 25.72 + 2 × (clearance 0.5 + wall 2), more with the
                    // joint's rim; round: the same in x and y
                    assert!(d > 30.7 && d < 36.0 && (d - (bb.max.y - bb.min.y)).abs() < 0.05, "{d}");
                }
            }
            doc.set_param("clearance", if k == joint::Kind::Snap { "1.5" } else { "1" }).unwrap();
            let r = doc.regenerate().clone();
            assert!(r.errors().is_empty(), "{k:?} after edit: {:?}", r.errors());
        }
    }

    #[test]
    fn builds_a_clean_enclosure_from_dxf_and_step() {
        let dir = std::env::temp_dir().join(format!("cw-wizard-b-{}", std::process::id()));
        for (file, data) in [
            ("board.dxf", crate::sample::board_dxf()),
            ("board.step", crate::sample::pcb_step()),
        ] {
            let board = Board::read(&write(&dir, file, &data)).unwrap();
            // snap fits: see GUI_NEEDS 12d (no position placed on these boxes)
            for k in [joint::Kind::Tongue, joint::Kind::Lip, joint::Kind::ORing] {
                let s = Settings { joint: k, components: false, ..Settings::new("FDM 0.4 generic") };
                let mut doc = build(&board, &s).unwrap_or_else(|e| panic!("{file}: {e}"));
                let r = doc.regenerate().clone();
                assert!(r.errors().is_empty(), "{file} {k:?}: {:?}", r.errors());
            }
            // the default: an assembly (Enclosure master -> Base + Lid, the board)
            let asm = build(&board, &Settings::new("FDM 0.4 generic")).unwrap_or_else(|e| panic!("{file}: {e}"));
            let names: Vec<_> = asm.data.components.iter().map(|c| c.name.clone()).collect();
            assert!(names.iter().any(|n| n == "Base") && names.iter().any(|n| n == "Lid"), "{names:?}");
            let mut asm = asm;
            let ar = asm.regenerate_assembly().clone();
            assert!(ar.errors().is_empty(), "{file}: {:?}", ar.errors());
            let placed: Vec<_> = ar.instances.iter().filter(|i| i.visible).map(|i| i.label(&asm.data)).collect();
            assert!(placed.iter().any(|l| l.starts_with("Base:1")) && placed.iter().any(|l| l.starts_with("Lid:1")), "{placed:?}");
            if file.ends_with(".step") {
                assert!(asm.data.components.iter().any(|c| c.purchased.is_some()), "board component");
            }
            let mut s = Settings::new("Injection molded (nominal)");
            s.components = false;
            let mut doc = build(&board, &s).unwrap_or_else(|e| panic!("{file}: {e}"));
            let names: Vec<_> = doc.data.features.iter().map(|f| f.kind.label()).collect();
            let r = doc.regenerate().clone();
            let statuses: Vec<_> = doc
                .data
                .features
                .iter()
                .map(|f| (f.name.clone(), r.report(f.id).map(|r| format!("{:?}", r.status))))
                .collect();
            assert!(r.errors().is_empty(), "{file}: {:?}\n{statuses:?}", r.errors());
            let solids: Vec<_> = r.solids().map(|b| b.name.clone()).collect();
            assert!(
                solids.contains(&"Base".to_string()) && solids.contains(&"Lid".to_string()),
                "{file}: {solids:?} {names:?}"
            );
            // editable: a wider clearance regenerates
            doc.set_param("clearance", "1").unwrap();
            let r = doc.regenerate().clone();
            assert!(r.errors().is_empty(), "{file} after edit: {:?}", r.errors());
            // too low is refused up front
            let mut low = Settings::new("FDM 0.4 generic");
            low.height = "8".into();
            low.components = false;
            assert!(build(&board, &low).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
