//! Feature dialogs: they float over the viewport (top-right), preview live through
//! the worker, bind every dimension to an expression, Enter = OK, Esc = cancel.

pub mod datum;
pub mod export;
pub mod fillet;
pub mod joint;
pub mod standoffs;

use easycad_model::document::DocData;
use easycad_model::feature::*;
use easycad_model::geom::{Axis, Plane};
use easycad_model::params::Expr;
use easycad_model::process::{Fit, FitClass};
use easycad_model::select::FaceSelector;
use egui::{Color32, RichText, Ui};
use glam::{DVec2, DVec3};

use crate::pickstack::{Filter, PickKey, PickKind};
use crate::theme;
use crate::widgets::{self, expr_field, ExprEnv};
use crate::worker::Output;

/// What builders need from the document.
pub struct BuildCx {
    pub next_id: u32,
    pub data: DocData,
}

impl BuildCx {
    pub fn id(&self, editing: Option<FeatureId>, i: usize) -> FeatureId {
        match (editing, i) {
            (Some(id), 0) => id,
            (Some(_), i) => FeatureId(self.next_id + i as u32 - 1),
            (None, i) => FeatureId(self.next_id + i as u32),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Extrude,
    Revolve,
    Fillet,
    Chamfer,
    Shell,
    DeleteFace,
    Draft,
    Split,
    Pattern,
    Mirror,
    Boss,
    Rib,
    Standoffs,
    Joint,
    /// Construction plane / axis / point.
    Datum(crate::datum_ui::DatumKind),
    /// The one inferred Construct tool.
    Construct,
    Combine,
    MoveBody,
    ImportMesh,
}

impl DialogKind {
    pub fn title(self) -> &'static str {
        match self {
            DialogKind::Extrude => "Extrude",
            DialogKind::Revolve => "Revolve",
            DialogKind::Fillet => "Fillet",
            DialogKind::Chamfer => "Chamfer",
            DialogKind::Shell => "Shell",
            DialogKind::DeleteFace => "Delete face",
            DialogKind::Draft => "Draft",
            DialogKind::Split => "Split body",
            DialogKind::Pattern => "Pattern",
            DialogKind::Mirror => "Mirror",
            DialogKind::Boss => "Screw boss",
            DialogKind::Rib => "Rib",
            DialogKind::Standoffs => "Standoffs from PCB",
            DialogKind::Joint => "Parting joint",
            DialogKind::Datum(k) => k.title(),
            DialogKind::Construct => "Construct",
            DialogKind::Combine => "Combine",
            DialogKind::MoveBody => "Move / copy body",
            DialogKind::ImportMesh => "Mesh import",
        }
    }
    /// Title-bar accent: modify = orange, enclosure = green, create = blue.
    pub fn accent(self) -> Color32 {
        match self {
            DialogKind::Fillet
            | DialogKind::Chamfer
            | DialogKind::Shell
            | DialogKind::DeleteFace
            | DialogKind::Draft
            | DialogKind::Split
            | DialogKind::Combine
            | DialogKind::MoveBody => theme::ORANGE_SOFT,
            DialogKind::Boss | DialogKind::Rib | DialogKind::Standoffs | DialogKind::Joint => {
                theme::GREEN
            }
            DialogKind::Datum(_) | DialogKind::Construct => theme::hex(0xe6c85a),
            _ => theme::ACCENT_LINK,
        }
    }
}

/// An expression input with an optional process fit.
#[derive(Clone, Debug)]
pub struct Field {
    pub text: String,
    pub fit: Option<Fit>,
    /// Take the focus with the text selected on the next draw (after an arrow drag).
    pub select: bool,
}

impl Field {
    pub fn new(s: impl Into<String>) -> Self {
        Field {
            text: s.into(),
            fit: None,
            select: false,
        }
    }
    pub fn with_fit(s: impl Into<String>, fit: Fit) -> Self {
        Field {
            text: s.into(),
            fit: Some(fit),
            select: false,
        }
    }
    pub fn from_expr(e: &Expr, default_fit: Option<Fit>) -> Self {
        Field {
            text: e.0.clone(),
            fit: e.1.or(default_fit),
            select: false,
        }
    }
    /// Expression for the feature. A fit equal to the engine default is not stored.
    pub fn expr(&self, default_fit: Option<Fit>) -> Expr {
        let e = Expr::new(crate::units::normalize_input(&self.text));
        match self.fit {
            Some(f) if Some(f) != default_fit => e.fit(f),
            _ => e,
        }
    }
    /// Labelled row: `label | [expr] [fit chip]`. Returns true when changed.
    pub fn row(&mut self, ui: &mut Ui, id: egui::Id, label: &str, env: &ExprEnv) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(12.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let w = if self.fit.is_some() { 96.0 } else { 132.0 };
                // right-to-left: lay out the chip column first by drawing the field in a sub-ui
                ui.allocate_ui_with_layout(
                    egui::vec2(w + if self.fit.is_some() { 82.0 } else { 0.0 }, 28.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if self.select {
                            ui.memory_mut(|m| m.request_focus(id));
                        }
                        let o = expr_field(ui, id, &mut self.text, w, env, self.fit.as_mut());
                        changed = o.changed || o.fit_changed;
                        if self.select {
                            self.select = false;
                            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                                let n = self.text.chars().count();
                                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    egui::text::CCursor::new(n),
                                )));
                                st.store(ui.ctx(), id);
                            }
                        }
                    },
                );
            });
        });
        changed
    }
}

/// The engine's default fit for a field of a feature built with `kind`
/// (`FeatureKind::default_fit`; field values do not matter for the defaults).
pub fn default_fit(kind: &FeatureKind, field: &str) -> Option<Fit> {
    kind.default_fit(field)
}

