//! Feature dialogs: they float over the viewport (top-right), preview live through
//! the worker, bind every dimension to an expression, Enter = OK, Esc = cancel.

pub mod export;
pub mod fillet;
pub mod joint;
pub mod standoffs;

use casework_model::document::DocData;
use casework_model::feature::*;
use casework_model::geom::{Axis, Plane};
use casework_model::params::Expr;
use casework_model::process::{Fit, FitClass};
use casework_model::select::FaceSelector;
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
    Draft,
    Split,
    Pattern,
    Mirror,
    Boss,
    Rib,
    Standoffs,
    Joint,
}

impl DialogKind {
    pub fn title(self) -> &'static str {
        match self {
            DialogKind::Extrude => "Extrude",
            DialogKind::Revolve => "Revolve",
            DialogKind::Fillet => "Fillet",
            DialogKind::Chamfer => "Chamfer",
            DialogKind::Shell => "Shell",
            DialogKind::Draft => "Draft",
            DialogKind::Split => "Split body",
            DialogKind::Pattern => "Pattern",
            DialogKind::Mirror => "Mirror",
            DialogKind::Boss => "Screw boss",
            DialogKind::Rib => "Rib",
            DialogKind::Standoffs => "Standoffs from PCB",
            DialogKind::Joint => "Parting joint",
        }
    }
    /// Title-bar accent: modify = orange, enclosure = green, create = blue.
    pub fn accent(self) -> Color32 {
        match self {
            DialogKind::Fillet
            | DialogKind::Chamfer
            | DialogKind::Shell
            | DialogKind::Draft
            | DialogKind::Split => theme::ORANGE_SOFT,
            DialogKind::Boss | DialogKind::Rib | DialogKind::Standoffs | DialogKind::Joint => {
                theme::GREEN
            }
            _ => theme::ACCENT_LINK,
        }
    }
}

/// An expression input with an optional process fit.
#[derive(Clone, Debug)]
pub struct Field {
    pub text: String,
    pub fit: Option<Fit>,
}

impl Field {
    pub fn new(s: impl Into<String>) -> Self {
        Field {
            text: s.into(),
            fit: None,
        }
    }
    pub fn with_fit(s: impl Into<String>, fit: Fit) -> Self {
        Field {
            text: s.into(),
            fit: Some(fit),
        }
    }
    pub fn from_expr(e: &Expr, default_fit: Option<Fit>) -> Self {
        Field {
            text: e.0.clone(),
            fit: e.1.or(default_fit),
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
                        let o = expr_field(ui, id, &mut self.text, w, env, self.fit.as_mut());
                        changed = o.changed || o.fit_changed;
                    },
                );
            });
        });
        changed
    }
}

