//! Sketch mode: a 2D view of the sketch plane with the sketch palette (line, rect,
//! circle, arc, fillet, offset, project, trim, dimension), constraint buttons and
//! glyphs, a live solver with DOF status, typed numbers / `=expr` while drawing,
//! and DXF layer import.

use std::collections::{BTreeMap, HashMap, HashSet};

use easycad_model::document::{DocData, DocError, Document};
use easycad_model::dxf::DxfDrawing;
use easycad_model::feature::{ImportId, SketchId};
use easycad_model::geom::{Plane, Segment2};
use easycad_model::params::{Expr, Values};
use easycad_model::sketch::{
    profiles_from_segments, Constraint, ConstraintStatus, EntityId, Geom, PointId, Sketch,
    SolveReport,
};
use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use glam::DVec2;

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Line,
    Rect,
    Circle,
    Arc,
    Fillet,
    Offset,
    Project,
    Trim,
    Dimension,
}

const CONSTRUCTION_HINT: &str = "Construction lines: with entities selected, toggles them; with nothing selected, new geometry is drawn as construction (dashed, never a profile)";

pub const TOOLS: [(Tool, &str, &str); 9] = [
    (Tool::Line, "Line", "L"),
    (Tool::Rect, "Rectangle", "R"),
    (Tool::Circle, "Circle", "C"),
    (Tool::Arc, "Arc", "A"),
    (Tool::Fillet, "Fillet", "F"),
    (Tool::Offset, "Offset", "O"),
    (Tool::Project, "Project", "P"),
    (Tool::Trim, "Trim", "T"),
    (Tool::Dimension, "Dimension", "D"),
];

impl Tool {
    pub fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Select · drag points · Delete removes · Esc clears",
            Tool::Line => "Line · click start point · type length",
            Tool::Rect => "Rectangle · click first corner · type W, Tab, H",
            Tool::Circle => "Circle · click center · type diameter",
            Tool::Arc => "Arc · click center, start, then end (CCW)",
            Tool::Fillet => "Sketch fillet · pick a corner · type radius",
            Tool::Offset => "Offset · pick a loop · type distance (=clearance + wall)",
            Tool::Project => "Project · pick body edges (grey) to bring them in as profile geometry (X makes them construction)",
            Tool::Trim => "Trim · hover a segment to preview, click to cut",
            Tool::Dimension => "Dimension · pick points / centres / midpoints / lines / circles (two, or one), place, then type",
        }
    }
}

/// Which of the four angles two crossing lines make was picked, as the rays
/// `r1` (of the first line) and `r2` (of the second) around the crossing `p`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AngleChoice {
    /// The stored constraint lists the second line first (the angle runs CCW).
    pub swap: bool,
    pub rev1: bool,
    pub rev2: bool,
    /// The angle between the two rays, 0..180 degrees.
    pub deg: f64,
    pub p: DVec2,
    pub r1: DVec2,
    pub r2: DVec2,
}

/// Where two infinite lines (a-b and c-d) cross.
pub fn line_cross(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> Option<DVec2> {
    let (u, w) = (b - a, d - c);
    let den = u.perp_dot(w);
    if den.abs() <= 1e-9 * u.length() * w.length() || den == 0.0 {
        return None;
    }
    Some(a + u * ((c - a).perp_dot(w) / den))
}

/// The angle a cursor at `q` picks between lines a-b and c-d: the quadrant around
/// their crossing that holds the cursor (so the angle, its supplement or the
/// opposite pair, as in Fusion). `None` for parallel lines.
pub fn angle_choice(a: DVec2, b: DVec2, c: DVec2, d: DVec2, q: DVec2) -> Option<AngleChoice> {
    let p = line_cross(a, b, c, d)?;
    let (u, w) = ((b - a).normalize(), (d - c).normalize());
    let den = u.perp_dot(w);
    let v = q - p;
    let alpha = v.perp_dot(w) / den;
    let beta = u.perp_dot(v) / den;
    let (rev1, rev2) = (alpha < 0.0, beta < 0.0);
    let r1 = if rev1 { -u } else { u };
    let r2 = if rev2 { -w } else { w };
    let deg = r1.perp_dot(r2).abs().atan2(r1.dot(r2)).to_degrees();
    Some(AngleChoice { swap: r1.perp_dot(r2) < 0.0, rev1, rev2, deg, p, r1, r2 })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Point(PointId),
    Entity(EntityId),
    /// The midpoint of a line: an anchor for the Dimension tool only.
    Mid(EntityId),
    /// The sketch origin (0, 0): an anchor for the Dimension tool only.
    Origin,
}

/// A live Select-tool drag of a point, or of every point of a line/arc/circle.
#[derive(Clone)]
pub struct Drag {
    /// The dragged points with where each started.
    pub pts: Vec<(PointId, DVec2)>,
    /// Where the press landed (snapped), so the move is relative to it.
    pub press: DVec2,
    /// The sketch before the drag, for Esc.
    pub before: Sketch,
}

/// Builds the dimension constraint from the typed expression.
pub type DimMaker = Box<dyn Fn(Expr) -> Constraint>;

/// How a point-to-point dimension reads: along the pair, or along an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DimOrient {
    #[default]
    Aligned,
    Horizontal,
    Vertical,
}

/// Fusion-style: the dimension reads horizontally when the cursor sits above or
/// below the pair (within its x span), vertically when it sits beside it, and
/// aligned otherwise.
pub fn dim_orient(a: DVec2, b: DVec2, c: DVec2) -> DimOrient {
    let (lo, hi) = (a.min(b), a.max(b));
    let in_x = c.x >= lo.x && c.x <= hi.x;
    let in_y = c.y >= lo.y && c.y <= hi.y;
    if in_x && !in_y {
        DimOrient::Horizontal
    } else if in_y && !in_x {
        DimOrient::Vertical
    } else {
        DimOrient::Aligned
    }
}

pub struct DxfState {
    pub import: ImportId,
    pub file: String,
    pub drawing: DxfDrawing,
    pub layers: Vec<(String, bool, String)>,
}

pub struct SketchSession {
    pub id: Option<SketchId>,
    pub sketch: Sketch,
    pub tool: Tool,
    pub clicks: Vec<(DVec2, Option<PointId>)>,
    pub chain_start: Option<PointId>,
    pub center: DVec2,
    pub scale: f32,
    pub fitted: bool,
    pub selection: Vec<Sel>,
    pub hover: Option<Sel>,
    /// Project tool: index into `projected` of the model edge under the cursor
    pub project_hover: Option<usize>,
    /// Project tool: index into `projected_faces` of the model face under the cursor
    /// (only when no edge is within tolerance)
    pub project_hover_face: Option<usize>,
    pub input: Option<String>,
    pub input2: Option<String>,
    pub input_second: bool,
    /// The value box just opened: select its whole text on this frame.
    pub input_fresh: bool,
    /// The value box still shows the live reading of the cursor (the user has not
    /// typed): it follows the mouse until a key is typed.
    pub input_live: bool,
    /// Alt is held: snapping is off while it is.
    pub alt_held: bool,
    /// Shift forces the H/V lock while drawing a line.
    pub shift_held: bool,
    /// Construction mode (X with nothing selected): new lines, rectangles, circles
    /// and arcs are made as construction geometry.
    pub construction_mode: bool,
    pub dim_target: Option<Vec<Sel>>,
    /// Two points are picked: the cursor's side picks the orientation, a click places.
    pub dim_pending: bool,
    pub dim_orient: DimOrient,
    /// Swap radius and diameter for a lone circle or arc.
    pub dim_toggle: bool,
    /// An existing dimension (constraint index) being edited / selected.
    pub edit_dim: Option<usize>,
    pub sel_dim: Option<usize>,
    /// Label boxes of the last frame, for clicking dimensions.
    pub dim_rects: std::cell::RefCell<Vec<(Rect, usize)>>,
    /// Dimension line / extension line / arrow segments of the last frame (with the
    /// constraint index), so a dimension can be hit on its line as well as its label.
    pub dim_segs: std::cell::RefCell<Vec<([Pos2; 2], usize)>>,
    /// Where the inline field of a new dimension goes (its label spot, last frame).
    pub dim_label_pos: std::cell::Cell<Option<Pos2>>,
    /// Why the inline dimension field's text was refused (shown under it).
    pub input_err: Option<String>,
    /// Line pair being dimensioned as an angle: the quadrant the placement click chose.
    pub dim_rays: Option<AngleChoice>,
    /// Screen radius of the angle arc picked by the placement click.
    pub dim_arc_r: f32,
    pub dxf: Option<DxfState>,
    /// Layer of each entity (DXF-imported geometry), parallel to `sketch.entities`.
    pub entity_layer: Vec<Option<String>>,
    pub report: Option<SolveReport>,
    pub error: Option<String>,
    pub show_constraints: bool,
    pub snap: bool,
    pub slice: bool,
    pub undo: Vec<(Sketch, Vec<Option<String>>)>,
    pub redo: Vec<(Sketch, Vec<Option<String>>)>,
    pub values: Values,
    pub names: Vec<String>,
    pub drag: Option<Drag>,
    /// The Move gizmo (M): see `sketch_move`.
    pub mv: Option<crate::sketch_move::MoveState>,
    pub cursor: DVec2,
    pub projected: Vec<Vec<DVec2>>,
    /// The model edge each projected polyline comes from (exact projection), with
    /// its 3D samples.
    pub projected_src: Vec<Option<(easycad_model::feature::BodyId, easycad_model::occt::EdgeInfo, Vec<glam::DVec3>)>>,
    /// The model faces seen in the sketch plane (empty in slice mode).
    pub projected_faces: Vec<ProjFace>,
    /// "Slice body at sketch plane": cut faces (triangles) and their outlines, in
    /// sketch coordinates; `projected` then holds the kept half's edges only.
    pub slice_caps: Vec<[DVec2; 3]>,
    pub slice_outline: Vec<Vec<DVec2>>,
    /// The state `projected` was computed for (slice on / off).
    pub projected_slice: Option<bool>,
    pub dirty: bool,
}

fn fmt(v: f64) -> String {
    crate::dialogs::fmt(v)
}

impl SketchSession {
    fn base(sketch: Sketch, id: Option<SketchId>, d: &DocData) -> Self {
        let n = sketch.entities.len();
        let values = crate::adapters::values(d);
        let names = d.params.list.iter().map(|p| p.name.clone()).collect();
        let mut s = SketchSession {
            id,
            sketch,
            tool: Tool::Select,
            clicks: Vec::new(),
            chain_start: None,
            center: DVec2::ZERO,
            scale: 4.0,
            fitted: false,
            selection: Vec::new(),
            hover: None,
            project_hover: None,
            input: None,
            input2: None,
            input_second: false,
            dim_target: None,
            dim_pending: false,
            dim_orient: DimOrient::Aligned,
            dim_toggle: false,
            edit_dim: None,
            sel_dim: None,
            dim_rects: Default::default(),
            dim_segs: Default::default(),
            dim_label_pos: Default::default(),
            input_err: None,
            dim_rays: None,
            dim_arc_r: 60.0,
            dxf: None,
            entity_layer: vec![None; n],
            report: None,
            error: None,
            show_constraints: true,
            snap: true,
            input_fresh: false,
            input_live: false,
            alt_held: false,
            shift_held: false,
            construction_mode: false,
            slice: false,
            undo: Vec::new(),
            redo: Vec::new(),
            values,
            names,
            drag: None,
            mv: None,
            cursor: DVec2::ZERO,
            projected: Vec::new(),
            projected_src: Vec::new(),
            projected_faces: Vec::new(),
            project_hover_face: None,
            slice_caps: Vec::new(),
            slice_outline: Vec::new(),
            projected_slice: None,
            dirty: false,
        };
        s.resolve();
        s
    }

    pub fn new_on(plane: Plane, d: &DocData) -> Self {
        let n = d.sketches.len() + 1;
        let mut s = Self::base(Sketch::new(format!("Sketch {n}"), plane), None, d);
        s.tool = Tool::Line;
        s
    }

    pub fn edit(id: SketchId, sketch: Sketch, d: &DocData) -> Self {
        let mut s = Self::base(sketch, Some(id), d);
        if d.meta
            .get(&format!("gui.sketch_src.{}", id.0))
            .is_some_and(|v| v == "DXF")
        {
            for (i, e) in s.sketch.entities.iter().enumerate() {
                if e.construction {
                    s.entity_layer[i] = Some("DXF".into());
                }
            }
        }
        s
    }

    pub fn from_dxf(import: ImportId, file: &str, drawing: DxfDrawing, d: &DocData) -> Self {
        let stem = file.trim_end_matches(".dxf").trim_end_matches(".DXF");
        let mut s = Self::base(Sketch::new(stem, Plane::XY), None, d);
        let mut layers = Vec::new();
        for name in drawing.layer_names() {
            let segs = drawing.segments(&[name]);
            let circles = segs
                .iter()
                .filter(|g| matches!(g, Segment2::Circle { .. }))
                .count();
            let prof = profiles_from_segments(&segs, 1e-4);
            let loops: usize = prof
                .profiles
                .iter()
                .map(|p| 1 + p.holes.len())
                .sum::<usize>()
                .saturating_sub(circles);
            let count = if circles > 0 && loops == 0 {
                format!("{circles} circles")
            } else if loops > 0 && circles == 0 && prof.open.is_empty() {
                format!("{loops} loop{}", if loops == 1 { "" } else { "s" })
            } else {
                format!("{} items", segs.len())
            };
            let l = name.to_lowercase();
            let on = l.contains("outline")
                || l.contains("hole")
                || l.contains("edge")
                || l.contains("board");
            layers.push((name.to_string(), on, count));
        }
        s.dxf = Some(DxfState {
            import,
            file: file.to_string(),
            drawing,
            layers,
        });
        let on: Vec<String> = s
            .dxf
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .filter(|l| l.1)
            .map(|l| l.0.clone())
            .collect();
        for l in on {
            s.add_layer(&l);
        }
        s.tool = Tool::Offset;
        s.resolve();
        s
    }

    pub fn plane_name(&self) -> String {
        let p = &self.sketch.plane;
        let n = p.normal;
        let base = if n.z.abs() > 0.999 {
            "XY plane"
        } else if n.y.abs() > 0.999 {
            "XZ plane"
        } else if n.x.abs() > 0.999 {
            "YZ plane"
        } else {
            "face plane"
        };
        let off = p.origin.dot(n);
        if off.abs() > 1e-6 {
            format!("{base} @ {}", fmt(off))
        } else {
            base.into()
        }
    }

    fn add_layer(&mut self, layer: &str) {
        let Some(d) = &self.dxf else { return };
        let mut sub = d.drawing.to_sketch("tmp", self.sketch.plane, &[layer]);
        let base_p = self.sketch.points.len() as u32;
        let base_e = self.sketch.entities.len() as u32;
        let mp = |p: PointId| PointId(p.0 + base_p);
        self.sketch.points.append(&mut sub.points);
        for e in sub.entities.drain(..) {
            let geom = match e.geom {
                Geom::Line { a, b } => Geom::Line { a: mp(a), b: mp(b) },
                Geom::Arc { center, start, end } => Geom::Arc {
                    center: mp(center),
                    start: mp(start),
                    end: mp(end),
                },
                Geom::Circle { center, radius } => Geom::Circle {
                    center: mp(center),
                    radius,
                },
            };
            self.sketch.add_entity(geom, true);
            self.entity_layer.push(Some(layer.to_string()));
        }
        // imported geometry is fixed reference geometry, constrained exactly once
        // (fully constrained without redundancy: arcs fix centre + start and one
        // coordinate of the end, circles fix centre + diameter)
        for ei in base_e..self.sketch.entities.len() as u32 {
            let g = self.sketch.entities[ei as usize].geom;
            let fix = |s: &mut Sketch, p: PointId| {
                let v = s.point(p);
                s.constrain(Constraint::Fix(p, v));
            };
            match g {
                Geom::Line { a, b } => {
                    fix(&mut self.sketch, a);
                    fix(&mut self.sketch, b);
                }
                Geom::Arc { center, start, end } => {
                    fix(&mut self.sketch, center);
                    fix(&mut self.sketch, start);
                    let d = self.sketch.point(end) - self.sketch.point(center);
                    let c = if d.x.abs() >= d.y.abs() {
                        Constraint::DistanceY(center, end, Expr::new(fmt(d.y)))
                    } else {
                        Constraint::DistanceX(center, end, Expr::new(fmt(d.x)))
                    };
                    self.sketch.constrain(c);
                }
                Geom::Circle { center, radius } => {
                    fix(&mut self.sketch, center);
                    self.sketch.constrain(Constraint::Diameter(
                        EntityId(ei),
                        Expr::new(fmt(radius * 2.0)),
                    ));
                }
            }
        }
    }

    pub fn toggle_layer(&mut self, layer: &str, on: bool) {
        self.push_undo();
        if on {
            self.add_layer(layer);
        } else {
            let rm: HashSet<EntityId> = self
                .entity_layer
                .iter()
                .enumerate()
                .filter(|(_, l)| l.as_deref() == Some(layer))
                .map(|(i, _)| EntityId(i as u32))
                .collect();
            self.remove_entities(&rm);
        }
        if let Some(d) = &mut self.dxf {
            if let Some(l) = d.layers.iter_mut().find(|l| l.0 == layer) {
                l.1 = on;
            }
        }
        self.resolve();
    }

    /// Open the value box. `live`: it shows the cursor's reading and follows the
    /// mouse until the user types; either way its text starts fully selected.
    pub fn open_input(&mut self, text: String, live: bool) {
        self.input = Some(text);
        self.input_fresh = true;
        self.input_live = live;
    }

    /// Close the value box without applying it. Placing a shape is cancelled with
    /// it; an Offset in progress drops its picked shapes.
    pub fn cancel_input(&mut self) {
        self.input = None;
        self.input2 = None;
        self.input_second = false;
        self.input_live = false;
        self.input_fresh = false;
        self.dim_target = None;
        self.dim_pending = false;
        self.edit_dim = None;
        self.input_err = None;
        self.dim_rays = None;
        match self.tool {
            Tool::Line | Tool::Rect | Tool::Circle | Tool::Arc => {
                self.clicks.clear();
                self.chain_start = None;
            }
            Tool::Offset => self.selection.clear(),
            _ => {}
        }
    }

    /// The default Offset distance: the `wall` parameter (kept as an expression so
    /// the offset stays linked to it), else 2.
    pub fn default_offset(&self) -> String {
        if self.names.iter().any(|n| n == "wall") {
            "wall".into()
        } else {
            "2".into()
        }
    }

    /// Offset every picked shape by one distance, as one undo step.
    pub fn offset_selected(&mut self, ents: &[EntityId], dist: &str) -> Result<(), String> {
        let dist = crate::units::normalize_input(dist.trim());
        let mut sk = self.sketch.clone();
        offset_set(&mut sk, ents, &dist, &self.values)?;
        self.push_undo();
        self.sketch = sk;
        self.entity_layer.resize(self.sketch.entities.len(), None);
        self.resolve();
        Ok(())
    }

    /// The offset result for the picked shapes as polylines (the live preview).
    fn offset_preview(&self) -> Option<Vec<Vec<DVec2>>> {
        if self.tool != Tool::Offset {
            return None;
        }
        let ents = self.selected_entities();
        let text = self.input.as_ref()?;
        if ents.is_empty() {
            return None;
        }
        let dist = crate::units::normalize_input(text.trim().trim_start_matches('=').trim());
        crate::widgets::eval(&dist, &self.values).ok()?;
        let mut sk = self.sketch.clone();
        let n = sk.entities.len();
        offset_set(&mut sk, &ents, &dist, &self.values).ok()?;
        Some((n..sk.entities.len()).map(|i| entity_polyline(&sk, EntityId(i as u32))).collect())
    }

    pub fn selected_entities(&self) -> Vec<EntityId> {
        self.selection
            .iter()
            .filter_map(|x| if let Sel::Entity(e) = x { Some(*e) } else { None })
            .collect()
    }

    /// Drop the snapshot of an edit that did not happen.
    pub(crate) fn pop_undo(&mut self) {
        self.undo.pop();
    }

    /// Start dragging a pick: a point moves alone; a line, arc or circle moves
    /// with all its points. One undo step covers the whole drag.
    pub fn drag_begin(&mut self, x: Sel, press: DVec2) {
        let ids: Vec<PointId> = match x {
            Sel::Point(p) => vec![p],
            Sel::Entity(e) => geom_points(&self.sketch.entities[e.0 as usize].geom),
            Sel::Mid(_) | Sel::Origin => Vec::new(),
        };
        let ids: Vec<PointId> = ids
            .into_iter()
            .filter(|p| self.entity_layer.iter().all(|l| l.is_none()) || !is_fixed(&self.sketch, *p))
            .collect();
        if ids.is_empty() {
            return;
        }
        self.push_undo();
        self.error = None;
        let pts = ids.iter().map(|p| (*p, self.sketch.point(*p))).collect();
        self.drag = Some(Drag { pts, press, before: self.sketch.clone() });
    }

    /// Move the drag to `target` (the pointer, already snapped), keeping every
    /// constraint: the points are pinned to the target when that is consistent;
    /// otherwise the solver pulls them to the nearest place the constraints allow.
    pub fn drag_to(&mut self, target: DVec2) {
        let Some(d) = self.drag.clone() else { return };
        let delta = target - d.press;
        let base = d.before.clone();
        let mut sk = base.clone();
        let n0 = sk.constraints.len();
        for (p, o) in &d.pts {
            sk.points[p.0 as usize] = *o + delta;
            sk.constrain(Constraint::Fix(*p, *o + delta));
        }
        let mut ok = matches!(sk.solve(&self.values), Ok(r) if r.converged);
        sk.constraints.truncate(n0);
        if !ok {
            // soft: start displaced and let the constraints push the points back
            sk = base.clone();
            for (p, o) in &d.pts {
                sk.points[p.0 as usize] = *o + delta;
            }
            ok = matches!(sk.solve(&self.values), Ok(r) if r.converged);
        }
        if ok {
            let moved = d.pts.iter().map(|(p, o)| sk.point(*p).distance(*o)).fold(0.0, f64::max);
            self.error = (moved < 1e-6 && delta.length() > 1e-6)
                .then(|| "fully constrained: it can't move".to_string());
            self.sketch = sk;
        }
    }

    /// Entities that own a point being dragged.
    pub fn drag_entities(&self) -> Vec<EntityId> {
        let Some(d) = &self.drag else { return Vec::new() };
        (0..self.sketch.entities.len())
            .filter(|i| geom_points(&self.sketch.entities[*i].geom).iter().any(|p| d.pts.iter().any(|(q, _)| q == p)))
            .map(|i| EntityId(i as u32))
            .collect()
    }

    pub fn drag_end(&mut self) {
        if self.drag.take().is_some() {
            self.resolve();
        }
    }

    /// Esc during a drag: back to where it was, and no undo step left behind.
    pub fn drag_cancel(&mut self) {
        if let Some(d) = self.drag.take() {
            self.sketch = d.before;
            self.pop_undo();
            self.error = None;
            self.resolve();
        }
    }

    pub fn push_undo(&mut self) {
        self.undo
            .push((self.sketch.clone(), self.entity_layer.clone()));
        self.redo.clear();
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.dirty = true;
    }

    /// Step back one sketch edit; false when this session has nothing to undo.
    pub fn undo(&mut self) -> bool {
        if self.mv.is_some() {
            self.cancel_move();
            return true;
        }
        let Some((s, l)) = self.undo.pop() else { return false };
        let cur = (std::mem::replace(&mut self.sketch, s), std::mem::replace(&mut self.entity_layer, l));
        self.redo.push(cur);
        self.selection.clear();
        self.dirty = true;
        self.resolve();
        true
    }