/// A boss template for `default_fit` before the real feature is built.
pub fn boss_template() -> FeatureKind {
    FeatureKind::ScrewBoss {
        body: None,
        plane: Plane::XY,
        points: Vec::new(),
        od: Expr::new("0"),
        hole_dia: Expr::new("0"),
        height: Expr::new("0"),
        hole_depth: None,
        base_fillet: Expr::new("0"),
        draft_deg: Expr::new("0"),
        offset: Expr::new("0"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    NewBody,
    Join,
    Cut,
    Intersect,
}

impl Op {
    fn to(self) -> BoolOp {
        match self {
            Op::NewBody => BoolOp::NewBody,
            Op::Join => BoolOp::Join,
            Op::Cut => BoolOp::Cut,
            Op::Intersect => BoolOp::Intersect,
        }
    }
    fn from(b: BoolOp) -> Self {
        match b {
            BoolOp::NewBody => Op::NewBody,
            BoolOp::Join => Op::Join,
            BoolOp::Cut => Op::Cut,
            BoolOp::Intersect => Op::Intersect,
        }
    }
}

/// A body's planar face used as the extrude profile.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSrc {
    pub sel: FaceSelector,
    /// Face centroid and outward normal (where the drag arrow sits).
    pub centroid: DVec3,
    pub normal: DVec3,
}

/// Smallest |distance| an auto extrude accepts; below it there is no tool.
pub const EXTRUDE_DEAD: f64 = 0.05;

pub struct ExtrudeD {
    /// Profile from a body face instead of a sketch (then `sketch` is unused).
    pub face: Option<FaceSrc>,
    /// Op follows the sign of the distance: positive Join, negative Cut.
    pub auto: bool,
    pub sketch: Option<SketchId>,
    /// Which regions of the sketch to extrude (`All` until the user clicks a region).
    pub profiles: ProfileSel,
    pub distance: Field,
    pub symmetric: bool,
    pub reverse: bool,
    pub draft: Field,
    pub op: Op,
    pub target: Option<BodyId>,
}

pub struct RevolveD {
    pub sketch: Option<SketchId>,
    pub profiles: ProfileSel,
    pub axis: usize,
    pub angle: Field,
    pub op: Op,
}

pub struct ShellD {
    pub body: Option<BodyId>,
    pub faces: Vec<(PickKey, FaceSelector, String)>,
    pub kept: Vec<FaceSelector>,
    pub thickness: Field,
    pub outward: bool,
}

pub struct DeleteFaceD {
    pub body: Option<BodyId>,
    pub faces: Vec<(PickKey, FaceSelector, String)>,
    pub kept: Vec<FaceSelector>,
}

pub struct DraftD {
    pub body: Option<BodyId>,
    pub faces: Vec<(PickKey, FaceSelector, String)>,
    pub kept: Vec<FaceSelector>,
    pub angle: Field,
    pub neutral_z: Field,
}

pub struct SplitD {
    pub body: Option<BodyId>,
    pub z: Field,
    pub new_name: String,
    pub keep: SplitKeep,
}

/// `MeshBoolean`: either side may be a mesh body.
pub struct CombineD {
    pub target: Option<BodyId>,
    pub tools: Vec<BodyId>,
    pub op: Op,
    pub clearance: Field,
    /// Target positions to clear, `x, y, z; x, y, z` (empty = where it is).
    pub positions: String,
    pub keep_tools: bool,
}

/// `TransformBody`.
pub struct MoveD {
    pub body: Option<BodyId>,
    pub t: [Field; 3],
    pub rot: [Field; 3],
    pub scale: Field,
    pub copy: bool,
    pub new_name: String,
}

/// `ImportMesh` (edit only: new imports come from File → Import).
pub struct MeshD {
    pub import: ImportId,
    pub scale: Field,
    pub up_y: bool,
    pub rot: [Field; 3],
    pub t: [Field; 3],
    pub reference: bool,
}

pub struct BossD {
    pub body: Option<BodyId>,
    pub plane_z: Field,
    pub points: String,
    pub od: Field,
    pub hole: Field,
    pub height: Field,
    pub base_fillet: Field,
    pub draft: Field,
}

pub struct RibD {
    pub sketch: Option<SketchId>,
    pub thickness: Field,
    pub height: Field,
    pub draft: Field,
    pub flip: bool,
}

pub struct PatternD {
    pub features: Vec<FeatureId>,
    pub circular: bool,
    pub dir: usize,
    pub spacing: Field,
    pub count: Field,
    pub total: Field,
}

pub struct MirrorD {
    pub features: Vec<FeatureId>,
    pub plane: usize,
}

#[allow(clippy::large_enum_variant)]
pub enum Body {
    Extrude(ExtrudeD),
    Revolve(RevolveD),
    Fillet(fillet::FilletD),
    Shell(ShellD),
    DeleteFace(DeleteFaceD),
    Draft(DraftD),
    Split(SplitD),
    Boss(BossD),
    Rib(RibD),
    Pattern(PatternD),
    Mirror(MirrorD),
    Standoffs(standoffs::StandoffsD),
    Joint(joint::JointD),
    Datum(datum::DatumD),
    Combine(CombineD),
    Move(MoveD),
    Mesh(MeshD),
}

pub struct Dialog {
    pub kind: DialogKind,
    pub editing: Option<FeatureId>,
    pub name: String,
    pub body: Body,
    pub error: Option<String>,
    /// Split / Parting joint: "Output as components" (`None`: not offered, e.g.
    /// when editing a feature whose halves may already be components).
    pub as_components: Option<bool>,
}

pub enum DialogEvent {
    None,
    Changed,
    Ok,
    Cancel,
    ZoomTo(glam::Vec3),
}

/// What dialogs see while drawing.
pub struct DialogCx<'a> {
    pub env: ExprEnv<'a>,
    pub data: &'a DocData,
    pub model: Option<&'a Output>,
    pub preview: Option<&'a Output>,
    pub regenerating: bool,
    pub next_id: u32,
}

pub fn sketch_of(f: &Feature) -> Option<SketchId> {
    let _ = f;
    None
}

fn last_sketch(d: &DocData, want_closed: bool) -> Option<SketchId> {
    d.sketches
        .iter()
        .rev()
        .find(|s| {
            let p = s.sketch.profiles();
            if want_closed {
                !p.profiles.is_empty() && !s.sketch.entities.iter().all(|e| e.construction)
            } else {
                !p.open.is_empty()
            }
        })
        .map(|s| s.id)
}

fn last_solid(m: Option<&Output>) -> Option<BodyId> {
    m.and_then(|m| {
        m.bodies
            .iter()
            .rev()
            .find(|b| !b.body.reference)
            .map(|b| b.body.id)
    })
}

fn last_mesh(m: Option<&Output>) -> Option<BodyId> {
    m.and_then(|m| m.bodies.iter().rev().find(|b| !b.body.reference && b.body.is_mesh()).map(|b| b.body.id))
}

fn fields3(v: &[Expr; 3]) -> [Field; 3] {
    [Field::from_expr(&v[0], None), Field::from_expr(&v[1], None), Field::from_expr(&v[2], None)]
}

fn nums3(v: DVec3) -> [Field; 3] {
    [Field::new(fmt(v.x)), Field::new(fmt(v.y)), Field::new(fmt(v.z))]
}

fn exprs3(f: &[Field; 3]) -> [Expr; 3] {
    [f[0].expr(None), f[1].expr(None), f[2].expr(None)]
}

/// `x, y, z; x, y, z` → expression triples.
fn parse_positions(s: &str) -> Result<Vec<[Expr; 3]>, String> {
    s.split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let c: Vec<&str> = p.split(',').map(str::trim).collect();
            if c.len() != 3 {
                return Err(format!("position `{p}` needs x, y, z"));
            }
            Ok([0, 1, 2].map(|i| Expr::new(crate::units::normalize_input(c[i]))))
        })
        .collect()
}

fn sel_body(sel: &[PickKey], m: Option<&Output>) -> Option<BodyId> {
    sel.iter()
        .map(|k| k.body)
        .find(|b| m.and_then(|m| m.body(*b)).is_some_and(|b| !b.reference))
}

pub fn face_label(m: Option<&Output>, k: &PickKey) -> String {
    match k.kind {
        PickKind::Face(f) => m
            .and_then(|m| crate::ui_viewport::provenance_label(m, k.body, f))
            .or_else(|| {
                m.and_then(|m| m.meshed(k.body))
                    .and_then(|b| b.faces.get(f as usize))
                    .map(crate::ui_viewport::face_desc)
            })
            .unwrap_or_else(|| format!("face {f}")),
        _ => "face".into(),
    }
}

pub(crate) fn face_sel(m: Option<&Output>, k: &PickKey) -> Option<FaceSelector> {
    let PickKind::Face(f) = k.kind else {
        return None;
    };
    let info = m?.meshed(k.body)?.faces.get(f as usize)?;
    Some(FaceSelector::from_pick(info))
}

/// The planar face to extrude when the selection is faces of one solid body.
fn face_src(sel: &[PickKey], m: Option<&Output>) -> Option<FaceSrc> {
    let m = m?;
    let mut body = None;
    let mut first = None;
    for k in sel {
        let PickKind::Face(f) = k.kind else { continue };
        if body.is_some_and(|b| b != k.body) || m.body(k.body).is_none_or(|b| b.reference) {
            return None;
        }
        body = Some(k.body);
        let info = m.meshed(k.body)?.faces.get(f as usize)?;
        if info.kind != easycad_model::occt::SurfaceKind::Plane {
            return None;
        }
        first.get_or_insert(info);
    }
    let info = first?;
    Some(FaceSrc { sel: FaceSelector::from_pick(info), centroid: info.centroid, normal: info.normal.normalize_or_zero() })
}

impl ExtrudeD {
    /// The op in force: auto picks Join/Cut from the sign of the distance.
    pub fn live_op(&self, v: Option<f64>) -> Op {
        match (self.auto, v) {
            (true, Some(v)) if v < 0.0 => Op::Cut,
            (true, _) => Op::Join,
            _ => self.op,
        }
    }
}

fn plane_offset(p: &Plane) -> f64 {
    p.origin.dot(p.normal)
}

/// The z field of a plane feature: its `offset` expression (GUI_NEEDS #1), plus
/// the plane's own offset when it has one (older files).
pub fn plane_text(p: &Plane, offset: &Expr) -> String {
    let po = plane_offset(p);
    let off = offset.0.trim();
    let zero = off.is_empty() || off.parse::<f64>().is_ok_and(|v| v == 0.0);
    match (po.abs() < 1e-9, zero) {
        (_, true) => fmt(po),
        (true, false) => off.to_string(),
        (false, false) => format!("{} + {off}", fmt(po)),
    }
}

/// A plane through the origin with `n`, moved by the field's expression.
fn z_expr(f: &Field) -> Expr {
    Expr::new(crate::units::normalize_input(f.text.trim()))
}

pub fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s
    }
}

