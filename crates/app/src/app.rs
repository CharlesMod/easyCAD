//! Application state, the frame loop, regeneration plumbing, commands and
//! keyboard shortcuts. Screens live in `ui_*` modules.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use easycad_model::document::{DocError, Document};
use easycad_model::feature::{BodyId, Feature, FeatureId};
use easycad_model::regen::FeatureStatus;
use egui::{Key, Modifiers};

use crate::adapters;
use crate::camera::Camera;
use crate::dialogs::{Dialog, DialogKind};
use crate::pickstack::{Filter, PickKey, PickStack};
use crate::sketch_mode::SketchSession;
use crate::viewport::gpu::DisplayMode;
use crate::worker::{Output, Purpose, Worker};
use easycad_model::regen::MeshQuality;

/// Debounce for parameter / dialog edits before regenerating.
pub const DEBOUNCE: f64 = 0.18;

pub enum Mode {
    Model,
    Sketch(Box<SketchSession>),
    Parts(Box<crate::ui_parts::PartsUi>),
    Drawing(Box<crate::ui_drawing::DrawingUi>),
}

impl Mode {
    pub fn is_model(&self) -> bool {
        matches!(self, Mode::Model)
    }
}

#[derive(Default)]
pub struct ParamsUi {
    pub buffers: std::collections::HashMap<String, String>,
    pub adding: Option<(String, String)>,
    pub pending: Option<(String, String, f64)>,
    pub error: Option<(String, String)>,
}

/// Derived data recomputed only when the document revision or the model changes
/// (never per frame: the panels read from here).
#[derive(Default)]
pub struct UiCache {
    pub key: (u64, u64),
    pub usage: std::collections::BTreeMap<String, usize>,
    pub users: std::collections::HashMap<String, String>,
    pub chips: std::collections::HashMap<FeatureId, String>,
    pub hot: Option<std::collections::BTreeSet<String>>,
    /// The active component's view (`None` = the root: use the document).
    pub scoped: Option<Arc<easycad_model::document::DocData>>,
    pub scoped_key: Option<(u64, easycad_model::assembly::OccPath)>,
}

pub struct Toast {
    pub text: String,
    pub until: f64,
    pub warn: bool,
}

pub struct EasyCadApp {
    pub doc: Document,
    pub path: Option<PathBuf>,
    pub saved_revision: u64,
    pub worker: Worker,
    /// Last good model output (always kept on screen while regenerating).
    pub model: Option<Arc<Output>>,
    /// Latest preview of the open dialog.
    pub preview: Option<Arc<Output>>,
    pub preview_dirty: Option<f64>,
    pub model_dirty: bool,
    pub camera: Camera,
    pub display: DisplayMode,
    pub filter: Filter,
    pub selection: Vec<PickKey>,
    pub stack: PickStack,
    pub stack_since: f64,
    pub hover_pos: Option<egui::Pos2>,
    pub hidden: HashSet<BodyId>,
    pub mode: Mode,
    pub dialog: Option<Dialog>,
    pub params: ParamsUi,
    pub browser_open: bool,
    pub toasts: Vec<Toast>,
    pub fit_pending: bool,
    pub viewport_size: egui::Vec2,
    pub viewport_rect: egui::Rect,
    pub gpu: bool,
    pub frame_ms: f32,
    pub last_regen_ms: f32,
    pub orientation_body: Option<BodyId>,
    pub export: Option<crate::dialogs::export::ExportDialog>,
    pub calibrate: Option<crate::ui_process::CalibrateUi>,
    pub export_job: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    pub cube_hover: Option<crate::camera::CubeRegion>,
    /// Debounced edit of the active process profile (fit table drags).
    pub pending_profile: Option<(easycad_model::process::ProcessProfile, f64)>,
    pub cache: UiCache,
    /// "New document" prompt: (name, profile).
    pub new_doc_prompt: Option<(String, String)>,
    pub show_build_plate: bool,
    /// Measured width of the floating dialog card (for right alignment).
    pub dialog_card_w: f32,
    /// A viewport handle / slider is being dragged (coarse previews).
    pub dragging: bool,
    /// A drag on the extrude / offset-plane arrow in progress.
    pub arrow_drag: Option<crate::drag_arrow::ArrowDrag>,
    pub now: f64,
    pub quit: bool,
    /// `EASYCAD_STATE_FILE` JSON dump for the E2E scripts.
    pub state_dump: Option<crate::e2e_state::StateDump>,
    /// Tab / Shift+Tab presses taken from the raw input for the pick stack.
    pub pending_cycle: i32,
    /// A text field had keyboard focus this frame.
    pub typing: bool,
    /// Where the history strip drew each chip last frame (for tests and hit-testing).
    pub hist_rects: Vec<(crate::hist_sel::HistItem, egui::Rect)>,
    /// The toolbar button (Undo/Redo) the pointer is over: the history strip previews its diff.
    pub hist_peek: Option<crate::history_ui::HistPeek>,
    pub hist_view: crate::history_tree::HistView,
    /// Screen rects of the branch row labels (name, rect) drawn last frame.
    pub hist_row_rects: Vec<(String, egui::Rect)>,
    /// The diff the history strip last previewed (undo/redo hover or branch tab hover).
    pub hist_diff: Option<easycad_model::branch::HistDiff>,
    /// History chips selected by click (Del deletes them).
    pub hist_sel: Vec<crate::hist_sel::HistItem>,
    /// A delete waiting on the "dependents" confirm.
    pub delete_ask: Option<crate::hist_sel::DeleteAsk>,
    /// The body whose face a new sketch was placed on, and once committed the
    /// sketch's id: the default target of an Extrude / Revolve on that sketch.
    /// Waiting for the click that names the plane of a new sketch.
    pub plane_pick: Option<crate::plane_pick::PlanePick>,
    /// Origin rows drawn in the viewport (XY XZ YZ X Y Z origin); hidden by default.
    pub origin_vis: [bool; 7],
    /// What a datum dialog's active slot would pick under the pointer.
    pub datum_hover: Option<crate::datum_ui::Picked>,
    pub sketch_body: Option<(Option<easycad_model::feature::SketchId>, BodyId)>,
    /// In-app path prompt (no system file dialog) and what to do with the path.
    pub path_prompt: Option<(crate::files::PathPrompt, OnPath)>,
    /// "New enclosure from PCB" wizard.
    pub wizard: Option<crate::ui_welcome::WizardUi>,
    /// Per-user settings (loaded at start, saved on change).
    pub settings: crate::settings::Settings,
    pub show_settings: bool,
    /// The `?` cheat sheet.
    pub show_keys: bool,
    pub autosave: crate::recovery::Autosaver,
    /// Recovery copies left by a crashed session, offered at start.
    pub recovery_offer: Vec<crate::recovery::Entry>,
    /// Assemblies: the occurrence being edited (`[]` = the root / whole assembly).
    pub active: easycad_model::assembly::OccPath,
    /// Assemblies: explode slider 0..1 (display only).
    pub explode: f32,
    pub asm: crate::ui_assembly::AsmUi,
    /// Explode slider rect (E2E state dump).
    pub asm_explode_rect: Option<egui::Rect>,
    /// Section view (view toolbar).
    pub section: Option<SectionUi>,
    /// Bounds of the uncut model, for placing the section plane.
    pub section_bbox: Option<easycad_model::occt::BBox>,
    /// Overhang heat map (view toolbar).
    pub overhang: crate::ui_overhang::OverhangUi,
    /// Google Drive sync (inert unless the binary starts it).
    pub cloud: crate::cloud::Cloud,
    /// The performance overlay (F12, `EASYCAD_PROFILE=1`).
    pub show_perf: bool,
    pub bench_start: Option<f64>,
    /// Live reload into a newly installed build (reload.rs).
    pub reload: crate::reload::State,
}