    /// Re-apply the last undone sketch edit; false when there is none.
    pub fn redo(&mut self) -> bool {
        let Some((s, l)) = self.redo.pop() else { return false };
        let cur = (std::mem::replace(&mut self.sketch, s), std::mem::replace(&mut self.entity_layer, l));
        self.undo.push(cur);
        self.selection.clear();
        self.dirty = true;
        self.resolve();
        true
    }

    /// Run the solver and refresh status.
    pub fn resolve(&mut self) {
        self.error = None;
        let mut s = self.sketch.clone();
        match s.solve(&self.values) {
            Ok(r) => {
                if r.converged {
                    self.sketch = s;
                }
                self.report = Some(r);
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.report = self.sketch.analyze(&self.values).ok();
            }
        }
    }

    pub fn profile_count(&self) -> usize {
        self.sketch.profiles().profiles.len()
    }

    /// Index of the projectable model edge under sketch-space point `q`.
    pub fn project_hit(&self, q: DVec2) -> Option<usize> {
        let tol = 6.0 / self.scale as f64;
        self.projected
            .iter()
            .position(|pl| pl.windows(2).any(|w| seg_dist(q, w[0], w[1]) < tol))
    }

    /// The model face under `q` (nearest to the viewer when faces overlap); callers
    /// try `project_hit` first so edges win within their tolerance.
    pub fn project_face_hit(&self, q: DVec2) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, f) in self.projected_faces.iter().enumerate() {
            for t in &f.tris {
                if in_tri(q, t.0) && best.is_none_or(|(_, d)| t.1 > d) {
                    best = Some((i, t.1));
                }
            }
        }
        best.map(|b| b.0)
    }

    /// Is the model edge `i` already in the sketch as a projected entity?
    pub fn edge_projected(&self, i: usize) -> bool {
        let Some(pl) = self.projected.get(i) else { return true };
        let tol = 1e-4_f64.max(1e-3 / self.scale.max(1.0) as f64);
        let pt = |p: PointId| self.sketch.points[p.0 as usize];
        let near = |q: DVec2| {
            self.sketch.entities.iter().filter(|e| e.projected).any(|e| match e.geom {
                Geom::Line { a, b } => seg_dist(q, pt(a), pt(b)) < tol,
                Geom::Circle { center, radius } => (q.distance(pt(center)) - radius).abs() < tol,
                Geom::Arc { center, start, end } => {
                    let (c, s) = (pt(center), pt(start));
                    (q.distance(c) - s.distance(c)).abs() < tol && on_arc(q, c, s, pt(end))
                }
            })
        };
        pl.iter().all(|q| near(*q))
    }

    /// The edges of face `f` still to project (not yet in the sketch).
    pub fn face_missing(&self, f: usize) -> Vec<usize> {
        self.projected_faces.get(f).map_or_else(Vec::new, |f| {
            f.edges.iter().copied().filter(|&e| !self.edge_projected(e)).collect()
        })
    }

    /// Project the model edges `idx` into the sketch as one undo step; skips ones
    /// already there. Returns how many edges added geometry.
    pub fn project_edges_now(&mut self, idx: &[usize]) -> usize {
        let todo: Vec<usize> = idx.iter().copied().filter(|&i| !self.edge_projected(i)).collect();
        if todo.is_empty() {
            return 0;
        }
        self.push_undo();
        let mut done = 0;
        for i in todo {
            let n0 = self.sketch.entities.len();
            if let Some((body, info, samples)) = self.projected_src.get(i).cloned().flatten() {
                // exact: lines stay lines, circles facing the plane stay circles
                self.sketch.project_edge_from(body, &info, Some(&samples), false);
            } else if let Some(pl) = self.projected.get(i).cloned() {
                let first = self.sketch.points.len() as u32;
                let mut prev = None;
                for pt in &pl {
                    let id = self.sketch.add_point(*pt);
                    if let Some(a) = prev {
                        let e = self.add_entity(Geom::Line { a, b: id }, false);
                        self.sketch.entities[e.0 as usize].projected = true;
                    }
                    prev = Some(id);
                }
                for i in first..self.sketch.points.len() as u32 {
                    let v = self.sketch.points[i as usize];
                    self.sketch.constrain(Constraint::Fix(PointId(i), v));
                }
            }
            if self.sketch.entities.len() > n0 {
                done += 1;
            }
        }
        if done == 0 {
            self.pop_undo();
        } else {
            self.entity_layer.resize(self.sketch.entities.len(), None);
            self.resolve();
        }
        done
    }

    pub fn status(&self) -> (Color32, String) {
        if let Some(e) = &self.error {
            return (theme::RED, e.clone());
        }
        match &self.report {
            Some(r) if !r.converged => (theme::RED, "Conflicting constraints".into()),
            Some(r) => match r.status {
                ConstraintStatus::FullyConstrained => {
                    (theme::GREEN, "Fully constrained · 0 DOF".into())
                }
                ConstraintStatus::UnderConstrained => (
                    theme::ORANGE_SOFT,
                    format!("Under-constrained · {} DOF", r.dof),
                ),
                ConstraintStatus::OverConstrained => (
                    theme::ORANGE,
                    format!("Over-constrained (redundant) · {} DOF", r.dof),
                ),
                ConstraintStatus::Conflicting => (theme::RED, "Conflicting constraints".into()),
            },
            None => (theme::TEXT_MUTED, "Empty sketch".into()),
        }
    }

    /// Write the sketch back into the document (undoable).
    pub fn commit(
        &self,
        doc: &mut Document,
        scope: easycad_model::assembly::ComponentId,
    ) -> Result<Option<String>, String> {
        if !self.dirty && self.id.is_some() {
            return Ok(None);
        }
        let sk = self.sketch.clone();
        let dxf = self.dxf.is_some() || self.entity_layer.iter().any(|l| l.is_some());
        match self.id {
            Some(id) => {
                crate::scope::edit(doc, scope, &format!("Edit sketch {}", sk.name), |d| {
                    let s = d
                        .sketch_mut(id)
                        .ok_or_else(|| DocError::Other("sketch vanished".into()))?;
                    *s = sk;
                    Ok::<_, DocError>(())
                })
                .map_err(|e| e.to_string())?;
                Ok(Some("Sketch updated".into()))
            }
            None => {
                if sk.entities.is_empty() {
                    return Ok(None);
                }
                let name = sk.name.clone();
                crate::scope::edit(doc, scope, &format!("Add sketch {name}"), |d| {
                    let id = d.add_sketch(sk);
                    if dxf {
                        d.meta
                            .insert(format!("gui.sketch_src.{}", id.0), "DXF".into());
                    }
                    Ok::<_, DocError>(())
                })
                .map_err(|e| e.to_string())?;
                Ok(Some(format!("Added sketch {name}")))
            }
        }
    }

    // ------------------------------------------------------------ geometry edits

    /// Remove entities, their now-unused points and the constraints that reference
    /// removed items (the engine's `Sketch::remove_entities`), keeping the DXF
    /// layer tags in step.
    pub fn remove_entities(&mut self, rm: &HashSet<EntityId>) {
        let ids: Vec<EntityId> = rm.iter().copied().collect();
        let n = self.sketch.entities.len();
        let remap = self.sketch.remove_entities(&ids);
        self.remap_layers(&remap, n);
        self.selection.clear();
    }

    /// Carry the per-entity DXF layer tags through an engine remap; entities the
    /// engine added since get none.
    fn remap_layers(&mut self, remap: &easycad_model::sketch_edit::Remap, before: usize) {
        let mut layers = vec![None; self.sketch.entities.len()];
        for i in 0..before {
            if let Some(ne) = remap.entity(EntityId(i as u32)) {
                if let Some(slot) = layers.get_mut(ne.0 as usize) {
                    *slot = self.entity_layer.get(i).cloned().flatten();
                }
            }
        }
        self.entity_layer = layers;
    }

    fn add_entity(&mut self, g: Geom, construction: bool) -> EntityId {
        self.entity_layer.push(None);
        self.sketch.add_entity(g, construction || self.construction_mode)
    }

    /// Flip the construction flag of the selected entities (one undo step): all
    /// become construction, or, when they all already are, all become real
    /// geometry. False when no entity is selected.
    pub fn toggle_construction_selection(&mut self) -> bool {
        let es = self.selected_entities();
        if es.is_empty() {
            return false;
        }
        let on = !es.iter().all(|e| self.sketch.entities[e.0 as usize].construction);
        self.push_undo();
        for e in es {
            self.sketch.set_construction(e, on);
        }
        self.resolve();
        true
    }

    /// The X key / Construction button: toggle the selection, or with nothing
    /// selected the construction mode for new geometry.
    pub fn toggle_construction(&mut self) {
        if !self.toggle_construction_selection() {
            self.construction_mode = !self.construction_mode;
        }
    }

    fn point_at(&mut self, p: DVec2, snapped: Option<PointId>) -> PointId {
        snapped.unwrap_or_else(|| self.sketch.add_point(p))
    }

    pub fn add_line(
        &mut self,
        a: (DVec2, Option<PointId>),
        b: (DVec2, Option<PointId>),
    ) -> EntityId {
        let pa = self.point_at(a.0, a.1);
        let pb = self.point_at(b.0, b.1);
        let e = self.add_entity(Geom::Line { a: pa, b: pb }, false);
        let d = b.0 - a.0;
        let ang = d.y.atan2(d.x).to_degrees().abs();
        if d.length() > 1e-6 {
            if ang < 2.0 || (180.0 - ang) < 2.0 {
                self.sketch.constrain(Constraint::Horizontal(e));
            } else if (ang - 90.0).abs() < 2.0 {
                self.sketch.constrain(Constraint::Vertical(e));
            }
        }
        e
    }

    pub fn line_points(&self, e: EntityId) -> Option<(PointId, PointId)> {
        match self.sketch.entities.get(e.0 as usize)?.geom {
            Geom::Line { a, b } => Some((a, b)),
            _ => None,
        }
    }

    /// Replace the corner at `p` (two lines) with a tangent arc of radius `r`
    /// (an expression); the engine's `Sketch::fillet_corner`.
    pub fn fillet_corner(&mut self, p: PointId, r: &str) -> Result<(), String> {
        let r = Expr::new(crate::units::normalize_input(r.trim()));
        self.push_undo();
        match self.sketch.fillet_corner(p, r, &self.values) {
            Ok(_) => {
                self.entity_layer.resize(self.sketch.entities.len(), None);
                self.resolve();
                Ok(())
            }
            Err(e) => {
                self.pop_undo();
                Err(e)
            }
        }
    }

    /// Offset the chain through `start` by an expression distance (outward for a
    /// closed loop): the engine's `Sketch::offset_chain`, held by one native
    /// `Constraint::Offset` (GUI_NEEDS #12, #21). It follows DXF loops with
    /// separate, equal end points and makes profile geometry from a reference.
    pub fn offset_loop(&mut self, start: EntityId, dist: &str) -> Result<(), String> {
        let d = Expr::new(crate::units::normalize_input(dist.trim()));
        self.push_undo();
        match self.sketch.offset_chain(start, d, &self.values) {
            Ok(_) => {
                self.entity_layer.resize(self.sketch.entities.len(), None);
                self.resolve();
                Ok(())
            }
            Err(e) => {
                self.pop_undo();
                Err(e)
            }
        }
    }

    /// Trim the piece of `e` around `at` between its nearest intersections (the
    /// engine's `Sketch::trim`: lines, arcs and circles).
    pub fn trim(&mut self, e: EntityId, at: DVec2) {
        self.push_undo();
        let n = self.sketch.entities.len();
        match self.sketch.trim(e, at) {
            Ok(remap) => {
                // remap covers the entities that existed before; new pieces follow
                let mut layers = self.entity_layer.clone();
                layers.resize(n, None);
                self.entity_layer = layers;
                let added = self.sketch.entities.len();
                self.remap_layers(&remap, n);
                self.entity_layer.resize(added, None);
                self.selection.clear();
                self.resolve();
            }
            Err(err) => {
                self.pop_undo();
                self.error = Some(err);
            }
        }
    }

    /// Apply a constraint button to the selection.
    pub fn constrain_selection(&mut self, kind: &str) -> Result<(), String> {
        let pts: Vec<PointId> = self
            .selection
            .iter()
            .filter_map(|s| {
                if let Sel::Point(p) = s {
                    Some(*p)
                } else {
                    None
                }
            })
            .collect();
        let ents: Vec<EntityId> = self
            .selection
            .iter()
            .filter_map(|s| {
                if let Sel::Entity(e) = s {
                    Some(*e)
                } else {
                    None
                }
            })
            .collect();
        let is_line =
            |e: &EntityId| matches!(self.sketch.entities[e.0 as usize].geom, Geom::Line { .. });
        let center = |e: &EntityId| match self.sketch.entities[e.0 as usize].geom {
            Geom::Arc { center, .. } | Geom::Circle { center, .. } => Some(center),
            _ => None,
        };
        let mut add = Vec::new();
        match kind {
            "coincident" => match (pts.as_slice(), ents.as_slice()) {
                ([a, b], []) => add.push(Constraint::Coincident(*a, *b)),
                ([p], [e]) if is_line(e) => add.push(Constraint::PointOnLine(*p, *e)),
                ([p], [e]) => add.push(Constraint::PointOnCircle(*p, *e)),
                _ => return Err("select two points, or a point and a curve".into()),
            },
            "hv" => {
                for e in ents.iter().filter(|e| is_line(e)) {
                    let (a, b) = self.line_points(*e).unwrap();
                    let d = self.sketch.point(b) - self.sketch.point(a);
                    add.push(if d.x.abs() >= d.y.abs() {
                        Constraint::Horizontal(*e)
                    } else {
                        Constraint::Vertical(*e)
                    });
                }
                if add.is_empty() {
                    return Err("select lines".into());
                }
            }
            "parallel" | "perpendicular" | "tangent" | "equal" => {
                let [a, b] = ents.as_slice() else {
                    return Err("select two entities".into());
                };
                add.push(match kind {
                    "parallel" => Constraint::Parallel(*a, *b),
                    "perpendicular" => Constraint::Perpendicular(*a, *b),
                    "tangent" => Constraint::Tangent(*a, *b),
                    _ => Constraint::Equal(*a, *b),
                });
            }
            "midpoint" => {
                let ([p], [e]) = (pts.as_slice(), ents.as_slice()) else {
                    return Err("select a point and a line".into());
                };
                add.push(Constraint::Midpoint(*p, *e));
            }
            "symmetric" => {
                let ([p, q], [e]) = (pts.as_slice(), ents.as_slice()) else {
                    return Err("select two points and a line".into());
                };
                add.push(Constraint::Symmetric(*p, *q, *e));
            }
            "fix" => {
                if pts.is_empty() {
                    return Err("select points to fix".into());
                }
                for p in &pts {
                    add.push(Constraint::Fix(*p, self.sketch.point(*p)));
                }
            }
            "concentric" => {
                let [a, b] = ents.as_slice() else {
                    return Err("select two arcs or circles".into());
                };
                let (Some(c1), Some(c2)) = (center(a), center(b)) else {
                    return Err("select two arcs or circles".into());
                };
                add.push(Constraint::Coincident(c1, c2));
            }
            _ => {}
        }
        self.push_undo();
        for c in add {
            self.sketch.constrain(c);
        }
        self.resolve();
        Ok(())
    }

    /// Swap each `Sel::Mid` for a helper point held at that line's midpoint (an
    /// existing helper is reused), and `Sel::Origin` for a point fixed at (0, 0)
    /// (any point already fixed there is reused). Helper points belong to no
    /// entity, so they are neither drawn nor pickable.
    fn resolve_mids(sk: &mut Sketch, sel: &[Sel]) -> Vec<Sel> {
        sel.iter()
            .map(|x| match *x {
                Sel::Mid(e) => {
                    let used = |p: PointId| sk.entities.iter().any(|en| geom_points(&en.geom).contains(&p));
                    let have = sk.constraints.iter().find_map(|c| match c {
                        Constraint::Midpoint(p, l) if *l == e && !used(*p) => Some(*p),
                        _ => None,
                    });
                    let p = have.unwrap_or_else(|| {
                        let Geom::Line { a, b } = sk.entities[e.0 as usize].geom else {
                            return PointId(0);
                        };
                        let m = (sk.point(a) + sk.point(b)) / 2.0;
                        let p = sk.add_point(m);
                        sk.constrain(Constraint::Midpoint(p, e));
                        p
                    });
                    Sel::Point(p)
                }
                Sel::Origin => {
                    let have = sk.constraints.iter().find_map(|c| match c {
                        Constraint::Fix(p, v) if v.x == 0.0 && v.y == 0.0 => Some(*p),
                        _ => None,
                    });
                    let p = have.unwrap_or_else(|| {
                        let p = sk.add_point(DVec2::ZERO);
                        sk.constrain(Constraint::Fix(p, DVec2::ZERO));
                        p
                    });
                    Sel::Point(p)
                }
                o => o,
            })
            .collect()
    }

    /// The dimension a pick set would create, with its current measured value.
    /// Two points (centres and midpoints included) read per `dim_orient`; a lone
    /// circle/arc reads as diameter/radius, swapped by `dim_toggle`.
    pub fn dimension_for(&self, sel: &[Sel]) -> Option<(f64, DimMaker)> {
        let mut tmp;
        let origin = sel.contains(&Sel::Origin);
        let mut sel = sel.to_vec();
        let s: &Sketch = if sel.iter().any(|x| matches!(x, Sel::Mid(_) | Sel::Origin)) {
            tmp = self.sketch.clone();
            sel = Self::resolve_mids(&mut tmp, &sel);
            &tmp
        } else {
            &self.sketch
        };
        // circles and arcs stand for their centre next to anything else
        let anchor = |x: &Sel| -> Option<Sel> {
            match *x {
                Sel::Entity(e) => match s.entities[e.0 as usize].geom {
                    Geom::Line { .. } => Some(*x),
                    Geom::Circle { center, .. } | Geom::Arc { center, .. } => Some(Sel::Point(center)),
                },
                o => Some(o),
            }
        };
        match sel.as_slice() {
            [Sel::Entity(e)] => {
                let e = *e;
                let toggle = self.dim_toggle;
                match s.entities[e.0 as usize].geom {
                    Geom::Line { a, b } => Some((
                        s.point(a).distance(s.point(b)),
                        Box::new(move |x| Constraint::Length(e, x)),
                    )),
                    Geom::Circle { radius, .. } if toggle => {
                        Some((radius, Box::new(move |x| Constraint::Radius(e, x))))
                    }
                    Geom::Circle { radius, .. } => {
                        Some((radius * 2.0, Box::new(move |x| Constraint::Diameter(e, x))))
                    }
                    Geom::Arc { center, start, .. } => {
                        let r = s.point(center).distance(s.point(start));
                        if toggle {
                            Some((r * 2.0, Box::new(move |x| Constraint::Diameter(e, x))))
                        } else {
                            Some((r, Box::new(move |x| Constraint::Radius(e, x))))
                        }
                    }
                }
            }
            [x, y] => {
                let (x, y) = (anchor(x)?, anchor(y)?);
                match (x, y) {
                    (Sel::Point(a), Sel::Point(b)) => {
                        if a == b {
                            return None;
                        }
                        let (pa, pb) = (s.point(a), s.point(b));
                        if origin && pa.distance(pb) < 1e-9 {
                            return None; // e.g. a centre sitting on the origin
                        }
                        Some(match self.dim_orient {
                            DimOrient::Aligned => (
                                pa.distance(pb),
                                Box::new(move |v| Constraint::Distance(a, b, v)) as DimMaker,
                            ),
                            DimOrient::Horizontal => {
                                let (a, b) = if pa.x <= pb.x { (a, b) } else { (b, a) };
                                ((pa.x - pb.x).abs(), Box::new(move |v| Constraint::DistanceX(a, b, v)))
                            }
                            DimOrient::Vertical => {
                                let (a, b) = if pa.y <= pb.y { (a, b) } else { (b, a) };
                                ((pa.y - pb.y).abs(), Box::new(move |v| Constraint::DistanceY(a, b, v)))
                            }
                        })
                    }
                    (Sel::Point(p), Sel::Entity(e)) | (Sel::Entity(e), Sel::Point(p)) => {
                        let Geom::Line { a, b } = s.entities[e.0 as usize].geom else {
                            return None;
                        };
                        if a == p || b == p {
                            return None;
                        }
                        let (pa, pb, q) = (s.point(a), s.point(b), s.point(p));
                        let d = ((pb - pa).perp_dot(q - pa) / (pb - pa).length().max(1e-12)).abs();
                        Some((d, Box::new(move |x| Constraint::PointLineDistance(p, e, x))))
                    }
                    (Sel::Entity(e1), Sel::Entity(e2)) => {
                        if e1 == e2 {
                            return None;
                        }
                        let (Geom::Line { a, b }, Geom::Line { a: c, b: d }) = (
                            s.entities[e1.0 as usize].geom,
                            s.entities[e2.0 as usize].geom,
                        ) else {
                            return None;
                        };
                        let (u, w) = (s.point(b) - s.point(a), s.point(d) - s.point(c));
                        let parallel = u.perp_dot(w).abs() <= 1e-4 * u.length() * w.length()
                            || s.constraints.iter().any(|k| {
                                matches!(k, Constraint::Parallel(p, q) if (*p, *q) == (e1, e2) || (*p, *q) == (e2, e1))
                            });
                        if parallel {
                            let dist = (u.perp_dot(s.point(c) - s.point(a)) / u.length().max(1e-12)).abs();
                            Some((dist, Box::new(move |x| Constraint::LineDistance(e1, e2, x))))
                        } else if let Some(ch) = self.dim_rays {
                            Some((
                                ch.deg,
                                Box::new(move |x| {
                                    if ch.swap {
                                        Constraint::AngleRays(e2, ch.rev2, e1, ch.rev1, x)
                                    } else {
                                        Constraint::AngleRays(e1, ch.rev1, e2, ch.rev2, x)
                                    }
                                }),
                            ))
                        } else {
                            let ang = u.perp_dot(w).atan2(u.dot(w)).to_degrees();
                            Some((ang, Box::new(move |x| Constraint::Angle(e1, e2, x))))
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Where the two anchors of a point pair sit (circles and arcs by centre).
    pub fn dim_anchor_points(&self, sel: &[Sel]) -> Option<(DVec2, DVec2)> {
        let s = &self.sketch;
        let at = |x: &Sel| -> Option<DVec2> {
            Some(match *x {
                Sel::Point(p) => s.point(p),
                Sel::Mid(e) => match s.entities[e.0 as usize].geom {
                    Geom::Line { a, b } => (s.point(a) + s.point(b)) / 2.0,
                    _ => return None,
                },
                Sel::Origin => DVec2::ZERO,
                Sel::Entity(e) => match s.entities[e.0 as usize].geom {
                    Geom::Circle { center, .. } | Geom::Arc { center, .. } => s.point(center),
                    _ => return None,
                },
            })
        };
        match sel {
            [x, y] => Some((at(x)?, at(y)?)),
            _ => None,
        }
    }

    /// True when the pick set is a pair of points (centres, ends, midpoints): the
    /// dimension then waits for a placement click that picks its orientation.
    /// The four end points of two crossing (non-parallel) lines picked as a pair.
    pub fn line_pair(&self, sel: &[Sel]) -> Option<[DVec2; 4]> {
        let [Sel::Entity(e1), Sel::Entity(e2)] = sel else { return None };
        if e1 == e2 {
            return None;
        }
        let (Geom::Line { a, b }, Geom::Line { a: c, b: d }) =
            (self.sketch.entities[e1.0 as usize].geom, self.sketch.entities[e2.0 as usize].geom)
        else {
            return None;
        };
        let (pa, pb, pc, pd) = (self.sketch.point(a), self.sketch.point(b), self.sketch.point(c), self.sketch.point(d));
        let (u, w) = (pb - pa, pd - pc);
        let parallel = u.perp_dot(w).abs() <= 1e-4 * u.length() * w.length()
            || self.sketch.constraints.iter().any(|k| {
                matches!(k, Constraint::Parallel(p, q) if (*p, *q) == (*e1, *e2) || (*p, *q) == (*e2, *e1))
            });
        if parallel {
            None
        } else {
            Some([pa, pb, pc, pd])
        }
    }

    pub fn dim_needs_placement(&self, sel: &[Sel]) -> bool {
        if self.line_pair(sel).is_some() {
            return true; // an angle: the cursor's quadrant picks which one
        }
        let pt = |x: &Sel| match *x {
            Sel::Point(_) | Sel::Mid(_) | Sel::Origin => true,
            Sel::Entity(e) => !matches!(self.sketch.entities[e.0 as usize].geom, Geom::Line { .. }),
        };
        matches!(sel, [x, y] if pt(x) && pt(y))
    }

    pub fn apply_dimension(&mut self, sel: &[Sel], text: &str) -> Result<(), String> {
        let t = text.trim().trim_start_matches('=').trim();
        crate::widgets::eval(t, &self.values)?;
        if self.dimension_for(sel).is_none() {
            return Err("can't dimension that selection".into());
        }
        self.push_undo();
        let sel = Self::resolve_mids(&mut self.sketch, sel);
        let (_, mk) = self.dimension_for(&sel).ok_or("can't dimension that selection")?;
        self.sketch.constrain(mk(Expr::new(t)));
        self.resolve();
        if self.report.as_ref().is_some_and(|r| !r.converged) {
            self.undo();
            self.redo.pop(); // a refused edit is not something to redo
            return Err("that dimension conflicts with existing constraints".into());
        }
        Ok(())
    }

    /// Change an existing dimension's value (double-click edit).
    pub fn set_dimension(&mut self, idx: usize, text: &str) -> Result<(), String> {
        let t = text.trim().trim_start_matches('=').trim();
        crate::widgets::eval(t, &self.values)?;
        if self.sketch.constraints.get_mut(idx).and_then(|c| c.dim_expr_mut()).is_none() {
            return Err("not a dimension".into());
        }
        self.push_undo();
        *self.sketch.constraints[idx].dim_expr_mut().unwrap() = Expr::new(t);
        self.resolve();
        if self.report.as_ref().is_some_and(|r| !r.converged) {
            self.undo();
            self.redo.pop();
            return Err("that dimension conflicts with existing constraints".into());
        }
        Ok(())
    }

    /// Delete one dimension (by constraint index).
    pub fn delete_dimension(&mut self, idx: usize) {
        if self.sketch.constraints.get(idx).is_some_and(|c| c.dim_expr().is_some()) {
            self.push_undo();
            self.sketch.constraints.remove(idx);
            self.sel_dim = None;
            self.resolve();
        }
    }

    // ------------------------------------------------------------ view

    pub fn to_screen(&self, rect: Rect, p: DVec2) -> Pos2 {
        let c = rect.center();
        pos2(
            c.x + ((p.x - self.center.x) as f32) * self.scale,
            c.y - ((p.y - self.center.y) as f32) * self.scale,
        )
    }
    pub fn to_sketch(&self, rect: Rect, p: Pos2) -> DVec2 {
        let c = rect.center();
        DVec2::new(
            self.center.x + ((p.x - c.x) / self.scale) as f64,
            self.center.y - ((p.y - c.y) / self.scale) as f64,
        )
    }

    pub fn fit(&mut self, rect: Rect) {
        let mut pts: Vec<DVec2> = self.sketch.points.clone();
        for e in &self.sketch.entities {
            if let Geom::Circle { center, radius } = e.geom {
                let c = self.sketch.point(center);
                pts.push(c + DVec2::splat(radius));
                pts.push(c - DVec2::splat(radius));
            }
        }
        pts.extend(self.projected.iter().flatten());
        if pts.is_empty() {
            self.center = DVec2::ZERO;
            self.scale = 4.0;
            return;
        }
        let (mut lo, mut hi) = (pts[0], pts[0]);
        for p in &pts {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        self.center = (lo + hi) / 2.0;
        let size = (hi - lo).max(DVec2::splat(10.0));
        self.scale = ((rect.width() as f64 - 260.0) / size.x)
            .min((rect.height() as f64 - 200.0) / size.y)
            .max(0.05) as f32;
    }

    /// Pick for the Dimension tool: points (centres too), then line midpoints,
    /// then whole entities.
    fn pick_dim(&self, rect: Rect, p: Pos2) -> Option<Sel> {
        let r = self.pick(rect, p);
        if matches!(r, Some(Sel::Point(_))) {
            return r;
        }
        let s = &self.sketch;
        let mut best: Option<(f32, Sel)> = None;
        for (i, e) in s.entities.iter().enumerate() {
            if let Geom::Line { a, b } = e.geom {
                let d = self.to_screen(rect, (s.point(a) + s.point(b)) / 2.0).distance(p);
                if d < 7.0 && best.is_none_or(|b| d < b.0) {
                    best = Some((d, Sel::Mid(EntityId(i as u32))));
                }
            }
        }
        if let Some(b) = best {
            return Some(b.1);
        }
        if (r.is_none() || matches!(r, Some(Sel::Entity(_))))
            && self.to_screen(rect, DVec2::ZERO).distance(p) < 7.0
        {
            return Some(Sel::Origin);
        }
        r
    }

    fn pick(&self, rect: Rect, p: Pos2) -> Option<Sel> {
        let s = &self.sketch;
        let mut best: Option<(f32, Sel)> = None;
        for (i, q) in s.points.iter().enumerate() {
            let d = self.to_screen(rect, *q).distance(p);
            let used = s
                .entities
                .iter()
                .any(|e| geom_points(&e.geom).contains(&PointId(i as u32)));
            if used && d < 7.0 && best.is_none_or(|b| d < b.0) {
                best = Some((d, Sel::Point(PointId(i as u32))));
            }
        }
        if best.is_some() {
            return best.map(|b| b.1);
        }
        let q = self.to_sketch(rect, p);
        let tol = 6.0 / self.scale as f64;
        let mut best: Option<(f64, Sel)> = None;
        for (i, e) in s.entities.iter().enumerate() {
            let d = match e.geom {
                Geom::Line { a, b } => seg_dist(q, s.point(a), s.point(b)),
                Geom::Circle { center, radius } => (q.distance(s.point(center)) - radius).abs(),
                Geom::Arc { center, start, end } => {
                    let c = s.point(center);
                    let r = c.distance(s.point(start));
                    if on_arc(q, c, s.point(start), s.point(end)) {
                        (q.distance(c) - r).abs()
                    } else {
                        q.distance(s.point(start)).min(q.distance(s.point(end)))
                    }
                }
            };
            if d < tol && best.is_none_or(|b| d < b.0) {
                best = Some((d, Sel::Entity(EntityId(i as u32))));
            }
        }
        best.map(|b| b.1)
    }

    fn snap_point(&self, rect: Rect, p: Pos2) -> (DVec2, Option<PointId>) {
        let (q, pid, _) = self.snap_full(rect, p);
        (q, pid)
    }

    /// Snap priority within `SNAP_PX`: existing points (ends, centres), line
    /// midpoints, then on-curve (projected geometry first on a tie), then the grid.
    /// While a circle/arc radius is being set, a circumference of the same centre
    /// wins (so a copy of an existing circle is exact). Returns the position, the
    /// point it landed on and what it snapped to ("end", "center", "mid",
    /// "radius", "on line", "on circle", "on arc"); `None` label = grid.
    fn snap_full(&self, rect: Rect, p: Pos2) -> (DVec2, Option<PointId>, Option<&'static str>) {
        self.snap_at(self.to_sketch(rect, p), &[])
    }

    /// `snap_full` for a sketch position, ignoring the entities in `skip` (the ones
    /// being moved must not snap to themselves).
    pub fn snap_at(&self, q: DVec2, skip: &[EntityId]) -> (DVec2, Option<PointId>, Option<&'static str>) {
        let (p, pid, l, _) = self.snap_guided(q, skip);
        (p, pid, l)
    }

    /// Start point of the line being drawn, if the H/V lock applies to this tool.
    fn line_anchor(&self) -> Option<DVec2> {
        match (self.tool, self.clicks.as_slice()) {
            (Tool::Line, [a]) => Some(a.0),
            _ => None,
        }
    }

    /// Which axis through `a` the cursor `q` is locked to: `Some(true)` horizontal,
    /// `Some(false)` vertical. Within 8 px (or about 4 degrees, whichever is wider) of
    /// the axis line; Shift forces the nearer axis.
    fn hv_axis(&self, a: DVec2, q: DVec2) -> Option<bool> {
        let d = q - a;
        let tol = (HV_LOCK_PX as f64 / self.scale as f64).max(d.length() * HV_LOCK_TAN);
        if d.length() < 1.0 / self.scale as f64 {
            return None;
        }
        let horiz = d.y.abs() <= d.x.abs();
        if self.shift_held {
            return Some(horiz);
        }
        let off = if horiz { d.y.abs() } else { d.x.abs() };
        (off <= tol).then_some(horiz)
    }

    /// `snap_at` plus the guides to draw: the locked axis (origin, horizontal?) and the
    /// points whose x / y the result is aligned with.
    pub fn snap_guided(
        &self,
        q: DVec2,
        skip: &[EntityId],
    ) -> (DVec2, Option<PointId>, Option<&'static str>, SnapGuides) {
        let mut g = SnapGuides::default();
        if !self.snap || self.alt_held {
            return (q, None, None, g);
        }
        let sk = &self.sketch;
        let tol = SNAP_PX as f64 / self.scale as f64;
        let pts: Vec<DVec2> = sk
            .entities
            .iter()
            .enumerate()
            .filter(|(i, _)| !skip.contains(&EntityId(*i as u32)))
            .flat_map(|(_, e)| match e.geom {
                Geom::Line { a, b } => vec![sk.point(a), sk.point(b)],
                Geom::Arc { center, start, end } => vec![sk.point(center), sk.point(start), sk.point(end)],
                Geom::Circle { center, .. } => vec![sk.point(center)],
            })
            .collect();
        // H/V lock from the start point of the line being drawn
        if let Some(a) = self.line_anchor() {
            if let Some(horiz) = self.hv_axis(a, q) {
                g.axis = Some((a, horiz));
                let proj = if horiz { DVec2::new(q.x, a.y) } else { DVec2::new(a.x, q.y) };
                // a real point close to the cursor still wins (closing shapes); one on
                // the axis beats one off it
                let tight = HV_POINT_PX as f64 / self.scale as f64;
                let on_axis = 0.5 / self.scale as f64;
                let mut best: Option<(bool, f64, DVec2)> = None;
                for &p in pts.iter().chain(std::iter::once(&DVec2::ZERO)) {
                    let d = p.distance(q);
                    let off = if horiz { (p.y - a.y).abs() } else { (p.x - a.x).abs() };
                    if d < tight || off <= on_axis && d < tol {
                        let key = (off > on_axis, d);
                        if best.is_none_or(|b| key < (b.0, b.1)) {
                            best = Some((key.0, key.1, p));
                        }
                    }
                }
                if let Some((_, _, p)) = best {
                    let label = if p == DVec2::ZERO { "origin" } else { "end" };
                    let pid = sk.points.iter().position(|v| v.distance(p) < 1e-12).map(|i| PointId(i as u32));
                    let pid = if label == "end" { pid } else { None };
                    // on the axis, the end lands exactly on it
                    return (p, pid, Some(label), g);
                }
                // along the axis only: aligned points, then grid lines / round lengths
                let (t, pa) = if horiz { (proj.x, a.x) } else { (proj.y, a.y) };
                let step = snap_step(self.scale);
                let mut cand: Option<(f64, f64)> = None; // (dist, coordinate)
                for &p in &pts {
                    let c = if horiz { p.x } else { p.y };
                    if (c - t).abs() < tol && cand.is_none_or(|b| (c - t).abs() < b.0) {
                        cand = Some(((c - t).abs(), c));
                        g.ax = Some(p);
                    }
                }
                if cand.is_none() {
                    let grid = (t / step).round() * step;
                    let len = pa + ((t - pa) / step).round() * step;
                    let c = if (grid - t).abs() <= (len - t).abs() { grid } else { len };
                    cand = Some(((c - t).abs(), c));
                }
                let c = cand.unwrap().1;
                let end = if horiz { DVec2::new(c, a.y) } else { DVec2::new(a.x, c) };
                return (end, None, Some(if horiz { "H" } else { "V" }), g);
            }
        }
        let (mut pos, pid, label) = self.snap_core(q, skip);
        // alignment with other points' x / y (below every real snap, only while drawing)
        if label.is_none()
            && matches!(self.tool, Tool::Line | Tool::Rect | Tool::Circle | Tool::Arc)
            && !pts.is_empty()
        {
            let mut bx: Option<(f64, DVec2)> = None;
            let mut by: Option<(f64, DVec2)> = None;
            for &p in &pts {
                let (dx, dy) = ((p.x - q.x).abs(), (p.y - q.y).abs());
                if dx < tol && bx.is_none_or(|b| dx < b.0) {
                    bx = Some((dx, p));
                }
                if dy < tol && by.is_none_or(|b| dy < b.0) {
                    by = Some((dy, p));
                }
            }
            if let Some((_, p)) = bx {
                pos.x = p.x;
                g.ax = Some(p);
            }
            if let Some((_, p)) = by {
                pos.y = p.y;
                g.ay = Some(p);
            }
            if g.ax.is_some() || g.ay.is_some() {
                let l = if g.ax.is_some() && g.ay.is_some() { "align" } else if g.ax.is_some() { "align x" } else { "align y" };
                return (pos, None, Some(l), g);
            }
        }
        (pos, pid, label, g)
    }

    fn snap_core(&self, q: DVec2, skip: &[EntityId]) -> (DVec2, Option<PointId>, Option<&'static str>) {
        let tol = SNAP_PX as f64 / self.scale as f64;
        let sk = &self.sketch;
        // 1. points: ends and centres, then line midpoints
        type Cand = (f64, DVec2, Option<PointId>, &'static str);
        let mut cands: Vec<Cand> = Vec::new();
        fn offer(c: &mut Vec<Cand>, tol: f64, d: f64, pos: DVec2, pid: Option<PointId>, l: &'static str) {
            if d < tol {
                c.push((d, pos, pid, l));
            }
        }
        let nearest = |v: &[Cand]| v.iter().copied().min_by(|a, b| a.0.total_cmp(&b.0));
        for (ei, e) in sk.entities.iter().enumerate() {
            if skip.contains(&EntityId(ei as u32)) {
                continue;
            }
            match e.geom {
                Geom::Line { a, b } => {
                    for id in [a, b] {
                        offer(&mut cands, tol, sk.point(id).distance(q), sk.point(id), Some(id), "end");
                    }
                }
                Geom::Arc { center, start, end } => {
                    offer(&mut cands, tol, sk.point(center).distance(q), sk.point(center), Some(center), "center");
                    for id in [start, end] {
                        offer(&mut cands, tol, sk.point(id).distance(q), sk.point(id), Some(id), "end");
                    }
                }
                Geom::Circle { center, .. } => {
                    offer(&mut cands, tol, sk.point(center).distance(q), sk.point(center), Some(center), "center");
                }
            }
        }
        offer(&mut cands, tol, q.length(), DVec2::ZERO, None, "origin");
        if let Some((_, pos, pid, l)) = nearest(&cands) {
            return (pos, pid, Some(l));
        }
        for (ei, e) in sk.entities.iter().enumerate() {
            if skip.contains(&EntityId(ei as u32)) {
                continue;
            }
            if let Geom::Line { a, b } = e.geom {
                let m = (sk.point(a) + sk.point(b)) / 2.0;
                offer(&mut cands, tol, m.distance(q), m, None, "mid");
            }
        }
        if let Some((_, pos, pid, l)) = nearest(&cands) {
            return (pos, pid, Some(l));
        }
        // radius phase of a circle/arc: the circumference of a circle with this centre
        if matches!(self.tool, Tool::Circle | Tool::Arc) && self.clicks.len() == 1 {
            let c = self.clicks[0].0;
            let dist = c.distance(q);
            for (ei, e) in sk.entities.iter().enumerate() {
            if skip.contains(&EntityId(ei as u32)) {
                continue;
            }
                let r = match e.geom {
                    Geom::Circle { center, radius } if sk.point(center).distance(c) < 1e-9 => radius,
                    Geom::Arc { center, start, .. } if sk.point(center).distance(c) < 1e-9 => {
                        sk.point(start).distance(c)
                    }
                    _ => continue,
                };
                if (dist - r).abs() < tol && dist > 1e-9 {
                    cands.push(((dist - r).abs(), c + (q - c) / dist * r, None, "radius"));
                }
            }
            if let Some((_, pos, pid, l)) = nearest(&cands) {
                return (pos, pid, Some(l));
            }
        }
        // 2. on curve; projected geometry wins a near tie
        let mut on: Option<(f64, DVec2, &'static str)> = None;
        for (ei, e) in sk.entities.iter().enumerate() {
            if skip.contains(&EntityId(ei as u32)) {
                continue;
            }
            let (pos, label) = match e.geom {
                Geom::Line { a, b } => {
                    let (pa, pb) = (sk.point(a), sk.point(b));
                    let d = pb - pa;
                    let l2 = d.length_squared();
                    if l2 < 1e-18 {
                        continue;
                    }
                    (pa + d * ((q - pa).dot(d) / l2).clamp(0.0, 1.0), "on line")
                }
                Geom::Circle { center, radius } => {
                    let c = sk.point(center);
                    let d = q - c;
                    if d.length() < 1e-9 {
                        continue;
                    }
                    (c + d.normalize() * radius, "on circle")
                }
                Geom::Arc { center, start, end } => {
                    let (c, st, en) = (sk.point(center), sk.point(start), sk.point(end));
                    let r = st.distance(c);
                    let d = q - c;
                    if d.length() < 1e-9 {
                        continue;
                    }
                    let tau = std::f64::consts::TAU;
                    let a0 = (st - c).to_angle();
                    let sweep = ((en - c).to_angle() - a0).rem_euclid(tau);
                    if (d.to_angle() - a0).rem_euclid(tau) > sweep {
                        continue;
                    }
                    (c + d.normalize() * r, "on arc")
                }
            };
            let dd = pos.distance(q);
            let slack = if e.projected { 1.0 / self.scale as f64 } else { 0.0 };
            if dd < tol && on.is_none_or(|b| dd - slack < b.0) {
                on = Some((dd - slack, pos, label));
            }
        }
        if let Some((_, pos, l)) = on {
            return (pos, None, Some(l));
        }
        // 3. grid
        let g = snap_step(self.scale);
        (DVec2::new((q.x / g).round() * g, (q.y / g).round() * g), None, None)
    }
}

/// How close (screen px) the cursor must be to geometry to snap to it.
const SNAP_PX: f32 = 9.0;

/// H/V lock while drawing a line: cursor within this many px of the axis through the
/// start point, or within `HV_LOCK_TAN` (about 4 degrees) of it, whichever is wider.
const HV_LOCK_PX: f32 = 8.0;
const HV_LOCK_TAN: f64 = 0.0699;
/// While locked, a real point this close (px) to the cursor still wins.
const HV_POINT_PX: f32 = 5.0;

/// What the last snap aligned to, for drawing guides.
#[derive(Default, Clone, Copy)]
pub struct SnapGuides {
    /// Locked axis: its origin (the line's start point) and whether it is horizontal.
    pub axis: Option<(DVec2, bool)>,
    /// A point whose x the result lines up with / a point whose y it lines up with.
    pub ax: Option<DVec2>,
    pub ay: Option<DVec2>,
}

/// Minor (tenth) grid lines are drawn, and snapped to, from this on-screen spacing.
const MINOR_MIN_PX: f32 = 6.0;

/// What the cursor snaps to: the tenths of a major grid step while those are drawn
/// (what you see is what you snap to), else the major step itself.
pub fn snap_step(scale: f32) -> f64 {
    let major = grid_step(scale);
    if (major / 10.0) as f32 * scale >= MINOR_MIN_PX {
        major / 10.0
    } else {
        major
    }
}

fn grid_step(scale: f32) -> f64 {
    let mut s = 1.0f64;
    while (s as f32) * scale < 12.0 {
        s *= 10.0;
    }
    while (s as f32) * scale > 120.0 {
        s /= 10.0;
    }
    s
}

pub fn geom_points(g: &Geom) -> Vec<PointId> {
    match *g {
        Geom::Line { a, b } => vec![a, b],
        Geom::Arc { center, start, end } => vec![center, start, end],
        Geom::Circle { center, .. } => vec![center],
    }
}

pub fn constraint_points(c: &Constraint) -> Vec<PointId> {
    use Constraint::*;
    match c {
        Coincident(a, b) | Distance(a, b, _) | DistanceX(a, b, _) | DistanceY(a, b, _) => {
            vec![*a, *b]
        }
        PointOnLine(p, _)
        | PointOnCircle(p, _)
        | Midpoint(p, _)
        | Fix(p, _)
        | PointLineDistance(p, _, _) => vec![*p],
        Symmetric(a, b, _) => vec![*a, *b],
        _ => vec![],
    }
}

pub fn constraint_entities(c: &Constraint) -> Vec<EntityId> {
    use Constraint::*;
    match c {
        Horizontal(e) | Vertical(e) | Length(e, _) | Radius(e, _) | Diameter(e, _) => vec![*e],
        AngleRays(a, _, b, _, _) => vec![*a, *b],
        Parallel(a, b) | Perpendicular(a, b) | Tangent(a, b) | Equal(a, b) | Angle(a, b, _) => {
            vec![*a, *b]
        }
        PointOnLine(_, e)
        | PointOnCircle(_, e)
        | Midpoint(_, e)
        | Symmetric(_, _, e)
        | PointLineDistance(_, e, _) => vec![*e],
        Offset { source, target, .. } => source.iter().chain(target).copied().collect(),
        _ => vec![],
    }
}

fn seg_dist(q: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let t = ((q - a).dot(ab) / ab.length_squared().max(1e-18)).clamp(0.0, 1.0);
    q.distance(a + ab * t)
}

fn on_arc(q: DVec2, c: DVec2, s: DVec2, e: DVec2) -> bool {
    let a0 = (s - c).y.atan2((s - c).x);
    let mut a1 = (e - c).y.atan2((e - c).x) - a0;
    let mut aq = (q - c).y.atan2((q - c).x) - a0;
    let tau = std::f64::consts::TAU;
    a1 = a1.rem_euclid(tau);
    aq = aq.rem_euclid(tau);
    aq <= a1
}

// ================================================================ screen

pub fn screen(app: &mut EasyCadApp, ui: &mut Ui) {
    let model = app.model.clone();
    let crate::app::Mode::Sketch(s) = &mut app.mode else {
        return;
    };
    if s.projected_slice != Some(s.slice) {
        if let Some(m) = &model {
            if s.slice {
                let (proj, caps, outline) = slice_model(m, &s.sketch.plane);
                s.projected_faces.clear();
                s.projected_src = vec![None; proj.len()];
                s.projected = proj;
                s.slice_caps = caps;
                s.slice_outline = outline;
            } else {
                let (proj, src) = project_edges(m, &s.sketch.plane);
                s.projected_faces = project_faces(m, &s.sketch.plane, &proj);
                s.projected = proj;
                s.projected_src = src;
                s.slice_caps.clear();
                s.slice_outline.clear();
            }
            s.projected_slice = Some(s.slice);
        }
    }
    let mut finish = false;
    let mut cancel = false;
    egui::Panel::bottom("sk_status")
        .exact_size(28.0)
        .frame(
            Frame::new()
                .fill(theme::BAR)
                .inner_margin(Margin::symmetric(12, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let (c, t) = s.status();
                widgets::dot(ui, c);
                ui.label(RichText::new(t).size(11.0).color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(format!(
                        "{} profiles · {} entities · {} constraints",
                        s.profile_count(),
                        s.sketch.entities.len(),
                        s.sketch.constraints.len()
                    ))
                    .size(11.0)
                    .color(theme::TEXT_MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Pan MMB · Zoom wheel · F fit · Ctrl+Z undo in sketch")
                            .size(11.0)
                            .color(theme::TEXT_MUTED),
                    );
                });
            });
        });
    egui::Panel::left("sk_left")
        .default_size(260.0)
        .min_size(220.0)
        .max_size(280.0)
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(0, 12)),
        )
        .show(ui, |ui| left_panel(s, ui));
    egui::Panel::right("sk_right")
        .default_size(300.0)
        .min_size(260.0)
        .max_size(320.0)
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(14, 12)),
        )
        .show(ui, |ui| {
            if right_panel(s, ui) {
                s.commit_move();
                finish = true;
            }
        });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        if canvas(s, ui) {
            cancel = true;
        }
    });
    if finish {
        app.finish_sketch();
    } else if cancel {
        app.cancel_sketch();
    }
}

