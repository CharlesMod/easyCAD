//! Construction-geometry dialogs: labelled pick slots (the active one is
//! highlighted, a viewport click fills it and advances), expression fields, flip.

use easycad_model::datum::{AxisDef, GeomRef, PlaneDef, PointDef};
use easycad_model::document::DocData;
use easycad_model::feature::{Feature, FeatureId, FeatureKind};
use egui::{RichText, Ui};

use super::{BuildCx, DialogCx, Field};
use crate::datum_ui::{Accept, DatumKind, Hi, Picked};
use crate::theme;

#[derive(Clone, Debug)]
pub struct SlotVal {
    pub r: GeomRef,
    pub label: String,
    pub hi: Hi,
}

pub struct DatumD {
    pub kind: DatumKind,
    pub slots: Vec<Option<SlotVal>>,
    /// The slot a viewport click fills; `None` once every required slot is full.
    pub active: Option<usize>,
    pub fields: Vec<Field>,
    pub flip: bool,
}

impl DatumD {
    pub fn new(kind: DatumKind) -> Self {
        DatumD {
            kind,
            slots: vec![None; kind.slots().len()],
            active: Some(0),
            fields: kind.fields().iter().map(|f| Field::new(f.default)).collect(),
            flip: false,
        }
    }

    /// The accept set of the active slot (`NONE` when no slot waits for a pick).
    pub fn active_accept(&self) -> Accept {
        self.active.and_then(|i| self.kind.slots().get(i).map(|s| s.accept)).unwrap_or(Accept::NONE)
    }

    fn next_empty(&self, from: usize) -> Option<usize> {
        let defs = self.kind.slots();
        (from..self.slots.len())
            .chain(0..from)
            .find(|&i| self.slots[i].is_none() && !defs[i].optional)
    }

    /// A viewport pick for the active slot; the next empty slot becomes active.
    pub fn fill(&mut self, p: Picked) {
        let Some(i) = self.active else { return };
        self.slots[i] = Some(SlotVal { r: p.r, label: p.label, hi: p.hi });
        self.active = self.next_empty(i + 1);
    }

    pub fn clear(&mut self, i: usize) {
        self.slots[i] = None;
        self.active = Some(i);
    }

    /// Prefill from the viewport selection (a face or edge the user had selected).
    pub fn prefill(&mut self, picks: Vec<Picked>) {
        for p in picks {
            let Some(i) = self.active else { break };
            if self.kind.slots()[i].accept.0 & accept_of(&p.r).0 != 0 {
                self.fill(p);
            }
        }
    }

    fn r(&self, i: usize) -> Result<GeomRef, String> {
        match self.slots.get(i).and_then(|s| s.as_ref()) {
            Some(s) => Ok(s.r.clone()),
            None => Err(self.kind.slots()[i].hint.to_string()),
        }
    }

    fn opt_r(&self, i: usize) -> Option<GeomRef> {
        self.slots.get(i).and_then(|s| s.as_ref()).map(|s| s.r.clone())
    }

    fn f(&self, i: usize) -> easycad_model::params::Expr {
        self.fields[i].expr(None)
    }

