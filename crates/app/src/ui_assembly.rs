//! Assemblies: the component tree in the browser (nested occurrences, instance
//! counts, visibility, grounding, link status, locked references, drag to
//! reparent), activation (edit one component in place with the rest dimmed, a
//! breadcrumb, Esc goes up), occurrence commands, isolate, the Mates folder, and
//! the display-side transforms (live gizmo drags, interference tints).

use std::collections::{HashMap, HashSet};

use easycad_model::assembly::{
    Component, ComponentId, ComponentSource, LinkStatus, MateStatus, OccPath, Occurrence,
    OccurrenceId, Transform,
};
use easycad_model::document::{DocData, DocError};
use egui::{pos2, vec2, Color32, Rect, RichText, Sense, Stroke, Ui};
use glam::DAffine3;

use crate::app::EasyCadApp;
use crate::pickstack::{PickKey, PickKind};
use crate::theme;
use crate::widgets::{self, Icon};
use crate::worker::{CtxBody, Output};

/// Assembly UI state (lives in the app).
#[derive(Default)]
pub struct AsmUi {
    pub collapsed: HashSet<OccPath>,
    /// The selected occurrence (browser row or a picked body), absolute path.
    pub sel: Option<OccPath>,
    /// Live gizmo drag: (occurrence path, delta in the view frame).
    pub drag: Option<(OccPath, DAffine3)>,
    /// A committed move shown until a model of at least that revision arrives.
    pub hold: Option<(OccPath, DAffine3, u64)>,
    /// Move gizmo shown on the selected occurrence.
    pub gizmo: bool,
    pub gdrag: Option<crate::ui_gizmo::GizmoDrag>,
    /// Gizmo centre and arrow tips on screen (E2E state dump).
    pub gizmo_screen: Option<(egui::Pos2, [egui::Pos2; 3])>,
    pub mates_collapsed: bool,
    /// Occurrence row being dragged onto another (reparent).
    pub tree_drag: Option<OccPath>,
    /// Link status per linked component: (status, revision checked, time checked).
    pub links: HashMap<ComponentId, (LinkStatus, u64, f64)>,
    /// Interference results and the highlighted pair.
    pub interference: Option<crate::ui_interference::InterferenceUi>,
    pub mate: Option<crate::ui_mates::MateUi>,
    pub bom: Option<crate::ui_bom::BomUi>,
    /// The explode slider is open in the view toolbar.
    pub explode_open: bool,
}

/// Is the document an assembly (anything beyond the root's own content)?
pub fn is_assembly(d: &DocData) -> bool {
    !d.occurrences.is_empty() || !d.components.is_empty()
}

/// The component being edited.
pub fn active_component(app: &EasyCadApp) -> ComponentId {
    app.doc.data.component_at(&app.active).unwrap_or(ComponentId::ROOT)
}

/// Component frame -> view frame of a context body, live drags included.
pub fn display_xf(app: &EasyCadApp, c: &CtxBody) -> DAffine3 {
    let mut x = c.xf;
    let held = app.asm.hold.as_ref().map(|h| (&h.0, &h.1));
    for (p, d) in [app.asm.drag.as_ref().map(|d| (&d.0, &d.1)), held].into_iter().flatten() {
        if c.path.starts_with(p) {
            x = *d * x;
        }
    }
    x
}

/// Interference highlight: the pair shown in the report is drawn red.
pub fn body_tint(app: &EasyCadApp, c: &CtxBody) -> Option<[f32; 4]> {
    let i = app.asm.interference.as_ref()?;
    // the pair goes see-through so the red overlap solid inside shows (GUI_NEEDS
    // #19); without an overlap solid the pair itself is tinted red
    let tint = if i.overlaps.iter().any(|m| m.is_some()) { [0.62, 0.66, 0.72, 0.28] } else { [0.86, 0.22, 0.18, 0.55] };
    i.highlighted(c).then_some(tint)
}

pub fn occ_name(d: &DocData, path: &[OccurrenceId]) -> String {
    path.last()
        .and_then(|o| d.occurrence(*o))
        .map(|o| o.name.clone())
        .unwrap_or_else(|| d.name.clone())
}

/// Number of placed occurrences of a component in the whole tree.
pub fn instance_count(d: &DocData, c: ComponentId) -> usize {
    d.all_occurrences()
        .iter()
        .filter(|(_, o, _)| o.component == c)
        .count()
}

/// Is the occurrence on a mate (as a moving or fixed side)?
pub fn is_mated(d: &DocData, path: &[OccurrenceId]) -> bool {
    d.mates.iter().filter(|m| !m.suppressed).any(|m| {
        m.a.occurrence.starts_with(path)
            || m.b.occurrence.starts_with(path)
            || path.starts_with(&m.a.occurrence)
            || path.starts_with(&m.b.occurrence)
    })
}

// ---------------------------------------------------------------- commands