fn region_sel(p: &ProfileSel) -> Result<ProfileSel, String> {
    match p {
        ProfileSel::Containing(v) if v.is_empty() => Err("pick at least one region (click regions of the sketch, or press All)".into()),
        p => Ok(p.clone()),
    }
}

/// "N regions selected" with All / Clear buttons; clicks in the viewport toggle regions
/// (see `region_pick`).
fn region_row(ui: &mut egui::Ui, sketch: Option<SketchId>, sel: &mut ProfileSel, cx: &DialogCx) -> bool {
    let Some(sk) = sketch.and_then(|id| cx.data.sketch(id)) else { return false };
    let regions = sk.regions();
    let n = easycad_model::regions::selected_regions(sel, sk, &regions).len();
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Regions").size(12.0).color(theme::TEXT_DIM));
        ui.label(RichText::new(format!("{n} region{} selected (of {})", if n == 1 { "" } else { "s" }, regions.len())).size(12.0));
        if ui.small_button("All").clicked() {
            *sel = ProfileSel::All;
            ch = true;
        }
        if ui.small_button("Clear").clicked() {
            *sel = ProfileSel::Containing(Vec::new());
            ch = true;
        }
    });
    ui.label(RichText::new("Click regions of the sketch in the view to add or remove them.").size(11.0).color(theme::TEXT_MUTED));
    ch
}

const AXES: [(&str, DVec3); 3] = [("X", DVec3::X), ("Y", DVec3::Y), ("Z", DVec3::Z)];
const PLANES: [(&str, Plane); 3] = [("XY", Plane::XY), ("XZ", Plane::XZ), ("YZ", Plane::YZ)];

impl Dialog {
    pub fn new(
        kind: DialogKind,
        cx: &BuildCx,
        model: Option<&Output>,
        sel: &[PickKey],
    ) -> Result<Dialog, String> {
        let d = &cx.data;
        let body = match kind {
            DialogKind::Extrude => {
                let face = face_src(sel, model);
                Body::Extrude(ExtrudeD {
                auto: face.is_some() || last_solid(model).is_some(),
                sketch: if face.is_some() { None } else { last_sketch(d, true) },
                profiles: ProfileSel::All,
                distance: Field::new("10"),
                symmetric: false,
                reverse: false,
                draft: Field::new("0"),
                op: if last_solid(model).is_some() {
                    Op::Join
                } else {
                    Op::NewBody
                },
                target: sel_body(sel, model),
                face,
            })
            }
            DialogKind::Revolve => Body::Revolve(RevolveD {
                sketch: last_sketch(d, true),
                profiles: ProfileSel::All,
                axis: 1,
                angle: Field::new("360"),
                op: Op::NewBody,
            }),
            DialogKind::Fillet | DialogKind::Chamfer => Body::Fillet(fillet::FilletD::new(
                kind == DialogKind::Chamfer,
                model,
                sel,
            )),
            DialogKind::Shell => Body::Shell(ShellD {
                body: sel_body(sel, model).or(last_solid(model)),
                faces: sel
                    .iter()
                    .filter_map(|k| face_sel(model, k).map(|s| (*k, s, face_label(model, k))))
                    .collect(),
                kept: Vec::new(),
                thickness: Field::new(if d.params.get("wall").is_some() {
                    "wall"
                } else {
                    "2"
                }),
                outward: false,
            }),
            DialogKind::DeleteFace => Body::DeleteFace(DeleteFaceD {
                body: sel_body(sel, model).or(last_solid(model)),
                faces: sel
                    .iter()
                    .filter_map(|k| face_sel(model, k).map(|s| (*k, s, face_label(model, k))))
                    .collect(),
                kept: Vec::new(),
            }),
            DialogKind::Draft => Body::Draft(DraftD {
                body: sel_body(sel, model).or(last_solid(model)),
                faces: sel
                    .iter()
                    .filter_map(|k| face_sel(model, k).map(|s| (*k, s, face_label(model, k))))
                    .collect(),
                kept: Vec::new(),
                angle: Field::new(if d.params.get("draft").is_some() {
                    "draft"
                } else {
                    "1 deg"
                }),
                neutral_z: Field::new("0"),
            }),
            DialogKind::Split => Body::Split(SplitD {
                body: sel_body(sel, model).or(last_solid(model)),
                z: Field::new(if d.params.get("lid_split").is_some() {
                    "lid_split"
                } else {
                    "10"
                }),
                new_name: "Lid".into(),
                keep: SplitKeep::Both,
            }),
            DialogKind::Combine => {
                // a picked mesh body is the usual target; other picked bodies are tools
                let picked: Vec<BodyId> = {
                    let mut v: Vec<BodyId> = sel.iter().map(|k| k.body).collect();
                    v.dedup();
                    v
                };
                let is_mesh = |b: &BodyId| model.and_then(|m| m.body(*b)).is_some_and(|b| b.is_mesh());
                let target = picked.iter().copied().find(|b| is_mesh(b)).or(picked.first().copied()).or(last_mesh(model)).or(last_solid(model));
                Body::Combine(CombineD {
                    target,
                    tools: picked.into_iter().filter(|b| Some(*b) != target).collect(),
                    op: Op::Cut,
                    clearance: Field::new("0"),
                    positions: String::new(),
                    keep_tools: true,
                })
            }
            DialogKind::MoveBody => Body::Move(MoveD {
                body: sel.first().map(|k| k.body).or(last_mesh(model)).or(last_solid(model)),
                t: [Field::new("0"), Field::new("0"), Field::new("0")],
                rot: [Field::new("0"), Field::new("0"), Field::new("0")],
                scale: Field::new("1"),
                copy: false,
                new_name: String::new(),
            }),
            DialogKind::ImportMesh => return Err("import a mesh with File → Import".into()),
            DialogKind::Boss => {
                let pts = boss_points_from_selection(model, sel);
                Body::Boss(BossD {
                    body: sel_body(sel, model).or(last_solid(model)),
                    plane_z: Field::new(if d.params.get("wall").is_some() {
                        "wall"
                    } else {
                        "0"
                    }),
                    points: if pts.is_empty() { "0, 0".into() } else { pts },
                    od: Field::new(if d.params.get("boss_od").is_some() {
                        "boss_od"
                    } else {
                        "7"
                    }),
                    hole: Field::with_fit(
                        if d.params.get("boss_hole").is_some() {
                            "boss_hole"
                        } else {
                            "2.5"
                        },
                        Fit::hole(FitClass::Snug),
                    ),
                    height: Field::new("10"),
                    base_fillet: Field::new("0.8"),
                    draft: Field::new("0"),
                })
            }
            DialogKind::Rib => Body::Rib(RibD {
                sketch: last_sketch(d, false),
                thickness: Field::new("1.5"),
                height: Field::new("8"),
                draft: Field::new("0"),
                flip: false,
            }),
            DialogKind::Pattern => Body::Pattern(PatternD {
                features: d.features.last().map(|f| vec![f.id]).unwrap_or_default(),
                circular: false,
                dir: 0,
                spacing: Field::new("10"),
                count: Field::new("3"),
                total: Field::new("360"),
            }),
            DialogKind::Mirror => Body::Mirror(MirrorD {
                features: d.features.last().map(|f| vec![f.id]).unwrap_or_default(),
                plane: 1,
            }),
            DialogKind::Standoffs => Body::Standoffs(standoffs::StandoffsD::new(d, model)?),
            DialogKind::Joint => Body::Joint(joint::JointD::new(d, model)),
            DialogKind::Construct => {
                let picks = match model {
                    Some(m) => sel.iter().filter_map(|k| crate::datum_ui::from_key(m, k, crate::datum_ui::Accept(63))).collect(),
                    None => Vec::new(),
                };
                Body::Datum(datum::DatumD::new_auto(picks))
            }
            DialogKind::Datum(k) => {
                let mut dd = datum::DatumD::new(k);
                if let Some(m) = model {
                    let acc = crate::datum_ui::Accept::PLANE
                        .or(crate::datum_ui::Accept::LINE)
                        .or(crate::datum_ui::Accept::CYLF)
                        .or(crate::datum_ui::Accept::CIRCLE);
                    dd.prefill(sel.iter().filter_map(|k| crate::datum_ui::from_key(m, k, acc)).collect());
                }
                Body::Datum(dd)
            }
        };
        let as_components = matches!(kind, DialogKind::Split | DialogKind::Joint).then_some(false);
        let mut dlg = Dialog {
            kind,
            editing: None,
            name: match kind {
                DialogKind::Datum(k) => datum::default_name(k, cx),
                DialogKind::Construct => "Construct".into(),
                _ => kind.title().into(),
            },
            body,
            error: None,
            as_components,
        };
        if let Body::Joint(j) = &mut dlg.body {
            dlg.editing = j.adopt_split(d);
        }
        Ok(dlg)
    }