/// Section view settings: a plane across the model along one axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionUi {
    /// 0 = X, 1 = Y, 2 = Z.
    pub axis: usize,
    /// Position across the model's bounds, 0..1.
    pub at: f32,
    /// Keep the other half.
    pub flip: bool,
}

/// What a file chooser does with the chosen path.
pub type OnPath = Box<dyn FnOnce(&mut EasyCadApp, PathBuf)>;

impl EasyCadApp {
    pub fn new(cc: &eframe::CreationContext<'_>, doc: Option<Document>) -> Self {
        crate::theme::apply(&cc.egui_ctx);
        let gpu = match &cc.wgpu_render_state {
            Some(rs) => {
                crate::viewport::gpu::ViewportGpu::install(rs);
                true
            }
            None => false,
        };
        crate::units::set_inch(crate::settings::Settings::load().inch);
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        let doc = doc.unwrap_or_else(|| Document::new("Untitled"));
        let mut app = Self::with_parts(doc, worker, gpu);
        app.recovery_offer = crate::recovery::orphans();
        app
    }

    pub fn with_parts(doc: Document, worker: Worker, gpu: bool) -> Self {
        let rev = doc.data.revision;
        EasyCadApp {
            doc,
            path: None,
            saved_revision: rev,
            worker,
            model: None,
            preview: None,
            preview_dirty: None,
            model_dirty: true,
            camera: Camera::default(),
            display: DisplayMode::ShadedEdges,
            filter: Filter::default(),
            selection: Vec::new(),
            stack: PickStack::default(),
            stack_since: 0.0,
            hover_pos: None,
            hidden: HashSet::new(),
            mode: Mode::Model,
            dialog: None,
            params: ParamsUi::default(),
            browser_open: true,
            toasts: Vec::new(),
            fit_pending: true,
            viewport_size: egui::vec2(800.0, 600.0),
            viewport_rect: egui::Rect::NOTHING,
            gpu,
            frame_ms: 0.0,
            last_regen_ms: 0.0,
            orientation_body: None,
            export: None,
            calibrate: None,
            export_job: None,
            cube_hover: None,
            pending_profile: None,
            cache: UiCache::default(),
            new_doc_prompt: None,
            show_build_plate: false,
            dialog_card_w: 390.0,
            dragging: false,
            arrow_drag: None,
            now: 0.0,
            quit: false,
            state_dump: crate::e2e_state::StateDump::from_env(),
            pending_cycle: 0,
            typing: false,
            hist_sel: Vec::new(),
            hist_rects: Vec::new(),
            hist_peek: None,
            hist_row_rects: Vec::new(),
            hist_view: crate::history_tree::HistView::from_settings(&crate::settings::Settings::load()),
            hist_diff: None,
            delete_ask: None,
            plane_pick: None,
            origin_vis: [false; 7],
            datum_hover: None,
            sketch_body: None,
            path_prompt: None,
            wizard: None,
            settings: crate::settings::Settings::load(),
            show_settings: false,
            show_keys: false,
            autosave: crate::recovery::Autosaver::default(),
            recovery_offer: Vec::new(),
            active: Vec::new(),
            explode: 0.0,
            asm: Default::default(),
            asm_explode_rect: None,
            section: None,
            section_bbox: None,
            overhang: Default::default(),
            cloud: crate::cloud::Cloud::disabled(),
            show_perf: std::env::var_os("EASYCAD_PROFILE").is_some(),
            bench_start: None,
            reload: Default::default(),
        }
    }

    // ------------------------------------------------------------ regeneration

    /// Features active in the viewport: a dialog editing feature i shows 0..i.
    pub fn active_count(&self) -> Option<usize> {
        if let Some(d) = &self.dialog {
            if let Some(i) = d.edit_index(&self.doc.data) {
                return Some(i);
            }
        }
        self.rollback()
    }

    /// The history rollback marker of the component being edited.
    pub fn rollback(&self) -> Option<usize> {
        let c = self.scope();
        if c == easycad_model::assembly::ComponentId::ROOT {
            self.doc.data.rollback
        } else {
            self.doc.data.component(c).and_then(|c| c.rollback)
        }
    }

    /// The last live feature: the chip drawn yellow as the history's head.
    pub fn head_feature(&self) -> Option<easycad_model::feature::FeatureId> {
        let f = &self.view().features;
        let n = self.rollback().unwrap_or(f.len()).min(f.len());
        n.checked_sub(1).and_then(|i| f.get(i)).map(|f| f.id)
    }

    /// Move the rollback marker of the component being edited (undoable, saved).
    pub fn set_rollback(&mut self, at: Option<usize>) {
        let c = self.scope();
        if let Err(e) = self.doc.set_component_rollback(c, at) {
            self.warn(e.to_string());
        }
        self.request_model();
    }

    pub fn request_model(&mut self) {
        self.model_dirty = true;
    }

    /// The section plane (point, normal) in the view frame, when on.
    pub fn section_cut(&self) -> Option<(glam::DVec3, glam::DVec3)> {
        let s = self.section?;
        let bb = self.section_bbox.or_else(|| self.model.as_ref()?.bbox)?;
        let mut n = glam::DVec3::ZERO;
        n[s.axis] = if s.flip { -1.0 } else { 1.0 };
        let mut p = bb.center();
        p[s.axis] = bb.min[s.axis] + (bb.max[s.axis] - bb.min[s.axis]) * s.at as f64;
        Some((p, n))
    }

    /// Section on / off; it starts across the middle, the kept half away from the
    /// viewer, on the horizontal axis most along the view.
    pub fn toggle_section(&mut self) {
        if self.section.take().is_none() {
            let f = self.camera.forward();
            // a vertical cut shows the wall profile; Z only when looking straight down
            let axis = if f.z.abs() > 0.95 { 2 } else if f.x.abs() > f.y.abs() { 0 } else { 1 };
            self.section_bbox = self.model.as_ref().and_then(|m| m.bbox);
            // normal towards the viewer: what stays is behind the plane
            self.section = Some(SectionUi { axis, at: 0.5, flip: f[axis] > 0.0 });
        }
        self.request_model();
    }

    /// The component being edited (`ROOT` unless a component is active).
    pub fn scope(&self) -> easycad_model::assembly::ComponentId {
        crate::ui_assembly::active_component(self)
    }