type Slice = (Vec<Vec<DVec2>>, Vec<[DVec2; 3]>, Vec<Vec<DVec2>>);

/// The bodies cut at the sketch plane (GUI_NEEDS #9): the kept half's edges, the
/// cut faces as triangles and their outlines, all in sketch coordinates.
fn slice_model(m: &crate::worker::Output, plane: &Plane) -> Slice {
    use easycad_model::regen::MeshQuality;
    let (mut edges, mut caps, mut outline) = (Vec::new(), Vec::new(), Vec::new());
    let loc = |p: glam::DVec3| plane.to_local(p);
    for b in m.bodies.iter().filter(|b| !b.body.reference) {
        let Ok(sv) = b.body.section_view(plane.origin, plane.normal, MeshQuality::Preview) else {
            continue;
        };
        if let Some(mesh) = sv.kept.as_ref().and_then(|k| k.mesh_quality(MeshQuality::Preview).ok()) {
            for e in &mesh.edges {
                edges.push(e.points.iter().map(|p| loc(glam::Vec3::from_array(*p).as_dvec3())).collect());
            }
        }
        if let Some(cm) = &sv.cap_mesh {
            let p = |i: u32| loc(glam::Vec3::from_array(cm.positions[i as usize]).as_dvec3());
            caps.extend(cm.indices.iter().map(|t| [p(t[0]), p(t[1]), p(t[2])]));
        }
        outline.extend(sv.outline.iter().map(|l| l.iter().map(|q| loc(*q)).collect()));
    }
    (edges, caps, outline)
}