    /// Open an existing feature for editing.
    pub fn edit(f: &Feature, cx: &BuildCx) -> Option<Dialog> {
        let d = &cx.data;
        let (kind, body) = match &f.kind {
            FeatureKind::Extrude {
                sketch,
                profiles,
                distance,
                symmetric,
                reverse,
                draft_deg,
                op,
                target,
            } => (
                DialogKind::Extrude,
                Body::Extrude(ExtrudeD {
                    face: None,
                    auto: false,
                    sketch: Some(*sketch),
                    profiles: profiles.clone(),
                    distance: Field::from_expr(distance, None),
                    symmetric: *symmetric,
                    reverse: *reverse,
                    draft: Field::from_expr(draft_deg, None),
                    op: Op::from(*op),
                    target: *target,
                }),
            ),
            FeatureKind::ExtrudeFace { face, distance, op, target } => {
                let (centroid, normal) = match face {
                    FaceSelector::Hint { centroid, normal, .. } => (*centroid, *normal),
                    FaceSelector::Extreme { normal, .. } | FaceSelector::AllWithNormal { normal } => (DVec3::ZERO, *normal),
                    FaceSelector::Cylinders { .. } => (DVec3::ZERO, DVec3::Z),
                };
                let v = crate::widgets::eval(&distance.0, &crate::adapters::values(d)).ok();
                let auto = v.is_some_and(|v| (*op == BoolOp::Join && v >= 0.0) || (*op == BoolOp::Cut && v < 0.0));
                (
                    DialogKind::Extrude,
                    Body::Extrude(ExtrudeD {
                        face: Some(FaceSrc { sel: face.clone(), centroid, normal }),
                        auto,
                        sketch: None,
                        profiles: ProfileSel::All,
                        distance: Field::from_expr(distance, None),
                        symmetric: false,
                        reverse: false,
                        draft: Field::new("0"),
                        op: Op::from(*op),
                        target: *target,
                    }),
                )
            }
            FeatureKind::Revolve {
                sketch,
                profiles,
                axis,
                angle_deg,
                op,
                ..
            } => (
                DialogKind::Revolve,
                Body::Revolve(RevolveD {
                    sketch: Some(*sketch),
                    profiles: profiles.clone(),
                    axis: AXES.iter().position(|a| a.1 == axis.dir).unwrap_or(1),
                    angle: Field::from_expr(angle_deg, None),
                    op: Op::from(*op),
                }),
            ),
            FeatureKind::Fillet { .. } | FeatureKind::Chamfer { .. } => (
                if matches!(f.kind, FeatureKind::Chamfer { .. }) {
                    DialogKind::Chamfer
                } else {
                    DialogKind::Fillet
                },
                Body::Fillet(fillet::FilletD::from_feature(f)),
            ),
            FeatureKind::Shell {
                body,
                open_faces,
                thickness,
                outward,
            } => (
                DialogKind::Shell,
                Body::Shell(ShellD {
                    body: *body,
                    faces: Vec::new(),
                    kept: open_faces.clone(),
                    thickness: Field::from_expr(thickness, None),
                    outward: *outward,
                }),
            ),
            FeatureKind::DeleteFace { body, faces } => (
                DialogKind::DeleteFace,
                Body::DeleteFace(DeleteFaceD { body: *body, faces: Vec::new(), kept: faces.clone() }),
            ),
            FeatureKind::Draft {
                body,
                faces,
                neutral,
                angle_deg,
                offset,
                ..
            } => (
                DialogKind::Draft,
                Body::Draft(DraftD {
                    body: *body,
                    faces: Vec::new(),
                    kept: faces.clone(),
                    angle: Field::from_expr(angle_deg, None),
                    neutral_z: Field::new(plane_text(neutral, offset)),
                }),
            ),
            FeatureKind::Split {
                body,
                plane,
                new_name,
                offset,
                keep,
            } => (
                DialogKind::Split,
                Body::Split(SplitD {
                    body: *body,
                    z: Field::new(plane_text(plane, offset)),
                    new_name: new_name.clone(),
                    keep: *keep,
                }),
            ),
            FeatureKind::MeshBoolean { target, tools, op, clearance, positions, keep_tools } => (
                DialogKind::Combine,
                Body::Combine(CombineD {
                    target: *target,
                    tools: tools.clone(),
                    op: Op::from(*op),
                    clearance: Field::from_expr(clearance, None),
                    positions: positions.iter().map(|p| format!("{}, {}, {}", p[0].0, p[1].0, p[2].0)).collect::<Vec<_>>().join("; "),
                    keep_tools: *keep_tools,
                }),
            ),
            FeatureKind::TransformBody { body, translate, rotate_deg, scale, copy, new_name, .. } => (
                DialogKind::MoveBody,
                Body::Move(MoveD {
                    body: Some(*body),
                    t: fields3(translate),
                    rot: nums3(*rotate_deg),
                    scale: Field::from_expr(scale, None),
                    copy: *copy,
                    new_name: new_name.clone(),
                }),
            ),
            FeatureKind::ImportMesh { import, scale, up, rotate_deg, translate, reference } => (
                DialogKind::ImportMesh,
                Body::Mesh(MeshD {
                    import: *import,
                    scale: Field::from_expr(scale, None),
                    up_y: *up == MeshUp::Y,
                    rot: nums3(*rotate_deg),
                    t: fields3(translate),
                    reference: *reference,
                }),
            ),
            FeatureKind::ScrewBoss {
                body,
                plane,
                points,
                od,
                hole_dia,
                height,
                base_fillet,
                draft_deg,
                offset,
                ..
            } => (
                DialogKind::Boss,
                Body::Boss(BossD {
                    body: *body,
                    plane_z: Field::new(
                        plane_text(plane, offset),
                    ),
                    points: points
                        .iter()
                        .map(|p| format!("{}, {}", fmt(p.x), fmt(p.y)))
                        .collect::<Vec<_>>()
                        .join("; "),
                    od: Field::from_expr(od, None),
                    hole: Field::from_expr(hole_dia, default_fit(&f.kind, "hole_dia")),
                    height: Field::from_expr(height, None),
                    base_fillet: Field::from_expr(base_fillet, None),
                    draft: Field::from_expr(draft_deg, None),
                }),
            ),
            FeatureKind::Rib {
                sketch,
                thickness,
                height,
                draft_deg,
                flip,
                ..
            } => (
                DialogKind::Rib,
                Body::Rib(RibD {
                    sketch: Some(*sketch),
                    thickness: Field::from_expr(thickness, None),
                    height: Field::from_expr(height, None),
                    draft: Field::from_expr(draft_deg, None),
                    flip: *flip,
                }),
            ),
            FeatureKind::Pattern {
                source: PatternSource::Features(fs),
                kind,
            } => {
                let mut p = PatternD {
                    features: fs.clone(),
                    circular: false,
                    dir: 0,
                    spacing: Field::new("10"),
                    count: Field::new("3"),
                    total: Field::new("360"),
                };
                match kind {
                    PatternKind::Linear {
                        dir,
                        spacing,
                        count,
                    } => {
                        p.dir = AXES.iter().position(|a| a.1 == *dir).unwrap_or(0);
                        p.spacing = Field::from_expr(spacing, None);
                        p.count = Field::from_expr(count, None);
                    }
                    PatternKind::Circular {
                        count, total_deg, ..
                    } => {
                        p.circular = true;
                        p.count = Field::from_expr(count, None);
                        p.total = Field::from_expr(total_deg, None);
                    }
                }
                (DialogKind::Pattern, Body::Pattern(p))
            }
            FeatureKind::Mirror {
                source: PatternSource::Features(fs),
                plane,
            } => (
                DialogKind::Mirror,
                Body::Mirror(MirrorD {
                    features: fs.clone(),
                    plane: PLANES
                        .iter()
                        .position(|p| p.1.normal == plane.normal)
                        .unwrap_or(1),
                }),
            ),
            FeatureKind::StandoffsFromPcb { .. } => (
                DialogKind::Standoffs,
                Body::Standoffs(standoffs::StandoffsD::from_feature(f, d)?),
            ),
            FeatureKind::PartingJoint { .. } => (
                DialogKind::Joint,
                Body::Joint(joint::JointD::from_feature(f, d)?),
            ),
            FeatureKind::DatumPlane { .. } | FeatureKind::DatumAxis { .. } | FeatureKind::DatumPoint { .. } => {
                let dd = datum::DatumD::from_feature(f, d)?;
                (DialogKind::Construct, Body::Datum(dd))
            }
            _ => return None,
        };
        Some(Dialog {
            kind,
            editing: Some(f.id),
            name: f.name.clone(),
            body,
            error: None,
            as_components: None,
        })
    }