/// Engine default fit for a feature field (mirrors regen.rs; see GUI_NEEDS).
pub fn default_fit(kind: DialogKind, field: &str) -> Option<Fit> {
    match (kind, field) {
        (DialogKind::Boss | DialogKind::Standoffs, "hole_dia") => Some(Fit::hole(FitClass::Snug)),
        _ => None,
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

pub struct ExtrudeD {
    pub sketch: Option<SketchId>,
    pub distance: Field,
    pub symmetric: bool,
    pub reverse: bool,
    pub draft: Field,
    pub op: Op,
    pub target: Option<BodyId>,
}

pub struct RevolveD {
    pub sketch: Option<SketchId>,
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
    Draft(DraftD),
    Split(SplitD),
    Boss(BossD),
    Rib(RibD),
    Pattern(PatternD),
    Mirror(MirrorD),
    Standoffs(standoffs::StandoffsD),
    Joint(joint::JointD),
}

pub struct Dialog {
    pub kind: DialogKind,
    pub editing: Option<FeatureId>,
    pub name: String,
    pub body: Body,
    pub error: Option<String>,
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

fn sel_body(sel: &[PickKey], m: Option<&Output>) -> Option<BodyId> {
    sel.iter()
        .map(|k| k.body)
        .find(|b| m.and_then(|m| m.body(*b)).is_some_and(|b| !b.reference))
}

pub fn face_label(m: Option<&Output>, k: &PickKey) -> String {
    match k.kind {
        PickKind::Face(f) => m
            .and_then(|m| m.meshed(k.body))
            .and_then(|b| b.faces.get(f as usize))
            .map(crate::ui_viewport::face_desc)
            .unwrap_or_else(|| format!("face {f}")),
        _ => "face".into(),
    }
}

fn face_sel(m: Option<&Output>, k: &PickKey) -> Option<FaceSelector> {
    let PickKind::Face(f) = k.kind else {
        return None;
    };
    let info = m?.meshed(k.body)?.faces.get(f as usize)?;
    Some(FaceSelector::from_pick(info))
}

fn plane_offset(p: &Plane) -> f64 {
    p.origin.dot(p.normal)
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
            DialogKind::Extrude => Body::Extrude(ExtrudeD {
                sketch: last_sketch(d, true),
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
            }),
            DialogKind::Revolve => Body::Revolve(RevolveD {
                sketch: last_sketch(d, true),
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
            }),
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
        };
        Ok(Dialog {
            kind,
            editing: None,
            name: kind.title().into(),
            body,
            error: None,
        })
    }

    /// Open an existing feature for editing.
    pub fn edit(f: &Feature, cx: &BuildCx) -> Option<Dialog> {
        let d = &cx.data;
        let (kind, body) = match &f.kind {
            FeatureKind::Extrude {
                sketch,
                distance,
                symmetric,
                reverse,
                draft_deg,
                op,
                target,
                ..
            } => (
                DialogKind::Extrude,
                Body::Extrude(ExtrudeD {
                    sketch: Some(*sketch),
                    distance: Field::from_expr(distance, None),
                    symmetric: *symmetric,
                    reverse: *reverse,
                    draft: Field::from_expr(draft_deg, None),
                    op: Op::from(*op),
                    target: *target,
                }),
            ),
            FeatureKind::Revolve {
                sketch,
                axis,
                angle_deg,
                op,
                ..
            } => (
                DialogKind::Revolve,
                Body::Revolve(RevolveD {
                    sketch: Some(*sketch),
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
            FeatureKind::Draft {
                body,
                faces,
                neutral,
                angle_deg,
                ..
            } => (
                DialogKind::Draft,
                Body::Draft(DraftD {
                    body: *body,
                    faces: Vec::new(),
                    kept: faces.clone(),
                    angle: Field::from_expr(angle_deg, None),
                    neutral_z: Field::new(
                        crate::adapters::binding(d, f.id)
                            .map(String::from)
                            .unwrap_or_else(|| fmt(plane_offset(neutral))),
                    ),
                }),
            ),
            FeatureKind::Split {
                body,
                plane,
                new_name,
            } => (
                DialogKind::Split,
                Body::Split(SplitD {
                    body: *body,
                    z: Field::new(
                        crate::adapters::binding(d, f.id)
                            .map(String::from)
                            .unwrap_or_else(|| fmt(plane_offset(plane))),
                    ),
                    new_name: new_name.clone(),
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
                ..
            } => (
                DialogKind::Boss,
                Body::Boss(BossD {
                    body: *body,
                    plane_z: Field::new(
                        crate::adapters::binding(d, f.id)
                            .map(String::from)
                            .unwrap_or_else(|| fmt(plane_offset(plane))),
                    ),
                    points: points
                        .iter()
                        .map(|p| format!("{}, {}", fmt(p.x), fmt(p.y)))
                        .collect::<Vec<_>>()
                        .join("; "),
                    od: Field::from_expr(od, None),
                    hole: Field::from_expr(hole_dia, default_fit(DialogKind::Boss, "hole_dia")),
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
            _ => return None,
        };
        Some(Dialog {
            kind,
            editing: Some(f.id),
            name: f.name.clone(),
            body,
            error: None,
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
        match self.editing {
            Some(_) => format!("Edit {}", self.name),
            None => self.kind.title().into(),
        }
    }

    pub fn filter(&self) -> Option<Filter> {
        match self.kind {
            DialogKind::Fillet | DialogKind::Chamfer => Some(Filter::EDGES_ONLY),
            DialogKind::Shell | DialogKind::Draft => Some(Filter::FACES_ONLY),
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
        for (_, e) in self.bindings(&self.build(cx).unwrap_or_default()) {
            s.extend(crate::adapters::refs_of(&e));
        }
        if let Body::Joint(j) = &self.body {
            s.extend(j.refs());
        }
        s
    }

    /// Plane bindings for the built features (see adapters).
    pub fn bindings(&self, feats: &[Feature]) -> Vec<(FeatureId, String)> {
        let first = feats.first().map(|f| f.id);
        let lit = |f: &Field| f.text.trim().parse::<f64>().is_ok();
        let b = |f: &Field| {
            if lit(f) {
                String::new()
            } else {
                f.text.trim().to_string()
            }
        };
        match (&self.body, first) {
            (Body::Split(s), Some(id)) => vec![(id, b(&s.z))],
            (Body::Draft(s), Some(id)) => vec![(id, b(&s.neutral_z))],
            (Body::Boss(s), Some(id)) => vec![(id, b(&s.plane_z))],
            (Body::Standoffs(s), Some(id)) => s.bindings(id),
            _ => Vec::new(),
        }
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
            Body::Extrude(e) => one(FeatureKind::Extrude {
                sketch: e.sketch.ok_or("pick a sketch with a closed profile")?,
                profiles: ProfileSel::All,
                distance: e.distance.expr(None),
                symmetric: e.symmetric,
                reverse: e.reverse,
                draft_deg: e.draft.expr(None),
                op: e.op.to(),
                target: if e.op == Op::NewBody { None } else { e.target },
            }),
            Body::Revolve(r) => one(FeatureKind::Revolve {
                sketch: r.sketch.ok_or("pick a sketch")?,
                profiles: ProfileSel::All,
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
                    neutral: Plane::XY.offset(z),
                    pull: DVec3::Z,
                    angle_deg: s.angle.expr(None),
                })
            }
            Body::Split(s) => {
                let z = num(&s.z)?;
                one(FeatureKind::Split {
                    body: s.body,
                    plane: Plane::XY.offset(z),
                    new_name: s.new_name.clone(),
                })
            }
            Body::Boss(b) => {
                let z = num(&b.plane_z)?;
                let pts = parse_points(&b.points)?;
                one(FeatureKind::ScrewBoss {
                    body: b.body,
                    plane: Plane::XY.offset(z),
                    points: pts,
                    od: b.od.expr(None),
                    hole_dia: b.hole.expr(default_fit(DialogKind::Boss, "hole_dia")),
                    height: b.height.expr(None),
                    hole_depth: None,
                    base_fillet: b.base_fillet.expr(None),
                    draft_deg: b.draft.expr(None),
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
        }
    }

    pub fn on_preview(&mut self, out: &Output, model: Option<&Output>) {
        match &mut self.body {
            Body::Fillet(f) => f.on_preview(out, model, self.editing),
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
            if let casework_model::regen::FeatureStatus::Error(e) = &r.status {
                self.error = Some(e.clone());
            }
        }
    }

    /// A viewport click while the dialog is open. Returns true if consumed.
    pub fn on_pick(&mut self, key: PickKey, model: Option<&Output>) -> bool {
        match &mut self.body {
            Body::Fillet(f) => f.toggle(key, model),
            Body::Shell(ShellD { faces, body, .. }) | Body::Draft(DraftD { faces, body, .. }) => {
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
                        if self.editing.is_some() {
                            ui.label(widgets::muted(format!("· {}", self.name)));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(widgets::muted("Enter ✓ · Esc ✕"));
                        });
                    });
                });
            widgets::divider(ui);
            let changed = egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    let (c, e) = self.body_ui(ui, id, cx);
                    if let Some(e) = e {
                        ev = e;
                    }
                    c
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
            widgets::divider(ui);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if cx.regenerating {
                            ui.add(egui::Spinner::new().size(12.0).color(theme::TEXT_MUTED));
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
                ch |= sketch_combo(ui, id.with("sk"), &mut e.sketch, &sketches);
                ch |= e.distance.row(ui, id.with("d"), "Distance", env);
                ui.horizontal(|ui| {
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
                ch |= e.draft.row(ui, id.with("dr"), "Taper angle", env);
                ch |= op_row(ui, &mut e.op);
                if e.op != Op::NewBody {
                    ch |= body_combo(ui, id.with("t"), &mut e.target, &solids, "last solid");
                }
            }
            Body::Revolve(r) => {
                ch |= sketch_combo(ui, id.with("sk"), &mut r.sketch, &sketches);
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
                casework_model::select::Extremum::Max => format!("{dir} face (extreme)"),
                casework_model::select::Extremum::Min => format!("{dir} face (nearest)"),
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