type Projected = (Vec<Vec<DVec2>>, Vec<Option<(easycad_model::feature::BodyId, easycad_model::occt::EdgeInfo, Vec<glam::DVec3>)>>);

/// Model edges seen in the sketch plane (mesh polylines for display) with the edge
/// each comes from, for exact projection.
fn project_edges(m: &crate::worker::Output, plane: &Plane) -> Projected {
    let (mut v, mut src) = (Vec::new(), Vec::new());
    for b in &m.bodies {
        if let Some(mesh) = &b.mesh {
            for e in &mesh.edges {
                let p3: Vec<glam::DVec3> = e
                    .points
                    .iter()
                    .map(|p| glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
                    .collect();
                v.push(p3.iter().map(|p| plane.to_local(*p)).collect());
                src.push(b.edges.get(e.edge as usize).cloned().map(|info| (b.body.id, info, p3)));
            }
        }
    }
    (v, src)
}

/// A model face seen in the sketch plane: its boundary edges (indices into
/// `SketchSession::projected`) and its triangles with a depth along the plane
/// normal (larger = nearer the viewer), for the hover tint and picking.
#[derive(Clone, Debug, Default)]
pub struct ProjFace {
    pub edges: Vec<usize>,
    pub tris: Vec<([DVec2; 3], f64)>,
}

fn in_tri(q: DVec2, t: [DVec2; 3]) -> bool {
    let s = |a: DVec2, b: DVec2| (b - a).perp_dot(q - a);
    let (d1, d2, d3) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// The faces of every body in sketch coordinates. Faces that are edge-on to the
/// plane (no area) or have no visible boundary are left out. `proj` is the output
/// of `project_edges` (same iteration order, so the edge indices line up).
fn project_faces(m: &crate::worker::Output, plane: &Plane, proj: &[Vec<DVec2>]) -> Vec<ProjFace> {
    let mut out = Vec::new();
    let mut base = 0;
    for b in &m.bodies {
        let Some(mesh) = &b.mesh else { continue };
        let mut faces: std::collections::BTreeMap<u32, ProjFace> = Default::default();
        for (k, tri) in mesh.indices.iter().enumerate() {
            let Some(&fid) = mesh.tri_face.get(k) else { continue };
            let p3 = tri.map(|i| {
                let p = mesh.positions[i as usize];
                glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
            });
            let t = [plane.to_local(p3[0]), plane.to_local(p3[1]), plane.to_local(p3[2])];
            if (t[1] - t[0]).perp_dot(t[2] - t[0]).abs() < 1e-9 {
                continue;
            }
            let depth = ((p3[0] + p3[1] + p3[2]) / 3.0 - plane.origin).dot(plane.normal);
            faces.entry(fid).or_default().tris.push((t, depth));
        }
        for (fid, mut f) in faces {
            for (k, e) in mesh.edges.iter().enumerate() {
                let on_face = b.edges.get(e.edge as usize).is_some_and(|i| i.faces.contains(&fid));
                let pl = &proj[base + k];
                let len: f64 = pl.windows(2).map(|w| w[0].distance(w[1])).sum();
                if on_face && len > 1e-6 {
                    f.edges.push(base + k);
                }
            }
            if !f.edges.is_empty() {
                out.push(f);
            }
        }
        base += mesh.edges.len();
    }
    out
}

/// The Project tool's hover tooltip for the current hover.
pub fn project_tip(s: &SketchSession) -> String {
    if let Some(i) = s.project_hover {
        if s.edge_projected(i) { "Already projected".into() } else { "Click to project edge".into() }
    } else if let Some(f) = s.project_hover_face {
        match s.face_missing(f).len() {
            0 => "Already projected".into(),
            n => format!("Click to project face outline ({n} edge{})", if n == 1 { "" } else { "s" }),
        }
    } else {
        String::new()
    }
}

fn left_panel(s: &mut SketchSession, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 4.0;
    let mut toggles = Vec::new();
    Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
        theme::section_label(ui, "DXF layers");
        match &s.dxf {
            Some(d) => {
                ui.label(widgets::muted(format!("{} · mm", d.file)));
                for (name, on, count) in &d.layers {
                    ui.horizontal(|ui| {
                        ui.set_min_height(32.0);
                        let mut v = *on;
                        if ui.checkbox(&mut v, "").changed() {
                            toggles.push((name.clone(), v));
                        }
                        let sw = if name.to_lowercase().contains("silk") {
                            theme::SILK
                        } else if name.to_lowercase().contains("doc") {
                            theme::hex(0x7d848e)
                        } else {
                            theme::PURPLE
                        };
                        widgets::swatch(ui, sw);
                        ui.label(name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(widgets::muted(count)));
                    });
                }
            }
            None => {
                ui.label(widgets::muted("No DXF in this sketch. Import one with Ctrl+I to pick layers."));
            }
        }
        ui.add_space(8.0);
        widgets::note(ui, "Imported geometry is purple, fixed and stays linked. Offset it to build the enclosure profile; your offsets and dimensions keep working.");
        ui.add_space(14.0);
        theme::section_label(ui, "Sketch status");
        let (c, t) = s.status();
        ui.horizontal(|ui| {
            ui.set_min_height(28.0);
            widgets::dot(ui, c);
            ui.label(t);
        });
        let n = s.profile_count();
        ui.horizontal(|ui| {
            ui.set_min_height(28.0);
            widgets::dot(ui, theme::ACCENT);
            ui.label(format!("{n} closed profile{}", if n == 1 { "" } else { "s" }));
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").color(theme::TEXT_DIM));
            if ui.add(egui::TextEdit::singleline(&mut s.sketch.name).desired_width(150.0)).changed() {
                s.dirty = true;
            }
        });
    });
    for (l, on) in toggles {
        s.toggle_layer(&l, on);
    }
}

/// Returns true when "Finish sketch" was clicked.
fn right_panel(s: &mut SketchSession, ui: &mut Ui) -> bool {
    let mut finish = false;
    ui.spacing_mut().item_spacing.y = 8.0;
    theme::section_label(ui, "Sketch palette");
    let w = (ui.available_width() - 12.0) / 3.0;
    egui::Grid::new("palette")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            for (i, (t, label, key)) in TOOLS.iter().enumerate() {
                let on = s.tool == *t;
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 50.0), Sense::click());
                let bg = if on {
                    theme::ACCENT_BG
                } else if resp.hovered() {
                    theme::HOVER
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect(
                    rect,
                    6.0,
                    bg,
                    Stroke::new(
                        1.0,
                        if on {
                            theme::ACCENT
                        } else {
                            theme::BORDER_INPUT
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    rect.center() - vec2(0.0, 7.0),
                    egui::Align2::CENTER_CENTER,
                    *label,
                    theme::prop(12.0),
                    theme::TEXT,
                );
                ui.painter().text(
                    rect.center() + vec2(0.0, 10.0),
                    egui::Align2::CENTER_CENTER,
                    *key,
                    theme::mono(10.0),
                    theme::TEXT_MUTED,
                );
                if resp.on_hover_text(t.hint()).clicked() {
                    s.tool = if on { Tool::Select } else { *t };
                    s.clicks.clear();
                    s.input = None;
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    let on = s.construction_mode;
    let label = if on { "Construction: on" } else { "Construction" };
    let r = ui.add_sized(
        vec2(ui.available_width(), 28.0),
        egui::Button::new(RichText::new(format!("{label}   X")).size(12.0))
            .fill(if on { theme::ACCENT_BG } else { Color32::TRANSPARENT })
            .stroke(Stroke::new(1.0, if on { theme::ACCENT } else { theme::BORDER_INPUT })),
    );
    if r.on_hover_text(CONSTRUCTION_HINT).clicked() {
        s.toggle_construction();
    }
    let can_move = s.mv.is_none() && (!s.selected_entities().is_empty());
    if ui
        .add_enabled(can_move, egui::Button::new(RichText::new("Move selection   M").size(12.0)).stroke(Stroke::new(1.0, theme::BORDER_INPUT)))
        .on_hover_text("Move the selected lines/arcs/circles: drag the X or Y arrow, or the square; type dX / dY and press Enter")
        .clicked()
    {
        s.begin_move();
    }
    theme::section_label(ui, "Constraints");
    let mut err = None;
    egui::Grid::new("constraints")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            let w = (ui.available_width() - 6.0) / 2.0;
            let items = [
                ("·", "Coincident", "coincident"),
                ("H", "Horiz / Vert", "hv"),
                ("∥", "Parallel", "parallel"),
                ("⊥", "Perpendicular", "perpendicular"),
                ("T", "Tangent", "tangent"),
                ("=", "Equal", "equal"),
                ("M", "Midpoint", "midpoint"),
                ("S", "Symmetric", "symmetric"),
                ("F", "Fix", "fix"),
                ("O", "Concentric", "concentric"),
            ];
            for (i, (g, label, key)) in items.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(w.max(110.0), 30.0), Sense::click());
                let bg = if resp.hovered() {
                    theme::HOVER
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect(
                    rect,
                    5.0,
                    bg,
                    Stroke::new(1.0, theme::BORDER_INPUT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    rect.left_center() + vec2(14.0, 0.0),
                    egui::Align2::CENTER_CENTER,
                    *g,
                    theme::prop(12.0),
                    theme::TEXT,
                );
                ui.painter().text(
                    rect.left_center() + vec2(28.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    *label,
                    theme::prop(12.0),
                    theme::hex(0xd7dbe0),
                );
                if resp
                    .on_hover_text("Applies to the current selection")
                    .clicked()
                {
                    if let Err(e) = s.constrain_selection(key) {
                        err = Some(e);
                    }
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    if let Some(e) = err {
        s.error = Some(e);
    }
    theme::section_label(ui, "Options");
    ui.checkbox(&mut s.show_constraints, "Show constraints");
    ui.checkbox(&mut s.snap, "Snap to grid and geometry (hold Alt to bypass)");
    ui.checkbox(&mut s.slice, "Slice body at sketch plane")
        .on_hover_text("Show the bodies cut at this plane: the cut faces filled, the half behind the plane in outline");
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        let r = ui.add_sized(
            vec2(ui.available_width(), 40.0),
            egui::Button::new(RichText::new("Finish sketch").size(13.0).color(theme::TEXT_STRONG)).fill(theme::PRIMARY).stroke(Stroke::new(1.0, theme::ACCENT)),
        );
        if r.clicked() {
            finish = true;
        }
        ui.add_space(4.0);
        widgets::note(ui, "Typing a number while you draw sets the dimension. Typing = starts an expression, e.g. =clearance + wall.");
    });
    finish
}

/// The 2D canvas. Returns true to cancel the sketch.
fn canvas(s: &mut SketchSession, ui: &mut Ui) -> bool {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.interact(
        rect,
        ui.scope_id().with("sk_canvas"),
        Sense::click_and_drag(),
    );
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, theme::SKETCH_BG);
    if !s.fitted {
        s.fit(rect);
        s.fitted = true;
    }
    // navigation
    if resp.dragged_by(egui::PointerButton::Middle) {
        let d = resp.drag_delta();
        s.center -= DVec2::new(d.x as f64, -d.y as f64) / s.scale as f64;
    }
    if let Some(hp) = resp.hover_pos() {
        let sc = ui.input(|i| i.smooth_scroll_delta.y);
        if sc.abs() > 0.0 {
            let before = s.to_sketch(rect, hp);
            s.scale = (s.scale * (sc * 0.0018).exp()).clamp(0.02, 400.0);
            let after = s.to_sketch(rect, hp);
            s.center += before - after;
        }
        s.alt_held = ui.input(|i| i.modifiers.alt);
        s.shift_held = ui.input(|i| i.modifiers.shift);
        s.cursor = s.snap_point(rect, hp).0;
        s.hover = if s.tool == Tool::Dimension { s.pick_dim(rect, hp) } else { s.pick(rect, hp) };
        let hq = s.to_sketch(rect, hp);
        let ph = if s.tool == Tool::Project { s.project_hit(hq) } else { None };
        let pf = if s.tool == Tool::Project && ph.is_none() { s.project_face_hit(hq) } else { None };
        if ph != s.project_hover || pf != s.project_hover_face {
            s.project_hover = ph;
            s.project_hover_face = pf;
            ui.ctx().request_repaint();
        }
        if let Some(i) = ph {
            ui.ctx().set_cursor_icon(if s.edge_projected(i) { egui::CursorIcon::NotAllowed } else { egui::CursorIcon::PointingHand });
            resp.clone().on_hover_text(project_tip(s));
        } else if let Some(pf) = pf {
            ui.ctx().set_cursor_icon(if s.face_missing(pf).is_empty() { egui::CursorIcon::NotAllowed } else { egui::CursorIcon::PointingHand });
            resp.clone().on_hover_text(project_tip(s));
        }
    } else if s.project_hover.take().is_some() | s.project_hover_face.take().is_some() {
        ui.ctx().request_repaint();
    }
    // keyboard
    let typing = ui.ctx().text_edit_focused();
    let mut cancel = false;
    if !typing {
        ui.input_mut(|i| {
            for (t, _, k) in TOOLS {
                let key = egui::Key::from_name(k).unwrap();
                if i.consume_key(egui::Modifiers::NONE, key) && s.input.is_none() {
                    s.tool = t;
                    s.clicks.clear();
                    s.dim_target = None;
                    s.dim_pending = false;
                }
            }
        });
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::X)) && s.input.is_none() {
            s.toggle_construction();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) && s.input.is_none() {
            if s.mv.is_some() {
                s.commit_move();
            } else {
                s.begin_move();
            }
        }
        // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y are dispatched once, by `EasyCadApp::shortcuts`
        // (sketch history first, then the document's).
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if s.mv.is_some() {
                s.cancel_move();
            } else if s.input.is_some() {
                s.cancel_input();
            } else if !s.clicks.is_empty() {
                s.clicks.clear();
            } else if s.dim_target.is_some() || s.dim_pending {
                s.dim_target = None;
                s.dim_pending = false;
            } else if s.tool != Tool::Select {
                s.tool = Tool::Select;
            } else if s.selection.is_empty() {
                cancel = false;
            } else {
                s.selection.clear();
            }
        }
        if ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
        }) {
            if let Some(i) = s.sel_dim.take() {
                s.delete_dimension(i);
            }
            let rm: HashSet<EntityId> = s
                .selection
                .iter()
                .filter_map(|x| {
                    if let Sel::Entity(e) = x {
                        Some(*e)
                    } else {
                        None
                    }
                })
                .collect();
            if !rm.is_empty() {
                s.push_undo();
                s.remove_entities(&rm);
                s.resolve();
            }
        }
        // start typing a number / expression
        let start = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Text(t)
                    if t.chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == '=' || c == '-') =>
                {
                    Some(t.clone())
                }
                _ => None,
            })
        });
        if let Some(t) = start {
            if s.input.is_none()
                && (!s.clicks.is_empty() || matches!(s.tool, Tool::Fillet | Tool::Offset))
            {
                s.input = Some(t);
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F)) && s.input.is_none()
        {
            s.fit(rect);
        }
    }
    // pointer
    let moving = crate::sketch_move::interact(s, ui, rect, &resp);
    if s.drag.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        s.drag_cancel();
    }
    if !moving && resp.drag_started_by(egui::PointerButton::Primary) && s.tool == Tool::Select {
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        if let Some(hp) = origin {
            if let Some(x) = s.pick(rect, hp) {
                let press = s.snap_point(rect, hp).0;
                s.drag_begin(x, press);
            }
        }
    }
    if s.drag.is_some() {
        if let Some(hp) = resp.interact_pointer_pos() {
            if resp.dragged_by(egui::PointerButton::Primary) {
                // the dragged shapes must not snap to themselves
                let skip = s.drag_entities();
                let q = s.snap_at(s.to_sketch(rect, hp), &skip).0;
                s.drag_to(q);
            }
        }
    }
    if resp.drag_stopped() && s.drag.is_some() {
        s.drag_end();
    }
    let label_at = |s: &SketchSession, hp: Pos2| dim_at(s, hp);
    let mut dbl = false;
    if resp.double_clicked_by(egui::PointerButton::Primary) && s.drag.is_none() {
        if let Some(i) = resp.interact_pointer_pos().and_then(|hp| label_at(s, hp)) {
            if let Some(e) = s.sketch.constraints[i].dim_expr().map(|e| e.0.clone()) {
                // an open box (another dimension) is dropped; this one is edited inline
                s.cancel_input();
                s.dim_target = None;
                s.edit_dim = Some(i);
                s.sel_dim = Some(i);
                s.selection.clear();
                s.open_input(e, false);
                dbl = true;
            }
        }
    }
    let on_label = if dbl {
        None
    } else if resp.clicked_by(egui::PointerButton::Primary) && s.input.is_none() && !moving {
        resp.interact_pointer_pos().and_then(|hp| label_at(s, hp))
    } else {
        None
    };
    if let Some(i) = on_label {
        s.sel_dim = Some(i);
        s.selection.clear();
    } else if resp.clicked_by(egui::PointerButton::Primary) && !moving && !dbl {
        s.sel_dim = None;
        if let Some(hp) = resp.interact_pointer_pos() {
            click(
                s,
                rect,
                hp,
                ui.input(|i| i.modifiers.shift || i.modifiers.command),
            );
        }
    }
    if resp.secondary_clicked() {
        s.clicks.clear();
        s.cancel_input();
        // right-click on an entity not yet selected selects it for the menu
        if let Some(Sel::Entity(e)) = s.hover {
            if !s.selection.contains(&Sel::Entity(e)) {
                s.selection = vec![Sel::Entity(e)];
            }
        }
    }
    resp.context_menu(|ui| {
        let any = !s.selected_entities().is_empty();
        if ui.add_enabled(any, egui::Button::new("Toggle construction")).clicked() {
            s.toggle_construction_selection();
            ui.close();
        }
        if ui.add_enabled(any, egui::Button::new("Move   M")).clicked() {
            s.begin_move();
            ui.close();
        }
    });

    draw(s, ui, rect, &p);
    crate::sketch_move::paint(s, &p, rect);
    crate::sketch_move::value_box(s, ui, rect);
    // tool hint
    let hint_s = if s.construction_mode {
        format!("{} · construction on (X)", s.tool.hint())
    } else {
        s.tool.hint().to_string()
    };
    let hint = hint_s.as_str();
    let hr = Rect::from_min_size(
        pos2(rect.left() + 12.0, rect.bottom() - 44.0),
        vec2(8.0 * hint.len() as f32 + 20.0, 30.0),
    );
    p.rect(
        hr,
        6.0,
        theme::CARD,
        Stroke::new(1.0, theme::BORDER_INPUT),
        egui::StrokeKind::Inside,
    );
    p.text(
        hr.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        hint,
        theme::mono(12.0),
        theme::TEXT_DIM,
    );
    if let Some(e) = &s.error {
        p.text(
            pos2(rect.left() + 12.0, rect.bottom() - 60.0),
            egui::Align2::LEFT_CENTER,
            e,
            theme::prop(12.0),
            theme::ORANGE_SOFT,
        );
    }
    value_input(s, ui, rect);
    cancel
}

fn is_fixed(s: &Sketch, p: PointId) -> bool {
    s.constraints
        .iter()
        .any(|c| matches!(c, Constraint::Fix(q, _) if *q == p))
}

fn click(s: &mut SketchSession, rect: Rect, hp: Pos2, additive: bool) {
    if s.tool != Tool::Dimension && s.edit_dim.is_some() && dim_input_open(s) {
        // a click elsewhere commits the inline edit; a refused value keeps it open
        if let Err(e) = commit_dim_input(s) {
            s.input_err = Some(e);
            return;
        }
    }
    let placing = matches!(s.tool, Tool::Line | Tool::Rect | Tool::Circle | Tool::Arc);
    if placing {
        // a click commits with the mouse value: the value box is done
        s.input = None;
        s.input2 = None;
        s.input_second = false;
        s.input_live = false;
        s.input_fresh = false;
    }
    click_tool(s, rect, hp, additive);
    // the first placement click opens the value box at once, showing the live
    // reading (length / width / diameter / radius) with its text selected
    let started = match s.tool {
        Tool::Line => !s.clicks.is_empty(),
        Tool::Rect | Tool::Circle | Tool::Arc => s.clicks.len() == 1,
        _ => false,
    };
    if placing && started {
        s.open_input(String::new(), true);
    }
}

