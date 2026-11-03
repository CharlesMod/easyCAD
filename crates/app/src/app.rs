//! Application state, the frame loop, regeneration plumbing, commands and
//! keyboard shortcuts. Screens live in `ui_*` modules.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use casework_model::document::{DocError, Document};
use casework_model::feature::{BodyId, Feature, FeatureId};
use casework_model::regen::FeatureStatus;
use egui::{Key, Modifiers};

use crate::adapters;
use crate::camera::Camera;
use crate::dialogs::{Dialog, DialogKind};
use crate::pickstack::{Filter, PickKey, PickStack};
use crate::sketch_mode::SketchSession;
use crate::viewport::gpu::DisplayMode;
use crate::worker::{Output, Purpose, Worker};
use casework_model::regen::MeshQuality;

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
}

pub struct Toast {
    pub text: String,
    pub until: f64,
    pub warn: bool,
}

pub struct CaseworkApp {
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
    pub rollback: Option<usize>,
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
    pub pending_profile: Option<(casework_model::process::ProcessProfile, f64)>,
    pub cache: UiCache,
    /// "New document" prompt: (name, profile).
    pub new_doc_prompt: Option<(String, String)>,
    pub show_build_plate: bool,
    /// Measured width of the floating dialog card (for right alignment).
    pub dialog_card_w: f32,
    /// A viewport handle / slider is being dragged (coarse previews).
    pub dragging: bool,
    pub now: f64,
    pub quit: bool,
    /// `CASEWORK_STATE_FILE` JSON dump for the E2E scripts.
    pub state_dump: Option<crate::e2e_state::StateDump>,
    /// Tab / Shift+Tab presses taken from the raw input for the pick stack.
    pub pending_cycle: i32,
    /// A text field had keyboard focus this frame.
    pub typing: bool,
    /// The body whose face a new sketch was placed on, and once committed the
    /// sketch's id: the default target of an Extrude / Revolve on that sketch.
    pub sketch_body: Option<(Option<casework_model::feature::SketchId>, BodyId)>,
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
}

/// What a file chooser does with the chosen path.
pub type OnPath = Box<dyn FnOnce(&mut CaseworkApp, PathBuf)>;

impl CaseworkApp {
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
        CaseworkApp {
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
            rollback: None,
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
            now: 0.0,
            quit: false,
            state_dump: crate::e2e_state::StateDump::from_env(),
            pending_cycle: 0,
            typing: false,
            sketch_body: None,
            path_prompt: None,
            wizard: None,
            settings: crate::settings::Settings::load(),
            show_settings: false,
            show_keys: false,
            autosave: crate::recovery::Autosaver::default(),
            recovery_offer: Vec::new(),
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
        self.rollback
    }

    pub fn request_model(&mut self) {
        self.model_dirty = true;
    }