impl EasyCadApp {
    /// Edit the component at `path` in place (`[]` = the whole assembly). The camera
    /// keeps its world pose: the view frame changes, not what the user sees.
    pub fn activate(&mut self, path: OccPath) {
        if path == self.active {
            return;
        }
        let d = &self.doc.data;
        let (Ok(old), Ok(new)) = (d.world_transform(&self.active), d.world_transform(&path)) else {
            return;
        };
        if let Ok(c) = d.component_at(&path) {
            if d.component(c).is_some_and(|c| c.locked) {
                self.warn(format!(
                    "{} is locked (a reference or linked file); break the link to edit it",
                    occ_name(d, &path)
                ));
                return;
            }
        }
        let m = new.inverse().mul(&old);
        let (r, t) = (m.rotation.as_quat(), m.translation.as_vec3());
        self.camera.rot = (r * self.camera.rot).normalize();
        self.camera.target = r * self.camera.target + t;
        self.camera.cancel_anim();
        if self.dialog.is_some() {
            self.close_dialog(false);
        }
        self.active = path;
        self.selection.clear();
        self.stack.clear();
        self.asm.sel = None;
        self.asm.gizmo = false;
        self.asm.drag = None;
        self.asm.hold = None;
        self.params = Default::default();
        self.cache.usage.clear();
        self.sketch_body = None;
        self.request_model();
    }

    /// Esc / breadcrumb: up one level.
    pub fn activate_parent(&mut self) {
        let mut p = self.active.clone();
        p.pop();
        self.activate(p);
    }

    /// Select an occurrence: its bodies get the body highlight.
    pub fn select_occurrence(&mut self, path: Option<OccPath>, additive: bool) {
        let keys: Vec<PickKey> = match (&path, &self.model) {
            (Some(p), Some(m)) => m
                .ctx
                .iter()
                .filter(|c| c.path.starts_with(p))
                .map(|c| PickKey { body: c.key, kind: PickKind::Body })
                .collect(),
            _ => Vec::new(),
        };
        if additive {
            self.selection.extend(keys);
        } else {
            self.selection = keys;
        }
        self.asm.sel = path;
    }

    pub fn occ_edit(&mut self, id: OccurrenceId, label: &str, f: impl FnOnce(&mut Occurrence)) {
        match self.doc.edit_occurrence(id, label, f) {
            Ok(()) => self.request_model(),
            Err(e) => self.warn(e.to_string()),
        }
    }

    pub fn toggle_ground(&mut self, id: OccurrenceId) {
        let Some(o) = self.doc.data.occurrence(id) else { return };
        let (g, name) = (o.grounded, o.name.clone());
        let label = if g { format!("Unground {name}") } else { format!("Ground {name}") };
        self.occ_edit(id, &label, |o| o.grounded = !g);
    }

    pub fn toggle_visible(&mut self, id: OccurrenceId) {
        let Some(o) = self.doc.data.occurrence(id) else { return };
        let (v, name) = (o.visible, o.name.clone());
        let label = if v { format!("Hide {name}") } else { format!("Show {name}") };
        self.occ_edit(id, &label, |o| o.visible = !v);
    }

    /// Hide every placed body outside `path` (display only; "Show all" undoes it).
    pub fn isolate(&mut self, path: &[OccurrenceId]) {
        let Some(m) = self.model.clone() else { return };
        self.hidden.clear();
        for c in &m.ctx {
            if !c.path.starts_with(path) {
                self.hidden.insert(c.key);
            }
        }
        if !path.is_empty() {
            for b in &m.bodies {
                self.hidden.insert(b.body.id);
            }
        }
        self.toast(format!("Isolated {} · right-click ▸ Show all to undo", occ_name(&self.doc.data, path)));
    }