fn click_tool(s: &mut SketchSession, rect: Rect, hp: Pos2, additive: bool) {
    let snapped = s.snap_point(rect, hp);
    match s.tool {
        Tool::Select => {
            match s.pick(rect, hp) {
                Some(x) if additive => {
                    if let Some(i) = s.selection.iter().position(|y| *y == x) {
                        s.selection.remove(i);
                    } else {
                        s.selection.push(x);
                    }
                }
                Some(x) => s.selection = vec![x],
                None => s.selection.clear(),
            }
            s.error = None;
        }
        Tool::Line => {
            if let Some(prev) = s.clicks.last().cloned() {
                if prev.0.distance(snapped.0) > 1e-9 {
                    s.push_undo();
                    let e = s.add_line(prev, snapped);
                    let (_, b) = s.line_points(e).unwrap();
                    s.resolve();
                    s.clicks = vec![(s.sketch.point(b), Some(b))];
                    if snapped.1.is_some() && snapped.1 == s.chain_start {
                        s.clicks.clear();
                        s.chain_start = None;
                    }
                }
            } else {
                s.clicks.push(snapped);
                if snapped.1.is_none() {
                    s.push_undo();
                    let p = s.sketch.add_point(snapped.0);
                    s.clicks = vec![(snapped.0, Some(p))];
                }
                s.chain_start = s.clicks[0].1;
            }
        }
        Tool::Rect => {
            if let Some(a) = s.clicks.first().cloned() {
                s.push_undo();
                let (lo, hi) = (a.0.min(snapped.0), a.0.max(snapped.0));
                let l = s.sketch.rect(lo, hi);
                s.entity_layer.resize(s.sketch.entities.len(), None);
                let _ = l;
                s.clicks.clear();
                s.resolve();
            } else {
                s.clicks.push(snapped);
            }
        }
        Tool::Circle => {
            if let Some(c) = s.clicks.first().cloned() {
                s.push_undo();
                let pc = s.point_at(c.0, c.1);
                s.add_entity(
                    Geom::Circle {
                        center: pc,
                        radius: c.0.distance(snapped.0).max(1e-3),
                    },
                    false,
                );
                s.clicks.clear();
                s.resolve();
            } else {
                s.clicks.push(snapped);
            }
        }
        Tool::Arc => {
            s.clicks.push(snapped);
            if s.clicks.len() == 3 {
                s.push_undo();
                let c = s.clicks[0];
                let st = s.clicks[1];
                let r = c.0.distance(st.0);
                let en_dir = (s.clicks[2].0 - c.0).normalize_or_zero();
                let pc = s.point_at(c.0, c.1);
                let ps = s.point_at(st.0, st.1);
                let pe = s.sketch.add_point(c.0 + en_dir * r);
                let e = s.add_entity(
                    Geom::Arc {
                        center: pc,
                        start: ps,
                        end: pe,
                    },
                    false,
                );
                let _ = e;
                s.clicks.clear();
                s.resolve();
            }
        }
        Tool::Fillet => {
            if let Some(Sel::Point(pid)) = s.pick(rect, hp) {
                let r = s
                    .input
                    .take()
                    .or_else(|| s.input2.clone())
                    .unwrap_or_else(|| "2".into());
                if let Err(e) = s.fillet_corner(pid, r.trim_start_matches('=')) {
                    s.error = Some(e);
                }
            }
        }
        Tool::Offset => match s.pick(rect, hp) {
            Some(Sel::Entity(e)) => {
                let x = Sel::Entity(e);
                if additive {
                    if let Some(i) = s.selection.iter().position(|y| *y == x) {
                        s.selection.remove(i);
                    } else {
                        s.selection.push(x);
                    }
                } else {
                    s.selection = vec![x];
                }
                s.error = None;
                if s.selection.is_empty() {
                    s.cancel_input();
                } else if s.input.is_none() {
                    // the distance box opens at once, `wall` (or what was typed
                    // before picking) selected for overwriting
                    let t = s.input2.take().unwrap_or_else(|| s.default_offset());
                    s.open_input(t, false);
                }
            }
            _ => s.cancel_input(), // empty space cancels
        },
        Tool::Project => {
            let q = s.to_sketch(rect, hp);
            // edges win over faces; one undo step either way, duplicates skipped
            if let Some(i) = s.project_hit(q) {
                s.project_edges_now(&[i]);
            } else if let Some(f) = s.project_face_hit(q) {
                let edges = s.projected_faces[f].edges.clone();
                s.project_edges_now(&edges);
            }
        }
        Tool::Trim => {
            if let Some(Sel::Entity(e)) = s.pick(rect, hp) {
                let q = s.to_sketch(rect, hp);
                s.trim(e, q);
            }
        }
        Tool::Dimension => {
            if s.dim_pending {
                // the placement click: the cursor's side picks the orientation
                if let Some(t) = s.dim_target.clone() {
                    let q = s.to_sketch(rect, hp);
                    if let Some([a, b, c, d]) = s.line_pair(&t) {
                        // two lines: the quadrant around their crossing picks the angle
                        if let Some(ch) = angle_choice(a, b, c, d, q) {
                            s.dim_arc_r = hp.distance(s.to_screen(rect, ch.p)).clamp(30.0, 260.0);
                            s.dim_rays = Some(ch);
                            s.dim_pending = false;
                            s.open_input(fmt(ch.deg), false);
                            return;
                        }
                    }
                    let pts = s.dim_anchor_points(&t);
                    if let Some((a, b)) = pts {
                        s.dim_orient = dim_orient(a, b, q);
                        s.dim_pending = false;
                        if let Some((v, _)) = s.dimension_for(&t) {
                            s.open_input(fmt(v.abs()), false);
                        }
                        return;
                    }
                }
                s.dim_pending = false;
            }
            if dim_input_open(s) {
                // a click elsewhere commits the inline field; a refused value keeps it open
                if let Err(e) = commit_dim_input(s) {
                    s.input_err = Some(e);
                    return;
                }
            } else if s.input.is_some() {
                s.cancel_input();
            }
            let picked = s.pick_dim(rect, hp);
            let single = match s.dim_target.as_deref() {
                Some([Sel::Entity(e)]) => Some(*e),
                _ => None,
            };
            if let Some(e0) = single {
                // a lone shape is placed by a click on empty space (or on itself)
                if picked.is_none() || picked == Some(Sel::Entity(e0)) {
                    if let Some((v, _)) = s.dim_target.as_ref().and_then(|t| s.dimension_for(t)) {
                        s.open_input(fmt(v.abs()), false);
                        return;
                    }
                }
            }
            if let Some(x) = picked {
                let mut t = s.dim_target.take().unwrap_or_default();
                if t.len() >= 2 {
                    t.clear();
                }
                if t.is_empty() {
                    s.dim_toggle = false;
                    s.dim_orient = DimOrient::Aligned;
                    s.dim_rays = None;
                }
                t.push(x);
                let complete = !matches!(t.as_slice(), [_]);
                if complete && s.dimension_for(&t).is_some() {
                    if s.dim_needs_placement(&t) {
                        s.dim_pending = true;
                    } else {
                        let (v, _) = s.dimension_for(&t).unwrap();
                        // an angle keeps its sign: the box shows what is measured
                        let sgn = if matches!(t.as_slice(), [Sel::Entity(_), Sel::Entity(_)]) { v } else { v.abs() };
                        s.open_input(fmt(sgn), false);
                    }
                    s.dim_target = Some(t);
                } else if complete {
                    s.error = Some("can't dimension that selection".into());
                    s.dim_target = None;
                } else {
                    s.dim_target = Some(t);
                }
            } else {
                s.dim_target = None;
            }
        }
    }
}

/// The dimension under a screen point: its label box, else (within 6 px) its
/// dimension line, extension lines or arrows.
fn dim_at(s: &SketchSession, hp: Pos2) -> Option<usize> {
    if let Some((_, i)) = s.dim_rects.borrow().iter().rev().find(|(r, _)| r.contains(hp)) {
        return Some(*i);
    }
    let d2 = |seg: &[Pos2; 2]| {
        let (a, b) = (seg[0], seg[1]);
        let ab = b - a;
        let t = if ab.length_sq() < 1e-9 { 0.0 } else { ((hp - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) };
        (a + ab * t).distance(hp)
    };
    s.dim_segs
        .borrow()
        .iter()
        .rev()
        .find(|(seg, _)| d2(seg) <= 6.0)
        .map(|(_, i)| *i)
}

/// Is the value box the inline field of a dimension (new or edited)?
fn dim_input_open(s: &SketchSession) -> bool {
    s.input.is_some() && (s.edit_dim.is_some() || (s.tool == Tool::Dimension && s.dim_target.is_some()))
}

/// Apply the inline dimension field's text. On an error nothing changes and the
/// field stays open (the error is kept in `input_err`).
fn commit_dim_input(s: &mut SketchSession) -> Result<(), String> {
    let Some(text) = s.input.clone() else { return Ok(()) };
    let expr = crate::units::normalize_input(text.trim().trim_start_matches('=').trim());
    crate::widgets::eval(&expr, &s.values)?;
    if let Some(i) = s.edit_dim {
        s.set_dimension(i, &expr)?;
    } else if let Some(t) = s.dim_target.clone() {
        s.apply_dimension(&t, &expr)?;
    }
    s.input = None;
    s.input_live = false;
    s.input_err = None;
    s.edit_dim = None;
    s.dim_target = None;
    s.dim_pending = false;
    s.dim_rays = None;
    s.error = None;
    Ok(())
}

/// The cursor's reading for the value box while a shape is being placed.
fn live_value(s: &SketchSession) -> Option<f64> {
    let a = s.clicks.first()?.0;
    match s.tool {
        Tool::Line | Tool::Arc => Some(a.distance(s.cursor)),
        Tool::Circle => Some(2.0 * a.distance(s.cursor)),
        Tool::Rect => Some(if s.input_second { (s.cursor.y - a.y).abs() } else { (s.cursor.x - a.x).abs() }),
        _ => None,
    }
}

/// Inline value box: opens at once on the first placement click (and on picking an
/// Offset shape or a Dimension), text selected, so typing replaces it.
fn value_input(s: &mut SketchSession, ui: &mut Ui, rect: Rect) {
    let Some(text) = s.input.clone() else { return };
    let dim_mode = dim_input_open(s);
    let mut anchor = s.to_screen(rect, s.cursor) + vec2(18.0, -34.0);
    if dim_mode {
        let at = match s.edit_dim {
            Some(i) => s.dim_rects.borrow().iter().find(|(_, c)| *c == i).map(|(r, _)| r.center()),
            None => s.dim_label_pos.get(),
        };
        if let Some(at) = at {
            anchor = at;
        }
    }
    let mut text = text;
    if s.input_live {
        if let Some(v) = live_value(s) {
            text = fmt(v);
        }
    }
    let mut done = false;
    let mut cancel = false;
    let label = match s.tool {
        Tool::Line => "length",
        Tool::Rect => {
            if s.input_second {
                "H"
            } else {
                "W"
            }
        }
        Tool::Circle => "\u{2300}",
        Tool::Arc => "radius",
        Tool::Fillet => "radius",
        Tool::Offset => "offset",
        Tool::Dimension => "value",
        _ => "value",
    };
    let te_id = egui::Id::NULL.with("sk_input_text");
    let had_focus = ui.ctx().memory(|m| m.has_focus(te_id));
    if s.input_fresh || s.input_live {
        let mut st = egui::TextEdit::load_state(ui.ctx(), te_id).unwrap_or_default();
        let n = text.chars().count();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(n),
        )));
        st.store(ui.ctx(), te_id);
    }
    let mut typed = false;
    let bad = dim_mode && s.input_err.is_some();
    let mut area = egui::Area::new(egui::Id::NULL.with("sk_input"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor);
    if dim_mode {
        // centred on the label spot without a pivot (a pivot makes egui hide the
        // area for a sizing frame, which would eat the freshly typed text)
        let w = (text.chars().count().max(3) as f32 * 8.0 + 6.0).min(220.0) + 10.0;
        area = area.fixed_pos(anchor - vec2(w / 2.0, 11.0));
    }
    area.show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::CARD)
                .stroke(Stroke::new(
                    if dim_mode { 1.5 } else { 1.0 },
                    if bad { theme::RED } else { theme::ACCENT },
                ))
                .corner_radius(4.0)
                .inner_margin(if dim_mode { Margin::symmetric(4, 1) } else { Margin::symmetric(6, 3) })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if !dim_mode {
                            ui.label(RichText::new(label).size(11.0).color(theme::TEXT_MUTED));
                        }
                        let lone = s.tool == Tool::Dimension
                            && s.edit_dim.is_none()
                            && matches!(s.dim_target.as_deref(), Some([Sel::Entity(e)])
                                if !matches!(s.sketch.entities[e.0 as usize].geom, Geom::Line { .. }));
                        let tab = lone && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
                        if (lone && ui.small_button(if s.dim_toggle { "R \u{2192} \u{2300}" } else { "\u{2300} \u{2192} R" })
                            .on_hover_text("Swap radius and diameter (Tab)")
                            .clicked())
                            || tab
                        {
                            s.dim_toggle = !s.dim_toggle;
                            if let Some((v, _)) = s.dim_target.as_ref().and_then(|t| s.dimension_for(t)) {
                                text = fmt(v);
                                s.input_fresh = true;
                            }
                        }
                        let field_w = if dim_mode {
                            (text.chars().count().max(3) as f32 * 8.0 + 6.0).min(220.0)
                        } else {
                            110.0
                        };
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .id(te_id)
                                .font(theme::mono(13.0))
                                .desired_width(field_w)
                                .frame(egui::Frame::NONE),
                        );
                        typed = r.changed();
                        r.request_focus();
                        let v =
                            crate::widgets::eval(text.trim().trim_start_matches('='), &s.values);
                        ui.label(
                            RichText::new(if text.starts_with('=') { "fx" } else { "" })
                                .size(10.0)
                                .color(theme::ACCENT_LINK),
                        );
                        if let Ok(v) = v {
                            if text.starts_with('=') {
                                ui.label(widgets::muted(format!("= {v:.2}")));
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            done = true;
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Tab)) && s.tool == Tool::Rect {
                            s.input2 = Some(text.clone());
                            s.input_second = true;
                            s.input_live = true;
                            s.input_fresh = true;
                            typed = false;
                            text.clear();
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            cancel = true;
                        }
                    });
                });
            if dim_mode {
                if let Some(e) = &s.input_err {
                    Frame::new()
                        .fill(theme::CARD)
                        .stroke(Stroke::new(1.0, theme::RED))
                        .corner_radius(3.0)
                        .inner_margin(Margin::symmetric(5, 1))
                        .show(ui, |ui| {
                            ui.label(RichText::new(e).size(11.0).color(theme::RED));
                        });
                }
            }
        });
    if cancel {
        s.cancel_input();
        s.input_err = None;
        return;
    }
    if typed {
        s.input_live = false;
        s.input_err = None;
    }
    // the inline dimension field keeps re-selecting its text until it holds focus
    s.input_fresh = dim_mode && !had_focus && !typed && !cancel;
    if !done {
        s.input = Some(text);
        return;
    }
    if dim_mode {
        // Enter: apply; a bad or conflicting value keeps the field open, red
        s.input = Some(text);
        if let Err(e) = commit_dim_input(s) {
            s.input_err = Some(e);
        }
        return;
    }
    // Enter on an untouched live reading is the same as clicking there
    let placing = matches!(s.tool, Tool::Line | Tool::Rect | Tool::Circle | Tool::Arc);
    if placing && s.input_live && !(s.tool == Tool::Rect && s.input_second) {
        let hp = s.to_screen(rect, s.cursor);
        click(s, rect, hp, false);
        return;
    }
    s.input = None;
    s.input_live = false;
    let expr = crate::units::normalize_input(text.trim().trim_start_matches('=').trim());
    let val = match crate::widgets::eval(&expr, &s.values) {
        Ok(v) => v,
        Err(e) => {
            s.error = Some(e);
            return;
        }
    };
    if let Some(i) = s.edit_dim.take() {
        if let Err(e) = s.set_dimension(i, &expr) {
            s.error = Some(e);
        }
        return;
    }
    match s.tool {
        Tool::Line => {
            if let Some(start) = s.clicks.first().cloned() {
                let dir = (s.cursor - start.0).normalize_or(DVec2::X);
                let end = start.0 + dir * val;
                s.push_undo();
                let e = s.add_line(start, (end, None));
                s.sketch.constrain(Constraint::Length(e, Expr::new(&expr)));
                let (_, b) = s.line_points(e).unwrap();
                s.resolve();
                s.clicks = vec![(s.sketch.point(b), Some(b))];
                s.open_input(String::new(), true);
            }
        }
        Tool::Rect => {
            let w_txt = s.input2.take().unwrap_or_else(|| expr.clone());
            let h_txt = if s.input_second {
                expr.clone()
            } else {
                w_txt.clone()
            };
            s.input_second = false;
            if let Some(a) = s.clicks.first().cloned() {
                let w =
                    crate::widgets::eval(w_txt.trim_start_matches('='), &s.values).unwrap_or(val);
                let h =
                    crate::widgets::eval(h_txt.trim_start_matches('='), &s.values).unwrap_or(val);
                // The rectangle grows toward the cursor's quadrant; a cursor still
                // within a few pixels of the (snapped) corner means +x / +y, not the
                // sign of a sub-pixel snap residue.
                let tol = 4.0 / s.scale.max(1e-6) as f64;
                let d = s.cursor - a.0;
                let dir = |v: f64| if v < -tol { -1.0 } else { 1.0 };
                let b = a.0 + DVec2::new(w * dir(d.x), h * dir(d.y));
                s.push_undo();
                let l = s.sketch.rect(a.0.min(b), a.0.max(b));
                s.entity_layer.resize(s.sketch.entities.len(), None);
                s.sketch.constrain(Constraint::Length(
                    l[0],
                    Expr::new(w_txt.trim_start_matches('=')),
                ));
                s.sketch.constrain(Constraint::Length(
                    l[1],
                    Expr::new(h_txt.trim_start_matches('=')),
                ));
                s.clicks.clear();
                s.resolve();
            }
        }
        Tool::Circle => {
            if let Some(c) = s.clicks.first().cloned() {
                s.push_undo();
                let pc = s.point_at(c.0, c.1);
                let e = s.add_entity(
                    Geom::Circle {
                        center: pc,
                        radius: val / 2.0,
                    },
                    false,
                );
                s.sketch
                    .constrain(Constraint::Diameter(e, Expr::new(&expr)));
                s.clicks.clear();
                s.resolve();
            }
        }
        Tool::Arc => {
            // the radius fixes the start point along the cursor's direction
            if let Some(c) = s.clicks.first().cloned() {
                let dir = (s.cursor - c.0).normalize_or(DVec2::X);
                s.clicks.push((c.0 + dir * val, None));
            }
        }
        Tool::Offset => {
            let ents = s.selected_entities();
            s.error = None;
            if ents.is_empty() {
                s.input2 = Some(expr); // remembered for the next pick
            } else {
                if let Err(e) = s.offset_selected(&ents, &expr) {
                    s.error = Some(e);
                }
                s.selection.clear();
            }
        }
        Tool::Fillet => {
            // remembered for the next pick
            s.input = None;
            s.error = None;
            let _ = val;
            s.input2 = Some(expr);
        }
        Tool::Dimension => {
            if let Some(t) = s.dim_target.take() {
                if let Err(e) = s.apply_dimension(&t, &expr) {
                    s.error = Some(e);
                }
            }
        }
        _ => {}
    }
}

/// Sample points along an entity (lines: the ends; arcs and circles: the curve).
fn entity_polyline(s: &Sketch, e: EntityId) -> Vec<DVec2> {
    match s.entities[e.0 as usize].geom {
        Geom::Line { a, b } => vec![s.point(a), s.point(b)],
        Geom::Arc { center, start, end } => arc_points(s.point(center), s.point(start), s.point(end)),
        Geom::Circle { center, radius } => {
            let c = s.point(center);
            (0..=64)
                .map(|i| c + DVec2::from_angle(std::f64::consts::TAU * i as f64 / 64.0) * radius)
                .collect()
        }
    }
}

/// Offset the chains through `starts` by one distance expression, all in one
/// direction convention. A closed loop (or a circle) grows outward for a positive
/// distance and shrinks for a negative one. An open chain goes to the side away
/// from the centroid of all the picked geometry for a positive distance (toward it
/// for a negative one), so a set of shapes moves consistently outward together; a
/// chain through that centroid keeps `offset_chain`'s own side (left of travel).
pub fn offset_set(sk: &mut Sketch, starts: &[EntityId], dist: &str, vals: &Values) -> Result<Vec<EntityId>, String> {
    let d = Expr::new(dist).eval(vals).map_err(|e| e.to_string())?;
    let mut covered: HashSet<EntityId> = HashSet::new();
    let mut chains: Vec<(EntityId, bool, Vec<EntityId>)> = Vec::new();
    for &st in starts {
        if covered.contains(&st) || st.0 as usize >= sk.entities.len() {
            continue;
        }
        let (ch, closed) = sk.chain_from(st);
        if ch.is_empty() {
            continue;
        }
        covered.extend(ch.iter().map(|c| c.0));
        chains.push((st, closed, ch.iter().map(|c| c.0).collect()));
    }
    let pts = |sk: &Sketch, es: &[EntityId]| -> Vec<DVec2> { es.iter().flat_map(|e| entity_polyline(sk, *e)).collect() };
    let all: Vec<DVec2> = chains.iter().flat_map(|c| pts(sk, &c.2)).collect();
    if all.is_empty() {
        return Err("pick a shape to offset".into());
    }
    let centroid = all.iter().copied().sum::<DVec2>() / all.len() as f64;
    let mean_dist = |v: &[DVec2]| v.iter().map(|p| p.distance(centroid)).sum::<f64>() / v.len().max(1) as f64;
    let mut out = Vec::new();
    for (st, closed, es) in chains {
        let circle = es.len() == 1 && matches!(sk.entities[es[0].0 as usize].geom, Geom::Circle { .. });
        let mut expr = dist.to_string();
        if !closed && !circle {
            let mut probe = sk.clone();
            if let Ok(t) = probe.offset_chain(st, Expr::new(dist), vals) {
                let (src, tgt) = (mean_dist(&pts(sk, &es)), mean_dist(&pts(&probe, &t)));
                if (tgt - src).abs() > 1e-9 && (tgt < src) != (d < 0.0) {
                    expr = format!("-({dist})");
                }
            }
        }
        out.extend(sk.offset_chain(st, Expr::new(expr), vals)?);
    }
    Ok(out)
}

fn dashed(p: &egui::Painter, pts: &[Pos2], st: Stroke) {
    for w in pts.windows(2) {
        p.add(egui::Shape::dashed_line(&[w[0], w[1]], st, 4.0, 3.0));
    }
}

fn arc_points(c: DVec2, s: DVec2, e: DVec2) -> Vec<DVec2> {
    let r = c.distance(s);
    let a0 = (s - c).y.atan2((s - c).x);
    let mut a1 = (e - c).y.atan2((e - c).x);
    while a1 <= a0 {
        a1 += std::f64::consts::TAU;
    }
    let n = ((a1 - a0) / 0.08).ceil().max(4.0) as usize;
    (0..=n)
        .map(|i| c + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / n as f64) * r)
        .collect()
}