    /// The document as the active component sees it: its features, sketches and
    /// imports, document params with its local params on top. The root: the document.
    pub fn view(&self) -> &easycad_model::document::DocData {
        match &self.cache.scoped {
            Some(v) if self.cache.scoped_key.as_ref().is_some_and(|k| k.0 == self.doc.data.revision && k.1 == self.active) => v,
            _ => &self.doc.data,
        }
    }

    /// Refresh the active component's view (call after activation or edits).
    pub fn refresh_scope(&mut self) {
        let key = (self.doc.data.revision, self.active.clone());
        if self.cache.scoped_key.as_ref() == Some(&key) {
            return;
        }
        let c = self.scope();
        self.cache.scoped = (c != easycad_model::assembly::ComponentId::ROOT)
            .then(|| Arc::new(crate::scope::view(&self.doc.data, c)));
        self.cache.scoped_key = Some(key);
    }

    fn submit_model(&mut self) {
        let mut data = adapters::effective_in(&self.doc.data, self.scope(), self.active_count());
        // the open mates dialog's draft is solved with the rest (live preview)
        if let Some(m) = self.asm.mate.as_ref().and_then(|m| m.draft()) {
            match data.mates.iter_mut().find(|x| x.id == m.id) {
                Some(x) => *x = m,
                None => data.mates.push(m),
            }
        }
        self.worker.submit_full(crate::worker::Request {
            id: 0,
            purpose: Purpose::Model,
            data,
            blobs: self.doc.blobs.clone(),
            library: self.doc.library.clone(),
            prefixes: Vec::new(),
            drawing: None,
            quality: self.settings.mesh.quality(),
            section: None,
            active: self.active.clone(),
            explode: self.explode,
            cut: self.section_cut(),
        });
        self.model_dirty = false;
    }

    /// Features the open dialog would add, inserted at the active position.
    pub fn preview_data(&self) -> Option<(easycad_model::document::DocData, Vec<usize>)> {
        let d = self.dialog.as_ref()?;
        let scope = self.scope();
        let mut data = adapters::effective_in(&self.doc.data, scope, self.active_count());
        let feats = d.build(&self.dialog_cx_data()).ok()?;
        if feats.is_empty() {
            return None;
        }
        let list = crate::scope::features_mut(&mut data, scope)?;
        let base = list.len();
        let mut prefixes = Vec::new();
        for (i, f) in feats.into_iter().enumerate() {
            prefixes.push(base + i);
            list.push(f);
        }
        Some((data, prefixes))
    }

    pub fn dialog_cx_data(&self) -> crate::dialogs::BuildCx {
        crate::dialogs::BuildCx {
            next_id: self.doc.data.next_id,
            data: self.view().clone(),
        }
    }

    fn submit_preview(&mut self) {
        self.preview_dirty = None;
        match self.preview_data() {
            Some((data, prefixes)) => {
                let section = self.joint_section_plane();
                self.worker.submit_full(crate::worker::Request {
                    id: 0,
                    purpose: Purpose::Preview,
                    data,
                    blobs: self.doc.blobs.clone(),
                    library: self.doc.library.clone(),
                    prefixes,
                    drawing: None,
                    // coarse meshes while a handle or slider is being dragged
                    quality: if self.dragging {
                        MeshQuality::Preview
                    } else {
                        self.settings.mesh.quality()
                    },
                    section,
                    active: self.active.clone(),
                    explode: self.explode,
                    cut: self.section_cut(),
                });
            }
            None => {
                self.worker.cancel(Purpose::Preview);
                self.preview = None;
            }
        }
    }

    pub fn request_preview(&mut self) {
        let wait = if self.dragging { 0.03 } else { DEBOUNCE };
        self.preview_dirty = Some(self.now + wait);
        self.cache.hot = None;
    }

    /// Plane through a straight stretch of wall for the joint dialog's live section:
    /// x = centre + 30 % of the width (clear of a centred cutout), seen along +X.
    pub fn joint_section_plane(&self) -> Option<(glam::DVec3, glam::DVec3, glam::DVec3)> {
        let d = self.dialog.as_ref()?;
        if !matches!(d.body, crate::dialogs::Body::Joint(_)) {
            return None;
        }
        let bb = self.model.as_ref()?.bbox?;
        let x = bb.center().x + 0.3 * bb.size().x;
        Some((
            glam::DVec3::new(x, 0.0, 0.0),
            glam::DVec3::X,
            glam::DVec3::Y,
        ))
    }

    /// Refresh `cache` if the document or the model changed.
    pub fn refresh_cache(&mut self) {
        let key = (
            self.doc.data.revision,
            self.model.as_ref().map(|m| m.id).unwrap_or(0),
        );
        self.refresh_scope();
        if key == self.cache.key && !self.cache.usage.is_empty() {
            return;
        }
        self.refresh_scope();
        let view = self.view().clone();
        // at the top of an assembly, a document param is used by every component
        let mut all = view.clone();
        if self.active.is_empty() {
            for c in &self.doc.data.components {
                all.features.extend(c.features.iter().cloned());
                all.sketches.extend(c.sketches.iter().cloned());
            }
        }
        let usage = adapters::usage_counts(&all);
        let users = usage
            .keys()
            .map(|n| {
                let u = adapters::users_of(&all, n);
                (
                    n.clone(),
                    if u.is_empty() {
                        "Not used yet".to_string()
                    } else {
                        format!("Used by: {}", u.join(", "))
                    },
                )
            })
            .collect();
        let chips = view
            .features
            .iter()
            .map(|f| (f.id, crate::ui_main::chip_label(self, f)))
            .collect();
        let (scoped, scoped_key) = (self.cache.scoped.take(), self.cache.scoped_key.take());
        self.cache = UiCache {
            key,
            usage,
            users,
            chips,
            hot: None,
            scoped,
            scoped_key,
        };
    }

    /// Parameters feeding the open dialog (cached until the dialog changes).
    pub fn hot_params(&mut self) -> std::collections::BTreeSet<String> {
        if self.cache.hot.is_none() {
            let h = match &self.dialog {
                Some(d) => d.hot_params(&self.dialog_cx_data()),
                None => Default::default(),
            };
            self.cache.hot = Some(h);
        }
        self.cache.hot.clone().unwrap_or_default()
    }

    pub fn poll_worker(&mut self) {
        for out in self.worker.poll() {
            self.accept(out);
        }
        if let Some(rx) = &self.export_job {
            if let Ok(r) = rx.try_recv() {
                match r {
                    Ok(m) => self.toast(m),
                    Err(e) => self.warn(format!("Export failed: {e}")),
                }
                self.export_job = None;
            }
        }
    }