    pub fn editing(&self) -> Option<FeatureId> {
        self.editing
    }

    /// Index of the edited feature (the viewport rolls back to it).
    pub fn edit_index(&self, d: &DocData) -> Option<usize> {
        let id = self.editing?;
        d.features.iter().position(|f| f.id == id)
    }

    pub fn title_case(&self) -> String {
        if let Body::Joint(joint::JointD {
            replaces: Some(r), ..
        }) = &self.body
        {
            return format!("Parting joint (replaces {r})");
        }
        match self.editing {
            Some(_) => format!("Edit {}", self.name),
            None => self.kind.title().into(),
        }
    }

    pub fn filter(&self) -> Option<Filter> {
        match self.kind {
            // faces too: a face pick dresses every edge bounding it (edges still win
            // under the cursor, as the ray pick ties go to edges)
            DialogKind::Fillet | DialogKind::Chamfer => Some(Filter {
                bodies: false,
                faces: true,
                edges: true,
                sketches: false,
            }),
            DialogKind::Shell | DialogKind::DeleteFace | DialogKind::Draft => Some(Filter::FACES_ONLY),
            DialogKind::Boss => Some(Filter {
                bodies: false,
                faces: false,
                edges: true,
                sketches: false,
            }),
            _ => None,
        }
    }

    pub fn filter_note(&self) -> Option<String> {
        self.filter()
            .map(|_| format!("filter set by {}", self.kind.title()))
    }

    /// Parameters the dialog's expressions reference (highlighted in the panel).
    pub fn hot_params(&self, cx: &BuildCx) -> std::collections::BTreeSet<String> {
        let mut s = std::collections::BTreeSet::new();
        if let Ok(fs) = self.build(cx) {
            for f in &fs {
                s.extend(crate::adapters::refs_of(&f.kind));
            }
        }
        if let Body::Joint(j) = &self.body {
            s.extend(j.refs());
        }
        s
    }

    pub fn build(&self, cx: &BuildCx) -> Result<Vec<Feature>, String> {
        let vals = crate::adapters::values(&cx.data);
        let num = |f: &Field| crate::widgets::eval(&f.text, &vals);
        let id0 = cx.id(self.editing, 0);
        let one = |kind: FeatureKind| {
            Ok(vec![Feature {
                id: id0,
                name: self.name.clone(),
                suppressed: false,
                kind,
            }])
        };
        match &self.body {
            Body::Extrude(e) => {
                let v = num(&e.distance).ok();
                let op = e.live_op(v);
                let neg = v.is_some_and(|v| v < 0.0);
                if e.auto && v.is_some_and(|v| v.abs() < EXTRUDE_DEAD) {
                    return Err("distance is too small".into());
                }
                let flipped = |t: &str| Expr::new(format!("-({})", t.trim()));
                if let Some(f) = &e.face {
                    // signed along the outward normal; an explicit Cut goes into the body
                    let invert = !e.auto && op == Op::Cut && v.is_some_and(|v| v > 0.0);
                    return one(FeatureKind::ExtrudeFace {
                        face: f.sel.clone(),
                        distance: if invert { flipped(&e.distance.text) } else { e.distance.expr(None) },
                        op: op.to(),
                        target: e.target,
                    });
                }
                // auto Cut from a sketch: |distance| the other way round
                let back = e.auto && neg;
                one(FeatureKind::Extrude {
                    sketch: e.sketch.ok_or("pick a sketch with a closed profile")?,
                    profiles: region_sel(&e.profiles)?,
                    distance: if back { flipped(&e.distance.text) } else { e.distance.expr(None) },
                    symmetric: e.symmetric,
                    reverse: e.reverse != back,
                    draft_deg: e.draft.expr(None),
                    op: op.to(),
                    target: if op == Op::NewBody { None } else { e.target },
                })
            }
            Body::Revolve(r) => one(FeatureKind::Revolve {
                sketch: r.sketch.ok_or("pick a sketch")?,
                profiles: region_sel(&r.profiles)?,
                axis: Axis {
                    origin: DVec3::ZERO,
                    dir: AXES[r.axis].1,
                },
                angle_deg: r.angle.expr(None),
                op: r.op.to(),
                target: None,
            }),
            Body::Fillet(f) => f.build(cx, self.editing, &self.name),
            Body::Shell(s) => {
                let mut faces = s.kept.clone();
                faces.extend(s.faces.iter().map(|f| f.1.clone()));
                one(FeatureKind::Shell {
                    body: s.body,
                    open_faces: faces,
                    thickness: s.thickness.expr(None),
                    outward: s.outward,
                })
            }
            Body::DeleteFace(s) => {
                let mut faces = s.kept.clone();
                faces.extend(s.faces.iter().map(|f| f.1.clone()));
                if faces.is_empty() {
                    return Err("pick the faces to delete".into());
                }
                one(FeatureKind::DeleteFace { body: s.body, faces })
            }
            Body::Draft(s) => {
                let mut faces = s.kept.clone();
                faces.extend(s.faces.iter().map(|f| f.1.clone()));
                if faces.is_empty() {
                    return Err("pick the faces to draft".into());
                }
                let z = num(&s.neutral_z)?;
                one(FeatureKind::Draft {
                    body: s.body,
                    faces,
                    neutral: Plane::XY,
                    pull: DVec3::Z,
                    angle_deg: s.angle.expr(None),
                    offset: {
                        let _ = z;
                        z_expr(&s.neutral_z)
                    },
                })
            }
            Body::Split(s) => {
                let z = num(&s.z)?;
                let _ = z;
                one(FeatureKind::Split {
                    body: s.body,
                    plane: Plane::XY,
                    new_name: s.new_name.clone(),
                    offset: z_expr(&s.z),
                    keep: s.keep,
                })
            }
            Body::Combine(c) => {
                if c.tools.is_empty() {
                    return Err("tick at least one tool body".into());
                }
                if c.op == Op::NewBody {
                    return Err("pick Join, Cut or Intersect".into());
                }
                num(&c.clearance)?;
                one(FeatureKind::MeshBoolean {
                    target: c.target,
                    tools: c.tools.clone(),
                    op: c.op.to(),
                    clearance: c.clearance.expr(None),
                    positions: parse_positions(&c.positions)?,
                    keep_tools: c.keep_tools,
                })
            }
            Body::Move(m) => {
                let mut rot = [0.0; 3];
                for (i, f) in m.rot.iter().enumerate() {
                    rot[i] = num(f)?;
                }
                for f in m.t.iter().chain([&m.scale]) {
                    num(f)?;
                }
                one(FeatureKind::TransformBody {
                    body: m.body.ok_or("pick a body")?,
                    translate: exprs3(&m.t),
                    rotate_deg: DVec3::from_array(rot),
                    pivot: DVec3::ZERO,
                    scale: m.scale.expr(None),
                    copy: m.copy,
                    new_name: m.new_name.clone(),
                })
            }
            Body::Mesh(m) => {
                let mut rot = [0.0; 3];
                for (i, f) in m.rot.iter().enumerate() {
                    rot[i] = num(f)?;
                }
                for f in m.t.iter().chain([&m.scale]) {
                    num(f)?;
                }
                one(FeatureKind::ImportMesh {
                    import: m.import,
                    scale: m.scale.expr(None),
                    up: if m.up_y { MeshUp::Y } else { MeshUp::Z },
                    rotate_deg: DVec3::from_array(rot),
                    translate: exprs3(&m.t),
                    reference: m.reference,
                })
            }
            Body::Boss(b) => {
                let z = num(&b.plane_z)?;
                let pts = parse_points(&b.points)?;
                let _ = z;
                one(FeatureKind::ScrewBoss {
                    body: b.body,
                    plane: Plane::XY,
                    points: pts,
                    od: b.od.expr(None),
                    hole_dia: b.hole.expr(default_fit(&boss_template(), "hole_dia")),
                    height: b.height.expr(None),
                    hole_depth: None,
                    base_fillet: b.base_fillet.expr(None),
                    draft_deg: b.draft.expr(None),
                    offset: z_expr(&b.plane_z),
                })
            }
            Body::Rib(r) => one(FeatureKind::Rib {
                sketch: r.sketch.ok_or("pick a sketch with an open line")?,
                thickness: r.thickness.expr(None),
                height: r.height.expr(None),
                draft_deg: r.draft.expr(None),
                flip: r.flip,
                body: None,
                extent: Default::default(),
            }),
            Body::Pattern(p) => {
                if p.features.is_empty() {
                    return Err("pick features to pattern".into());
                }
                let kind = if p.circular {
                    PatternKind::Circular {
                        axis: Axis::Z,
                        count: p.count.expr(None),
                        total_deg: p.total.expr(None),
                    }
                } else {
                    PatternKind::Linear {
                        dir: AXES[p.dir].1,
                        spacing: p.spacing.expr(None),
                        count: p.count.expr(None),
                    }
                };
                one(FeatureKind::Pattern {
                    source: PatternSource::Features(p.features.clone()),
                    kind,
                })
            }
            Body::Mirror(m) => {
                if m.features.is_empty() {
                    return Err("pick features to mirror".into());
                }
                one(FeatureKind::Mirror {
                    source: PatternSource::Features(m.features.clone()),
                    plane: PLANES[m.plane].1,
                })
            }
            Body::Standoffs(s) => s.build(cx, self.editing, &self.name),
            Body::Joint(j) => j.build(cx, self.editing, &self.name),
            Body::Datum(dd) => {
                for (i, f) in dd.fields.iter().enumerate() {
                    if f.text.trim().is_empty() || num(f).is_err() {
                        return Err(format!("{}: enter a number or expression", dd.kind.fields()[i].label));
                    }
                }
                if dd.auto.is_some() && self.editing.is_none() {
                    let name = datum::default_name(dd.kind, cx);
                    return Ok(vec![dd.build(id0, &name)?]);
                }
                Ok(vec![dd.build(id0, &self.name)?])
            }
        }
    }