    /// New empty component placed in the active component (one undo step).
    pub fn new_component(&mut self, parent: ComponentId) {
        let d = &self.doc.data;
        let mut n = 1;
        while d.components.iter().any(|c| c.name == format!("Component{n}")) {
            n += 1;
        }
        let name = format!("Component{n}");
        let r = self.doc.edit(&format!("New component {name}"), |d| {
            let id = ComponentId(d.next_id);
            d.next_id += 1;
            d.components.push(Component { id, name: name.clone(), ..Default::default() });
            let occ = OccurrenceId(d.next_id);
            d.next_id += 1;
            let o = Occurrence {
                id: occ,
                component: id,
                name: format!("{name}:1"),
                transform: Transform::IDENTITY,
                visible: true,
                grounded: false,
                explode: glam::DVec3::ZERO,
                in_bom: true,
            };
            children_mut(d, parent).ok_or_else(|| DocError::Other("parent vanished".into()))?.push(o);
            Ok::<_, DocError>(occ)
        });
        match r {
            Ok(_) => {
                self.toast(format!("New component {name} · double-click to edit it"));
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    /// Insert a `.ecad` as a linked component placed in the active component.
    pub fn insert_ecad(&mut self) {
        let req = crate::files::Request::folder("Insert a .ecad document as a linked component");
        self.ask_path(req, |app, dir| {
            let host = app.host_dir();
            let parent = active_component(app);
            match app.doc.link_component(&dir, &host) {
                Ok(c) => {
                    let name = app.doc.data.component_name(c);
                    let n = instance_count(&app.doc.data, c) + 1;
                    match app.doc.add_occurrence(parent, c, &format!("{name}:{n}"), Transform::IDENTITY) {
                        Ok(_) => app.toast(format!("Linked {name} · it updates from {}", dir.display())),
                        Err(e) => app.warn(e.to_string()),
                    }
                    app.fit_pending = true;
                    app.request_model();
                }
                Err(e) => app.warn(format!("Cannot link {}: {e}", dir.display())),
            }
        });
    }

    /// Another occurrence of the same component.
    pub fn place_again(&mut self, path: &[OccurrenceId]) {
        let d = &self.doc.data;
        let Some(o) = path.last().and_then(|o| d.occurrence(*o)).cloned() else { return };
        let parent = parent_component(d, path);
        let n = instance_count(d, o.component) + 1;
        let base = o.name.rsplit_once(':').map(|x| x.0).unwrap_or(&o.name).to_string();
        let t = Transform::translation(glam::DVec3::X * 20.0).mul(&o.transform);
        match self.doc.add_occurrence(parent, o.component, &format!("{base}:{n}"), t) {
            Ok(_) => self.request_model(),
            Err(e) => self.warn(e.to_string()),
        }
    }

    /// Give one occurrence its own copy of the component definition.
    pub fn make_independent(&mut self, id: OccurrenceId) {
        let r = self.doc.edit("Make independent copy", |d| {
            let o = d.occurrence(id).cloned().ok_or_else(|| DocError::Other("occurrence vanished".into()))?;
            let mut c = d
                .component(o.component)
                .cloned()
                .ok_or_else(|| DocError::Other("component vanished".into()))?;
            let nid = ComponentId(d.next_id);
            d.next_id += 1;
            // nested occurrences get fresh ids (they are unique per document)
            for k in &mut c.children {
                k.id = OccurrenceId(d.next_id);
                d.next_id += 1;
            }
            let base = c.name.clone();
            let mut n = 2;
            while d.components.iter().any(|x| x.name == format!("{base} ({n})")) {
                n += 1;
            }
            c.id = nid;
            c.name = format!("{base} ({n})");
            c.link_ids.clear();
            if matches!(c.source, ComponentSource::Linked { .. }) {
                c.source = ComponentSource::Native;
                c.locked = false;
            }
            let name = c.name.clone();
            d.components.push(c);
            let occ = d.occurrence_mut(id).ok_or_else(|| DocError::Other("occurrence vanished".into()))?;
            occ.component = nid;
            occ.name = format!("{name}:1");
            Ok::<_, DocError>(name)
        });
        match r {
            Ok(n) => {
                self.toast(format!("Now its own component: {n}"));
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    pub fn remove_occurrence(&mut self, path: &[OccurrenceId]) {
        let Some(id) = path.last().copied() else { return };
        match self.doc.remove_occurrence(id) {
            Ok(()) => {
                if self.active.starts_with(path) {
                    self.active = path[..path.len() - 1].to_vec();
                }
                self.asm.sel = None;
                self.selection.clear();
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    /// Move an occurrence into another component (keeping where it is in the world).
    pub fn reparent(&mut self, path: &[OccurrenceId], into: &[OccurrenceId]) {
        let d = &self.doc.data;
        if into.starts_with(path) || path.is_empty() {
            return;
        }
        let Some(id) = path.last().copied() else { return };
        let Ok(target) = d.component_at(into) else { return };
        if parent_component(d, path) == target {
            return;
        }
        let Some(occ) = d.occurrence(id).cloned() else { return };
        if d.contains_component(occ.component, target) {
            self.warn(format!("{} cannot go inside itself", occ.name));
            return;
        }
        if d.component(target).is_some_and(|c| c.locked) {
            self.warn("That component is locked");
            return;
        }
        let (Ok(w), Ok(pw)) = (d.world_transform(path), d.world_transform(into)) else { return };
        let local = pw.inverse().mul(&w);
        let into_name = occ_name(d, into);
        let r = self.doc.edit(&format!("Move {} into {into_name}", occ.name), |d| {
            d.occurrences.retain(|o| o.id != id);
            for c in &mut d.components {
                c.children.retain(|o| o.id != id);
            }
            // mates through the old path no longer reach it
            d.mates.retain(|m| !m.a.occurrence.contains(&id) && !m.b.occurrence.contains(&id));
            let mut o = occ.clone();
            o.transform = local;
            children_mut(d, target).ok_or_else(|| DocError::Other("target vanished".into()))?.push(o);
            Ok::<_, DocError>(())
        });
        match r {
            Ok(()) => {
                self.asm.sel = Some(into.iter().copied().chain([id]).collect());
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    /// The folder linked paths are relative to (where the document is saved).
    pub fn host_dir(&self) -> std::path::PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    pub fn link_status(&mut self, c: ComponentId) -> LinkStatus {
        let rev = self.doc.data.revision;
        if let Some((s, r, t)) = self.asm.links.get(&c) {
            if *r == rev && self.now - t < 5.0 {
                return s.clone();
            }
        }
        let s = self.doc.link_status(c, &self.host_dir());
        self.asm.links.insert(c, (s.clone(), rev, self.now));
        s
    }

    pub fn update_link(&mut self, c: ComponentId) {
        let host = self.host_dir();
        match self.doc.update_link(c, &host) {
            Ok(()) => {
                self.asm.links.remove(&c);
                self.toast(format!("Updated {}", self.doc.data.component_name(c)));
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    pub fn break_link(&mut self, c: ComponentId) {
        match self.doc.break_link(c) {
            Ok(()) => {
                self.asm.links.remove(&c);
                self.toast(format!("{} is now an editable component", self.doc.data.component_name(c)));
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }
}

fn children_mut(d: &mut DocData, c: ComponentId) -> Option<&mut Vec<Occurrence>> {
    if c == ComponentId::ROOT {
        Some(&mut d.occurrences)
    } else {
        d.component_mut(c).map(|c| &mut c.children)
    }
}

pub fn parent_component(d: &DocData, path: &[OccurrenceId]) -> ComponentId {
    if path.len() <= 1 {
        ComponentId::ROOT
    } else {
        d.component_at(&path[..path.len() - 1]).unwrap_or(ComponentId::ROOT)
    }
}

// ---------------------------------------------------------------- browser tree

enum TreeAct {
    Select(OccPath, bool),
    Activate(OccPath),
    Toggle(OccPath),
    Visible(OccurrenceId),
    Ground(OccurrenceId),
    Menu(OccPath, MenuAct),
    Drop(OccPath, OccPath),
}

enum MenuAct {
    Activate,
    Isolate,
    Copy,
    PlaceAgain,
    NewInside,
    InsertEcad,
    Update,
    Break,
    Remove,
    Move,
    Explode,
}

/// The occurrence tree (and the Mates folder) of the browser.
pub fn tree(app: &mut EasyCadApp, ui: &mut Ui) {
    let d = app.doc.data.clone();
    let mut acts: Vec<TreeAct> = Vec::new();
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let released = ui.input(|i| i.pointer.any_released());
    let mut drop_target: Option<OccPath> = None;
    // the root row is a drop target too (move to the top level)
    let (r, resp) = tree_row(ui, 14.0, 30.0, false);
    let p = ui.painter().clone();
    let root_active = app.active.is_empty();
    paint_cube(&p, r.left_center() + vec2(6.0, 0.0), theme::ACCENT_LINK);
    p.text(
        r.left_center() + vec2(18.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &d.name,
        theme::prop(13.0),
        if root_active { theme::TEXT_STRONG } else { theme::TEXT_DIM },
    );
    p.text(
        r.right_center() - vec2(12.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!("v{}", d.revision),
        theme::prop(12.0),
        theme::TEXT_LABEL,
    );
    if app.asm.tree_drag.is_some() && pointer.is_some_and(|q| r.contains(q)) {
        drop_target = Some(Vec::new());
        p.rect_stroke(r, 3.0, Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
    }
    if resp.double_clicked() {
        acts.push(TreeAct::Activate(Vec::new()));
    }
    resp.context_menu(|ui| {
        if ui.button("New component").clicked() {
            app.new_component(ComponentId::ROOT);
            ui.close();
        }
        if ui.button("Insert component (.ecad)…").clicked() {
            app.activate(Vec::new());
            app.insert_ecad();
            ui.close();
        }
        if ui.button("Insert from library…").clicked() {
            app.mode = crate::app::Mode::Parts(Box::new(crate::ui_parts::PartsUi::new(&app.doc)));
            ui.close();
        }
        if ui.button("Show all").clicked() {
            app.hidden.clear();
            ui.close();
        }
    });
    let model = app.model.clone();
    rows(app, ui, &d, model.as_deref(), ComponentId::ROOT, &Vec::new(), 0, pointer, &mut acts, &mut drop_target);
    mates_folder(app, ui, &d);
    // drag to reparent
    if let Some(src) = app.asm.tree_drag.clone() {
        if let Some(q) = pointer {
            ui.painter().text(
                q + vec2(14.0, 4.0),
                egui::Align2::LEFT_CENTER,
                format!("Move {}", occ_name(&d, &src)),
                theme::prop(12.0),
                theme::ACCENT_LINK,
            );
        }
        if released {
            app.asm.tree_drag = None;
            if let Some(t) = drop_target {
                acts.push(TreeAct::Drop(src, t));
            }
        }
    }
    for a in acts {
        match a {
            TreeAct::Select(p, add) => app.select_occurrence(Some(p), add),
            TreeAct::Activate(p) => app.activate(p),
            TreeAct::Toggle(p) => {
                if !app.asm.collapsed.remove(&p) {
                    app.asm.collapsed.insert(p);
                }
            }
            TreeAct::Visible(id) => app.toggle_visible(id),
            TreeAct::Ground(id) => app.toggle_ground(id),
            TreeAct::Drop(src, into) => app.reparent(&src, &into),
            TreeAct::Menu(p, m) => menu_act(app, &p, m),
        }
    }
}

fn menu_act(app: &mut EasyCadApp, p: &OccPath, m: MenuAct) {
    let d = &app.doc.data;
    let comp = d.component_at(p).unwrap_or(ComponentId::ROOT);
    let id = p.last().copied();
    match m {
        MenuAct::Activate => app.activate(p.clone()),
        MenuAct::Isolate => app.isolate(p),
        MenuAct::Copy => {
            if let Some(id) = id {
                app.make_independent(id)
            }
        }
        MenuAct::PlaceAgain => app.place_again(p),
        MenuAct::NewInside => app.new_component(comp),
        MenuAct::InsertEcad => {
            app.activate(p.clone());
            app.insert_ecad();
        }
        MenuAct::Update => app.update_link(comp),
        MenuAct::Break => app.break_link(comp),
        MenuAct::Remove => app.remove_occurrence(p),
        MenuAct::Move => {
            app.select_occurrence(Some(p.clone()), false);
            app.asm.gizmo = true;
        }
        MenuAct::Explode => app.asm.explode_open = true,
    }
}

fn tree_row(ui: &mut Ui, indent: f32, height: f32, selected: bool) -> (Rect, egui::Response) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click_and_drag());
    if selected {
        ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme::hex(0x20242a));
    }
    (rect.with_min_x(rect.left() + indent), resp)
}

/// A small isometric cube (component glyph).
fn paint_cube(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let s = 5.5;
    let top = [c + vec2(0.0, -s), c + vec2(s, -s * 0.5), c, c + vec2(-s, -s * 0.5)];
    let st = Stroke::new(1.2, col);
    p.add(egui::Shape::closed_line(top.to_vec(), st));
    p.line_segment([c + vec2(-s, -s * 0.5), c + vec2(-s, s * 0.5)], st);
    p.line_segment([c + vec2(s, -s * 0.5), c + vec2(s, s * 0.5)], st);
    p.line_segment([c, c + vec2(0.0, s)], st);
    p.line_segment([c + vec2(-s, s * 0.5), c + vec2(0.0, s)], st);
    p.line_segment([c + vec2(s, s * 0.5), c + vec2(0.0, s)], st);
}

fn paint_pin(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let st = Stroke::new(1.4, col);
    p.circle_stroke(c + vec2(0.0, -3.0), 3.0, st);
    p.line_segment([c, c + vec2(0.0, 6.0)], st);
    p.line_segment([c + vec2(-4.0, 0.5), c + vec2(4.0, 0.5)], st);
}

#[allow(clippy::too_many_arguments)]
fn rows(
    app: &mut EasyCadApp,
    ui: &mut Ui,
    d: &DocData,
    model: Option<&Output>,
    comp: ComponentId,
    prefix: &OccPath,
    depth: usize,
    pointer: Option<egui::Pos2>,
    acts: &mut Vec<TreeAct>,
    drop_target: &mut Option<OccPath>,
) {
    if depth > 16 {
        return;
    }
    let Some(kids) = d.children_of(comp) else { return };
    for o in kids {
        let mut path = prefix.clone();
        path.push(o.id);
        let c = d.component(o.component);
        let has_kids = c.is_some_and(|c| !c.children.is_empty());
        // locked references (a STEP board's sub-parts) start collapsed
        let collapsed = app.asm.collapsed.contains(&path) != c.is_some_and(|c| c.locked);
        let sel = app.asm.sel.as_ref() == Some(&path);
        let is_active = app.active == path;
        let on_active_path = app.active.starts_with(&path);
        let indent = 22.0 + depth as f32 * 14.0;
        let (r, resp) = tree_row(ui, indent, 30.0, sel || is_active);
        let p = ui.painter().clone();
        let id_salt = ("occ", path.clone());
        // chevron
        if has_kids {
            let cr = Rect::from_center_size(r.left_center() + vec2(-8.0, 0.0), vec2(14.0, 14.0));
            let cresp = ui.interact(cr, ui.scope_id().with((&id_salt, "chev")), Sense::click());
            let col = if cresp.hovered() { theme::TEXT_STRONG } else { theme::TEXT_MUTED };
            let cc = cr.center();
            let pts = if collapsed {
                vec![cc + vec2(-2.0, -4.0), cc + vec2(2.5, 0.0), cc + vec2(-2.0, 4.0)]
            } else {
                vec![cc + vec2(-4.0, -2.0), cc + vec2(0.0, 2.5), cc + vec2(4.0, -2.0)]
            };
            p.add(egui::Shape::line(pts, Stroke::new(1.4, col)));
            if cresp.clicked() {
                acts.push(TreeAct::Toggle(path.clone()));
            }
        }
        let locked = c.is_some_and(|c| c.locked);
        let purchased = c.is_some_and(|c| c.purchased.is_some());
        let reference = matches!(c.map(|c| &c.source), Some(ComponentSource::Step { reference: true, .. }));
        let glyph = if reference {
            theme::GREEN_REF
        } else if is_active {
            theme::ACCENT
        } else {
            theme::hex(0xa3aab4)
        };
        paint_cube(&p, r.left_center() + vec2(6.0, 0.0), glyph);
        let dimmed = !o.visible;
        let col = if is_active {
            theme::ACCENT_LINK
        } else if dimmed {
            theme::TEXT.gamma_multiply(0.45)
        } else if !app.active.is_empty() && !on_active_path && !app.active.is_empty() {
            theme::TEXT_DIM
        } else {
            theme::TEXT
        };
        let name_w = p
            .layout_no_wrap(o.name.clone(), theme::prop(13.0), col)
            .size()
            .x;
        p.text(r.left_center() + vec2(18.0, 0.0), egui::Align2::LEFT_CENTER, &o.name, theme::prop(13.0), col);
        let mut x = r.left() + 18.0 + name_w + 6.0;
        // instance count badge
        let n = instance_count(d, o.component);
        if n > 1 {
            let t = format!("×{n}");
            let w = p.layout_no_wrap(t.clone(), theme::mono(10.0), theme::TEXT).size().x + 8.0;
            let br = Rect::from_min_size(pos2(x, r.center().y - 8.0), vec2(w, 16.0));
            p.rect_filled(br, 8.0, theme::hex(0x2c3138));
            p.text(br.center(), egui::Align2::CENTER_CENTER, t, theme::mono(10.0), theme::TEXT_DIM);
            x += w + 4.0;
        }
        // right side: eye, ground pin, lock / link status
        let eye = Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), vec2(24.0, 24.0));
        let er = ui.interact(eye, ui.scope_id().with((&id_salt, "eye")), Sense::click());
        if er.hovered() {
            p.rect_filled(eye, 6.0, theme::HOVER);
        }
        widgets::paint_icon(
            &p,
            eye.shrink(5.0),
            if o.visible { Icon::Eye } else { Icon::EyeOff },
            if er.hovered() { theme::TEXT_STRONG } else { theme::TEXT_DIM },
        );
        let er = er.on_hover_text(if o.visible { "Hide" } else { "Show" });
        if er.clicked() {
            acts.push(TreeAct::Visible(o.id));
        }
        let pin = Rect::from_center_size(pos2(r.right() - 46.0, r.center().y), vec2(20.0, 24.0));
        let pr = ui.interact(pin, ui.scope_id().with((&id_salt, "pin")), Sense::click());
        if o.grounded || pr.hovered() {
            paint_pin(
                &p,
                pin.center(),
                if o.grounded { theme::ORANGE_SOFT } else { theme::TEXT_MUTED },
            );
        }
        let pr = pr.on_hover_text(if o.grounded {
            "Grounded: fixed for mates (click to unground)"
        } else {
            "Click to ground (fix for mates)"
        });
        if pr.clicked() {
            acts.push(TreeAct::Ground(o.id));
        }
        let mut rx = r.right() - 60.0;
        if let Some(c) = c {
            if let ComponentSource::Linked { path: lp, .. } = &c.source {
                let st = app.link_status(c.id);
                let (t, colr, tip) = match &st {
                    LinkStatus::UpToDate => ("linked", theme::GREEN, format!("Linked to {lp} · up to date")),
                    LinkStatus::OutOfDate => ("update", theme::ORANGE, format!("{lp} changed · click to update")),
                    LinkStatus::Missing(e) => ("missing", theme::RED, format!("{lp}: {e}")),
                    LinkStatus::NotLinked => ("", theme::TEXT_MUTED, String::new()),
                };
                if !t.is_empty() {
                    let w = p.layout_no_wrap(t.into(), theme::prop(10.5), colr).size().x + 10.0;
                    let lr = Rect::from_min_size(pos2(rx - w, r.center().y - 9.0), vec2(w, 18.0));
                    let lresp = ui.interact(lr, ui.scope_id().with((&id_salt, "link")), Sense::click());
                    p.rect(lr, 9.0, Color32::TRANSPARENT, Stroke::new(1.0, colr), egui::StrokeKind::Inside);
                    p.text(lr.center(), egui::Align2::CENTER_CENTER, t, theme::prop(10.5), colr);
                    if lresp.on_hover_text(tip).clicked() && st == LinkStatus::OutOfDate {
                        acts.push(TreeAct::Menu(path.clone(), MenuAct::Update));
                    }
                    rx -= w + 4.0;
                }
            } else if locked {
                let lr = Rect::from_center_size(pos2(rx - 8.0, r.center().y), vec2(14.0, 14.0));
                widgets::paint_icon(&p, lr, Icon::Lock, theme::TEXT_LABEL);
                ui.interact(lr, ui.scope_id().with((&id_salt, "lock")), Sense::hover()).on_hover_text(if purchased {
                    "Locked reference (purchased): read-only, one BOM line"
                } else {
                    "Locked (read-only)"
                });
                rx -= 18.0;
            }
        }
        let _ = (x, rx);
        // interactions
        if app.asm.tree_drag.is_some()
            && app.asm.tree_drag.as_ref() != Some(&path)
            && pointer.is_some_and(|q| r.contains(q))
        {
            *drop_target = Some(path.clone());
            p.rect_stroke(r.with_min_x(r.left() - indent), 3.0, Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
        }
        let tip = format!(
            "{}{}\nDouble-click to edit in place · drag onto another component to move it there",
            d.component_name(o.component),
            if o.grounded { " · grounded" } else { "" }
        );
        let resp = resp.on_hover_text(tip);
        if resp.drag_started() && !er.hovered() && !pr.hovered() {
            app.asm.tree_drag = Some(path.clone());
        }
        if resp.double_clicked() {
            acts.push(TreeAct::Activate(path.clone()));
        } else if resp.clicked() {
            let add = ui.input(|i| i.modifiers.command || i.modifiers.shift);
            acts.push(TreeAct::Select(path.clone(), add));
        }
        let pc = path.clone();
        let linked = c.is_some_and(|c| matches!(c.source, ComponentSource::Linked { .. }));
        let movable = !o.grounded && !is_mated(d, &path);
        let vis_label = if o.visible { "Hide" } else { "Show" };
        let ground_label = if o.grounded { "Unground" } else { "Ground" };
        let oid = o.id;
        resp.context_menu(|ui| {
            let mut chosen: Option<TreeAct> = None;
            let item = |ui: &mut Ui, label: &str, enabled: bool| {
                let c = ui.add_enabled(enabled, egui::Button::new(label)).clicked();
                if c {
                    ui.close();
                }
                c
            };
            if item(ui, "Activate (edit in place)", !locked) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Activate));
            }
            if item(ui, "Isolate", true) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Isolate));
            }
            if item(ui, vis_label, true) {
                chosen = Some(TreeAct::Visible(oid));
            }
            ui.separator();
            if item(ui, "Move   G", movable) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Move));
            }
            if item(ui, ground_label, true) {
                chosen = Some(TreeAct::Ground(oid));
            }
            if item(ui, "Explode…", true) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Explode));
            }
            ui.separator();
            if item(ui, "New component inside", !locked) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::NewInside));
            }
            if item(ui, "Insert component (.ecad) inside…", !locked) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::InsertEcad));
            }
            if item(ui, "Place another", true) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::PlaceAgain));
            }
            if item(ui, "Make independent copy", n > 1) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Copy));
            }
            if linked {
                ui.separator();
                if item(ui, "Update link", true) {
                    chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Update));
                }
                if item(ui, "Break link", true) {
                    chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Break));
                }
            }
            ui.separator();
            if item(ui, "Remove", true) {
                chosen = Some(TreeAct::Menu(pc.clone(), MenuAct::Remove));
            }
            if let Some(c) = chosen {
                acts.push(c);
            }
        });
        // bodies of this occurrence (a one-line hint: the active component lists its own below)
        let _ = model;
        if has_kids && !collapsed {
            rows(app, ui, d, model, o.component, &path, depth + 1, pointer, acts, drop_target);
        }
    }
}

