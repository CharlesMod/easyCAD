//! Construction-geometry dialogs: labelled pick slots (the active one is
//! highlighted, a viewport click fills it and advances), expression fields, flip.

use easycad_model::datum::{AxisDef, GeomRef, PlaneDef, PointDef};
use easycad_model::document::DocData;
use easycad_model::feature::{Feature, FeatureId, FeatureKind};
use egui::{RichText, Ui};

use super::{BuildCx, DialogCx, DialogEvent, Field};
use crate::construct::{infer, Cand};
use crate::datum_ui::{Accept, Cls, DatumKind, Hi, Picked};
use crate::theme;

#[derive(Clone, Debug)]
pub struct SlotVal {
    pub r: GeomRef,
    pub label: String,
    pub hi: Hi,
}

/// State of the one-click Construct tool: the clicked things and what they can make.
pub struct Auto {
    pub picks: Vec<Picked>,
    pub alts: Vec<Cand>,
    pub alt: usize,
    /// The value field takes the focus (and selects its text) this frame.
    pub focus: bool,
    /// Where the last click was (the popup sits next to it).
    pub anchor: Option<egui::Pos2>,
}

pub struct DatumD {
    pub auto: Option<Auto>,
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
            auto: None,
            kind,
            slots: vec![None; kind.slots().len()],
            active: Some(0),
            fields: kind.fields().iter().map(|f| Field::new(f.default)).collect(),
            flip: false,
        }
    }

    /// A fresh Construct tool; `picks` are things already selected.
    pub fn new_auto(picks: Vec<Picked>) -> Self {
        let mut d = DatumD::new(DatumKind::PointAt);
        d.active = None;
        d.auto = Some(Auto { picks, alts: Vec::new(), alt: 0, focus: false, anchor: None });
        d.recompute();
        d
    }

    /// Add a pick, or remove it when it is already there (the tool's click).
    pub fn toggle(&mut self, p: Picked, at: Option<egui::Pos2>) {
        let Some(a) = &mut self.auto else { return };
        match a.picks.iter().position(|q| q.r == p.r) {
            Some(i) => {
                a.picks.remove(i);
            }
            None => a.picks.push(p),
        }
        a.anchor = at.or(a.anchor);
        a.alt = 0;
        self.recompute();
    }

    pub fn clear_picks(&mut self) {
        if let Some(a) = &mut self.auto {
            a.picks.clear();
            a.alt = 0;
        }
        self.recompute();
    }

    pub fn has_picks(&self) -> bool {
        self.auto.as_ref().is_some_and(|a| !a.picks.is_empty())
    }

    pub fn cycle_alt(&mut self, dir: i32) {
        if let Some(a) = &mut self.auto {
            if a.alts.len() > 1 {
                a.alt = (a.alt as i64 + dir as i64).rem_euclid(a.alts.len() as i64) as usize;
                self.apply_alt(true);
            }
        }
    }

    fn recompute(&mut self) {
        let Some(a) = &mut self.auto else { return };
        a.alts = infer(&a.picks);
        a.alt = a.alt.min(a.alts.len().saturating_sub(1));
        self.apply_alt(true);
    }

    /// Make the chosen alternative the dialog's kind, slots and (reset) fields.
    fn apply_alt(&mut self, reset_fields: bool) {
        let Some(a) = &mut self.auto else { return };
        match a.alts.get(a.alt).cloned() {
            Some(c) => {
                self.kind = c.kind;
                self.slots = c
                    .order
                    .iter()
                    .map(|o| o.and_then(|i| a.picks.get(i)).map(|p| SlotVal { r: p.r.clone(), label: p.label.clone(), hi: p.hi.clone() }))
                    .collect();
                if reset_fields {
                    self.fields = c.kind.fields().iter().map(|_| Field::new(c.value)).collect();
                    a.focus = !self.fields.is_empty();
                }
            }
            None => {
                self.slots = vec![None; self.kind.slots().len()];
            }
        }
    }

    /// Everything the viewport should show as held: the tool's picks or the slots.
    pub fn held(&self) -> Vec<(&GeomRef, &Hi)> {
        match &self.auto {
            Some(a) => a.picks.iter().map(|p| (&p.r, &p.hi)).collect(),
            None => self.slots.iter().flatten().map(|s| (&s.r, &s.hi)).collect(),
        }
    }

    /// Every required slot is filled and the fields hold something.
    pub fn ready(&self) -> bool {
        let defs = self.kind.slots();
        defs.iter().enumerate().all(|(i, d)| d.optional || self.slots.get(i).is_some_and(|s| s.is_some()))
            && self.fields.iter().all(|f| !f.text.trim().is_empty())
    }

    /// The accept set of the active slot (`NONE` when no slot waits for a pick).
    pub fn active_accept(&self) -> Accept {
        if self.auto.is_some() {
            return Accept(63);
        }
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
        // the Construct tool reopens on it: its refs held, its kind fixed until a click
        let picks = me
            .slots
            .iter()
            .zip(me.kind.slots())
            .filter_map(|(s, d)| {
                let s = s.as_ref()?;
                let cls = if d.accept.has(Accept::PLANE) {
                    Cls::Plane
                } else if d.accept.has(Accept::POINT) {
                    Cls::Point
                } else if d.accept.has(Accept::CYLF) {
                    Cls::Cyl
                } else if d.accept.has(Accept::CIRCLE) {
                    Cls::Circle
                } else {
                    Cls::Line
                };
                Some(Picked { r: s.r.clone(), label: s.label.clone(), hi: s.hi.clone(), cls, p: glam::DVec3::ZERO, n: glam::DVec3::ZERO })
            })
            .collect();
        let order = {
            let mut k = 0;
            me.slots.iter().map(|s| s.as_ref().map(|_| { k += 1; k - 1 })).collect()
        };
        me.auto = Some(Auto {
            picks,
            alts: vec![Cand { kind: me.kind, order, value: "" }],
            alt: 0,
            focus: !me.fields.is_empty(),
            anchor: None,
        });
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

impl DatumD {
    /// The Construct tool's popup next to the last click. Returns the user's intent.
    pub fn popup(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx, error: Option<&str>) -> DialogEvent {
        let mut ev = DialogEvent::None;
        let Some(auto) = &self.auto else { return ev };
        let (n_alts, n_picks) = (auto.alts.len(), auto.picks.len());
        // keys first, so a focused field does not swallow them
        if n_alts > 1 && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            self.cycle_alt(1);
            ev = DialogEvent::Changed;
        } else if n_alts > 1 && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)) {
            self.cycle_alt(-1);
            ev = DialogEvent::Changed;
        }
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if esc {
            if self.has_picks() {
                self.clear_picks();
                return DialogEvent::Changed;
            }
            return DialogEvent::Cancel;
        }
        crate::widgets::card_frame().show(ui, |ui| {
            ui.set_max_width(270.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            let ready = self.ready() && n_alts > 0;
            if n_alts == 0 {
                ui.label(widgets_muted(crate::construct::nothing_hint(n_picks)));
                return;
            }
            let alts: Vec<(DatumKind, bool)> = {
                let a = self.auto.as_ref().unwrap();
                a.alts.iter().enumerate().map(|(i, c)| (c.kind, i == a.alt)).collect()
            };
            ui.label(RichText::new(self.kind.title()).strong().size(13.0));
            if alts.len() > 1 {
                ui.horizontal_wrapped(|ui| {
                    for (i, (k, on)) in alts.iter().enumerate() {
                        if crate::widgets::pill(ui, k.title(), *on).clicked() {
                            if let Some(a) = &mut self.auto {
                                a.alt = i;
                            }
                            self.apply_alt(true);
                            ev = DialogEvent::Changed;
                        }
                    }
                });
                ui.label(widgets_muted("Tab cycles the alternatives"));
            }
            let focus = self.auto.as_ref().is_some_and(|a| a.focus);
            for (i, fd) in self.kind.fields().iter().enumerate() {
                let fid = id.with(("construct", i));
                if focus && i == 0 {
                    ui.memory_mut(|m| m.request_focus(fid));
                }
                if self.fields[i].row(ui, fid, fd.label, &cx.env) {
                    ev = DialogEvent::Changed;
                }
                if focus && i == 0 {
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), fid) {
                        let n = self.fields[i].text.chars().count();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(n),
                        )));
                        st.store(ui.ctx(), fid);
                    }
                }
            }
            if let Some(a) = &mut self.auto {
                a.focus = false;
            }
            if self.kind.group() == crate::datum_ui::Group::Plane
                && ui.checkbox(&mut self.flip, RichText::new("Flip").size(12.0)).changed()
            {
                ev = DialogEvent::Changed;
            }
            if let Some(e) = error {
                ui.label(RichText::new(e).size(11.5).color(theme::ORANGE_SOFT));
            }
            ui.horizontal(|ui| {
                ui.label(widgets_muted("Enter \u{2713} \u{b7} Esc clears"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ready && crate::widgets::primary(ui, " OK ").clicked() {
                        ev = DialogEvent::Ok;
                    }
                });
            });
            if enter && ready {
                ev = DialogEvent::Ok;
            }
        });
        ev
    }
}

fn widgets_muted(s: &str) -> RichText {
    crate::widgets::muted(s)
}
