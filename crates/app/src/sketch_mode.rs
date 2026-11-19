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
use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
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
            Tool::Project => "Project · pick body edges (grey) to bring them in",
            Tool::Trim => "Trim · hover a segment to preview, click to cut",
            Tool::Dimension => "Dimension · pick one or two entities, then type",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Point(PointId),
    Entity(EntityId),
}

/// Builds the dimension constraint from the typed expression.
pub type DimMaker = Box<dyn Fn(Expr) -> Constraint>;

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
    pub dim_target: Option<Vec<Sel>>,
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
    pub drag: Option<PointId>,
    pub cursor: DVec2,
    pub projected: Vec<Vec<DVec2>>,
    /// The model edge each projected polyline comes from (exact projection), with
    /// its 3D samples.
    pub projected_src: Vec<Option<(easycad_model::occt::EdgeInfo, Vec<glam::DVec3>)>>,
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
            input: None,
            input2: None,
            input_second: false,
            dim_target: None,
            dxf: None,
            entity_layer: vec![None; n],
            report: None,
            error: None,
            show_constraints: true,
            snap: true,
            input_fresh: false,
            input_live: false,
            alt_held: false,
            slice: false,
            undo: Vec::new(),
            redo: Vec::new(),
            values,
            names,
            drag: None,
            cursor: DVec2::ZERO,
            projected: Vec::new(),
            projected_src: Vec::new(),
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
    fn pop_undo(&mut self) {
        self.undo.pop();
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
        self.sketch.add_entity(g, construction)
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

    /// The dimension a pick set would create, with its current measured value.
    pub fn dimension_for(&self, sel: &[Sel]) -> Option<(f64, DimMaker)> {
        let s = &self.sketch;
        match sel {
            [Sel::Entity(e)] => {
                let e = *e;
                match s.entities[e.0 as usize].geom {
                    Geom::Line { a, b } => Some((
                        s.point(a).distance(s.point(b)),
                        Box::new(move |x| Constraint::Length(e, x)),
                    )),
                    Geom::Circle { radius, .. } => {
                        Some((radius * 2.0, Box::new(move |x| Constraint::Diameter(e, x))))
                    }
                    Geom::Arc { center, start, .. } => Some((
                        s.point(center).distance(s.point(start)),
                        Box::new(move |x| Constraint::Radius(e, x)),
                    )),
                }
            }
            [Sel::Point(a), Sel::Point(b)] => {
                let (a, b) = (*a, *b);
                Some((
                    s.point(a).distance(s.point(b)),
                    Box::new(move |x| Constraint::Distance(a, b, x)),
                ))
            }
            [Sel::Point(p), Sel::Entity(e)] | [Sel::Entity(e), Sel::Point(p)] => {
                let (p, e) = (*p, *e);
                let Geom::Line { a, b } = s.entities[e.0 as usize].geom else {
                    return None;
                };
                let (pa, pb, q) = (s.point(a), s.point(b), s.point(p));
                let d = ((pb - pa).perp_dot(q - pa) / (pb - pa).length().max(1e-12)).abs();
                Some((d, Box::new(move |x| Constraint::PointLineDistance(p, e, x))))
            }
            [Sel::Entity(e1), Sel::Entity(e2)] => {
                let (e1, e2) = (*e1, *e2);
                let (Geom::Line { a, b }, Geom::Line { a: c, b: d }) = (
                    s.entities[e1.0 as usize].geom,
                    s.entities[e2.0 as usize].geom,
                ) else {
                    return None;
                };
                let (u, w) = (s.point(b) - s.point(a), s.point(d) - s.point(c));
                let ang = u.perp_dot(w).atan2(u.dot(w)).to_degrees();
                Some((ang, Box::new(move |x| Constraint::Angle(e1, e2, x))))
            }
            _ => None,
        }
    }

    pub fn apply_dimension(&mut self, sel: &[Sel], text: &str) -> Result<(), String> {
        let t = text.trim().trim_start_matches('=').trim();
        crate::widgets::eval(t, &self.values)?;
        let (_, mk) = self
            .dimension_for(sel)
            .ok_or("can't dimension that selection")?;
        self.push_undo();
        self.sketch.constrain(mk(Expr::new(t)));
        self.resolve();
        if self.report.as_ref().is_some_and(|r| !r.converged) {
            self.undo();
            self.redo.pop(); // a refused edit is not something to redo
            return Err("that dimension conflicts with existing constraints".into());
        }
        Ok(())
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
        if self.snap && !self.alt_held {
            if let Some(Sel::Point(pid)) = self.pick(rect, p) {
                return (self.sketch.point(pid), Some(pid));
            }
            let q = self.to_sketch(rect, p);
            let g = snap_step(self.scale);
            return (
                DVec2::new((q.x / g).round() * g, (q.y / g).round() * g),
                None,
            );
        }
        (self.to_sketch(rect, p), None)
    }
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
                s.projected_src = vec![None; proj.len()];
                s.projected = proj;
                s.slice_caps = caps;
                s.slice_outline = outline;
            } else {
                let (proj, src) = project_edges(m, &s.sketch.plane);
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

type Projected = (Vec<Vec<DVec2>>, Vec<Option<(easycad_model::occt::EdgeInfo, Vec<glam::DVec3>)>>);

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
                src.push(b.edges.get(e.edge as usize).cloned().map(|info| (info, p3)));
            }
        }
    }
    (v, src)
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
        s.cursor = s.snap_point(rect, hp).0;
        s.hover = s.pick(rect, hp);
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
                }
            }
        });
        // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y are dispatched once, by `EasyCadApp::shortcuts`
        // (sketch history first, then the document's).
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if s.input.is_some() {
                s.cancel_input();
            } else if !s.clicks.is_empty() {
                s.clicks.clear();
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
    if let Some(hp) = resp.interact_pointer_pos() {
        if resp.drag_started_by(egui::PointerButton::Primary) && s.tool == Tool::Select {
            if let Some(Sel::Point(pid)) = s.pick(rect, hp) {
                if s.entity_layer.iter().all(|l| l.is_none()) || !is_fixed(&s.sketch, pid) {
                    s.push_undo();
                    s.drag = Some(pid);
                }
            }
        }
        if let Some(pid) = s.drag {
            if resp.dragged_by(egui::PointerButton::Primary) {
                let q = s.to_sketch(rect, hp);
                let mut sk = s.sketch.clone();
                sk.points[pid.0 as usize] = q;
                sk.constrain(Constraint::Fix(pid, q));
                if let Ok(r) = sk.solve(&s.values) {
                    if r.converged {
                        sk.constraints.pop();
                        s.sketch = sk;
                    }
                }
            }
        }
    }
    if resp.drag_stopped() && s.drag.is_some() {
        s.drag = None;
        s.resolve();
    }
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(hp) = resp.interact_pointer_pos() {
            click(
                s,
                rect,
                hp,
                ui.input(|i| i.modifiers.shift || i.modifiers.command),
            );
        }
    }
    if resp.clicked_by(egui::PointerButton::Secondary) {
        s.clicks.clear();
        s.cancel_input();
    }

    draw(s, ui, rect, &p);
    // tool hint
    let hint = s.tool.hint();
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
            let tol = 6.0 / s.scale as f64;
            let hit = s
                .projected
                .iter()
                .position(|pl| pl.windows(2).any(|w| seg_dist(q, w[0], w[1]) < tol));
            // exact: lines stay lines, circles facing the plane stay circles (GUI_NEEDS #14)
            if let Some((info, samples)) = hit.and_then(|i| s.projected_src.get(i).cloned().flatten()) {
                s.push_undo();
                if s.sketch.project_edge(&info, Some(&samples), true).is_empty() {
                    s.pop_undo();
                } else {
                    s.entity_layer.resize(s.sketch.entities.len(), None);
                    s.resolve();
                }
            } else if let Some(pl) = hit.and_then(|i| s.projected.get(i).cloned()) {
                s.push_undo();
                let first = s.sketch.points.len() as u32;
                let mut prev = None;
                for pt in &pl {
                    let id = s.sketch.add_point(*pt);
                    if let Some(a) = prev {
                        s.add_entity(Geom::Line { a, b: id }, true);
                    }
                    prev = Some(id);
                }
                for i in first..s.sketch.points.len() as u32 {
                    let v = s.sketch.points[i as usize];
                    s.sketch.constrain(Constraint::Fix(PointId(i), v));
                }
                s.resolve();
            }
        }
        Tool::Trim => {
            if let Some(Sel::Entity(e)) = s.pick(rect, hp) {
                let q = s.to_sketch(rect, hp);
                s.trim(e, q);
            }
        }
        Tool::Dimension => {
            if let Some(x) = s.pick(rect, hp) {
                let mut t = s.dim_target.take().unwrap_or_default();
                t.push(x);
                let complete = match t.as_slice() {
                    [Sel::Entity(e)] => {
                        !matches!(s.sketch.entities[e.0 as usize].geom, Geom::Line { .. })
                            || !additive
                    }
                    _ => t.len() >= 2,
                };
                if complete && s.dimension_for(&t).is_some() {
                    let (v, _) = s.dimension_for(&t).unwrap();
                    s.open_input(fmt(v.abs()), false);
                    s.dim_target = Some(t);
                } else {
                    s.dim_target = Some(t);
                }
            }
        }
    }
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
    let anchor = s.to_screen(rect, s.cursor) + vec2(18.0, -34.0);
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
    egui::Area::new(egui::Id::NULL.with("sk_input"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::CARD)
                .stroke(Stroke::new(1.0, theme::ACCENT))
                .corner_radius(4.0)
                .inner_margin(Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).size(11.0).color(theme::TEXT_MUTED));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .id(te_id)
                                .font(theme::mono(13.0))
                                .desired_width(110.0)
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
        });
    if cancel {
        s.cancel_input();
        return;
    }
    if typed {
        s.input_live = false;
    }
    s.input_fresh = false;
    if !done {
        s.input = Some(text);
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
    for pl in &s.projected {
        let pts: Vec<Pos2> = pl.iter().map(|q| to(*q)).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.0, proj_col)));
    }
    for pl in &s.slice_outline {
        let pts: Vec<Pos2> = pl.iter().map(|q| to(*q)).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.4, theme::ORANGE_SOFT)));
    }
    // profile fill
    for pr in s.sketch.profiles().profiles {
        let poly = easycad_model::sketch::polygonize(&pr.outer);
        let pts: Vec<Pos2> = poly.iter().map(|q| to(*q)).collect();
        if pts.len() > 2 {
            let mut mesh = egui::Mesh::default();
            // fan from centroid (profiles are usually convex enough for a hint fill)
            let c = pts.iter().fold(vec2(0.0, 0.0), |a, b| a + b.to_vec2()) / pts.len() as f32;
            let col = Color32::from_rgba_unmultiplied(76, 155, 255, 18);
            mesh.colored_vertex(c.to_pos2(), col);
            for q in &pts {
                mesh.colored_vertex(*q, col);
            }
            for i in 0..pts.len() as u32 {
                mesh.add_triangle(0, 1 + i, 1 + (i + 1) % pts.len() as u32);
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
        for c in &s.sketch.constraints {
            let (expr, anchor, prefix) = match c {
                Constraint::Length(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), ""),
                Constraint::Diameter(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "⌀"),
                Constraint::Radius(e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "R"),
                Constraint::PointLineDistance(_, e, x) => {
                    (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "offset ")
                }
                Constraint::Distance(a, b, x)
                | Constraint::DistanceX(a, b, x)
                | Constraint::DistanceY(a, b, x) => (
                    x,
                    Some((
                        (s.sketch.point(*a) + s.sketch.point(*b)) / 2.0,
                        EntityId(u32::MAX),
                    )),
                    "",
                ),
                Constraint::Angle(_, e, x) => (x, entity_mid(&s.sketch, *e).map(|m| (m, *e)), "∠"),
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
            let at = to(m) + vec2(0.0, 18.0);
            let font = theme::mono(12.0);
            let w = p
                .layout_no_wrap(txt.clone(), font.clone(), theme::TEXT)
                .size()
                .x
                + if is_expr { 30.0 } else { 14.0 };
            let r = Rect::from_center_size(at, vec2(w, 22.0));
            p.rect(
                r,
                4.0,
                theme::CARD,
                Stroke::new(
                    1.0,
                    if is_expr {
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
        let (q, snapped) = s.snap_point(rect, cur);
        let pv = Stroke::new(1.2, theme::ACCENT);
        if snapped.is_some() {
            p.circle_stroke(to(q), 6.0, Stroke::new(1.2, theme::ORANGE));
        } else if s.snap && !s.alt_held && s.tool != Tool::Select {
            // snapped to the grid: a small cross on the node
            let c = to(q);
            let m = Stroke::new(1.2, theme::ACCENT);
            p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], m);
            p.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], m);
        }
        match (s.tool, s.clicks.as_slice()) {
            (Tool::Line, [a]) => {
                p.add(egui::Shape::dashed_line(&[to(a.0), to(q)], pv, 6.0, 3.0));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SketchSession {
        let d = DocData::new("t", "test");
        SketchSession::new_on(Plane::XY, &d)
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
}