    pub fn on_preview(&mut self, out: &Output, model: Option<&Output>) {
        match &mut self.body {
            Body::Fillet(f) => {
                f.keep_failing = false;
                f.on_preview(out, model, self.editing)
            }
            Body::Joint(j) => j.on_preview(out),
            _ => {}
        }
        self.error = None;
        // first error among the dialog's own features
        let mine: Vec<FeatureId> = out
            .prefix_bodies
            .keys()
            .filter_map(|i| out.data.features.get(*i))
            .map(|f| f.id)
            .collect();
        for r in out
            .result
            .reports
            .iter()
            .filter(|r| mine.contains(&r.feature))
        {
            if matches!(self.body, Body::Fillet(_) | Body::Joint(_)) {
                continue;
            }
            if let easycad_model::regen::FeatureStatus::Error(e) = &r.status {
                self.error = Some(e.clone());
            }
        }
    }

    /// The sketch and region selection of an Extrude/Revolve dialog.
    pub fn region_target(&self) -> Option<(SketchId, &ProfileSel)> {
        match &self.body {
            Body::Extrude(e) => e.sketch.map(|s| (s, &e.profiles)),
            Body::Revolve(r) => r.sketch.map(|s| (s, &r.profiles)),
            _ => None,
        }
    }

    pub fn set_region_sel(&mut self, sel: ProfileSel) {
        match &mut self.body {
            Body::Extrude(e) => e.profiles = sel,
            Body::Revolve(r) => r.profiles = sel,
            _ => {}
        }
    }