    fn submit_model(&mut self) {
        let data = adapters::effective(&self.doc.data, self.active_count());
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
        });
        self.model_dirty = false;
    }

    /// Features the open dialog would add, inserted at the active position.
    pub fn preview_data(&self) -> Option<(casework_model::document::DocData, Vec<usize>)> {
        let d = self.dialog.as_ref()?;
        let mut data = adapters::effective(&self.doc.data, self.active_count());
        let feats = d.build(&self.dialog_cx_data()).ok()?;
        if feats.is_empty() {
            return None;
        }
        let base = data.features.len();
        let mut prefixes = Vec::new();
        for (i, f) in feats.into_iter().enumerate() {
            prefixes.push(base + i);
            data.features.push(f);
        }
        Some((data, prefixes))
    }

    pub fn dialog_cx_data(&self) -> crate::dialogs::BuildCx {
        crate::dialogs::BuildCx {
            next_id: self.doc.data.next_id,
            data: self.doc.data.clone(),
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
        if key == self.cache.key && !self.cache.usage.is_empty() {
            return;
        }
        let usage = adapters::usage_counts(&self.doc.data);
        let users = usage
            .keys()
            .map(|n| {
                let u = adapters::users_of(&self.doc.data, n);
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
        let chips = self
            .doc
            .data
            .features
            .iter()
            .map(|f| (f.id, crate::ui_main::chip_label(self, f)))
            .collect();
        self.cache = UiCache {
            key,
            usage,
            users,
            chips,
            hot: None,
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
                let ids: HashSet<BodyId> = out.bodies.iter().map(|b| b.body.id).collect();
                self.selection.retain(|k| ids.contains(&k.body));
                self.stack.clear();
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

    pub fn undo(&mut self) {
        if let Some(l) = self.doc.undo() {
            self.toast(format!("Undo: {l}"));
            self.params.buffers.clear();
            self.after_edit();
        }
    }
    pub fn redo(&mut self) {
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

    /// Insert features from a dialog (replacing the edited one, if any).
    pub fn commit_features(
        &mut self,
        label: &str,
        feats: Vec<Feature>,
        replace: Option<FeatureId>,
        bind: Vec<(FeatureId, String)>,
    ) {
        let at = self.rollback;
        let r = self.doc.edit(label, |d| {
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
            for (fid, expr) in &bind {
                if expr.is_empty() {
                    d.meta.remove(&adapters::bind_key(*fid));
                } else {
                    d.meta.insert(adapters::bind_key(*fid), expr.clone());
                }
            }
            adapters::apply_bindings(d);
            Ok::<_, DocError>(())
        });
        match r {
            Ok(()) => {
                if let Some(n) = self.rollback.as_mut() {
                    *n += 1;
                }
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
        let Some(f) = self.doc.data.feature(id).cloned() else {
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
                    let bind = d.bindings(&feats);
                    self.commit_features(&label, feats, d.editing(), bind);
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
    }

    pub fn open_sketch(&mut self, id: Option<casework_model::feature::SketchId>) {
        let s = match id {
            Some(id) => self
                .doc
                .data
                .sketch(id)
                .cloned()
                .map(|s| SketchSession::edit(id, s, &self.doc.data)),
            None => {
                self.sketch_body = self
                    .selection
                    .iter()
                    .find(|k| matches!(k.kind, crate::pickstack::PickKind::Face(_)))
                    .map(|k| (None, k.body));
                Some(SketchSession::new_on(
                    self.sketch_plane_from_selection(),
                    &self.doc.data,
                ))
            }
        };
        if let Some(s) = s {
            self.dialog = None;
            self.mode = Mode::Sketch(Box::new(s));
        }
    }

    /// A planar face in the selection becomes the sketch plane (else XY).
    fn sketch_plane_from_selection(&self) -> casework_model::geom::Plane {
        use crate::pickstack::PickKind;
        let m = self.model.as_deref();
        for k in &self.selection {
            if let (PickKind::Face(f), Some(m)) = (k.kind, m) {
                if let Some(info) = m.meshed(k.body).and_then(|b| b.faces.get(f as usize)) {
                    if info.kind == casework_model::occt::SurfaceKind::Plane {
                        let n = info.normal;
                        let x = if n.z.abs() > 0.9 {
                            glam::DVec3::X
                        } else {
                            glam::DVec3::Z.cross(n)
                        };
                        let origin = n * info.point.dot(n);
                        return casework_model::geom::Plane::new(origin, x, n);
                    }
                }
            }
        }
        casework_model::geom::Plane::XY
    }

    pub fn finish_sketch(&mut self) {
        let mode = std::mem::replace(&mut self.mode, Mode::Model);
        if let Mode::Sketch(s) = mode {
            match s.commit(&mut self.doc) {
                Ok(Some(label)) => {
                    // the face the sketch was placed on has been used up; its body
                    // stays the default target of an extrude on this sketch
                    self.selection.clear();
                    if s.id.is_none() {
                        let new = self.doc.data.sketches.last().map(|s| s.id);
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
            None => format!("{}.cwk", self.doc.data.name),
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
        let mut d = Document::new(name);
        if d.data.profile_named(profile).is_some() {
            d.data.active_profile = profile.to_string();
        }
        let mut s = crate::settings::Settings::load();
        s.last_profile = Some(profile.to_string());
        s.save();
        self.load(d, None);
    }

    pub fn load(&mut self, doc: Document, path: Option<PathBuf>) {
        self.saved_revision = doc.data.revision;
        self.doc = doc;
        self.path = path;
        self.selection.clear();
        self.hidden.clear();
        self.rollback = None;
        self.dialog = None;
        self.mode = Mode::Model;
        self.model = None;
        self.preview = None;
        self.params = ParamsUi::default();
        self.fit_pending = true;
        self.request_model();
    }

    /// Ask for a path (system dialog, else the in-app prompt), then `then` it.
    pub fn ask_path(
        &mut self,
        req: crate::files::Request,
        then: impl FnOnce(&mut CaseworkApp, PathBuf) + 'static,
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
        let req = crate::files::Request::folder("Open a .cwk document folder");
        self.ask_path(req, |app, dir| {
            if let Err(e) = app.open_path(&dir) {
                app.warn(format!("Open failed: {e}"));
            }
        });
    }

    pub fn save(&mut self, save_as: bool) {
        let save = |app: &mut CaseworkApp, p: PathBuf| match app.save_to(p) {
            Ok(p) => app.toast(format!("Saved {}", p.display())),
            Err(e) => app.warn(format!("Save failed: {e}")),
        };
        match (&self.path, save_as) {
            (Some(p), false) => save(self, p.clone()),
            _ => {
                let name = format!("{}.cwk", self.doc.data.name);
                let req = crate::files::Request::save("Save document", "Casework", "cwk", &name);
                self.ask_path(req, save);
            }
        }
    }

    /// Save to a `.cwk` folder (the path the Save dialogs use).
    pub fn save_to(&mut self, mut p: PathBuf) -> Result<PathBuf, String> {
        if p.extension().is_none_or(|e| e != "cwk") {
            p.set_extension("cwk");
        }
        adapters::apply_bindings(&mut self.doc.data);
        self.doc.save_to_dir(&p).map_err(|e| e.to_string())?;
        self.saved_revision = self.doc.data.revision;
        crate::recovery::remove(&self.doc.data.doc_id);
        self.autosave.saved_rev = Some(self.doc.data.revision);
        self.path = Some(p.clone());
        crate::settings::Settings::push_recent(&p);
        Ok(p)
    }

    /// Open a `.cwk` folder (the path the Open dialog uses).
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
            self.doc.import_step(&stem, bytes, true);
            self.fit_pending = true;
            self.request_model();
            Ok(format!("Imported {stem} as a locked reference body"))
        }
    }

    // ------------------------------------------------------------ frame

    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let t0 = std::time::Instant::now();
        self.now = ui.input(|i| i.time);
        self.poll_worker();
        if self.camera.tick(self.now) {
            ui.ctx().request_repaint();
        }
        self.shortcuts(ui.ctx());
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
                if let Err(e) = self.set_param(&name, &expr) {
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
                adapters::update_profile(&mut self.doc, &label, p);
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
        if self.dialog.is_some() {
            if pressed(Key::Escape) {
                self.close_dialog(false);
            } else if pressed(Key::Enter) {
                self.close_dialog(true);
            }
            if pressed(Key::F) {
                self.fit_all();
            }
            return;
        }
        if pressed(Key::Escape) {
            self.selection.clear();
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
        let keymap: [(Key, Option<DialogKind>); 9] = [
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
        let interval = std::env::var("CASEWORK_AUTOSAVE_SECS")
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

impl CaseworkApp {
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

impl eframe::App for CaseworkApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::env::var_os("CASEWORK_DEBUG_KEYS").is_some() {
            for e in &raw_input.events {
                if matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)) {
                    eprintln!("key event: {e:?}");
                }
            }
        }
        self.intercept_keys(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
        if self.quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        self.on_close();
    }
}