fn mates_folder(app: &mut EasyCadApp, ui: &mut Ui, d: &DocData) {
    if d.mates.is_empty() && app.asm.mate.is_none() {
        return;
    }
    let (r, resp) = tree_row(ui, 22.0, 28.0, false);
    let p = ui.painter().clone();
    let solve = app.model.as_ref().and_then(|m| m.asm.as_ref()).and_then(|a| a.solve.clone());
    let bad = solve.as_ref().map(|s| !s.ok()).unwrap_or(false);
    p.text(
        r.left_center() + vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("{} Mates ({})", if app.asm.mates_collapsed { "▸" } else { "▾" }, d.mates.len()),
        theme::prop(13.0),
        if bad { theme::ORANGE } else { theme::TEXT_DIM },
    );
    if resp.clicked() {
        app.asm.mates_collapsed = !app.asm.mates_collapsed;
    }
    resp.context_menu(|ui| {
        if ui.button("New mate…   A").clicked() {
            app.open_mate(None);
            ui.close();
        }
    });
    if app.asm.mates_collapsed {
        return;
    }
    let mut edit = None;
    let mut toggle = None;
    let mut delete = None;
    for m in &d.mates {
        let status = solve.as_ref().and_then(|s| s.mates.iter().find(|r| r.mate == m.id)).map(|r| r.status.clone());
        let (r, resp) = tree_row(ui, 36.0, 28.0, false);
        let (col, tip) = match &status {
            Some(MateStatus::Solved) => (theme::TEXT, "Solved".to_string()),
            Some(MateStatus::Redundant) => (theme::TEXT_DIM, "Redundant: both sides were already placed and it holds".into()),
            Some(MateStatus::OverConstrained(w)) => (theme::ORANGE, format!("Over-constrained: {w}")),
            Some(MateStatus::Unsolved(w)) => (theme::ORANGE, format!("Unsolved: {w}")),
            Some(MateStatus::Suppressed) => (theme::TEXT_MUTED, "Suppressed".into()),
            None => (theme::TEXT_DIM, String::new()),
        };
        p.text(r.left_center(), egui::Align2::LEFT_CENTER, &m.name, theme::prop(13.0), col);
        p.text(
            r.right_center() - vec2(12.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!("{:?}", m.kind),
            theme::prop(11.0),
            if matches!(status, Some(MateStatus::OverConstrained(_) | MateStatus::Unsolved(_))) {
                theme::ORANGE
            } else {
                theme::TEXT_LABEL
            },
        );
        let resp = resp.on_hover_text(format!("{tip}\nDouble-click to edit (jog its free motion)"));
        if resp.double_clicked() {
            edit = Some(m.id);
        }
        resp.context_menu(|ui| {
            if ui.button("Edit…").clicked() {
                edit = Some(m.id);
                ui.close();
            }
            if ui.button(if m.suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                toggle = Some(m.id);
                ui.close();
            }
            if ui.button("Delete").clicked() {
                delete = Some(m.id);
                ui.close();
            }
        });
    }
    if let Some(id) = edit {
        app.open_mate(Some(id));
    }
    if let Some(id) = toggle {
        let r = app.doc.edit_mate(id, "Suppress mate", |m| m.suppressed = !m.suppressed);
        if let Err(e) = r {
            app.warn(e.to_string());
        }
        app.request_model();
    }
    if let Some(id) = delete {
        let r = app.doc.edit("Delete mate", |d| {
            d.mates.retain(|m| m.id != id);
            Ok::<_, DocError>(())
        });
        if let Err(e) = r {
            app.warn(e.to_string());
        }
        app.request_model();
    }
}

// ---------------------------------------------------------------- breadcrumb

/// `sensor-node / Enclosure / Lid` in the title bar; a click goes to that level.
pub fn breadcrumb(app: &mut EasyCadApp, ui: &mut Ui) {
    if app.active.is_empty() {
        return;
    }
    let d = &app.doc.data;
    let mut segs: Vec<(OccPath, String)> = vec![(Vec::new(), d.name.clone())];
    for i in 1..=app.active.len() {
        let p = app.active[..i].to_vec();
        let c = d.component_at(&p).map(|c| d.component_name(c)).unwrap_or_default();
        segs.push((p, c));
    }
    let mut go = None;
    let n = segs.len();
    for (i, (p, name)) in segs.into_iter().enumerate() {
        if i > 0 {
            ui.label(RichText::new("/").color(theme::TEXT_FAINT));
        }
        let last = i + 1 == n;
        let t = RichText::new(&name).size(13.0).color(if last { theme::ACCENT_LINK } else { theme::TEXT_DIM });
        let r = ui.add(egui::Label::new(if last { t.strong() } else { t }).sense(Sense::click()));
        if !last && r.on_hover_text("Go up to here (Esc goes up one level)").clicked() {
            go = Some(p);
        }
    }
    if let Some(p) = go {
        app.activate(p);
    }
}

/// Status line text while a component is active.
pub fn active_label(app: &EasyCadApp) -> Option<String> {
    if app.active.is_empty() {
        return None;
    }
    let d = &app.doc.data;
    let c = d.component_at(&app.active).ok()?;
    Some(format!("Editing {} in place · Esc goes up", d.component_name(c)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycad_model::document::Document;
    use glam::DVec3;

    fn app(doc: Document) -> EasyCadApp {
        EasyCadApp::with_parts(doc, crate::worker::Worker::spawn(|| {}), false)
    }

    fn occ(app: &EasyCadApp, name: &str) -> OccPath {
        app.doc
            .data
            .all_occurrences()
            .into_iter()
            .find(|(_, o, _)| o.name == name)
            .unwrap_or_else(|| panic!("{name}"))
            .0
    }

    #[test]
    fn reparent_keeps_the_world_placement_and_copies_split_off() {
        let mut a = app(Document::new("asm"));
        a.new_component(ComponentId::ROOT);
        a.new_component(ComponentId::ROOT);
        let (p1, p2) = (occ(&a, "Component1:1"), occ(&a, "Component2:1"));
        a.occ_edit(p1[0], "Move", |o| o.transform = Transform::translation(DVec3::new(0.0, 5.0, 0.0)));
        a.occ_edit(p2[0], "Move", |o| o.transform = Transform::translation(DVec3::new(10.0, 0.0, 0.0)));
        a.reparent(&p2, &p1);
        let p2 = occ(&a, "Component2:1");
        assert_eq!(p2.len(), 2, "now inside Component1");
        let w = a.doc.data.world_transform(&p2).unwrap();
        assert!((w.translation - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9);
        assert_eq!(a.doc.undo_label(), Some("Move Component2:1 into Component1:1"));
        // a component cannot go inside itself
        a.reparent(&p1, &p2);
        assert_eq!(occ(&a, "Component1:1").len(), 1);
        // two occurrences, then one gets its own definition
        a.place_again(&occ(&a, "Component1:1"));
        let c = a.doc.data.component_at(&occ(&a, "Component1:1")).unwrap();
        assert_eq!(instance_count(&a.doc.data, c), 2);
        let second = occ(&a, "Component1:2");
        a.make_independent(second[0]);
        assert_eq!(instance_count(&a.doc.data, c), 1);
        assert!(a.doc.data.components.iter().any(|x| x.name == "Component1 (2)"));
    }

    #[test]
    fn linked_component_status_update_and_break() {
        let dir = std::env::temp_dir().join(format!("cw-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("bracket.ecad");
        let mut b = Document::new("bracket");
        b.set_param("t", "2").unwrap();
        b.save_to_dir(&part).unwrap();
        let mut a = app(Document::new("host"));
        a.path = Some(dir.join("host.ecad"));
        let c = a.doc.link_component(&part, &a.host_dir()).unwrap();
        a.doc.add_occurrence(ComponentId::ROOT, c, "bracket:1", Transform::IDENTITY).unwrap();
        assert_eq!(a.link_status(c), LinkStatus::UpToDate);
        b.set_param("t", "3").unwrap();
        b.save_to_dir(&part).unwrap();
        a.asm.links.clear();
        assert_eq!(a.link_status(c), LinkStatus::OutOfDate);
        a.update_link(c);
        assert_eq!(a.link_status(c), LinkStatus::UpToDate);
        assert!(a.doc.data.component(c).unwrap().locked);
        a.break_link(c);
        assert!(!a.doc.data.component(c).unwrap().locked);
        assert_eq!(a.link_status(c), LinkStatus::NotLinked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn features_go_into_the_active_component() {
        use easycad_model::feature::{Feature, FeatureId, FeatureKind};
        let mut a = app(Document::new("asm"));
        a.new_component(ComponentId::ROOT);
        let p = occ(&a, "Component1:1");
        a.activate(p.clone());
        assert_eq!(a.active, p);
        let c = a.scope();
        let f = Feature {
            id: FeatureId(0),
            name: "Round".into(),
            suppressed: false,
            kind: FeatureKind::Fillet {
                body: None,
                edges: Vec::new(),
                radius: "1".into(),
                auto_reduce: false,
                sets: Vec::new(),
                skipped: Vec::new(),
            },
        };
        a.commit_features("Fillet Round", vec![f], None);
        assert!(a.doc.data.features.is_empty(), "nothing at the root");
        assert_eq!(a.doc.data.component(c).unwrap().features.len(), 1);
        a.refresh_scope();
        assert_eq!(a.view().features.len(), 1);
        // local params shadow the document's
        a.doc.set_param("wall", "2").unwrap();
        a.set_local_param("wall", "3").unwrap();
        a.refresh_scope();
        assert_eq!(a.view().params.get("wall").map(|p| p.expr.0.as_str()), Some("3"));
        assert_eq!(a.doc.data.params.get("wall").map(|p| p.expr.0.as_str()), Some("2"));
        // Esc-style: up one level
        a.activate_parent();
        assert!(a.active.is_empty());
        assert_eq!(a.scope(), ComponentId::ROOT);
    }
}