    pub fn accept(&mut self, out: Output) {
        self.last_regen_ms = out.elapsed.as_secs_f32() * 1000.0;
        match out.purpose {
            Purpose::Model => {
                if self.fit_pending || self.model.is_none() {
                    if let Some(bb) = out.bbox {
                        let c = bb.center().as_vec3();
                        let r = (bb.size().length() * 0.5) as f32;
                        let aspect = self.viewport_size.x / self.viewport_size.y.max(1.0);
                        self.camera.fit_sphere(c, r, aspect);
                        self.fit_pending = false;
                    }
                } else if let Some(bb) = out.bbox {
                    self.camera.scene_center = bb.center().as_vec3();
                    self.camera.scene_radius = (bb.size().length() * 0.5) as f32;
                }
                // drop selections that no longer exist
                let ids: HashSet<BodyId> = out
                    .bodies
                    .iter()
                    .map(|b| b.body.id)
                    .chain(out.ctx.iter().map(|c| c.key))
                    .collect();
                self.selection.retain(|k| ids.contains(&k.body));
                self.stack.clear();
                if self.asm.hold.as_ref().is_some_and(|h| out.data.revision >= h.2) {
                    self.asm.hold = None;
                }
                // sketches on construction planes follow their plane (a cache, not an edit)
                if out.data.revision == self.doc.data.revision {
                    self.doc.data.refresh_sketch_planes(&out.result);
                    self.doc.data.apply_relinked(&out.result);
                }
                self.model = Some(Arc::new(out));
            }
            Purpose::Drawing => {
                if let Mode::Drawing(d) = &mut self.mode {
                    if out.drawing.is_some() {
                        d.result = out.drawing.clone();
                    }
                }
            }
            Purpose::Preview => {
                if self.dialog.is_some() {
                    let out = Arc::new(out);
                    if let Some(d) = &mut self.dialog {
                        d.on_preview(&out, self.model.as_deref());
                    }
                    self.preview = Some(out);
                }
            }
        }
    }

    /// Wait for the worker (tests / headless).
    pub fn settle(&mut self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            if self.model_dirty {
                self.submit_model();
            }
            if self.preview_dirty.is_some() {
                self.submit_preview();
            }
            let mut sink = Vec::new();
            let ok = self
                .worker
                .wait_idle(timeout.saturating_sub(start.elapsed()), &mut sink);
            let any = !sink.is_empty();
            for o in sink {
                self.accept(o);
            }
            if !ok {
                return false;
            }
            if !self.model_dirty && self.preview_dirty.is_none() && !any {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
        }
    }

    pub fn regenerating(&self) -> bool {
        self.worker.busy() || self.model_dirty
    }