    pub fn field_texts(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.text.as_str()).collect()
    }

    pub fn build(&self, id: FeatureId, name: &str) -> Result<Feature, String> {
        use DatumKind::*;
        let kind = match self.kind {
            PlaneOffset => FeatureKind::DatumPlane {
                def: PlaneDef::Offset { base: self.r(0)?, distance: self.f(0) },
                flip: self.flip,
            },
            PlaneAngle => FeatureKind::DatumPlane {
                def: PlaneDef::Angle { axis: self.r(0)?, base: self.opt_r(1), angle: self.f(0) },
                flip: self.flip,
            },
            PlaneMid => FeatureKind::DatumPlane { def: PlaneDef::Midplane { a: self.r(0)?, b: self.r(1)? }, flip: self.flip },
            PlaneTwoLines => FeatureKind::DatumPlane { def: PlaneDef::TwoLines { a: self.r(0)?, b: self.r(1)? }, flip: self.flip },
            PlaneThreePoints => FeatureKind::DatumPlane {
                def: PlaneDef::ThreePoints { a: self.r(0)?, b: self.r(1)?, c: self.r(2)? },
                flip: self.flip,
            },
            PlaneTangent => FeatureKind::DatumPlane {
                def: PlaneDef::Tangent { face: self.r(0)?, base: self.opt_r(1), angle: self.f(0) },
                flip: self.flip,
            },
            PlaneNormal => FeatureKind::DatumPlane { def: PlaneDef::NormalAt { path: self.r(0)?, t: self.f(0) }, flip: self.flip },
            AxisCylinder => FeatureKind::DatumAxis { def: AxisDef::Cylinder { face: self.r(0)? } },
            AxisLine => FeatureKind::DatumAxis { def: AxisDef::Line { line: self.r(0)? } },
            AxisTwoPoints => FeatureKind::DatumAxis { def: AxisDef::TwoPoints { a: self.r(0)?, b: self.r(1)? } },
            AxisTwoPlanes => FeatureKind::DatumAxis { def: AxisDef::TwoPlanes { a: self.r(0)?, b: self.r(1)? } },
            AxisNormal => FeatureKind::DatumAxis { def: AxisDef::NormalAt { face: self.r(0)?, point: self.r(1)? } },
            PointAt => FeatureKind::DatumPoint { def: PointDef::At { point: self.r(0)? } },
            PointCenter => FeatureKind::DatumPoint { def: PointDef::Center { circle: self.r(0)? } },
            PointEdgePlane => FeatureKind::DatumPoint { def: PointDef::EdgePlane { edge: self.r(0)?, plane: self.r(1)? } },
            PointThreePlanes => FeatureKind::DatumPoint { def: PointDef::ThreePlanes { a: self.r(0)?, b: self.r(1)?, c: self.r(2)? } },
            PointTwoLines => FeatureKind::DatumPoint { def: PointDef::TwoLines { a: self.r(0)?, b: self.r(1)? } },
        };
        Ok(Feature { id, name: name.to_string(), suppressed: false, kind })
    }

    /// The dialog that edits an existing datum feature.
    pub fn from_feature(f: &Feature, d: &DocData) -> Option<Self> {
        use DatumKind::*;
        let mut me;
        let set = |me: &mut DatumD, i: usize, r: Option<&GeomRef>| {
            me.slots[i] = r.map(|r| SlotVal { label: crate::datum_ui::ref_label(d, r), r: r.clone(), hi: Hi::None });
        };
        match &f.kind {
            FeatureKind::DatumPlane { def, flip } => {
                match def {
                    PlaneDef::Offset { base, distance } => {
                        me = DatumD::new(PlaneOffset);
                        set(&mut me, 0, Some(base));
                        me.fields[0] = Field::from_expr(distance, None);
                    }
                    PlaneDef::Angle { axis, base, angle } => {
                        me = DatumD::new(PlaneAngle);
                        set(&mut me, 0, Some(axis));
                        set(&mut me, 1, base.as_ref());
                        me.fields[0] = Field::from_expr(angle, None);
                    }
                    PlaneDef::Midplane { a, b } => {
                        me = DatumD::new(PlaneMid);
                        set(&mut me, 0, Some(a));
                        set(&mut me, 1, Some(b));
                    }
                    PlaneDef::TwoLines { a, b } => {
                        me = DatumD::new(PlaneTwoLines);
                        set(&mut me, 0, Some(a));
                        set(&mut me, 1, Some(b));
                    }
                    PlaneDef::ThreePoints { a, b, c } => {
                        me = DatumD::new(PlaneThreePoints);
                        set(&mut me, 0, Some(a));
                        set(&mut me, 1, Some(b));
                        set(&mut me, 2, Some(c));
                    }
                    PlaneDef::Tangent { face, base, angle } => {
                        me = DatumD::new(PlaneTangent);
                        set(&mut me, 0, Some(face));
                        set(&mut me, 1, base.as_ref());
                        me.fields[0] = Field::from_expr(angle, None);
                    }
                    PlaneDef::NormalAt { path, t } => {
                        me = DatumD::new(PlaneNormal);
                        set(&mut me, 0, Some(path));
                        me.fields[0] = Field::from_expr(t, None);
                    }
                }
                me.flip = *flip;
            }
            FeatureKind::DatumAxis { def } => match def {
                AxisDef::Cylinder { face } => {
                    me = DatumD::new(AxisCylinder);
                    set(&mut me, 0, Some(face));
                }
                AxisDef::Line { line } => {
                    me = DatumD::new(AxisLine);
                    set(&mut me, 0, Some(line));
                }
                AxisDef::TwoPoints { a, b } => {
                    me = DatumD::new(AxisTwoPoints);
                    set(&mut me, 0, Some(a));
                    set(&mut me, 1, Some(b));
                }
                AxisDef::TwoPlanes { a, b } => {
                    me = DatumD::new(AxisTwoPlanes);
                    set(&mut me, 0, Some(a));
                    set(&mut me, 1, Some(b));
                }
                AxisDef::NormalAt { face, point } => {
                    me = DatumD::new(AxisNormal);
                    set(&mut me, 0, Some(face));
                    set(&mut me, 1, Some(point));
                }
            },
            FeatureKind::DatumPoint { def } => match def {
                PointDef::At { point } => {
                    me = DatumD::new(PointAt);
                    set(&mut me, 0, Some(point));
                }
                PointDef::Center { circle } => {
                    me = DatumD::new(PointCenter);
                    set(&mut me, 0, Some(circle));
                }
                PointDef::EdgePlane { edge, plane } => {
                    me = DatumD::new(PointEdgePlane);
                    set(&mut me, 0, Some(edge));
                    set(&mut me, 1, Some(plane));
                }
                PointDef::ThreePlanes { a, b, c } => {
                    me = DatumD::new(PointThreePlanes);
                    set(&mut me, 0, Some(a));
                    set(&mut me, 1, Some(b));
                    set(&mut me, 2, Some(c));
                }
                PointDef::TwoLines { a, b } => {
                    me = DatumD::new(PointTwoLines);
                    set(&mut me, 0, Some(a));
                    set(&mut me, 1, Some(b));
                }
            },
            _ => return None,
        }
        me.active = None;
        Some(me)
    }

    /// Slots and fields. Returns true when something changed.
    pub fn ui(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx) -> bool {
        let mut changed = false;
        let defs = self.kind.slots();
        for (i, def) in defs.iter().enumerate() {
            let active = self.active == Some(i);
            ui.horizontal(|ui| {
                ui.label(RichText::new(def.label).size(12.0).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.slots[i].is_some() && ui.small_button("×").on_hover_text("Clear").clicked() {
                        self.clear(i);
                        changed = true;
                    }
                    let (text, col) = match &self.slots[i] {
                        Some(s) => (s.label.clone(), theme::TEXT),
                        None if def.optional => (format!("{} (optional)", def.hint), theme::TEXT_MUTED),
                        None => (def.hint.to_string(), theme::TEXT_MUTED),
                    };
                    let stroke = if active {
                        egui::Stroke::new(1.5, theme::ACCENT)
                    } else {
                        egui::Stroke::new(1.0, theme::BORDER_INPUT)
                    };
                    let b = egui::Button::new(RichText::new(text).size(12.0).color(col))
                        .fill(if active { theme::ACCENT_BG } else { theme::INPUT })
                        .stroke(stroke)
                        .min_size(egui::vec2(200.0, 24.0));
                    if ui.add(b).on_hover_text("Click, then pick in the viewport").clicked() {
                        self.active = Some(i);
                    }
                });
            });
        }
        let env = &cx.env;
        for (i, fd) in self.kind.fields().iter().enumerate() {
            changed |= self.fields[i].row(ui, id.with(("datum", i)), fd.label, env);
        }
        if self.kind.group() == crate::datum_ui::Group::Plane
            && ui.checkbox(&mut self.flip, RichText::new("Flip").size(12.0)).on_hover_text("Reverse the plane's normal").changed()
        {
            changed = true;
        }
        changed
    }
}

/// What kind of slot a reference could fill (for prefilling from a selection).
fn accept_of(r: &GeomRef) -> Accept {
    match r {
        GeomRef::Face(_) => Accept::PLANE.or(Accept::CYLF).or(Accept::CIRCLE),
        GeomRef::Edge(_) => Accept::LINE.or(Accept::CIRCLE).or(Accept::CURVE),
        _ => Accept::NONE,
    }
}

/// Default name for a new datum: "Plane 1", "Axis 2" ...
pub fn default_name(kind: DatumKind, cx: &BuildCx) -> String {
    let stem = kind.stem();
    let n = cx
        .data
        .features
        .iter()
        .filter(|f| match (&f.kind, kind.group()) {
            (FeatureKind::DatumPlane { .. }, crate::datum_ui::Group::Plane) => true,
            (FeatureKind::DatumAxis { .. }, crate::datum_ui::Group::Axis) => true,
            (FeatureKind::DatumPoint { .. }, crate::datum_ui::Group::Point) => true,
            _ => false,
        })
        .count()
        + 1;
    format!("{stem} {n}")
}