    /// A viewport click while the dialog is open. Returns true if consumed.
    pub fn on_pick(&mut self, key: PickKey, model: Option<&Output>) -> bool {
        match &mut self.body {
            Body::Fillet(f) => f.toggle(key, model),
            Body::Shell(ShellD { faces, body, .. })
            | Body::DeleteFace(DeleteFaceD { faces, body, .. })
            | Body::Draft(DraftD { faces, body, .. }) => {
                let Some(s) = face_sel(model, &key) else {
                    return false;
                };
                if let Some(i) = faces.iter().position(|f| f.0 == key) {
                    faces.remove(i);
                } else {
                    faces.push((key, s, face_label(model, &key)));
                    *body = Some(key.body);
                }
                true
            }
            Body::Boss(b) => {
                let PickKind::Edge(e) = key.kind else {
                    return false;
                };
                let Some(info) = model
                    .and_then(|m| m.meshed(key.body))
                    .and_then(|m| m.edges.get(e as usize))
                else {
                    return false;
                };
                if let Some((c, _, _)) = info.circle {
                    let p = format!("{}, {}", fmt(c.x), fmt(c.y));
                    if b.points.trim() == "0, 0" {
                        b.points = p;
                    } else {
                        b.points = format!("{}; {p}", b.points);
                    }
                    b.body = Some(key.body);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Picked items to highlight in the viewport.
    pub fn picked(&self) -> Vec<PickKey> {
        match &self.body {
            Body::Fillet(f) => f.picked(),
            Body::Shell(s) => s.faces.iter().map(|f| f.0).collect(),
            Body::DeleteFace(s) => s.faces.iter().map(|f| f.0).collect(),
            Body::Draft(s) => s.faces.iter().map(|f| f.0).collect(),
            _ => Vec::new(),
        }
    }

    /// The floating card. Returns the user's intent.
    pub fn ui(&mut self, ui: &mut Ui, cx: &DialogCx) -> DialogEvent {
        let mut ev = DialogEvent::None;
        let id = ui.scope_id().with("dlg");
        crate::widgets::card_frame().show(ui, |ui| {
            ui.set_width(360.0);
            // rules are painted once the card's final width is known (a wide
            // report can grow it past 360 after the header rule was placed)
            let mut rules: Vec<f32> = Vec::new();
            // header
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(self.kind.title().to_uppercase())
                                .strong()
                                .size(12.0),
                        );
                        if self.editing.is_some() && self.name != self.kind.title() {
                            ui.label(widgets::muted(format!("· {}", self.name)));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(widgets::muted("Enter ✓ · Esc ✕"));
                        });
                    });
                });
            rules.push(ui.cursor().top());
            ui.add_space(1.0);
            // a long dialog (joint checks, fillet report) scrolls instead of
            // pushing OK / Cancel below the window
            let max_h = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 120.0).max(160.0);
            let changed = egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .max_height(max_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(14, 12))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let (c, e) = self.body_ui(ui, id, cx);
                            if let Some(e) = e {
                                ev = e;
                            }
                            if let Some(on) = &mut self.as_components {
                                widgets::divider(ui);
                                ui.checkbox(on, RichText::new("Output as components").size(12.0)).on_hover_text(
                                    "Each half becomes its own component, built in context from this one (it follows every edit) and free to move or mate. The source stays as the hidden design master.",
                                );
                            }
                            c
                        })
                        .inner
                })
                .inner;
            if changed {
                ev = DialogEvent::Changed;
            }
            if let Some(err) = &self.error {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(14, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(err).size(11.5).color(theme::ORANGE_SOFT));
                    });
            }
            rules.push(ui.cursor().top());
            ui.add_space(1.0);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if cx.regenerating {
                            crate::widgets::spinner(ui, 12.0, Some(theme::TEXT_MUTED));
                            ui.label(widgets::muted("previewing…"));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::primary(ui, "   OK   ")
                                .on_hover_text("Enter")
                                .clicked()
                            {
                                ev = DialogEvent::Ok;
                            }
                            if widgets::button(ui, "Cancel").on_hover_text("Esc").clicked() {
                                ev = DialogEvent::Cancel;
                            }
                        });
                    });
                });
            let x = ui.min_rect().x_range();
            for y in rules {
                ui.painter()
                    .hline(x, y, egui::Stroke::new(1.0, theme::DIVIDER));
            }
        });
        ev
    }

    fn body_ui(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx) -> (bool, Option<DialogEvent>) {
        let env = &cx.env;
        let mut ch = false;
        let mut ev = None;
        let solids: Vec<(BodyId, String)> = cx
            .model
            .map(|m| {
                m.bodies
                    .iter()
                    .filter(|b| !b.body.reference)
                    .map(|b| (b.body.id, b.body.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let sketches: Vec<(SketchId, String)> = cx
            .data
            .sketches
            .iter()
            .map(|s| (s.id, s.sketch.name.clone()))
            .collect();
        match &mut self.body {
            Body::Extrude(e) => {
                let before = e.sketch;
                if e.face.is_some() {
                    ui.label(RichText::new("Profile: picked face").size(12.0).color(theme::TEXT_DIM));
                } else {
                    ch |= sketch_combo(ui, id.with("sk"), &mut e.sketch, &sketches);
                    if e.sketch != before {
                        e.profiles = ProfileSel::All;
                    }
                    ch |= region_row(ui, e.sketch, &mut e.profiles, cx);
                }
                ch |= e.distance.row(ui, id.with("d"), "Distance", env);
                let live = e.live_op(crate::widgets::eval(&e.distance.text, &crate::adapters::values(cx.data)).ok());
                if e.face.is_none() { ui.horizontal(|ui| {
                    ui.label(RichText::new("Direction").size(12.0).color(theme::TEXT_DIM));
                    for (label, sym, rev) in [
                        ("One side", false, false),
                        ("Symmetric", true, false),
                        ("Flip", false, true),
                    ] {
                        if widgets::pill(ui, label, e.symmetric == sym && e.reverse == rev)
                            .clicked()
                        {
                            e.symmetric = sym;
                            e.reverse = rev;
                            ch = true;
                        }
                    }
                }); 
                ch |= e.draft.row(ui, id.with("dr"), "Taper angle", env); }
                let mut shown = live;
                if op_row(ui, &mut shown) {
                    e.op = shown;
                    e.auto = false;
                    ch = true;
                }
                if e.auto {
                    ui.label(RichText::new("Auto: drag out to Join, in to Cut").size(11.0).color(theme::TEXT_DIM));
                }
                if live != Op::NewBody {
                    ch |= body_combo(ui, id.with("t"), &mut e.target, &solids, "last solid");
                }
            }
            Body::Revolve(r) => {
                let before = r.sketch;
                ch |= sketch_combo(ui, id.with("sk"), &mut r.sketch, &sketches);
                if r.sketch != before {
                    r.profiles = ProfileSel::All;
                }
                ch |= region_row(ui, r.sketch, &mut r.profiles, cx);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Axis").size(12.0).color(theme::TEXT_DIM));
                    for (i, (n, _)) in AXES.iter().enumerate() {
                        if widgets::pill(ui, n, r.axis == i).clicked() {
                            r.axis = i;
                            ch = true;
                        }
                    }
                });
                ch |= r.angle.row(ui, id.with("a"), "Angle", env);
                ch |= op_row(ui, &mut r.op);
            }
            Body::Fillet(f) => {
                let (c, e) = f.ui(ui, id, cx);
                ch |= c;
                ev = e;
            }
            Body::Shell(s) => {
                ch |= body_combo(ui, id.with("b"), &mut s.body, &solids, "last solid");
                ch |= face_list(ui, "Faces to remove", &mut s.faces, &mut s.kept);
                ch |= s.thickness.row(ui, id.with("t"), "Thickness", env);
                ch |= ui.checkbox(&mut s.outward, "Grow outward").changed();
            }
            Body::DeleteFace(s) => {
                ch |= body_combo(ui, id.with("b"), &mut s.body, &solids, "last solid");
                ch |= face_list(ui, "Faces to delete", &mut s.faces, &mut s.kept);
                ui.label(widgets::muted("The neighbouring faces grow to close the gap."));
            }
            Body::Draft(s) => {
                ch |= body_combo(ui, id.with("b"), &mut s.body, &solids, "last solid");
                ch |= face_list(ui, "Faces", &mut s.faces, &mut s.kept);
                ch |= s.angle.row(ui, id.with("a"), "Angle", env);
                ch |= s.neutral_z.row(ui, id.with("n"), "Neutral plane z", env);
                ui.label(widgets::muted("Pull direction +Z (mold opening)."));
            }
            Body::Split(s) => {
                ch |= body_combo(ui, id.with("b"), &mut s.body, &solids, "last solid");
                ch |= s.z.row(ui, id.with("z"), "Split plane z", env);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Keep").size(12.0).color(theme::TEXT_DIM));
                    for (k, n) in [(SplitKeep::Both, "Both"), (SplitKeep::Below, "Below"), (SplitKeep::Above, "Above")] {
                        if widgets::pill(ui, n, s.keep == k).clicked() {
                            s.keep = k;
                            ch = true;
                        }
                    }
                });
                if s.keep == SplitKeep::Both {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Upper body")
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                    ch |= ui
                        .add(egui::TextEdit::singleline(&mut s.new_name).desired_width(132.0))
                        .changed();
                });
                }
            }
            Body::Combine(c) => {
                let all: Vec<(BodyId, String)> = cx
                    .model
                    .map(|m| m.bodies.iter().map(|b| (b.body.id, format!("{}{}", b.body.name, if b.body.is_mesh() { " (mesh)" } else { "" }))).collect())
                    .unwrap_or_default();
                ch |= body_combo(ui, id.with("t"), &mut c.target, &all, "last body");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Operation").size(12.0).color(theme::TEXT_DIM));
                    for (o, n) in [(Op::Join, "Join"), (Op::Cut, "Cut"), (Op::Intersect, "Intersect")] {
                        if widgets::pill(ui, n, c.op == o).clicked() {
                            c.op = o;
                            ch = true;
                        }
                    }
                });
                ui.label(RichText::new("Tool bodies").size(12.0).color(theme::TEXT_DIM));
                egui::ScrollArea::vertical().id_salt(id.with("tools")).max_height(120.0).show(ui, |ui| {
                    for (bid, n) in all.iter().filter(|b| Some(b.0) != c.target) {
                        let mut on = c.tools.contains(bid);
                        if ui.checkbox(&mut on, n).changed() {
                            if on {
                                c.tools.push(*bid);
                            } else {
                                c.tools.retain(|x| x != bid);
                            }
                            ch = true;
                        }
                    }
                });
                if c.op != Op::Join {
                    ch |= c.clearance.row(ui, id.with("c"), "Clearance", env);
                }
                ui.label(RichText::new("Clear at target positions (x, y, z; …)").size(12.0).color(theme::TEXT_DIM));
                ch |= ui
                    .add(egui::TextEdit::singleline(&mut c.positions).font(theme::mono(12.0)).hint_text("0, 0, 0; 0, 0, 14").desired_width(f32::INFINITY))
                    .changed();
                ch |= ui.checkbox(&mut c.keep_tools, "Keep tool bodies").changed();
                ui.label(widgets::muted("Runs on triangle meshes; the result is a mesh body unless every body is a solid."));
            }
            Body::Move(m) => {
                let all: Vec<(BodyId, String)> = cx.model.map(|o| o.bodies.iter().map(|b| (b.body.id, b.body.name.clone())).collect()).unwrap_or_default();
                ch |= body_combo(ui, id.with("b"), &mut m.body, &all, "pick a body");
                for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
                    ch |= m.t[i].row(ui, id.with(("t", i)), &format!("Move {a}"), env);
                }
                for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
                    ch |= m.rot[i].row(ui, id.with(("r", i)), &format!("Rotate {a} (deg)"), env);
                }
                ch |= m.scale.row(ui, id.with("s"), "Scale (mesh)", env);
                ch |= ui.checkbox(&mut m.copy, "Copy (keep the original)").changed();
                if m.copy {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Copy name").size(12.0).color(theme::TEXT_DIM));
                        ch |= ui.add(egui::TextEdit::singleline(&mut m.new_name).desired_width(132.0)).changed();
                    });
                }
            }
            Body::Mesh(m) => {
                ch |= m.scale.row(ui, id.with("s"), "Scale", env);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("File up axis").size(12.0).color(theme::TEXT_DIM));
                    for (y, n) in [(false, "Z"), (true, "Y")] {
                        if widgets::pill(ui, n, m.up_y == y).clicked() {
                            m.up_y = y;
                            ch = true;
                        }
                    }
                });
                for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
                    ch |= m.rot[i].row(ui, id.with(("r", i)), &format!("Rotate {a} (deg)"), env);
                }
                for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
                    ch |= m.t[i].row(ui, id.with(("t", i)), &format!("Move {a}"), env);
                }
                ch |= ui.checkbox(&mut m.reference, "Reference (never printed)").changed();
            }
            Body::Boss(b) => {
                ch |= body_combo(ui, id.with("b"), &mut b.body, &solids, "last solid");
                ch |= b.plane_z.row(ui, id.with("z"), "Base plane z", env);
                ui.label(
                    RichText::new("Centres (x, y; …) · click hole edges to add")
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
                ch |= ui
                    .add(
                        egui::TextEdit::multiline(&mut b.points)
                            .font(theme::mono(12.0))
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
                ch |= b.od.row(ui, id.with("od"), "Boss OD", env);
                ch |= b.hole.row(ui, id.with("h"), "Hole", env);
                ch |= b.height.row(ui, id.with("ht"), "Height", env);
                ch |= b.base_fillet.row(ui, id.with("bf"), "Base fillet", env);
                ch |= b.draft.row(ui, id.with("dr"), "Draft", env);
            }
            Body::Rib(r) => {
                ch |= sketch_combo(ui, id.with("sk"), &mut r.sketch, &sketches);
                ch |= r.thickness.row(ui, id.with("t"), "Thickness", env);
                ch |= r.height.row(ui, id.with("h"), "Height", env);
                ch |= r.draft.row(ui, id.with("d"), "Draft", env);
                ch |= ui.checkbox(&mut r.flip, "Flip direction").changed();
            }
            Body::Pattern(p) => {
                ch |= feature_checklist(ui, cx.data, &mut p.features);
                ui.horizontal(|ui| {
                    if widgets::pill(ui, "Linear", !p.circular).clicked() {
                        p.circular = false;
                        ch = true;
                    }
                    if widgets::pill(ui, "Circular (Z)", p.circular).clicked() {
                        p.circular = true;
                        ch = true;
                    }
                });
                if !p.circular {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Direction").size(12.0).color(theme::TEXT_DIM));
                        for (i, (n, _)) in AXES.iter().enumerate() {
                            if widgets::pill(ui, n, p.dir == i).clicked() {
                                p.dir = i;
                                ch = true;
                            }
                        }
                    });
                    ch |= p.spacing.row(ui, id.with("s"), "Spacing", env);
                } else {
                    ch |= p.total.row(ui, id.with("t"), "Total angle", env);
                }
                ch |= p.count.row(ui, id.with("c"), "Count", env);
            }
            Body::Mirror(m) => {
                ch |= feature_checklist(ui, cx.data, &mut m.features);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Plane").size(12.0).color(theme::TEXT_DIM));
                    for (i, (n, _)) in PLANES.iter().enumerate() {
                        if widgets::pill(ui, n, m.plane == i).clicked() {
                            m.plane = i;
                            ch = true;
                        }
                    }
                });
            }
            Body::Standoffs(s) => ch |= s.ui(ui, id, cx),
            Body::Joint(j) => ch |= j.ui(ui, id, cx),
            Body::Datum(dd) => ch |= dd.ui(ui, id, cx),
        }
        (ch, ev)
    }
}