    // ------------------------------------------------------------ messages

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            until: self.now + 3.0,
            warn: false,
        });
    }
    pub fn warn(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            until: self.now + 6.0,
            warn: true,
        });
    }

    // ------------------------------------------------------------ document commands

    pub fn after_edit(&mut self) {
        self.request_model();
        if self.dialog.is_some() {
            self.request_preview();
        }
    }

    /// The one undo dispatch (Ctrl+Z, the toolbar button). In sketch mode the
    /// sketch's own history is stepped and the document's (extrudes, parameters)
    /// is never touched while the sketch is open.
    pub fn undo(&mut self) {
        if let Mode::Sketch(s) = &mut self.mode {
            if s.undo() {
                self.toast("Undo: sketch edit");
            }
            return; // never the document's history while a sketch is open
        }
        if let Some(l) = self.doc.undo() {
            self.toast(format!("Undo: {l}"));
            self.params.buffers.clear();
            self.after_edit();
        }
    }
    /// Redo (Ctrl+Shift+Z, Ctrl+Y): the sketch's redo stack in sketch mode, else the document's.
    pub fn redo(&mut self) {
        if let Mode::Sketch(s) = &mut self.mode {
            if s.redo() {
                self.toast("Redo: sketch edit");
            }
            return;
        }
        if let Some(l) = self.doc.redo() {
            self.toast(format!("Redo: {l}"));
            self.params.buffers.clear();
            self.after_edit();
        }
    }

    pub fn set_param(&mut self, name: &str, expr: &str) -> Result<(), DocError> {
        let expr = crate::units::normalize_input(expr);
        adapters::set_param(&mut self.doc, name, &expr)?;
        self.after_edit();
        Ok(())
    }

    /// Set a parameter of the active component (a local one shadows the document's).
    pub fn set_local_param(&mut self, name: &str, expr: &str) -> Result<(), DocError> {
        let expr = crate::units::normalize_input(expr);
        let c = self.scope();
        self.doc.set_component_param(c, name, expr.as_str())?;
        self.after_edit();
        Ok(())
    }

    /// Insert features from a dialog (replacing the edited one, if any).
    pub fn commit_features(
        &mut self,
        label: &str,
        feats: Vec<Feature>,
        replace: Option<FeatureId>,
    ) {
        let at = self.rollback();
        let scope = self.scope();
        let added = feats.len() as isize - replace.is_some() as isize;
        let r = crate::scope::edit(&mut self.doc, scope, label, |d| {
            // the marker moves with what was inserted before it (root or component)
            if let Some(r) = d.rollback {
                d.rollback = Some((r as isize + added).max(0) as usize);
            }
            let pos = match replace {
                Some(id) => d
                    .features
                    .iter()
                    .position(|f| f.id == id)
                    .ok_or_else(|| DocError::Other("feature vanished".into()))?,
                None => at.unwrap_or(d.features.len()).min(d.features.len()),
            };
            if replace.is_some() {
                d.features.remove(pos);
            }
            for (k, mut f) in feats.into_iter().enumerate() {
                // the edited feature keeps its id (bodies keep theirs); new ones are allocated
                // in order, matching the ids the preview used (`BuildCx::next_id`)
                if replace != Some(f.id) {
                    f.id = FeatureId(d.next_id);
                    d.next_id += 1;
                }
                d.features.insert(pos + k, f);
            }
            Ok::<_, DocError>(())
        });
        match r {
            Ok(()) => {
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }

    pub fn open_dialog(&mut self, kind: DialogKind) {
        let cx = self.dialog_cx_data();
        match Dialog::new(kind, &cx, self.model.as_deref(), &self.selection) {
            Ok(mut d) => {
                if let (crate::dialogs::Body::Extrude(e), Some((Some(sk), body))) =
                    (&mut d.body, self.sketch_body)
                {
                    let alive = self.model.as_ref().is_some_and(|m| m.meshed(body).is_some());
                    if e.target.is_none() && e.sketch == Some(sk) && alive {
                        e.target = Some(body);
                    }
                }
                if let Some(f) = d.filter() {
                    self.filter = f;
                }
                self.dialog = Some(d);
                self.preview = None;
                self.browser_open = false;
                self.request_model();
                self.request_preview();
            }
            Err(e) => self.warn(e),
        }
    }

    pub fn edit_feature(&mut self, id: FeatureId) {
        let Some(f) = self.view().feature(id).cloned() else {
            return;
        };
        if let Some(sk) = crate::dialogs::sketch_of(&f) {
            self.open_sketch(Some(sk));
            return;
        }
        let cx = self.dialog_cx_data();
        match Dialog::edit(&f, &cx) {
            Some(d) => {
                if let Some(fl) = d.filter() {
                    self.filter = fl;
                }
                self.dialog = Some(d);
                self.preview = None;
                self.browser_open = false;
                self.request_model();
                self.request_preview();
            }
            None => self.toast(format!("{} has no editor yet", f.kind.label())),
        }
    }

    pub fn close_dialog(&mut self, ok: bool) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut reopen = false;
        if ok {
            // a fillet whose report still has failing edges: say so once
            if let crate::dialogs::Body::Fillet(f) = &mut d.body {
                let (_, _, failing, _) = f.totals();
                if failing > 0 && !f.keep_failing {
                    f.keep_failing = true;
                    d.error = Some(format!(
                        "{failing} edge{} still won't take this size. Use the largest size or skip {}; OK again keeps the feature failing.",
                        if failing == 1 { "" } else { "s" },
                        if failing == 1 { "it" } else { "them" },
                    ));
                    self.dialog = Some(d);
                    return;
                }
            }
            let cx = self.dialog_cx_data();
            match d.build(&cx) {
                Ok(feats) if !feats.is_empty() => {
                    let label = d.title_case();
                    let before: HashSet<FeatureId> = crate::scope::features(&self.doc.data, self.scope()).iter().map(|f| f.id).collect();
                    reopen = d.kind == DialogKind::Construct && d.editing().is_none();
                    self.commit_features(&label, feats, d.editing());
                    if d.as_components == Some(true) {
                        // the split just committed: a new one, or the Split a joint replaced
                        let fid = crate::scope::features(&self.doc.data, self.scope())
                            .iter()
                            .find(|f| (!before.contains(&f.id) || d.editing() == Some(f.id)) && matches!(f.kind, easycad_model::feature::FeatureKind::Split { .. } | easycad_model::feature::FeatureKind::PartingJoint { .. }))
                            .map(|f| f.id);
                        if let Some(fid) = fid {
                            self.output_as_components(fid);
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    self.warn(e);
                    self.dialog = Some(d);
                    return;
                }
            }
        }
        self.preview = None;
        self.browser_open = true;
        self.worker.cancel(Purpose::Preview);
        self.filter = Filter::default();
        self.request_model();
        if reopen {
            // the Construct tool stays active for the next one
            self.selection.clear();
            self.open_dialog(DialogKind::Construct);
        }
    }

    pub fn open_sketch(&mut self, id: Option<easycad_model::feature::SketchId>) {
        let s = match id {
            Some(id) => self
                .view()
                .sketch(id)
                .cloned()
                .map(|s| SketchSession::edit(id, s, self.view())),
            // a new sketch first asks which plane or flat face it goes on
            None => {
                self.dialog = None;
                self.plane_pick = Some(crate::plane_pick::PlanePick::default());
                None
            }
        };
        if let Some(s) = s {
            self.dialog = None;
            self.mode = Mode::Sketch(Box::new(s));
        }
    }

    /// Open a new sketch on `plane` (on a face of `body`, if any).
    pub fn start_sketch_on(&mut self, plane: easycad_model::geom::Plane, body: Option<BodyId>) {
        self.plane_pick = None;
        self.sketch_body = body.map(|b| (None, b));
        let s = SketchSession::new_on(plane, self.view());
        self.dialog = None;
        self.mode = Mode::Sketch(Box::new(s));
    }

    pub fn start_sketch_on_target(&mut self, t: &crate::plane_pick::Target) {
        if let Some((pl, b)) = crate::plane_pick::resolve(self, t) {
            self.start_sketch_on(pl, b);
            // a sketch on a construction plane follows it when it moves
            if let (crate::plane_pick::Target::Datum(f), Mode::Sketch(s)) = (t, &mut self.mode) {
                s.sketch.plane_src = Some(*f);
            }
        }
    }

    /// Sketch on the selected planar face (else XY) without asking.
    pub fn open_sketch_on_selection(&mut self) {
        self.sketch_body = self
            .selection
            .iter()
            .find(|k| matches!(k.kind, crate::pickstack::PickKind::Face(_)))
            .map(|k| (None, k.body));
        let plane = self.sketch_plane_from_selection();
        self.start_sketch_on(plane, self.sketch_body.map(|(_, b)| b));
    }

    /// A planar face in the selection becomes the sketch plane (else XY).
    fn sketch_plane_from_selection(&self) -> easycad_model::geom::Plane {
        use crate::pickstack::PickKind;
        let m = self.model.as_deref();
        for k in &self.selection {
            if let (PickKind::Face(f), Some(m)) = (k.kind, m) {
                if let Some(info) = m.meshed(k.body).and_then(|b| b.faces.get(f as usize)) {
                    if info.kind == easycad_model::occt::SurfaceKind::Plane {
                        let n = info.normal;
                        let x = if n.z.abs() > 0.9 {
                            glam::DVec3::X
                        } else {
                            glam::DVec3::Z.cross(n)
                        };
                        let origin = n * info.point.dot(n);
                        return easycad_model::geom::Plane::new(origin, x, n);
                    }
                }
            }
        }
        easycad_model::geom::Plane::XY
    }

    pub fn finish_sketch(&mut self) {
        let mode = std::mem::replace(&mut self.mode, Mode::Model);
        if let Mode::Sketch(s) = mode {
            let scope = self.scope();
            match s.commit(&mut self.doc, scope) {
                Ok(Some(label)) => {
                    // the face the sketch was placed on has been used up; its body
                    // stays the default target of an extrude on this sketch
                    self.selection.clear();
                    if s.id.is_none() {
                        let new = crate::scope::view(&self.doc.data, scope).sketches.last().map(|s| s.id);
                        self.sketch_body = self.sketch_body.map(|(_, b)| (new, b));
                    }
                    self.toast(label)
                }
                Ok(None) => {}
                Err(e) => self.warn(e),
            }
            self.request_model();
        }
    }

    pub fn cancel_sketch(&mut self) {
        self.mode = Mode::Model;
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.data.revision != self.saved_revision
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => format!("{}.ecad", self.doc.data.name),
        }
    }

    pub fn feature_status(&self, id: FeatureId) -> Option<&FeatureStatus> {
        self.model.as_ref()?.result.report(id).map(|r| &r.status)
    }

    pub fn body_visible(&self, id: BodyId) -> bool {
        !self.hidden.contains(&id)
    }

    // ------------------------------------------------------------ files

    /// Ask for a name and a process profile (defaults to the last one used).
    pub fn new_doc(&mut self) {
        let st = crate::settings::Settings::load();
        let last = st
            .default_profile
            .or(st.last_profile)
            .unwrap_or_else(|| Document::new("x").data.active_profile);
        self.new_doc_prompt = Some(("Untitled".into(), last));
    }

    pub fn create_doc(&mut self, name: &str, profile: &str) {
        let d = Document::new_with_profile(name, profile);
        let mut s = crate::settings::Settings::load();
        s.last_profile = Some(profile.to_string());
        s.save();
        self.load(d, None);
    }

    pub fn load(&mut self, mut doc: Document, path: Option<PathBuf>) {
        adapters::migrate_bindings(&mut doc.data);
        adapters::migrate_anchors(&mut doc.data);
        self.saved_revision = doc.data.revision;
        self.doc = doc;
        self.path = path;
        self.selection.clear();
        self.hidden.clear();
        self.dialog = None;
        self.mode = Mode::Model;
        self.model = None;
        self.preview = None;
        self.params = ParamsUi::default();
        self.fit_pending = true;
        self.active = Vec::new();
        self.explode = 0.0;
        self.section = None;
        self.asm = Default::default();
        self.request_model();
    }

    /// Ask for a path (system dialog, else the in-app prompt), then `then` it.
    pub fn ask_path(
        &mut self,
        req: crate::files::Request,
        then: impl FnOnce(&mut EasyCadApp, PathBuf) + 'static,
    ) {
        use crate::files::Pick;
        match crate::files::native(&req) {
            Pick::Path(p) => then(self, p),
            Pick::Cancelled => {}
            Pick::Unavailable => {
                self.path_prompt = Some((crate::files::PathPrompt::new(req), Box::new(then)));
            }
        }
    }

    fn path_prompt_ui(&mut self, ctx: &egui::Context) {
        use crate::files::PromptEvent;
        if let Some((mut p, then)) = self.path_prompt.take() {
            match p.ui(ctx) {
                PromptEvent::Chosen(path) => then(self, path),
                PromptEvent::Cancel => {}
                PromptEvent::None => self.path_prompt = Some((p, then)),
            }
        }
    }

    pub fn open_dialog_file(&mut self) {
        let req = crate::files::Request::folder("Open a .ecad document folder");
        self.ask_path(req, |app, dir| {
            if let Err(e) = app.open_path(&dir) {
                app.warn(format!("Open failed: {e}"));
            }
        });
    }

    pub fn save(&mut self, save_as: bool) {
        let save = |app: &mut EasyCadApp, p: PathBuf| match app.save_to(p) {
            Ok(p) => app.toast(format!("Saved {}", p.display())),
            Err(e) => app.warn(format!("Save failed: {e}")),
        };
        match (&self.path, save_as) {
            (Some(p), false) => save(self, p.clone()),
            _ => {
                let name = format!("{}.ecad", self.doc.data.name);
                let mut req = crate::files::Request::save("Save document", "easyCAD", "ecad", &name);
                req.dir = Some(self.save_dir());
                self.ask_path(req, save);
            }
        }
    }

    /// Where Save offers to put the document: the folder it is already in,
    /// else the synced easyCAD Documents folder (created on demand).
    pub fn save_dir(&self) -> PathBuf {
        if let Some(d) = self.path.as_ref().and_then(|p| p.parent()).filter(|d| d.is_dir()) {
            return d.to_path_buf();
        }
        let docs = crate::cloud::documents_dir();
        let _ = std::fs::create_dir_all(&docs);
        docs
    }

    /// Save to a `.ecad` folder (the path the Save dialogs use).
    pub fn save_to(&mut self, mut p: PathBuf) -> Result<PathBuf, String> {
        // `.cwk` is the pre-rename extension: keep saving those folders in place.
        if p.extension().is_none_or(|e| e != "ecad" && e != "cwk") {
            p.set_extension("ecad");
        }
        self.doc.save_to_dir(&p).map_err(|e| e.to_string())?;
        self.saved_revision = self.doc.data.revision;
        crate::recovery::remove(&self.doc.data.doc_id);
        self.autosave.saved_rev = Some(self.doc.data.revision);
        self.path = Some(p.clone());
        crate::settings::Settings::push_recent(&p);
        self.cloud.sync_now();
        Ok(p)
    }

    /// Open a `.ecad` folder (the path the Open dialog uses).
    pub fn open_path(&mut self, dir: &std::path::Path) -> Result<(), String> {
        let d = Document::load_from_dir(dir).map_err(|e| e.to_string())?;
        self.load(d, Some(dir.to_path_buf()));
        crate::settings::Settings::push_recent(dir);
        Ok(())
    }

    pub fn import_file(&mut self) {
        let req = crate::files::Request::open("Import STEP or DXF", "STEP / DXF", &["step", "stp", "dxf"]);
        self.ask_path(req, |app, p| match app.import_path(&p) {
            Ok(msg) => app.toast(msg),
            Err(e) => app.warn(e),
        });
    }

    /// Import a STEP (locked reference body) or a DXF (opens sketch mode with its
    /// layers). Shared by the Import dialog and the smoke test.
    pub fn import_path(&mut self, p: &std::path::Path) -> Result<String, String> {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(p).map_err(|e| format!("Import failed: {e}"))?;
        if ext == "dxf" {
            let imp = self.doc.import_dxf(&name, bytes);
            let d = self.doc.dxf(imp).map_err(|e| format!("DXF: {e}"))?;
            self.dialog = None;
            self.mode = Mode::Sketch(Box::new(SketchSession::from_dxf(
                imp,
                &name,
                d,
                &self.doc.data,
            )));
            Ok(format!("Imported {name}: pick layers, then Finish sketch"))
        } else {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(name);
            if crate::ui_assembly::is_assembly(&self.doc.data) {
                let parent = self.scope();
                self.doc
                    .import_step_assembly(&stem, bytes, parent, true)
                    .map_err(|e| format!("Import failed: {e}"))?;
                self.fit_pending = true;
                self.request_model();
                return Ok(format!("Imported {stem} as a locked reference component"));
            }
            self.doc.import_step(&stem, bytes, true);
            self.fit_pending = true;
            self.request_model();
            Ok(format!("Imported {stem} as a locked reference body"))
        }
    }

    // ------------------------------------------------------------ frame

    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let t0 = std::time::Instant::now();
        crate::profiler::frame_start();
        self.now = ui.input(|i| i.time);
        if ui.input(|i| i.key_pressed(egui::Key::F12)) {
            self.show_perf = !self.show_perf;
        }
        self.bench_pan(ui.ctx());
        crate::viewport::gpu::set_hidden(!crate::widgets::window_visible(ui.ctx()));
        self.poll_worker();
        for n in self.cloud.poll(self.path.as_deref()) {
            match n {
                crate::cloud::Notice::Info(t) => self.toast(t),
                crate::cloud::Notice::Warn(t) => self.warn(t),
            }
        }
        if self.camera.tick(self.now) {
            ui.ctx().request_repaint();
        }
        self.shortcuts(ui.ctx());
        self.reload_tick(ui.ctx());
        if self.model_dirty {
            self.submit_model();
        }
        if let Some(t) = self.preview_dirty {
            if self.now >= t {
                self.submit_preview();
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(t - self.now));
            }
        }
        if let Some((name, expr, t)) = self.params.pending.clone() {
            if self.now >= t {
                self.params.pending = None;
                let local = self
                    .doc
                    .data
                    .component(self.scope())
                    .is_some_and(|c| c.params.get(&name).is_some());
                let r = if local {
                    self.set_local_param(&name, &expr)
                } else {
                    self.set_param(&name, &expr)
                };
                if let Err(e) = r {
                    self.params.error = Some((name, e.to_string()));
                } else {
                    self.params.error = None;
                }
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(t - self.now));
            }
        }
        if let Some((p, t)) = self.pending_profile.clone() {
            if self.now >= t {
                self.pending_profile = None;
                let label = format!("Fit allowances ({})", p.name);
                self.doc.update_profile(&label, p);
                self.after_edit();
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(t - self.now));
            }
        }
        self.toasts.retain(|t| t.until > self.now);
        if !self.toasts.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        self.refresh_cache();
        self.autosave_tick(ui.ctx());
        crate::ui_main::show(self, ui);
        crate::ui_settings::windows(self, ui);
        self.path_prompt_ui(ui.ctx());
        self.frame_ms = t0.elapsed().as_secs_f32() * 1000.0;
        if let Some(mut d) = self.state_dump.take() {
            d.write(self);
            self.state_dump = Some(d);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Only a text field holds the keyboard. A button or checkbox that took focus
        // (Tab out of a field) must not swallow Enter / Esc / shortcuts: Enter there
        // "clicked" e.g. "+ New edge set" instead of the dialog's OK.
        let typing = ctx.text_edit_focused();
        if !typing {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        self.typing = typing;
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let pressed_m = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        // modal windows own the keyboard
        if self.wizard.is_some() || self.path_prompt.is_some() || !self.recovery_offer.is_empty() {
            return;
        }
        if pressed_m(Modifiers::COMMAND, Key::Comma) {
            self.show_settings = !self.show_settings;
        }
        if !typing {
            // `?` is Shift+/ on most layouts: match the key and the text event
            let q = ctx.input_mut(|i| {
                let k = i.consume_key(Modifiers::SHIFT, Key::Questionmark)
                    || i.consume_key(Modifiers::NONE, Key::Questionmark)
                    || i.consume_key(Modifiers::SHIFT, Key::Slash);
                let t = i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?"));
                i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "?"));
                k || t
            });
            if q {
                self.show_keys = !self.show_keys;
            }
            if self.show_keys && pressed(Key::Escape) {
                self.show_keys = false;
            }
            if self.show_settings && pressed(Key::Escape) {
                self.show_settings = false;
            }
        }
        // Chords are matched on each key event's own modifiers: a fast Ctrl+Z (or one
        // typed during a slow frame) has Ctrl released again by the end of the frame.
        // In a text field Ctrl+Z / Ctrl+Shift+Z stay the field's own undo.
        if !typing {
            if pressed_m(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
                || pressed_m(Modifiers::COMMAND, Key::Y)
            {
                self.redo();
            } else if pressed_m(Modifiers::COMMAND, Key::Z) {
                self.undo();
            }
        }
        if pressed_m(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
            self.save(true);
        } else if pressed_m(Modifiers::COMMAND, Key::S) {
            self.save(false);
        }
        if pressed_m(Modifiers::COMMAND, Key::O) {
            self.open_dialog_file();
        }
        if pressed_m(Modifiers::COMMAND, Key::N) {
            self.new_doc();
        }
        if pressed_m(Modifiers::COMMAND, Key::I) {
            self.import_file();
        }
        if typing {
            return;
        }
        match &self.mode {
            Mode::Sketch(_) | Mode::Drawing(_) => return,
            Mode::Parts(_) => {
                if pressed(Key::Escape) {
                    self.mode = Mode::Model;
                }
                return;
            }
            Mode::Model => {}
        }
        if self.plane_pick.is_some() && pressed(Key::Escape) {
            self.plane_pick = None;
            return;
        }
        if self.asm.mate.is_some() {
            if pressed(Key::Escape) {
                self.cancel_mate();
            } else if pressed(Key::Enter) {
                self.commit_mate();
            }
            if pressed(Key::F) {
                self.fit_all();
            }
            return;
        }
        if self.dialog.as_ref().is_some_and(|d| d.kind == DialogKind::Construct) {
            // the Construct popup owns Enter, Esc and Tab
            if pressed(Key::F) {
                self.fit_all();
            }
            return;
        }
        if self.dialog.is_some() {
            // Esc during an arrow drag cancels the drag (the viewport takes the key), not the dialog
            if self.arrow_drag.is_none() && pressed(Key::Escape) {
                self.close_dialog(false);
            } else if pressed(Key::Enter) {
                self.close_dialog(true);
            }
            if pressed(Key::F) {
                self.fit_all();
            }
            return;
        }
        if self.delete_ask.is_some() {
            return; // the confirm popup owns the keyboard
        }
        if !self.hist_sel.is_empty() && (pressed(Key::Delete) || pressed(Key::Backspace)) {
            self.hist_delete_selected();
            return;
        }
        if !self.hist_sel.is_empty() && pressed(Key::Escape) {
            self.hist_sel.clear();
            return;
        }
        if pressed(Key::Escape) {
            if self.asm.gizmo {
                self.asm.gizmo = false;
            } else if self.selection.is_empty() && !self.active.is_empty() {
                self.activate_parent();
            } else {
                self.selection.clear();
                self.asm.sel = None;
            }
        }
        if pressed(Key::F) {
            if self
                .selection
                .iter()
                .any(|k| !matches!(k.kind, crate::pickstack::PickKind::Body))
                && !self.selection.is_empty()
            {
                self.open_dialog(DialogKind::Fillet);
            } else {
                self.fit_all();
            }
        }
        let alt = Modifiers::ALT;
        let steps = [
            (Key::ArrowUp, crate::camera::StepAxis::X, 1.0),
            (Key::ArrowDown, crate::camera::StepAxis::X, -1.0),
            (Key::PageUp, crate::camera::StepAxis::Y, 1.0),
            (Key::PageDown, crate::camera::StepAxis::Y, -1.0),
            (Key::ArrowLeft, crate::camera::StepAxis::Z, -1.0),
            (Key::ArrowRight, crate::camera::StepAxis::Z, 1.0),
        ];
        for (k, axis, s) in steps {
            if pressed_m(alt, k) {
                self.camera.go_step(axis, s, self.now);
            }
        }
        if pressed(Key::Home) {
            let a = self.viewport_size.x / self.viewport_size.y.max(1.0);
            self.camera.go_home(self.now, a);
        }
        let keymap: [(Key, Option<DialogKind>); 10] = [
            (Key::Q, Some(DialogKind::Construct)),
            (Key::E, Some(DialogKind::Extrude)),
            (Key::H, Some(DialogKind::Shell)),
            (Key::B, Some(DialogKind::Boss)),
            (Key::J, Some(DialogKind::Joint)),
            (Key::R, Some(DialogKind::Revolve)),
            (Key::M, Some(DialogKind::Mirror)),
            (Key::S, None),
            (Key::P, None),
            (Key::D, None),
        ];
        for (k, d) in keymap {
            if pressed(k) {
                match (k, d) {
                    (_, Some(d)) => self.open_dialog(d),
                    (Key::S, None) => self.open_sketch(None),
                    (Key::P, None) => {
                        self.mode = Mode::Parts(Box::new(crate::ui_parts::PartsUi::new(&self.doc)))
                    }
                    (Key::D, None) => self.mode = Mode::Drawing(Box::default()),
                    _ => {}
                }
            }
        }
        if crate::ui_assembly::is_assembly(&self.doc.data) {
            if pressed(Key::A) {
                self.open_mate(None);
            }
            if pressed(Key::G) {
                self.toggle_gizmo();
            }
            if pressed(Key::X) {
                // X explodes to 60 % (the slider fine-tunes); X again puts it back
                self.asm.explode_open = !self.asm.explode_open;
                self.explode = if self.asm.explode_open { 0.6 } else { 0.0 };
                self.request_model();
            }
            if pressed(Key::I) {
                self.check_interference();
            }
        }
        if pressed(Key::Num1) {
            self.display = DisplayMode::Shaded;
        }
        if pressed(Key::Num2) {
            self.display = DisplayMode::ShadedEdges;
        }
        if pressed(Key::Num3) {
            self.display = DisplayMode::Wireframe;
        }
    }

    /// Periodic recovery copy of unsaved work (off the UI thread).
    fn autosave_tick(&mut self, ctx: &egui::Context) {
        let interval = self.settings.autosave_min as f64 * 60.0;
        let interval = std::env::var("EASYCAD_AUTOSAVE_SECS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(interval);
        let dirty = self.is_dirty() || (self.path.is_none() && !crate::ui_welcome::is_empty(self));
        if !dirty {
            self.autosave.saved_rev = Some(self.doc.data.revision);
        }
        let path = self.path.clone();
        self.autosave
            .tick(self.now, interval, &self.doc, dirty, path.as_deref());
        if interval > 0.0 && dirty {
            ctx.request_repaint_after(Duration::from_secs_f64(interval.clamp(0.5, 60.0)));
        }
    }

    /// `EASYCAD_BENCH_PAN=<seconds>`: once the model is shown, pan back and forth
    /// every frame for that long, print the profiler's last second, and quit.
    fn bench_pan(&mut self, ctx: &egui::Context) {
        let Some(secs) = std::env::var("EASYCAD_BENCH_PAN").ok().and_then(|s| s.parse::<f64>().ok()) else { return };
        if self.model.is_none() || self.regenerating() {
            ctx.request_repaint();
            return;
        }
        let start = *self.bench_start.get_or_insert(self.now);
        let t = self.now - start;
        if t > secs {
            println!("{}", crate::profiler::report());
            self.quit = true;
            return;
        }
        let d = if (t * 2.0) as i64 % 2 == 0 { 4.0 } else { -4.0 };
        self.camera.pan(glam::Vec2::new(d, d * 0.5), self.viewport_size.y.max(1.0));
        ctx.request_repaint();
    }

    /// Clean exit: unsaved work keeps a last recovery copy, saved work leaves none.
    pub fn on_close(&mut self) {
        self.autosave.flush();
        let dirty = self.is_dirty() || (self.path.is_none() && !crate::ui_welcome::is_empty(self));
        if dirty && self.settings.autosave_min > 0 {
            let _ = crate::recovery::write(&self.doc, self.path.as_deref());
        } else {
            crate::recovery::remove(&self.doc.data.doc_id);
        }
    }

    /// The modal-ish window on top, if any (for the E2E state dump).
    pub fn overlay_name(&self) -> Option<&'static str> {
        if !self.recovery_offer.is_empty() {
            Some("recovery")
        } else if self.show_settings {
            Some("settings")
        } else if self.show_keys {
            Some("keys")
        } else if self.export.is_some() {
            Some("export")
        } else if self.new_doc_prompt.is_some() {
            Some("new_doc")
        } else if self.wizard.is_some() {
            Some("wizard")
        } else if self.calibrate.is_some() {
            Some("calibrate")
        } else {
            None
        }
    }

    /// G: the move gizmo on the selected occurrence (explains when it cannot move).
    pub fn toggle_gizmo(&mut self) {
        if self.asm.gizmo {
            self.asm.gizmo = false;
            return;
        }
        let Some(p) = self.asm.sel.clone() else {
            self.toast("Select a component to move (click it or its browser row), then G");
            return;
        };
        match crate::ui_gizmo::blocked(self, &p) {
            Some(why) => self.warn(why),
            None => self.asm.gizmo = true,
        }
    }

    pub fn fit_all(&mut self) {
        if let Some(bb) = self.model.as_ref().and_then(|m| m.bbox) {
            let aspect = self.viewport_size.x / self.viewport_size.y.max(1.0);
            let mut c = self.camera.clone();
            c.fit_sphere(
                bb.center().as_vec3(),
                (bb.size().length() * 0.5) as f32,
                aspect,
            );
            let p = c.pose();
            self.camera.scene_center = c.scene_center;
            self.camera.scene_radius = c.scene_radius;
            self.camera.animate_to(p, self.now);
        }
    }
}