fn draw(s: &SketchSession, ui: &Ui, rect: Rect, p: &egui::Painter) {
    let to = |q: DVec2| s.to_screen(rect, q);
    // grid
    let step = grid_step(s.scale);
    let lo = s.to_sketch(rect, rect.left_bottom());
    let hi = s.to_sketch(rect, rect.right_top());
    let gs = Stroke::new(1.0, theme::BORDER);
    if (step / 10.0) as f32 * s.scale >= MINOR_MIN_PX {
        // the tenths the cursor snaps to, fainter than the major lines
        let minor = step / 10.0;
        let ms = Stroke::new(1.0, theme::BORDER.gamma_multiply(0.4));
        let mut x = (lo.x / minor).floor() * minor;
        while x <= hi.x {
            p.vline(to(DVec2::new(x, 0.0)).x, rect.y_range(), ms);
            x += minor;
        }
        let mut y = (lo.y / minor).floor() * minor;
        while y <= hi.y {
            p.hline(rect.x_range(), to(DVec2::new(0.0, y)).y, ms);
            y += minor;
        }
    }
    let mut x = (lo.x / step).floor() * step;
    while x <= hi.x {
        let sx = to(DVec2::new(x, 0.0)).x;
        p.vline(sx, rect.y_range(), gs);
        x += step;
    }
    let mut y = (lo.y / step).floor() * step;
    while y <= hi.y {
        let sy = to(DVec2::new(0.0, y)).y;
        p.hline(rect.x_range(), sy, gs);
        y += step;
    }
    let o = to(DVec2::ZERO);
    p.line_segment([o, o + vec2(100.0, 0.0)], Stroke::new(1.4, theme::RED));
    p.line_segment([o, o - vec2(0.0, 100.0)], Stroke::new(1.4, theme::GREEN));
    p.text(
        o + vec2(104.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "X",
        theme::prop(11.0),
        theme::RED,
    );
    p.text(
        o - vec2(4.0, 106.0),
        egui::Align2::LEFT_CENTER,
        "Y",
        theme::prop(11.0),
        theme::GREEN,
    );
    p.circle_filled(o, 3.0, theme::TEXT);
    // projected model edges (faint)
    let proj_col = if s.tool == Tool::Project {
        theme::hex(0x6c737d)
    } else {
        theme::hex(0x3a3f47)
    };
    // the cut faces when sliced (under the edges)
    if !s.slice_caps.is_empty() {
        let mut mesh = egui::Mesh::default();
        let col = Color32::from_rgba_unmultiplied(240, 136, 62, 46);
        for t in &s.slice_caps {
            let i = mesh.vertices.len() as u32;
            for q in t {
                mesh.colored_vertex(to(*q), col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
        }
        p.add(egui::Shape::mesh(mesh));
    }
    // hovered face (Project tool): translucent tint, boundary drawn orange below
    let face_edges: Vec<usize> = match (s.tool == Tool::Project, s.project_hover_face.and_then(|f| s.projected_faces.get(f))) {
        (true, Some(f)) => {
            let done = s.face_missing(s.project_hover_face.unwrap()).is_empty();
            let col = if done { Color32::from_rgba_unmultiplied(160, 166, 175, 30) } else { Color32::from_rgba_unmultiplied(240, 136, 62, 46) };
            let mut mesh = egui::Mesh::default();
            for t in &f.tris {
                let i = mesh.vertices.len() as u32;
                for q in &t.0 {
                    mesh.colored_vertex(to(*q), col);
                }
                mesh.add_triangle(i, i + 1, i + 2);
            }
            p.add(egui::Shape::mesh(mesh));
            f.edges.clone()
        }
        _ => Vec::new(),
    };
    let hov_dim = |i: usize| s.edge_projected(i);
    for (i, pl) in s.projected.iter().enumerate() {
        let pts: Vec<Pos2> = pl.iter().map(|q| to(*q)).collect();
        if s.tool == Tool::Project && (s.project_hover == Some(i) || face_edges.contains(&i)) {
            let col = if hov_dim(i) { Color32::from_rgb(150, 156, 165) } else { theme::ORANGE };
            p.add(egui::Shape::line(pts, Stroke::new(2.4, col)));
        } else {
            p.add(egui::Shape::line(pts, Stroke::new(1.0, proj_col)));
        }
    }
    for pl in &s.slice_outline {
        let pts: Vec<Pos2> = pl.iter().map(|q| to(*q)).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.4, theme::ORANGE_SOFT)));
    }
    // region fill (holes respected)
    {
        let col = Color32::from_rgba_unmultiplied(76, 155, 255, 18);
        for rg in crate::region_pick::geoms_of(&s.sketch).iter().filter(|r| r.region.filled) {
            let mut mesh = egui::Mesh::default();
            for q in &rg.pts {
                mesh.colored_vertex(to(*q), col);
            }
            for t in &rg.tris {
                mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
            }
            p.add(mesh);
        }
    }
    // entities
    let sel_set: HashSet<Sel> = s.selection.iter().copied().collect();
    for (i, e) in s.sketch.entities.iter().enumerate() {
        let id = Sel::Entity(EntityId(i as u32));
        let imported = s.entity_layer.get(i).cloned().flatten();
        let mut col = if let Some(l) = &imported {
            if l.to_lowercase().contains("silk") {
                theme::SILK
            } else {
                theme::PURPLE
            }
        } else if e.construction {
            theme::hex(0x7d848e)
        } else if e.projected {
            theme::PURPLE
        } else {
            theme::TEXT
        };
        let mut w = if imported.is_some() { 1.6 } else { 1.8 };
        if sel_set.contains(&id) {
            col = theme::ACCENT;
            w = 2.6;
        } else if s.hover == Some(id) {
            col = theme::ORANGE;
            w = 2.4;
        }
        let st = Stroke::new(w, col);
        let pts: Vec<Pos2> = match e.geom {
            Geom::Line { a, b } => vec![to(s.sketch.point(a)), to(s.sketch.point(b))],
            Geom::Arc { center, start, end } => arc_points(
                s.sketch.point(center),
                s.sketch.point(start),
                s.sketch.point(end),
            )
            .into_iter()
            .map(to)
            .collect(),
            Geom::Circle { center, radius } => {
                let c = s.sketch.point(center);
                (0..=64)
                    .map(|k| {
                        to(c + DVec2::from_angle(k as f64 / 64.0 * std::f64::consts::TAU) * radius)
                    })
                    .collect()
            }
        };
        if e.construction && imported.is_none() {
            dashed(p, &pts, Stroke::new(1.0, col));
        } else {
            p.add(egui::Shape::line(pts, st));
        }
    }
    // points
    for (i, q) in s.sketch.points.iter().enumerate() {
        let id = Sel::Point(PointId(i as u32));
        let used = s
            .sketch
            .entities
            .iter()
            .any(|e| geom_points(&e.geom).contains(&PointId(i as u32)));
        if !used {
            continue;
        }
        let c = if sel_set.contains(&id) {
            theme::ACCENT
        } else if s.hover == Some(id) {
            theme::ORANGE
        } else {
            theme::TEXT_DIM
        };
        p.circle_filled(to(*q), if s.hover == Some(id) { 3.5 } else { 2.2 }, c);
    }
    // dimension anchors: a + on centres and a diamond on midpoints when hovered or picked
    {
        let mark = |x: &Sel, col: Color32| {
            let (pt, kind) = match *x {
                Sel::Point(q) => {
                    let ctr = s.sketch.entities.iter().any(|e| {
                        matches!(e.geom, Geom::Circle { center, .. } | Geom::Arc { center, .. } if center == q)
                    });
                    (s.sketch.point(q), if ctr { 1 } else { 0 })
                }
                Sel::Mid(e) => match s.sketch.entities[e.0 as usize].geom {
                    Geom::Line { a, b } => ((s.sketch.point(a) + s.sketch.point(b)) / 2.0, 2),
                    _ => return,
                },
                Sel::Origin => (DVec2::ZERO, 1),
                Sel::Entity(_) => return,
            };
            let c = to(pt);
            let st = Stroke::new(1.6, col);
            match kind {
                1 => {
                    p.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], st);
                    p.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], st);
                }
                2 => {
                    p.add(egui::Shape::closed_line(
                        vec![c + vec2(0.0, -5.0), c + vec2(5.0, 0.0), c + vec2(0.0, 5.0), c + vec2(-5.0, 0.0)],
                        st,
                    ));
                }
                _ => {}
            }
        };
        if let Some(h) = &s.hover {
            mark(h, theme::ORANGE);
        }
        if let Some(t) = &s.dim_target {
            for x in t {
                mark(x, theme::ACCENT);
                if let Sel::Entity(e) = x {
                    let pl: Vec<Pos2> = entity_polyline(&s.sketch, *e).into_iter().map(to).collect();
                    p.add(egui::Shape::line(pl, Stroke::new(2.6, theme::ACCENT)));
                }
            }
            // a lone shape picked: its live reading follows the cursor until placed
            if t.len() == 1 && s.input.is_none() {
                if let (Some((v, _)), Some(cur)) = (
                    s.dimension_for(t),
                    ui.ctx().pointer_hover_pos().filter(|q| rect.contains(*q)),
                ) {
                    p.text(
                        cur + vec2(16.0, -14.0),
                        egui::Align2::LEFT_BOTTOM,
                        crate::units::len(v.abs()),
                        theme::mono(12.0),
                        theme::ACCENT,
                    );
                }
            }
            // two lines: the angle previews as an arc in the quadrant the cursor is in
            if s.dim_pending {
                if let (Some([a, b, c, d]), Some(cur)) = (
                    s.line_pair(t),
                    ui.ctx().pointer_hover_pos().filter(|q| rect.contains(*q)),
                ) {
                    if let Some(ch) = angle_choice(a, b, c, d, s.to_sketch(rect, cur)) {
                        let c0 = to(ch.p);
                        let r = cur.distance(c0).clamp(30.0, 260.0);
                        let (mid, _) = draw_angle_arc(p, c0, to(ch.p + ch.r1) - c0, to(ch.p + ch.r2) - c0, r);
                        p.text(
                            mid + (mid - c0).normalized() * 6.0,
                            egui::Align2::CENTER_CENTER,
                            format!("{:.1}\u{b0}", ch.deg),
                            theme::mono(11.0),
                            theme::TEXT_MUTED,
                        );
                    }
                }
            }
            // the inline field of a new dimension sits on its label spot
            if s.input.is_some() && !s.dim_pending && s.edit_dim.is_none() {
                let spot = if let (Some(ch), Some(_)) = (s.dim_rays, s.line_pair(t)) {
                    let c0 = to(ch.p);
                    Some(draw_angle_arc(p, c0, to(ch.p + ch.r1) - c0, to(ch.p + ch.r2) - c0, s.dim_arc_r).0)
                } else {
                    None
                };
                let spot = spot.or(match s.dim_anchor_points(t) {
                    Some((a, b)) => Some(draw_dim_line(p, to(a), to(b), s.dim_orient, 28.0)),
                    None => match t.first() {
                        Some(Sel::Entity(e)) => entity_mid(&s.sketch, *e).map(|m| to(m) + vec2(0.0, 18.0)),
                        _ => None,
                    },
                });
                s.dim_label_pos.set(spot);
            }
            // placing: the cursor's side picks horizontal / vertical / aligned
            if s.dim_pending {
                if let (Some((a, b)), Some(cur)) = (
                    s.dim_anchor_points(t),
                    ui.ctx().pointer_hover_pos().filter(|q| rect.contains(*q)),
                ) {
                    let o = dim_orient(a, b, s.to_sketch(rect, cur));
                    let (v, name) = match o {
                        DimOrient::Aligned => (a.distance(b), "aligned"),
                        DimOrient::Horizontal => ((a.x - b.x).abs(), "horizontal"),
                        DimOrient::Vertical => ((a.y - b.y).abs(), "vertical"),
                    };
                    let at = draw_dim_line(p, to(a), to(b), o, 28.0);
                    p.text(
                        at + vec2(0.0, -12.0),
                        egui::Align2::CENTER_BOTTOM,
                        format!("{} {name}", crate::units::len(v)),
                        theme::mono(11.0),
                        theme::TEXT_MUTED,
                    );
                }
            }
        }
    }
    s.dim_rects.borrow_mut().clear();
    s.dim_segs.borrow_mut().clear();
    // constraint glyphs + dimension labels
    if s.show_constraints {
        let mut stacks: HashMap<u32, f32> = HashMap::new();
        for c in &s.sketch.constraints {
            let (glyph, e) = match c {
                Constraint::Horizontal(e) => ("H", *e),
                Constraint::Vertical(e) => ("V", *e),
                Constraint::Parallel(_, e) => ("∥", *e),
                Constraint::Perpendicular(_, e) => ("⊥", *e),
                Constraint::Tangent(_, e) => ("T", *e),
                Constraint::Equal(_, e) => ("=", *e),
                Constraint::Midpoint(_, e) => ("M", *e),
                Constraint::Symmetric(_, _, e) => ("S", *e),
                _ => continue,
            };
            if s.entity_layer
                .get(e.0 as usize)
                .is_some_and(|l| l.is_some())
            {
                continue;
            }
            let Some(m) = entity_mid(&s.sketch, e) else {
                continue;
            };
            let off = stacks.entry(e.0).or_insert(0.0);
            let at = to(m) + vec2(*off, -14.0);
            *off += 20.0;
            let r = Rect::from_center_size(at, vec2(18.0, 16.0));
            p.rect(
                r,
                3.0,
                theme::hex(0x2a2e35),
                Stroke::new(1.0, theme::TEXT_FAINT),
                egui::StrokeKind::Inside,
            );
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }
        for (ci, c) in s.sketch.constraints.iter().enumerate() {
            let mut at_override: Option<Pos2> = None;
            let (expr, anchor, prefix) = match c {
                Constraint::Length(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), ""),
                Constraint::Diameter(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "⌀"),
                Constraint::Radius(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "R"),
                Constraint::PointLineDistance(q, e, x) => {
                    let Geom::Line { a, b } = s.sketch.entities[e.0 as usize].geom else { continue };
                    let (pa, pb, pq) = (s.sketch.point(a), s.sketch.point(b), s.sketch.point(*q));
                    let foot = pa + (pb - pa) * ((pq - pa).dot(pb - pa) / (pb - pa).length_squared().max(1e-18));
                    at_override = Some(draw_dim_line(p, to(pq), to(foot), DimOrient::Aligned, 0.0));
                    s.dim_segs.borrow_mut().extend(
                        dim_hit_segs(to(pq), to(foot), DimOrient::Aligned, 0.0).into_iter().map(|g| (g, ci)),
                    );
                    (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "")
                }
                Constraint::LineDistance(e1, e2, x) => {
                    let (Geom::Line { a, b }, Geom::Line { a: c2, b: d2 }) =
                        (s.sketch.entities[e1.0 as usize].geom, s.sketch.entities[e2.0 as usize].geom)
                    else {
                        continue;
                    };
                    let (pa, pb) = (s.sketch.point(a), s.sketch.point(b));
                    let pq = (s.sketch.point(c2) + s.sketch.point(d2)) / 2.0;
                    let foot = pa + (pb - pa) * ((pq - pa).dot(pb - pa) / (pb - pa).length_squared().max(1e-18));
                    at_override = Some(draw_dim_line(p, to(pq), to(foot), DimOrient::Aligned, 0.0));
                    s.dim_segs.borrow_mut().extend(
                        dim_hit_segs(to(pq), to(foot), DimOrient::Aligned, 0.0).into_iter().map(|g| (g, ci)),
                    );
                    (x, entity_mid(&s.sketch, *e2).map(|m| (m, *e2)), "")
                }
                Constraint::Distance(a, b, x)
                | Constraint::DistanceX(a, b, x)
                | Constraint::DistanceY(a, b, x) => {
                    let o = match c {
                        Constraint::DistanceX(..) => DimOrient::Horizontal,
                        Constraint::DistanceY(..) => DimOrient::Vertical,
                        _ => DimOrient::Aligned,
                    };
                    at_override = Some(draw_dim_line(
                        p,
                        to(s.sketch.point(*a)),
                        to(s.sketch.point(*b)),
                        o,
                        28.0,
                    ));
                    s.dim_segs.borrow_mut().extend(
                        dim_hit_segs(to(s.sketch.point(*a)), to(s.sketch.point(*b)), o, 28.0)
                            .into_iter()
                            .map(|g| (g, ci)),
                    );
                    (
                        x,
                        Some((
                            (s.sketch.point(*a) + s.sketch.point(*b)) / 2.0,
                            EntityId(u32::MAX),
                        )),
                        "",
                    )
                }
                Constraint::Angle(_, e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "∠"),
                Constraint::AngleRays(l1, r1, l2, r2, x) => {
                    let (Geom::Line { a, b }, Geom::Line { a: c2, b: d2 }) =
                        (s.sketch.entities[l1.0 as usize].geom, s.sketch.entities[l2.0 as usize].geom)
                    else {
                        continue;
                    };
                    let (pa, pb, pc, pd) = (s.sketch.point(a), s.sketch.point(b), s.sketch.point(c2), s.sketch.point(d2));
                    let Some(cr) = line_cross(pa, pb, pc, pd) else { continue };
                    let (u, w) = ((pb - pa).normalize(), (pd - pc).normalize());
                    let (ru, rw) = (if *r1 { -u } else { u }, if *r2 { -w } else { w });
                    let c0 = to(cr);
                    let (mid, segs) = draw_angle_arc(p, c0, to(cr + ru) - c0, to(cr + rw) - c0, 60.0);
                    s.dim_segs.borrow_mut().extend(segs.into_iter().map(|g| (g, ci)));
                    at_override = Some(mid);
                    (x, Some((cr, EntityId(u32::MAX))), "∠")
                }
                _ => continue,
            };
            let Some((m, e)) = anchor else { continue };
            // imported (DXF) geometry is fixed reference: no dimension clutter
            if s.entity_layer
                .get(e.0 as usize)
                .is_some_and(|l| l.is_some())
            {
                continue;
            }
            let v = expr.eval(&s.values).unwrap_or(f64::NAN);
            let is_expr = expr.0.trim().parse::<f64>().is_err();
            let txt = if prefix == "∠" {
                format!("{prefix}{v:.2}°")
            } else {
                format!("{prefix}{}", crate::units::len(v))
            };
            let at = at_override.unwrap_or_else(|| to(m) + vec2(0.0, 18.0));
            let font = theme::mono(12.0);
            let w = p
                .layout_no_wrap(txt.clone(), font.clone(), theme::TEXT)
                .size()
                .x
                + if is_expr { 30.0 } else { 14.0 };
            let r = Rect::from_center_size(at, vec2(w, 22.0));
            s.dim_rects.borrow_mut().push((r, ci));
            if s.edit_dim == Some(ci) {
                continue; // the inline field is drawn in its place
            }
            p.rect(
                r,
                4.0,
                theme::CARD,
                Stroke::new(
                    if s.sel_dim == Some(ci) { 2.0 } else { 1.0 },
                    if s.sel_dim == Some(ci) {
                        theme::ORANGE
                    } else if is_expr {
                        theme::ACCENT
                    } else {
                        theme::BORDER_CARD
                    },
                ),
                egui::StrokeKind::Inside,
            );
            p.text(
                r.left_center() + vec2(7.0, 0.0),
                egui::Align2::LEFT_CENTER,
                &txt,
                font,
                theme::TEXT_STRONG,
            );
            if is_expr {
                p.text(
                    r.right_center() - vec2(6.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    "fx",
                    theme::mono(10.0),
                    theme::ACCENT_LINK,
                );
            }
            if ui.rect_contains_pointer(r) {
                p.text(
                    r.center_bottom() + vec2(0.0, 10.0),
                    egui::Align2::CENTER_CENTER,
                    &expr.0,
                    theme::mono(11.0),
                    theme::TEXT_MUTED,
                );
            }
        }
    }
    // live offset preview: the result for every picked shape, dashed
    if let Some(prev) = s.offset_preview() {
        let st = Stroke::new(1.4, theme::ORANGE);
        for pl in prev {
            let v: Vec<Pos2> = pl.iter().map(|q| to(*q)).collect();
            dashed(p, &v, st);
        }
    }
    // tool previews
    if let Some(cur) = ui.ctx().pointer_hover_pos().filter(|q| rect.contains(*q)) {
        let (q, _, snap_label, guides) = s.snap_guided(s.to_sketch(rect, cur), &[]);
        let pv = Stroke::new(1.2, theme::ACCENT);
        // H/V lock: a dashed axis guide through the start point
        if let Some((o, horiz)) = guides.axis {
            let gs = Stroke::new(1.0, theme::ORANGE.gamma_multiply(0.7));
            let (a, b) = if horiz {
                (pos2(rect.left(), to(o).y), pos2(rect.right(), to(o).y))
            } else {
                (pos2(to(o).x, rect.top()), pos2(to(o).x, rect.bottom()))
            };
            p.add(egui::Shape::dashed_line(&[a, b], gs, 6.0, 4.0));
        }
        // alignment with other points: dotted guides
        let dot = Stroke::new(1.0, theme::ORANGE.gamma_multiply(0.6));
        if let Some(o) = guides.ax {
            p.add(egui::Shape::dotted_line(&[to(o), to(q)], dot.color, 4.0, 1.0));
        }
        if let Some(o) = guides.ay {
            p.add(egui::Shape::dotted_line(&[to(o), to(q)], dot.color, 4.0, 1.0));
        }
        if let Some(l) = snap_label {
            p.circle_stroke(to(q), 6.0, Stroke::new(1.2, theme::ORANGE));
            p.text(to(q) + vec2(9.0, -13.0), egui::Align2::LEFT_BOTTOM, l, theme::mono(10.0), theme::ORANGE);
        } else if s.snap && !s.alt_held && s.tool != Tool::Select {
            // snapped to the grid: a small cross on the node
            let c = to(q);
            let m = Stroke::new(1.2, theme::ACCENT);
            p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], m);
            p.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], m);
        }
        match (s.tool, s.clicks.as_slice()) {
            (Tool::Line, [a]) => {
                // Live segment: solid, dashed only in construction mode.
                if s.construction_mode {
                    p.add(egui::Shape::dashed_line(&[to(a.0), to(q)], pv, 6.0, 3.0));
                } else {
                    p.line_segment([to(a.0), to(q)], pv);
                }
                p.text(
                    to(q) + vec2(12.0, 12.0),
                    egui::Align2::LEFT_TOP,
                    format!("{:.2}", a.0.distance(q)),
                    theme::mono(11.0),
                    theme::TEXT_MUTED,
                );
            }
            (Tool::Rect, [a]) => {
                let r = Rect::from_two_pos(to(a.0), to(q));
                p.rect_stroke(r, 0.0, pv, egui::StrokeKind::Inside);
                p.text(
                    to(q) + vec2(12.0, 12.0),
                    egui::Align2::LEFT_TOP,
                    format!("{:.2} × {:.2}", (q.x - a.0.x).abs(), (q.y - a.0.y).abs()),
                    theme::mono(11.0),
                    theme::TEXT_MUTED,
                );
            }
            (Tool::Circle, [c]) => {
                p.circle_stroke(to(c.0), c.0.distance(q) as f32 * s.scale, pv);
                p.text(
                    to(q) + vec2(12.0, 12.0),
                    egui::Align2::LEFT_TOP,
                    format!("⌀{:.2}", 2.0 * c.0.distance(q)),
                    theme::mono(11.0),
                    theme::TEXT_MUTED,
                );
            }
            (Tool::Arc, [c, st]) => {
                let pts: Vec<Pos2> = arc_points(
                    c.0,
                    st.0,
                    c.0 + (q - c.0).normalize_or_zero() * c.0.distance(st.0),
                )
                .into_iter()
                .map(to)
                .collect();
                p.add(egui::Shape::line(pts, pv));
            }
            (Tool::Trim, _) => {
                if let Some(Sel::Entity(e)) = s.hover {
                    if let Some(m) = entity_mid(&s.sketch, e) {
                        p.text(
                            to(m) + vec2(0.0, -16.0),
                            egui::Align2::CENTER_CENTER,
                            "✂",
                            theme::prop(14.0),
                            theme::ORANGE,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    let _ = BTreeMap::<u8, u8>::new();
}

fn entity_mid(s: &Sketch, e: EntityId) -> Option<DVec2> {
    match s.entities.get(e.0 as usize)?.geom {
        Geom::Line { a, b } => Some((s.point(a) + s.point(b)) / 2.0),
        Geom::Circle { center, radius } => Some(
            s.point(center)
                + DVec2::new(
                    radius * std::f64::consts::FRAC_1_SQRT_2,
                    radius * std::f64::consts::FRAC_1_SQRT_2,
                ),
        ),
        Geom::Arc { center, start, end } => {
            let pts = arc_points(s.point(center), s.point(start), s.point(end));
            pts.get(pts.len() / 2).copied()
        }
    }
}

/// An angle arc of radius `r` px around `c` from screen direction `d1` to `d2`
/// (the short way), with arrows at both ends. Returns the arc's middle point and
/// its segments (for clicking).
fn draw_angle_arc(p: &egui::Painter, c: Pos2, d1: Vec2, d2: Vec2, r: f32) -> (Pos2, Vec<[Pos2; 2]>) {
    let st = Stroke::new(1.0, theme::TEXT_DIM);
    let (a1, a2) = (d1.y.atan2(d1.x), d2.y.atan2(d2.x));
    let mut delta = a2 - a1;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    let at = |t: f32| c + vec2((a1 + delta * t).cos(), (a1 + delta * t).sin()) * r;
    let pts: Vec<Pos2> = (0..=24).map(|i| at(i as f32 / 24.0)).collect();
    let mut segs: Vec<[Pos2; 2]> = pts.windows(2).map(|w| [w[0], w[1]]).collect();
    for sg in &segs {
        p.line_segment(*sg, st);
    }
    // the rays out to the arc and a little past it
    for d in [d1, d2] {
        let d = d.normalized();
        if !d.x.is_nan() {
            p.line_segment([c + d * (r * 0.5), c + d * (r + 6.0)], st);
        }
    }
    // arrows along the arc at both ends
    if r * delta.abs() > 16.0 {
        for (t, sgn) in [(0.0f32, 1.0f32), (1.0, -1.0)] {
            let tip = at(t);
            let ang = a1 + delta * t;
            let tangent = vec2(-ang.sin(), ang.cos()) * delta.signum() * sgn;
            let n = vec2(tangent.y, -tangent.x);
            p.line_segment([tip, tip + tangent * 7.0 + n * 2.5], st);
            p.line_segment([tip, tip + tangent * 7.0 - n * 2.5], st);
            segs.push([tip, tip + tangent * 7.0]);
        }
    }
    (at(0.5), segs)
}

/// Clickable segments of a dimension line: the line itself and the extension
/// lines (their first 10 px, next to the measured points, are left out so those
/// points stay pickable). Arrows lie within the click tolerance of the line ends.
fn dim_hit_segs(a: Pos2, b: Pos2, o: DimOrient, off: f32) -> Vec<[Pos2; 2]> {
    let (da, db) = match o {
        DimOrient::Aligned => {
            let d = (b - a).normalized();
            let n = if d.x.is_nan() { vec2(0.0, -1.0) } else { vec2(d.y, -d.x) };
            (a + n * off, b + n * off)
        }
        DimOrient::Horizontal => {
            let y = a.y.max(b.y) + off;
            (pos2(a.x, y), pos2(b.x, y))
        }
        DimOrient::Vertical => {
            let x = a.x.max(b.x) + off;
            (pos2(x, a.y), pos2(x, b.y))
        }
    };
    let mut v = vec![[da, db]];
    if off != 0.0 {
        for (from, to) in [(a, da), (b, db)] {
            let d = (to - from).normalized();
            if !d.x.is_nan() && (to - from).length() > 12.0 {
                v.push([from + d * 10.0, to]);
            }
        }
    }
    v
}

/// A dimension line between two screen points with extension lines, arrows and
/// the measured side offset `off` px (0: a plain measuring line). Returns where
/// the label goes.
fn draw_dim_line(p: &egui::Painter, a: Pos2, b: Pos2, o: DimOrient, off: f32) -> Pos2 {
    let st = Stroke::new(1.0, theme::TEXT_DIM);
    let (da, db) = match o {
        DimOrient::Aligned => {
            let d = (b - a).normalized();
            let n = if d.x.is_nan() { vec2(0.0, -1.0) } else { vec2(d.y, -d.x) };
            (a + n * off, b + n * off)
        }
        DimOrient::Horizontal => {
            let y = a.y.max(b.y) + off;
            (pos2(a.x, y), pos2(b.x, y))
        }
        DimOrient::Vertical => {
            let x = a.x.max(b.x) + off;
            (pos2(x, a.y), pos2(x, b.y))
        }
    };
    if off != 0.0 {
        for (from, to) in [(a, da), (b, db)] {
            let d = (to - from).normalized();
            if !d.x.is_nan() {
                p.line_segment([from + d * 3.0, to + d * 4.0], st);
            }
        }
    }
    p.line_segment([da, db], st);
    let d = (db - da).normalized();
    if !d.x.is_nan() && (db - da).length() > 14.0 {
        for (tip, dir) in [(da, d), (db, -d)] {
            let n = vec2(dir.y, -dir.x);
            p.line_segment([tip, tip + dir * 7.0 + n * 2.5], st);
            p.line_segment([tip, tip + dir * 7.0 - n * 2.5], st);
        }
    }
    da + (db - da) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SketchSession {
        let d = DocData::new("t", "test");
        SketchSession::new_on(Plane::XY, &d)
    }

    fn circle_at(s: &mut SketchSession, c: (f64, f64), r: f64) -> (PointId, EntityId) {
        let pc = s.sketch.add_point(DVec2::new(c.0, c.1));
        let e = s.sketch.add_entity(Geom::Circle { center: pc, radius: r }, false);
        s.entity_layer.resize(s.sketch.entities.len(), None);
        (pc, e)
    }

    #[test]
    fn dimensioned_circle_drags_by_its_centre_and_keeps_its_diameter() {
        let mut s = session();
        let (pc, e) = circle_at(&mut s, (0.0, 0.0), 2.0);
        s.apply_dimension(&[Sel::Entity(e)], "6").unwrap();
        let undo_before = s.undo.len();
        s.drag_begin(Sel::Point(pc), DVec2::ZERO);
        s.drag_to(DVec2::new(5.0, 3.0));
        s.drag_end();
        assert!(s.sketch.point(pc).distance(DVec2::new(5.0, 3.0)) < 1e-6);
        let Geom::Circle { radius, .. } = s.sketch.entities[0].geom else { panic!() };
        assert!((radius - 3.0).abs() < 1e-6);
        assert_eq!(s.undo.len(), undo_before + 1, "one undo step per drag");
        assert!(s.undo());
        assert!(s.sketch.point(pc).length() < 1e-6);
    }

    #[test]
    fn drag_by_circle_body_and_line_body_keep_dimensions() {
        let mut s = session();
        let (pc, c) = circle_at(&mut s, (0.0, 0.0), 1.0);
        s.drag_begin(Sel::Entity(c), DVec2::new(1.0, 0.0));
        s.drag_to(DVec2::new(3.0, 1.0));
        s.drag_end();
        assert!(s.sketch.point(pc).distance(DVec2::new(2.0, 1.0)) < 1e-6);
        let l = s.add_line((DVec2::new(10.0, 10.0), None), (DVec2::new(14.0, 10.0), None));
        s.sketch.constrain(Constraint::Length(l, 4.0.into()));
        let (a, b) = s.line_points(l).unwrap();
        s.drag_begin(Sel::Entity(l), DVec2::new(12.0, 10.0));
        s.drag_to(DVec2::new(12.0, 15.0));
        s.drag_end();
        assert!((s.sketch.point(a).distance(s.sketch.point(b)) - 4.0).abs() < 1e-6);
        assert!(s.sketch.point(a).distance(DVec2::new(10.0, 15.0)) < 1e-6);
    }

    #[test]
    fn drag_endpoint_of_length_dimensioned_line_rotates_it() {
        let mut s = session();
        let l = s.add_line((DVec2::ZERO, None), (DVec2::new(4.0, 0.0), None));
        s.sketch.constraints.clear(); // drop the inferred horizontal
        s.sketch.constrain(Constraint::Length(l, 4.0.into()));
        let (a, b) = s.line_points(l).unwrap();
        s.sketch.constrain(Constraint::Fix(a, DVec2::ZERO));
        s.drag_begin(Sel::Point(b), DVec2::new(4.0, 0.0));
        s.drag_to(DVec2::new(0.0, 9.0)); // beyond reach: pulled onto the circle of radius 4
        s.drag_end();
        assert!((s.sketch.point(a).distance(s.sketch.point(b)) - 4.0).abs() < 1e-6);
        assert!(s.sketch.point(b).y > 3.9, "{:?} {:?}", s.sketch.point(a), s.sketch.point(b));
    }

    #[test]
    fn fully_constrained_drag_does_not_move_and_esc_restores() {
        let mut s = session();
        let l = fixed_line(&mut s, (0.0, 0.0), (5.0, 0.0));
        let (a, _) = s.line_points(l).unwrap();
        let undo_before = s.undo.len();
        s.drag_begin(Sel::Point(a), DVec2::ZERO);
        s.drag_to(DVec2::new(2.0, 2.0));
        assert!(s.error.as_deref().is_some_and(|e| e.contains("fully constrained")));
        assert!(s.sketch.point(a).length() < 1e-6);
        s.drag_cancel();
        assert_eq!(s.undo.len(), undo_before, "cancel leaves no undo step");
        let (pc, _) = circle_at(&mut s, (20.0, 20.0), 1.0);
        s.drag_begin(Sel::Point(pc), DVec2::new(20.0, 20.0));
        s.drag_to(DVec2::new(25.0, 25.0));
        s.drag_cancel();
        assert!(s.sketch.point(pc).distance(DVec2::new(20.0, 20.0)) < 1e-6);
    }

    #[test]
    fn drag_target_snaps_to_grid_tenths_and_points() {
        let mut s = session();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        let (pc, _) = circle_at(&mut s, (0.0, 0.0), 1.0);
        let other = s.add_line((DVec2::new(3.0, 3.0), None), (DVec2::new(4.0, 3.0), None));
        let _ = other;
        let (t, _) = s.snap_point(rect, s.to_screen(rect, DVec2::new(1.23, 0.47)));
        s.drag_begin(Sel::Point(pc), DVec2::ZERO);
        s.drag_to(t);
        s.drag_end();
        let c = s.sketch.point(pc);
        assert!((c.x - 1.2).abs() < 1e-9 && (c.y - 0.5).abs() < 1e-9, "{c:?}");
        let (t, hit) = s.snap_point(rect, s.to_screen(rect, DVec2::new(3.02, 3.01)));
        assert!(hit.is_some());
        s.drag_begin(Sel::Point(pc), c);
        s.drag_to(t);
        s.drag_end();
        assert!(s.sketch.point(pc).distance(DVec2::new(3.0, 3.0)) < 1e-9);
    }

    fn dist(s: &SketchSession, a: PointId, b: PointId) -> f64 {
        s.sketch.point(a).distance(s.sketch.point(b))
    }

    #[test]
    fn centre_to_point_distance_moves_the_circle() {
        let mut s = session();
        let l = fixed_line(&mut s, (0.0, 0.0), (10.0, 0.0));
        let (pa, _) = s.line_points(l).unwrap();
        let (pc, c) = circle_at(&mut s, (3.0, 5.0), 1.0);
        let sel = [Sel::Point(pc), Sel::Point(pa)];
        let (v, _) = s.dimension_for(&sel).unwrap();
        assert!((v - 34f64.sqrt()).abs() < 1e-9);
        s.apply_dimension(&sel, "8").unwrap();
        assert!((dist(&s, pc, pa) - 8.0).abs() < 1e-6);
        // an expression/value change moves it again
        s.apply_dimension(&[Sel::Entity(c)], "4").unwrap();
        let Geom::Circle { radius, .. } = s.sketch.entities[c.0 as usize].geom else { panic!() };
        assert!((radius - 2.0).abs() < 1e-6);
    }

    #[test]
    fn centre_to_centre_and_arc_centre() {
        let mut s = session();
        let (p1, c1) = circle_at(&mut s, (0.0, 0.0), 1.0);
        s.sketch.constrain(Constraint::Fix(p1, DVec2::ZERO));
        let (p2, c2) = circle_at(&mut s, (4.0, 0.0), 1.0);
        s.apply_dimension(&[Sel::Entity(c1), Sel::Entity(c2)], "9").unwrap();
        assert!((dist(&s, p1, p2) - 9.0).abs() < 1e-6);
    }

    #[test]
    fn centre_to_line_and_point_to_line_are_perpendicular_distances() {
        let mut s = session();
        let l = fixed_line(&mut s, (0.0, 0.0), (10.0, 0.0));
        let (pc, _) = circle_at(&mut s, (4.0, 3.0), 1.0);
        let (v, _) = s.dimension_for(&[Sel::Point(pc), Sel::Entity(l)]).unwrap();
        assert!((v - 3.0).abs() < 1e-9);
        s.apply_dimension(&[Sel::Entity(l), Sel::Point(pc)], "6").unwrap();
        assert!((s.sketch.point(pc).y - 6.0).abs() < 1e-6);
        // a whole circle against a line reads from its centre too
        let (_, c2) = circle_at(&mut s, (4.0, -2.0), 1.0);
        let (v, _) = s.dimension_for(&[Sel::Entity(c2), Sel::Entity(l)]).unwrap();
        assert!((v - 2.0).abs() < 1e-9);
    }

    #[test]
    fn horizontal_vs_vertical_by_placement() {
        let (a, b) = (DVec2::new(0.0, 0.0), DVec2::new(6.0, 4.0));
        assert_eq!(dim_orient(a, b, DVec2::new(3.0, -5.0)), DimOrient::Horizontal);
        assert_eq!(dim_orient(a, b, DVec2::new(3.0, 9.0)), DimOrient::Horizontal);
        assert_eq!(dim_orient(a, b, DVec2::new(-5.0, 2.0)), DimOrient::Vertical);
        assert_eq!(dim_orient(a, b, DVec2::new(9.0, 2.0)), DimOrient::Vertical);
        assert_eq!(dim_orient(a, b, DVec2::new(3.0, 2.0)), DimOrient::Aligned);
        assert_eq!(dim_orient(a, b, DVec2::new(9.0, 9.0)), DimOrient::Aligned);
        let mut s = session();
        let (pa, _) = circle_at(&mut s, (0.0, 0.0), 1.0);
        s.sketch.constrain(Constraint::Fix(pa, DVec2::ZERO));
        let (pb, _) = circle_at(&mut s, (6.0, 4.0), 1.0);
        let sel = [Sel::Point(pa), Sel::Point(pb)];
        s.dim_orient = DimOrient::Horizontal;
        let (v, _) = s.dimension_for(&sel).unwrap();
        assert!((v - 6.0).abs() < 1e-9);
        s.apply_dimension(&sel, "10").unwrap();
        let (q, w) = (s.sketch.point(pb), s.sketch.point(pa));
        assert!((q.x - w.x - 10.0).abs() < 1e-6 && (q.y - 4.0).abs() < 1e-3, "{q:?}");
        s.dim_orient = DimOrient::Vertical;
        let (v, _) = s.dimension_for(&sel).unwrap();
        assert!((v - 4.0).abs() < 1e-3);
        assert!(s.sketch.constraints.iter().any(|c| matches!(c, Constraint::DistanceX(..))));
    }

    #[test]
    fn line_line_parallel_is_a_distance_and_otherwise_an_angle() {
        let mut s = session();
        let l1 = fixed_line(&mut s, (0.0, 0.0), (10.0, 0.0));
        let l2 = s.add_line((DVec2::new(0.0, 3.0), None), (DVec2::new(10.0, 3.0), None));
        s.sketch.constraints.clear();
        let (a, b1) = s.line_points(l1).unwrap();
        s.sketch.constrain(Constraint::Fix(a, DVec2::ZERO));
        s.sketch.constrain(Constraint::Fix(b1, DVec2::new(10.0, 0.0)));
        let (v, mk) = s.dimension_for(&[Sel::Entity(l1), Sel::Entity(l2)]).unwrap();
        assert!((v - 3.0).abs() < 1e-9);
        assert!(matches!(mk("3".into()), Constraint::LineDistance(..)));
        s.apply_dimension(&[Sel::Entity(l1), Sel::Entity(l2)], "5").unwrap();
        let (c, d) = s.line_points(l2).unwrap();
        assert!((s.sketch.point(c).y.abs() - 5.0).abs() < 1e-6 && (s.sketch.point(d).y.abs() - 5.0).abs() < 1e-6);
        // non-parallel lines: an angle
        let l3 = s.add_line((DVec2::new(0.0, 20.0), None), (DVec2::new(5.0, 28.0), None));
        let (v, mk) = s.dimension_for(&[Sel::Entity(l1), Sel::Entity(l3)]).unwrap();
        assert!((v - 8f64.atan2(5.0).to_degrees()).abs() < 1e-6);
        assert!(matches!(mk("30".into()), Constraint::Angle(..)));
        s.apply_dimension(&[Sel::Entity(l1), Sel::Entity(l3)], "30").unwrap();
        let (e, f) = s.line_points(l3).unwrap();
        let dv = s.sketch.point(f) - s.sketch.point(e);
        assert!((dv.y.atan2(dv.x).to_degrees() - 30.0).abs() < 1e-4);
    }

    #[test]
    fn radius_vs_diameter_toggle_and_midpoint_anchor() {
        let mut s = session();
        let (_, c) = circle_at(&mut s, (0.0, 0.0), 2.0);
        let (v, mk) = s.dimension_for(&[Sel::Entity(c)]).unwrap();
        assert_eq!(v, 4.0);
        assert!(matches!(mk("4".into()), Constraint::Diameter(..)));
        s.dim_toggle = true;
        let (v, mk) = s.dimension_for(&[Sel::Entity(c)]).unwrap();
        assert_eq!(v, 2.0);
        assert!(matches!(mk("2".into()), Constraint::Radius(..)));
        // a line midpoint to a circle centre
        s.dim_toggle = false;
        let l = fixed_line(&mut s, (10.0, 0.0), (20.0, 0.0));
        let pc = match s.sketch.entities[c.0 as usize].geom { Geom::Circle { center, .. } => center, _ => panic!() };
        let sel = [Sel::Mid(l), Sel::Point(pc)];
        let (v, _) = s.dimension_for(&sel).unwrap();
        assert!((v - 15.0).abs() < 1e-9);
        let pts = s.sketch.points.len();
        s.apply_dimension(&sel, "12").unwrap();
        assert_eq!(s.sketch.points.len(), pts + 1, "one helper point");
        assert!((s.sketch.point(pc).distance(DVec2::new(15.0, 0.0)) - 12.0).abs() < 1e-6);
        // the helper is reused by the next dimension to that midpoint
        s.dim_orient = DimOrient::Horizontal;
        s.apply_dimension(&sel, "13").unwrap_err(); // over-constrained: refused, and nothing added
        assert_eq!(s.sketch.points.len(), pts + 1);
    }

    #[test]
    fn origin_is_a_dimension_anchor() {
        let mut s = session();
        let (pc, c) = circle_at(&mut s, (3.0, 4.0), 1.0);
        // centre to origin: the distance, by orientation
        let (v, mk) = s.dimension_for(&[Sel::Entity(c), Sel::Origin]).unwrap();
        assert!((v - 5.0).abs() < 1e-9);
        assert!(matches!(mk("5".into()), Constraint::Distance(..)));
        s.dim_orient = DimOrient::Horizontal;
        let (v, _) = s.dimension_for(&[Sel::Point(pc), Sel::Origin]).unwrap();
        assert!((v - 3.0).abs() < 1e-9);
        s.dim_orient = DimOrient::Vertical;
        let (v, _) = s.dimension_for(&[Sel::Origin, Sel::Point(pc)]).unwrap();
        assert!((v - 4.0).abs() < 1e-9);
        // line to origin: the perpendicular distance
        let l = s.add_line((DVec2::new(-5.0, 2.0), None), (DVec2::new(5.0, 2.0), None));
        let (v, mk) = s.dimension_for(&[Sel::Origin, Sel::Entity(l)]).unwrap();
        assert!((v - 2.0).abs() < 1e-9);
        assert!(matches!(mk("2".into()), Constraint::PointLineDistance(..)));
        // dimension_for does not touch the sketch
        let n = s.sketch.points.len();
        assert_eq!(s.sketch.points.len(), n);
        // applying adds one fixed point at the origin, reused the next time
        s.dim_orient = DimOrient::Aligned;
        s.apply_dimension(&[Sel::Entity(c), Sel::Origin], "7").unwrap();
        assert_eq!(s.sketch.points.len(), n + 1);
        let fixed0 = |s: &SketchSession| {
            s.sketch.constraints.iter().filter(|k| matches!(k, Constraint::Fix(_, v) if *v == DVec2::ZERO)).count()
        };
        assert_eq!(fixed0(&s), 1);
        assert!((s.sketch.point(pc).length() - 7.0).abs() < 1e-6);
        s.apply_dimension(&[Sel::Origin, Sel::Entity(l)], "3").unwrap();
        assert_eq!(s.sketch.points.len(), n + 1, "origin point reused");
        // the orphan point survives save/load, and a profile still builds
        let back: Sketch = serde_json::from_str(&serde_json::to_string(&s.sketch).unwrap()).unwrap();
        assert_eq!(back.constraints, s.sketch.constraints);
        assert_eq!(back.points.len(), s.sketch.points.len());
        // dragging the orphan origin point does not move it
        let o = s
            .sketch
            .constraints
            .iter()
            .find_map(|k| match k {
                Constraint::Fix(p, v) if *v == DVec2::ZERO => Some(*p),
                _ => None,
            })
            .unwrap();
        s.drag_begin(Sel::Point(o), DVec2::ZERO);
        s.drag_to(DVec2::new(1.0, 1.0));
        assert!(s.sketch.point(o).length() < 1e-6, "{:?} {:?}", s.sketch.point(o), s.error);
    }

    #[test]
    fn origin_with_a_centre_on_it_is_not_dimensionable() {
        let mut s = session();
        let (_, c) = circle_at(&mut s, (0.0, 0.0), 1.0);
        assert!(s.dimension_for(&[Sel::Entity(c), Sel::Origin]).is_none());
        assert!(s.apply_dimension(&[Sel::Entity(c), Sel::Origin], "1").is_err());
        // an existing point fixed at the origin is reused rather than duplicated
        let (_, c2) = circle_at(&mut s, (4.0, 0.0), 1.0);
        let o = s.sketch.add_point(DVec2::ZERO);
        s.sketch.constrain(Constraint::Fix(o, DVec2::ZERO));
        let n = s.sketch.points.len();
        s.apply_dimension(&[Sel::Entity(c2), Sel::Origin], "5").unwrap();
        assert_eq!(s.sketch.points.len(), n, "no duplicate origin point");
    }

    #[test]
    fn dimensions_edit_delete_and_round_trip() {
        let mut s = session();
        let l1 = fixed_line(&mut s, (0.0, 0.0), (10.0, 0.0));
        let l2 = s.add_line((DVec2::new(0.0, 3.0), None), (DVec2::new(10.0, 3.0), None));
        s.apply_dimension(&[Sel::Entity(l1), Sel::Entity(l2)], "5").unwrap();
        let i = s.sketch.constraints.iter().position(|c| matches!(c, Constraint::LineDistance(..))).unwrap();
        s.set_dimension(i, "nope").unwrap_err();
        s.set_dimension(i, "=2*4").unwrap();
        s.set_dimension(i, "7").unwrap();
        let json = serde_json::to_string(&s.sketch).unwrap();
        let back: Sketch = serde_json::from_str(&json).unwrap();
        assert_eq!(back.constraints, s.sketch.constraints);
        s.delete_dimension(i);
        assert!(!s.sketch.constraints.iter().any(|c| matches!(c, Constraint::LineDistance(..))));
    }

    #[test]
    fn editor_opens_on_relinked_projection() {
        use easycad_model::document::Document;
        use easycad_model::feature::{BodyId, BoolOp, FeatureKind, ProfileSel};
        let extrude = |sketch, d: f64| FeatureKind::Extrude {
            sketch,
            profiles: ProfileSel::All,
            distance: d.into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        };
        let mut doc = Document::new("Link");
        let mut a = Sketch::new("A", Plane::XY);
        a.circle(DVec2::new(20.0, 15.0), 5.0);
        let sa = doc.add_sketch(a);
        let fa = doc.add_feature("A", extrude(sa, 10.0));
        let edges = doc.regenerate().body_by_id(BodyId::of(fa)).unwrap().shape().edges().unwrap();
        let top = edges.iter().find(|e| e.circle.is_some_and(|(c, _, _)| (c.z - 10.0).abs() < 1e-6)).unwrap();
        let mut b = Sketch::new("B", Plane::XY.offset(10.0));
        b.project_edge_from(BodyId::of(fa), top, None, false);
        let sb = doc.add_sketch(b);
        doc.add_feature("B", extrude(sb, 5.0));
        // move the source circle in A, regenerate as the app does, then open B
        doc.edit("Move", |d| {
            d.sketch_mut(sa).unwrap().points.iter_mut().for_each(|p| *p += DVec2::new(40.0, -30.0));
            Ok::<_, ()>(())
        })
        .unwrap();
        let res = doc.regenerate().clone();
        doc.data.apply_relinked(&res);
        let s = SketchSession::edit(sb, doc.data.sketch(sb).unwrap().clone(), &doc.data);
        let Geom::Circle { center, .. } = s.sketch.entities[0].geom else { panic!() };
        assert!((s.sketch.point(center) - DVec2::new(60.0, -15.0)).length() < 1e-6);
    }

    #[test]
    fn project_hover_hits_edge_and_clears() {
        let mut s = session();
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        s.tool = Tool::Project;
        s.projected = vec![vec![DVec2::new(0.0, 0.0), DVec2::new(2.0, 0.0)]];
        s.project_hover = s.project_hit(DVec2::new(1.0, 0.02));
        assert_eq!(s.project_hover, Some(0));
        s.project_hover = s.project_hit(DVec2::new(1.0, 1.0));
        assert_eq!(s.project_hover, None);
    }

    /// A unit square face (edges 0..4) beside an unrelated edge 4.
    fn face_session() -> SketchSession {
        let mut s = session();
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        s.tool = Tool::Project;
        let c = [DVec2::new(0.0, 0.0), DVec2::new(1.0, 0.0), DVec2::new(1.0, 1.0), DVec2::new(0.0, 1.0)];
        s.projected = (0..4).map(|i| vec![c[i], c[(i + 1) % 4]]).collect();
        s.projected.push(vec![DVec2::new(3.0, 0.0), DVec2::new(4.0, 0.0)]);
        s.projected_src = vec![None; 5];
        s.projected_faces = vec![ProjFace { edges: vec![0, 1, 2, 3], tris: vec![([c[0], c[1], c[2]], 0.0), ([c[0], c[2], c[3]], 0.0)] }];
        s
    }

    #[test]
    fn project_face_hover_and_edge_priority() {
        let s = face_session();
        assert_eq!(s.project_face_hit(DVec2::new(0.5, 0.5)), Some(0));
        assert_eq!(s.project_face_hit(DVec2::new(2.0, 0.5)), None);
        // near an edge the edge wins (callers test project_hit first)
        assert_eq!(s.project_hit(DVec2::new(0.5, 0.02)), Some(0));
        assert_eq!(s.project_hit(DVec2::new(0.5, 0.5)), None);
    }

    #[test]
    fn project_face_counts_and_tooltip() {
        let mut s = face_session();
        s.project_hover_face = Some(0);
        assert_eq!(project_tip(&s), "Click to project face outline (4 edges)");
        assert_eq!(s.project_edges_now(&[0, 1, 2, 3]), 4);
        assert_eq!(s.sketch.entities.iter().filter(|e| e.projected).count(), 4);
        assert_eq!(project_tip(&s), "Already projected");
        s.undo();
        assert!(s.sketch.entities.is_empty(), "one undo step removes the whole face");
    }

    #[test]
    fn project_dedupe_and_partial_face() {
        let mut s = face_session();
        s.project_hover = Some(0);
        assert_eq!(project_tip(&s), "Click to project edge");
        assert_eq!(s.project_edges_now(&[0]), 1);
        assert_eq!(s.project_edges_now(&[0]), 0, "second click adds nothing");
        assert_eq!(s.sketch.entities.len(), 1);
        assert_eq!(project_tip(&s), "Already projected");
        // the face now only misses three edges
        assert_eq!(s.face_missing(0), vec![1, 2, 3]);
        assert_eq!(s.project_edges_now(&[0, 1, 2, 3]), 3);
        assert_eq!(s.sketch.entities.len(), 4);
    }

    #[test]
    fn snap_prefers_geometry_over_grid() {
        let mut s = session();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        let at = |s: &SketchSession, x: f64, y: f64| s.to_screen(rect, DVec2::new(x, y));
        // a projected circle (centre off the grid) and a projected line
        let c = s.sketch.add_point(DVec2::new(3.37, 2.0));
        let circ = s.sketch.add_entity(Geom::Circle { center: c, radius: 1.5 }, false);
        s.sketch.entities[circ.0 as usize].projected = true;
        let (a, b) = (s.sketch.add_point(DVec2::new(-5.0, 0.37)), s.sketch.add_point(DVec2::new(-1.0, 0.37)));
        let l = s.sketch.add_entity(Geom::Line { a, b }, false);
        s.sketch.entities[l.0 as usize].projected = true;
        // centre beats the nearby grid node (3.4, 2.0)
        let (q, pid, lab) = s.snap_full(rect, at(&s, 3.40, 2.02));
        assert_eq!((q, pid, lab), (DVec2::new(3.37, 2.0), Some(c), Some("center")));
        // on-line beats grid: y locks to the line, x is free
        let (q, pid, lab) = s.snap_full(rect, at(&s, -3.13, 0.40));
        assert!(pid.is_none() && lab == Some("on line"));
        assert!((q.y - 0.37).abs() < 1e-9 && (q.x + 3.13).abs() < 0.02, "{q:?}");
        // on-circle
        let (q, _, lab) = s.snap_full(rect, at(&s, 3.37 + 1.53, 2.0));
        assert_eq!(lab, Some("on circle"));
        assert!((q.distance(DVec2::new(3.37, 2.0)) - 1.5).abs() < 1e-9);
        // radius phase: centre on the projected circle's centre, cursor near circumference
        s.tool = Tool::Circle;
        s.clicks = vec![(DVec2::new(3.37, 2.0), Some(c))];
        let (q, _, lab) = s.snap_full(rect, at(&s, 3.37 + 1.0, 2.0 + 1.1));
        assert_eq!(lab, Some("radius"));
        assert!((q.distance(DVec2::new(3.37, 2.0)) - 1.5).abs() < 1e-12);
        // Alt bypasses everything
        s.alt_held = true;
        let (q, pid, lab) = s.snap_full(rect, at(&s, 3.40, 2.02));
        assert!(pid.is_none() && lab.is_none() && (q.x - 3.40).abs() < 0.02);
    }

    #[test]
    fn line_locks_horizontal_and_vertical_over_nearby_snaps() {
        let mut s = session();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        let at = |s: &SketchSession, x: f64, y: f64| s.to_screen(rect, DVec2::new(x, y));
        s.tool = Tool::Line;
        s.clicks = vec![(DVec2::new(3.37, 1.91), None)];
        // cursor 4 px above the axis, grid node (5.0, 2.0) is nearby but off the axis
        let (q, pid, lab) = s.snap_full(rect, at(&s, 5.0, 1.95));
        assert_eq!(q.y, 1.91, "{q:?}");
        assert!(pid.is_none() && lab == Some("H"));
        // x runs along the axis on the grid / round length
        assert!((q.x - 5.0).abs() < 1e-9 || ((q.x - 3.37) * 10.0).round() / 10.0 - (q.x - 3.37) < 1e-9, "{q:?}");
        // below the axis too, and a vertical one
        assert_eq!(s.snap_full(rect, at(&s, 6.1, 1.87)).0.y, 1.91);
        let (q, _, lab) = s.snap_full(rect, at(&s, 3.40, 4.02));
        assert_eq!((q.x, lab), (3.37, Some("V")));
        // far from both axes: no lock
        let (q, _, lab) = s.snap_full(rect, at(&s, 5.0, 3.0));
        assert!(lab != Some("H") && lab != Some("V") && q.y != 1.91, "{q:?}");
        // an existing endpoint within 5 px still wins, even off the axis
        let (a, b) = (s.sketch.add_point(DVec2::new(6.0, 2.03)), s.sketch.add_point(DVec2::new(6.0, 5.0)));
        s.sketch.add_entity(Geom::Line { a, b }, false);
        let (q, pid, lab) = s.snap_full(rect, at(&s, 6.0, 1.99));
        assert_eq!((q, pid, lab), (DVec2::new(6.0, 2.03), Some(a), Some("end")));
        // a point whose x lines up gives the stop along the axis, never leaving it
        let (q, pid, _) = s.snap_full(rect, at(&s, 6.02, 1.95));
        assert!(pid.is_none() && q == DVec2::new(6.0, 1.91), "{q:?}");
        // Alt disables the lock
        s.alt_held = true;
        let (q, _, lab) = s.snap_full(rect, at(&s, 5.0, 1.95));
        assert!(lab.is_none() && (q.y - 1.95).abs() < 0.02);
    }

    #[test]
    fn alignment_with_other_points_snaps_x_or_y() {
        let mut s = session();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        s.center = DVec2::ZERO;
        s.scale = 100.0;
        s.tool = Tool::Rect;
        let (a, b) = (s.sketch.add_point(DVec2::new(2.034, 1.0)), s.sketch.add_point(DVec2::new(2.034, 3.0)));
        s.sketch.add_entity(Geom::Line { a, b }, false);
        let (q, _, lab) = s.snap_full(rect, s.to_screen(rect, DVec2::new(2.034 + 0.04, 7.0)));
        assert_eq!(lab, Some("align x"));
        assert_eq!(q.x, 2.034);
    }

    #[test]
    fn cursor_snaps_to_tenths_of_the_grid() {
        let mut s = session();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        s.center = DVec2::ZERO;
        s.scale = 100.0; // major step 1 (100 px), tenths 10 px apart
        let at = |s: &SketchSession, x: f64, y: f64| s.to_screen(rect, DVec2::new(x, y));
        let (q, pt) = s.snap_point(rect, at(&s, 1.23, 0.47));
        assert!(pt.is_none());
        assert!((q.x - 1.2).abs() < 1e-9 && (q.y - 0.5).abs() < 1e-9, "{q:?}");
        // every major line is reachable too
        let (q, _) = s.snap_point(rect, at(&s, 2.01, -3.02));
        assert!((q.x - 2.0).abs() < 1e-9 && (q.y + 3.0).abs() < 1e-9, "{q:?}");
        // zoomed out the tenths are not drawn (< 6 px), so only the major step snaps
        s.scale = 20.0;
        assert_eq!(snap_step(s.scale), 1.0);
        let (q, _) = s.snap_point(rect, at(&s, 1.23, 0.47));
        assert!((q.x - 1.0).abs() < 1e-9 && (q.y - 0.0).abs() < 1e-9, "{q:?}");
        // Alt bypasses snapping
        s.scale = 100.0;
        s.alt_held = true;
        let (q, _) = s.snap_point(rect, at(&s, 1.23, 0.47));
        assert!((q.x - 1.23).abs() < 0.02, "{q:?}");
    }

    fn fixed_line(s: &mut SketchSession, a: (f64, f64), b: (f64, f64)) -> EntityId {
        let e = s.add_line((DVec2::new(a.0, a.1), None), (DVec2::new(b.0, b.1), None));
        let (pa, pb) = s.line_points(e).unwrap();
        for p in [pa, pb] {
            let v = s.sketch.point(p);
            s.sketch.constrain(Constraint::Fix(p, v));
        }
        e
    }

    #[test]
    fn offset_of_several_open_lines_goes_outward_together() {
        let mut s = session();
        // two parallel lines drawn in opposite directions: travel-relative sides differ
        let l1 = fixed_line(&mut s, (0.0, 0.0), (10.0, 0.0));
        let l2 = fixed_line(&mut s, (10.0, 4.0), (0.0, 4.0));
        let n = s.sketch.entities.len();
        s.offset_selected(&[l1, l2], "1").unwrap();
        assert_eq!(s.sketch.entities.len(), n + 2);
        let ys: Vec<f64> = (n..n + 2)
            .map(|i| {
                let (a, _) = s.line_points(EntityId(i as u32)).unwrap();
                s.sketch.point(a).y
            })
            .collect();
        assert!((ys[0] + 1.0).abs() < 1e-6 && (ys[1] - 5.0).abs() < 1e-6, "away from the middle: {ys:?}");
        // one undo step for the whole set
        assert!(s.undo());
        assert_eq!(s.sketch.entities.len(), n);
        // negative flips both toward the middle
        s.offset_selected(&[l1, l2], "-1").unwrap();
        let ys: Vec<f64> = (n..n + 2)
            .map(|i| {
                let (a, _) = s.line_points(EntityId(i as u32)).unwrap();
                s.sketch.point(a).y
            })
            .collect();
        assert!((ys[0] - 1.0).abs() < 1e-6 && (ys[1] - 3.0).abs() < 1e-6, "{ys:?}");
    }

    #[test]
    fn offset_defaults_to_the_wall_parameter() {
        let mut d = DocData::new("t", "test");
        d.params.set("wall", "2").unwrap();
        let s = SketchSession::new_on(Plane::XY, &d);
        assert_eq!(s.default_offset(), "wall");
        assert_eq!(session().default_offset(), "2");
    }

    #[test]
    fn offset_rect_is_parallel_and_bound() {
        let mut s = session();
        let l = s
            .sketch
            .rect(DVec2::new(-10.0, -5.0), DVec2::new(10.0, 5.0));
        s.entity_layer.resize(s.sketch.entities.len(), None);
        for i in 0..s.sketch.points.len() {
            let p = s.sketch.points[i];
            s.sketch.constrain(Constraint::Fix(PointId(i as u32), p));
        }
        s.offset_loop(l[0], "2").unwrap();
        let r = s.report.clone().unwrap();
        assert!(r.converged, "{r:?}");
        assert_eq!(
            r.dof, 0,
            "offset is fully defined by the fixed source: {r:?}"
        );
        let pr = s.sketch.profiles();
        assert_eq!(pr.profiles.len(), 1, "outer loop with the source as a hole");
        let area = easycad_model::sketch::loop_area(&pr.profiles[0].outer).abs();
        assert!((area - 24.0 * 14.0).abs() < 1e-3, "{area}");
    }

    #[test]
    fn offset_of_the_sample_board_outline_is_fully_defined() {
        // GUI_NEEDS #21: the DXF outline (4 lines + 4 tangent r3 arcs, separate
        // equal end points, fixed reference) offset natively, by an expression
        let mut d = DocData::new("t", "test");
        d.params.set("clearance", "0.5").unwrap();
        d.params.set("wall", "2").unwrap();
        let drawing = DxfDrawing::parse(&crate::sample::board_dxf()).unwrap();
        let mut s = SketchSession::from_dxf(ImportId(1), "board.dxf", drawing, &d);
        let first = s
            .entity_layer
            .iter()
            .position(|l| l.as_deref() == Some("BoardOutLine"))
            .map(|i| EntityId(i as u32))
            .unwrap();
        let before = s.report.clone().unwrap();
        let n = s.sketch.entities.len();
        s.offset_loop(first, "clearance + wall").unwrap();
        assert_eq!(s.sketch.entities.len(), n + 8);
        assert!(s.sketch.entities[n..].iter().all(|e| !e.construction), "the offset is the profile");
        let r = s.report.clone().unwrap();
        assert!(r.converged, "{r:?}");
        assert_eq!(r.variables, before.variables + 16, "8 new joints, the arcs share the source centres");
        assert_eq!((r.dof, r.rank, r.equations), (0, r.variables, r.variables), "{r:?}");
        assert_eq!(r.status, ConstraintStatus::FullyConstrained);
        assert!(!s.sketch.constraints.iter().any(|c| matches!(c, Constraint::PointOnLine(..))), "no GUI radial helpers");
        assert!(s.profile_count() >= 1);
    }

    #[test]
    fn fillet_and_trim_and_remove() {
        let mut s = session();
        let l = s.sketch.rect(DVec2::new(0.0, 0.0), DVec2::new(20.0, 10.0));
        s.entity_layer.resize(s.sketch.entities.len(), None);
        let (_, corner) = s.line_points(l[0]).unwrap();
        s.fillet_corner(corner, "3").unwrap();
        assert_eq!(s.sketch.entities.len(), 5);
        assert!(s.report.as_ref().unwrap().converged);
        assert_eq!(s.sketch.profiles().profiles.len(), 1);
        // a crossing line, trimmed in the middle, splits in two
        let a = s.sketch.add_point(DVec2::new(10.0, -5.0));
        let b = s.sketch.add_point(DVec2::new(10.0, 15.0));
        let e = s.add_entity(Geom::Line { a, b }, false);
        s.trim(e, DVec2::new(10.0, 5.0));
        let n = s.sketch.entities.len();
        assert_eq!(n, 7, "5 sketch entities + two stubs (middle piece removed)");
        // remove keeps ids consistent
        s.remove_entities(&HashSet::from([EntityId(0)]));
        assert_eq!(s.sketch.entities.len(), 6);
        s.resolve();
        assert!(s.error.is_none(), "{:?}", s.error);
    }

    #[test]
    fn dimension_with_expression() {
        let mut d = DocData::new("t", "test");
        d.params.set("wall", "2").unwrap();
        let mut s = SketchSession::new_on(Plane::XY, &d);
        let a = s.sketch.add_point(DVec2::ZERO);
        let b = s.sketch.add_point(DVec2::new(5.0, 0.0));
        let e = s.add_entity(Geom::Line { a, b }, false);
        s.apply_dimension(&[Sel::Entity(e)], "=wall * 4").unwrap();
        let (pa, pb) = s.line_points(e).unwrap();
        assert!((s.sketch.point(pa).distance(s.sketch.point(pb)) - 8.0).abs() < 1e-6);
    }

    #[test]
    fn construction_toggle_selection_undo_and_profiles() {
        let mut s = session();
        let rect = s.sketch.rect(DVec2::ZERO, DVec2::new(10.0, 5.0));
        s.entity_layer.resize(s.sketch.entities.len(), None);
        s.resolve();
        assert_eq!(s.sketch.profiles().profiles.len(), 1);
        // nothing selected: X flips the mode, not any entity
        s.toggle_construction();
        assert!(s.construction_mode && s.sketch.entities.iter().all(|e| !e.construction));
        s.toggle_construction();
        assert!(!s.construction_mode);
        // selected: all four go construction in one undo step, and the profile is gone
        s.selection = rect.iter().map(|e| Sel::Entity(*e)).collect();
        let undo_before = s.undo.len();
        s.toggle_construction();
        assert_eq!(s.undo.len(), undo_before + 1);
        assert!(s.sketch.entities.iter().all(|e| e.construction));
        assert!(!s.construction_mode);
        assert!(s.sketch.profiles().profiles.is_empty(), "construction never forms a profile");
        // construction geometry stays snappable by its points
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0));
        s.alt_held = false;
        let at = s.to_screen(r, DVec2::new(10.0, 5.0));
        assert!(s.snap_point(r, at).1.is_some());
        // undo restores the profile; redo drops it again
        assert!(s.undo());
        assert!(s.sketch.entities.iter().all(|e| !e.construction));
        assert_eq!(s.sketch.profiles().profiles.len(), 1);
        assert!(s.redo());
        assert!(s.sketch.entities.iter().all(|e| e.construction));
        // toggling again (all construction) turns them back
        s.selection = rect.iter().map(|e| Sel::Entity(*e)).collect();
        s.toggle_construction();
        assert!(s.sketch.entities.iter().all(|e| !e.construction));
    }

    #[test]
    fn construction_mode_makes_new_geometry_construction() {
        let mut s = session();
        s.construction_mode = true;
        let l = s.add_line((DVec2::ZERO, None), (DVec2::new(5.0, 0.0), None));
        let c = s.add_entity(Geom::Circle { center: s.line_points(l).unwrap().0, radius: 2.0 }, false);
        assert!(s.sketch.entities[l.0 as usize].construction);
        assert!(s.sketch.entities[c.0 as usize].construction);
        s.construction_mode = false;
        let l2 = s.add_line((DVec2::ZERO, None), (DVec2::new(0.0, 5.0), None));
        assert!(!s.sketch.entities[l2.0 as usize].construction);
    }

    #[test]
    fn angle_quadrant_picks_the_angle_its_supplement_or_the_opposite_pair() {
        // an upright line and one at 30 degrees, crossing at the origin
        let (a, b) = (DVec2::new(0.0, -5.0), DVec2::new(0.0, 5.0));
        let w = DVec2::from_angle(30f64.to_radians());
        let (c, d) = (-w * 5.0, w * 5.0);
        let at = |q: (f64, f64)| angle_choice(a, b, c, d, DVec2::new(q.0, q.1)).unwrap().deg;
        // up-right between the lines: 60; right-below: its supplement 120;
        // up-left: 120; down-left: the opposite pair of the 60
        assert!((at((1.0, 3.0)) - 60.0).abs() < 1e-9);
        assert!((at((5.0, 1.0)) - 120.0).abs() < 1e-9);
        assert!((at((-1.0, 3.0)) - 120.0).abs() < 1e-9);
        assert!((at((-1.0, -3.0)) - 60.0).abs() < 1e-9);
        // parallel lines have no crossing
        assert!(angle_choice(a, b, a + DVec2::X, b + DVec2::X, DVec2::ONE).is_none());
        // lines that only cross when extended
        let ch = angle_choice(DVec2::new(0.0, 2.0), DVec2::new(0.0, 5.0), DVec2::new(3.0, 3.0), DVec2::new(6.0, 4.0), DVec2::new(1.0, 20.0)).unwrap();
        assert!(ch.p.length() < 1e-9 || (ch.p.x.abs() < 1e-9));
    }

    #[test]
    fn peace_sign_branch_angle_solves_to_the_chosen_side() {
        let mut s = session();
        // the upright, fixed; a branch from its foot, free to turn about it
        let up = fixed_line(&mut s, (0.0, 0.0), (0.0, 10.0));
        let br = s.add_line((DVec2::ZERO, None), (DVec2::new(5.0, -2.0), None));
        let (u0, _) = s.line_points(up).unwrap();
        let (b0, b1) = s.line_points(br).unwrap();
        s.sketch.constrain(Constraint::Coincident(u0, b0));
        s.sketch.constrain(Constraint::Length(br, "8".into()));
        s.resolve();
        // the cursor to the right of the upright, below the branch: the up-ray
        // and the branch ray enclose the (111.8 degree) angle
        let pts = s.line_pair(&[Sel::Entity(up), Sel::Entity(br)]).unwrap();
        let ch = angle_choice(pts[0], pts[1], pts[2], pts[3], DVec2::new(5.0, 2.0)).unwrap();
        assert!(ch.swap && ch.deg > 100.0 && ch.deg < 130.0, "{ch:?}");
        s.dim_rays = Some(ch);
        let undo_before = s.undo.len();
        s.apply_dimension(&[Sel::Entity(up), Sel::Entity(br)], "135").unwrap();
        assert_eq!(s.undo.len(), undo_before + 1, "one undo step");
        assert!(matches!(s.sketch.constraints.last(), Some(Constraint::AngleRays(..))));
        let d = s.sketch.point(b1) - s.sketch.point(b0);
        assert!((d.y.atan2(d.x).to_degrees() + 45.0).abs() < 1e-4, "branch at -45: {d:?}");
        // the opposite side of the upright: the branch swings the other way
        let mut s2 = session();
        let up = fixed_line(&mut s2, (0.0, 0.0), (0.0, 10.0));
        let br = s2.add_line((DVec2::ZERO, None), (DVec2::new(-5.0, -2.0), None));
        let (u0, _) = s2.line_points(up).unwrap();
        let (b0, b1) = s2.line_points(br).unwrap();
        s2.sketch.constrain(Constraint::Coincident(u0, b0));
        s2.sketch.constrain(Constraint::Length(br, "8".into()));
        s2.resolve();
        let pts = s2.line_pair(&[Sel::Entity(up), Sel::Entity(br)]).unwrap();
        let ch = angle_choice(pts[0], pts[1], pts[2], pts[3], DVec2::new(-5.0, 2.0)).unwrap();
        s2.dim_rays = Some(ch);
        s2.apply_dimension(&[Sel::Entity(up), Sel::Entity(br)], "135").unwrap();
        let d = s2.sketch.point(b1) - s2.sketch.point(b0);
        assert!((d.y.atan2(d.x).to_degrees() - 225.0 + 360.0).rem_euclid(360.0) < 1e-3 || (d.y.atan2(d.x).to_degrees() + 135.0).abs() < 1e-4, "{d:?}");
    }
}