fn parse_points(s: &str) -> Result<Vec<DVec2>, String> {
    let mut v = Vec::new();
    for part in s
        .split([';', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let xy: Vec<&str> = part.split(',').map(str::trim).collect();
        if xy.len() != 2 {
            return Err(format!("`{part}` is not `x, y`"));
        }
        let x = xy[0]
            .parse::<f64>()
            .map_err(|_| format!("bad x in `{part}`"))?;
        let y = xy[1]
            .parse::<f64>()
            .map_err(|_| format!("bad y in `{part}`"))?;
        v.push(DVec2::new(x, y));
    }
    if v.is_empty() {
        return Err("add at least one boss centre".into());
    }
    Ok(v)
}

fn boss_points_from_selection(m: Option<&Output>, sel: &[PickKey]) -> String {
    let mut v = Vec::new();
    for k in sel {
        if let (PickKind::Edge(e), Some(m)) = (k.kind, m) {
            if let Some((c, _, _)) = m
                .meshed(k.body)
                .and_then(|b| b.edges.get(e as usize))
                .and_then(|i| i.circle)
            {
                v.push(format!("{}, {}", fmt(c.x), fmt(c.y)));
            }
        }
    }
    v.join("; ")
}

fn op_row(ui: &mut Ui, op: &mut Op) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Operation").size(12.0).color(theme::TEXT_DIM));
        for (o, n) in [
            (Op::NewBody, "New body"),
            (Op::Join, "Join"),
            (Op::Cut, "Cut"),
            (Op::Intersect, "Intersect"),
        ] {
            if widgets::pill(ui, n, *op == o).clicked() {
                *op = o;
                ch = true;
            }
        }
    });
    ch
}

fn sketch_combo(
    ui: &mut Ui,
    id: egui::Id,
    cur: &mut Option<SketchId>,
    list: &[(SketchId, String)],
) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Sketch").size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = cur
                .and_then(|c| list.iter().find(|s| s.0 == c))
                .map(|s| s.1.clone())
                .unwrap_or_else(|| "pick a sketch".into());
            egui::ComboBox::from_id_salt(id)
                .selected_text(name)
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (sid, n) in list {
                        if ui.selectable_label(*cur == Some(*sid), n).clicked() {
                            *cur = Some(*sid);
                            ch = true;
                        }
                    }
                });
        });
    });
    ch
}

fn body_combo(
    ui: &mut Ui,
    id: egui::Id,
    cur: &mut Option<BodyId>,
    list: &[(BodyId, String)],
    none: &str,
) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Body").size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = cur
                .and_then(|c| list.iter().find(|s| s.0 == c))
                .map(|s| s.1.clone())
                .unwrap_or_else(|| none.into());
            egui::ComboBox::from_id_salt(id)
                .selected_text(name)
                .width(170.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_none(), none).clicked() {
                        *cur = None;
                        ch = true;
                    }
                    for (bid, n) in list {
                        if ui.selectable_label(*cur == Some(*bid), n).clicked() {
                            *cur = Some(*bid);
                            ch = true;
                        }
                    }
                });
        });
    });
    ch
}

fn face_list(
    ui: &mut Ui,
    label: &str,
    faces: &mut Vec<(PickKey, FaceSelector, String)>,
    kept: &mut Vec<FaceSelector>,
) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = faces.len() + kept.len();
            selected_chip(ui, &format!("{n} selected"));
        });
    });
    let mut remove = None;
    for (i, f) in faces.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&f.2).size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        faces.remove(i);
        ch = true;
    }
    let mut remove = None;
    for (i, f) in kept.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(selector_desc(f))
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        kept.remove(i);
        ch = true;
    }
    if faces.is_empty() && kept.is_empty() {
        ui.label(widgets::muted("Click faces in the view."));
    }
    ch
}

pub fn selector_desc(f: &FaceSelector) -> String {
    match f {
        FaceSelector::Extreme { normal, which } => {
            let dir = if normal.z > 0.5 {
                "top"
            } else if normal.z < -0.5 {
                "bottom"
            } else {
                "side"
            };
            match which {
                easycad_model::select::Extremum::Max => format!("{dir} face (extreme)"),
                easycad_model::select::Extremum::Min => format!("{dir} face (nearest)"),
            }
        }
        FaceSelector::Hint { normal, .. } => format!(
            "picked face ({:+.0}, {:+.0}, {:+.0})",
            normal.x, normal.y, normal.z
        ),
        FaceSelector::AllWithNormal { .. } => "all faces with a normal".into(),
        FaceSelector::Cylinders { radius, .. } => match radius {
            Some(r) => format!("cylinders ⌀{:.2}", 2.0 * r),
            None => "all cylinders".into(),
        },
    }
}

pub fn selected_chip(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::ACCENT_DEEP)
        .stroke(egui::Stroke::new(1.0, theme::ACCENT))
        .corner_radius(14.0)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0));
        });
}

fn feature_checklist(ui: &mut Ui, d: &DocData, sel: &mut Vec<FeatureId>) -> bool {
    let mut ch = false;
    ui.label(RichText::new("Features").size(12.0).color(theme::TEXT_DIM));
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .show(ui, |ui| {
            for f in &d.features {
                let mut on = sel.contains(&f.id);
                if ui
                    .checkbox(&mut on, format!("{} · {}", f.name, f.kind.label()))
                    .changed()
                {
                    if on {
                        sel.push(f.id);
                    } else {
                        sel.retain(|x| *x != f.id);
                    }
                    ch = true;
                }
            }
        });
    ch
}