impl EasyCadApp {
    /// Tab over the viewport cycles the pick stack. egui's own focus navigation
    /// would otherwise take the key first (moving focus into a parameter field),
    /// so presses are removed from the raw input before egui sees them.
    pub fn intercept_keys(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if !self.mode.is_model()
            || self.hover_pos.is_none()
            || self.stack.is_empty()
            || ctx.text_edit_focused()
        {
            return;
        }
        let mut steps = 0;
        raw.events.retain(|e| match e {
            egui::Event::Key {
                key: Key::Tab,
                pressed,
                modifiers,
                ..
            } if !modifiers.ctrl && !modifiers.alt && !modifiers.command => {
                if *pressed {
                    steps += if modifiers.shift { -1 } else { 1 };
                }
                false
            }
            _ => true,
        });
        self.pending_cycle += steps;
    }
}

impl eframe::App for EasyCadApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::env::var_os("EASYCAD_DEBUG_KEYS").is_some() {
            for e in &raw_input.events {
                if matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)) {
                    eprintln!("key event: {e:?}");
                }
            }
        }
        self.intercept_keys(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if std::env::var_os("EASYCAD_DEBUG_REPAINT").is_some() {
            debug_repaint(ui.ctx());
        }
        self.frame(ui);
        crate::profiler::app_ms(self.frame_ms, frame.info().cpu_usage.map(|s| s * 1000.0));
        if self.show_perf {
            let area = if self.viewport_rect.is_positive() { self.viewport_rect } else { ui.ctx().content_rect() };
            crate::profiler::ui(ui.ctx(), area);
        }
        if self.quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        self.on_close();
    }
}

/// `EASYCAD_DEBUG_REPAINT`: once a second, frames drawn and what asked for them.
fn debug_repaint(ctx: &egui::Context) {
    use std::sync::Mutex;
    type Second = (std::time::Instant, u32, std::collections::BTreeMap<String, u32>);
    static LOG: Mutex<Option<Second>> = Mutex::new(None);
    let mut g = LOG.lock().unwrap();
    let (t0, n, causes) = g.get_or_insert_with(|| (std::time::Instant::now(), 0, Default::default()));
    *n += 1;
    for c in ctx.repaint_causes() {
        *causes.entry(c.to_string()).or_default() += 1;
    }
    if t0.elapsed() >= std::time::Duration::from_secs(1) {
        let r = crate::viewport::gpu::RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed);
        let (focused, visible) = (ctx.input(|i| i.focused), crate::widgets::window_visible(ctx));
        eprintln!("repaint: {n} frames/s, {r} 3D renders, focused {focused}, visible {visible} {causes:?}");
        *g = None;
    }
}
