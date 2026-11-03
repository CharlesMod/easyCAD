//! Fillet / chamfer with edge sets (each its own radius expression) and the inline
//! self-heal report from the engine's `DressUpReport`: applied, reduced to the
//! largest size that fits, or failed with "Use r_max" / "Skip edge" actions.
//!
//! The engine takes one radius per feature, so each edge set becomes one Fillet
//! feature (committed together, one undo step). Report outcomes name edges on each
//! feature's *input* shape; the worker returns those intermediate bodies
//! (`prefixes`) so outcomes map back to the picked edges geometrically.

use casework_model::feature::*;
use casework_model::occt::{CurveKind, EdgeInfo, EdgeStatus, Strategy};
use casework_model::regen::FeatureStatus;
use casework_model::select::{EdgeSelector, Selector};
use egui::{RichText, Ui};
use glam::DVec3;

use super::{BuildCx, DialogCx, DialogEvent, Field};
use crate::pickstack::{PickKey, PickKind};
use crate::theme;
use crate::widgets::{self, Icon};
use crate::worker::Output;

#[derive(Clone, Debug)]
pub struct EdgeRef {
    pub key: Option<PickKey>,
    pub sel: EdgeSelector,
    pub label: String,
    pub midpoint: Option<DVec3>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EdgeResult {
    Applied,
    Reduced { requested: f64, applied: f64 },
    Failed { reason: String, max: Option<f64> },
}

#[derive(Clone, Debug)]
pub struct EdgeSet {
    pub name: Option<String>,
    pub edges: Vec<EdgeRef>,
    pub skipped: Vec<EdgeRef>,
    pub radius: Field,
    pub auto_reduce: bool,
    /// Per edge results from the last preview (same order as `edges`).
    pub results: Vec<Vec<EdgeResult>>,
    pub strategy: Option<Strategy>,
    pub healed: bool,
    pub status: Option<FeatureStatus>,
}

impl EdgeSet {
    fn new(radius: &str) -> Self {
        EdgeSet {
            name: None,
            edges: Vec::new(),
            skipped: Vec::new(),
            radius: Field::new(radius),
            auto_reduce: false,
            results: Vec::new(),
            strategy: None,
            healed: false,
            status: None,
        }
    }

    pub fn label(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        auto_name(&self.edges)
    }
}

pub fn auto_name(edges: &[EdgeRef]) -> String {
    match edges.len() {
        0 => "No edges yet".into(),
        1 => edges[0].label.clone(),
        n => {
            let vertical = edges.iter().all(|e| e.label.starts_with("Vertical"));
            let circ = edges.iter().all(|e| e.label.starts_with("Circular"));
            if vertical {
                format!("Vertical corners ×{n}")
            } else if circ {
                format!("Circular edges ×{n}")
            } else {
                format!("Edges ×{n}")
            }
        }
    }
}

pub fn edge_label(e: &EdgeInfo) -> String {
    match e.kind {
        CurveKind::Line if e.tangent.z.abs() > 0.99 => format!("Vertical edge · {:.1}", e.length),
        CurveKind::Line if e.tangent.z.abs() < 0.01 => format!("Horizontal edge · {:.1}", e.length),
        CurveKind::Line => format!("Edge · {:.1}", e.length),
        CurveKind::Circle => format!(
            "Circular edge · ⌀{:.2}",
            e.circle.map(|c| c.2 * 2.0).unwrap_or(0.0)
        ),
        _ => format!("Curved edge · {:.1}", e.length),
    }
}

fn sel_label(s: &EdgeSelector) -> String {
    match s {
        EdgeSelector::Hint { .. } => "Picked edge".into(),
        EdgeSelector::OfFace(_) => "Edges of a face".into(),
        EdgeSelector::Parallel { dir } if dir.z.abs() > 0.99 => "Vertical edges (all)".into(),
        EdgeSelector::Parallel { .. } => "Parallel edges".into(),
        EdgeSelector::Between(..) => "Edges between faces".into(),
        EdgeSelector::Circles { radius, .. } => format!("Circular edges ⌀{:.2}", radius * 2.0),
    }
}

pub struct FilletD {
    pub chamfer: bool,
    pub body: Option<BodyId>,
    pub sets: Vec<EdgeSet>,
    pub active: usize,
    pub tangent_chains: bool,
    pub setback: bool,
    pub auto_heal: bool,
    /// Feature report says a stored edge reference no longer matches.
    pub unresolved: Option<(Selector, Option<Selector>, f64)>,
}

impl FilletD {
    pub fn new(chamfer: bool, model: Option<&Output>, sel: &[PickKey]) -> Self {
        let mut set = EdgeSet::new(if chamfer { "0.5" } else { "1" });
        let mut body = None;
        for k in sel {
            if let Some(r) = edge_ref(model, *k) {
                body = Some(k.body);
                set.edges.push(r);
            }
        }
        FilletD {
            chamfer,
            body,
            sets: vec![set],
            active: 0,
            tangent_chains: true,
            setback: false,
            auto_heal: true,
            unresolved: None,
        }
    }

    pub fn from_feature(f: &Feature) -> Self {
        let (chamfer, body, edges, radius, auto_reduce) = match &f.kind {
            FeatureKind::Fillet {
                body,
                edges,
                radius,
                auto_reduce,
            } => (false, *body, edges, radius, *auto_reduce),
            FeatureKind::Chamfer {
                body,
                edges,
                distance,
                auto_reduce,
            } => (true, *body, edges, distance, *auto_reduce),
            _ => unreachable!(),
        };
        let mut set = EdgeSet::new(&radius.0);
        set.radius.fit = radius.1;
        set.auto_reduce = auto_reduce;
        set.name = Some(f.name.clone()).filter(|n| n != "Fillet" && n != "Chamfer");
        set.edges = edges
            .iter()
            .map(|s| EdgeRef {
                key: None,
                sel: s.clone(),
                label: sel_label(s),
                midpoint: hint_mid(s),
            })
            .collect();
        FilletD {
            chamfer,
            body,
            sets: vec![set],
            active: 0,
            tangent_chains: true,
            setback: false,
            auto_heal: true,
            unresolved: None,
        }
    }

    pub fn picked(&self) -> Vec<PickKey> {
        self.sets
            .iter()
            .flat_map(|s| s.edges.iter().filter_map(|e| e.key))
            .collect()
    }

    pub fn toggle(&mut self, key: PickKey, model: Option<&Output>) -> bool {
        if !matches!(key.kind, PickKind::Edge(_)) {
            return false;
        }
        for s in &mut self.sets {
            if let Some(i) = s.edges.iter().position(|e| e.key == Some(key)) {
                s.edges.remove(i);
                return true;
            }
        }
        let Some(r) = edge_ref(model, key) else {
            return false;
        };
        if self.body.is_some_and(|b| b != key.body) {
            self.sets.clear();
            self.sets
                .push(EdgeSet::new(if self.chamfer { "0.5" } else { "1" }));
            self.active = 0;
        }
        self.body = Some(key.body);
        // a re-pick replaces the unresolved reference
        if let Some((Selector::Edge(old), _, _)) = self.unresolved.take() {
            for s in &mut self.sets {
                if let Some(e) = s.edges.iter_mut().find(|e| e.sel == old) {
                    *e = r;
                    return true;
                }
            }
        }
        let a = self.active.min(self.sets.len() - 1);
        self.sets[a].edges.push(r);
        true
    }

    pub fn build(
        &self,
        cx: &BuildCx,
        editing: Option<FeatureId>,
        name: &str,
    ) -> Result<Vec<Feature>, String> {
        let mut out = Vec::new();
        for s in self.sets.iter().filter(|s| !s.edges.is_empty()) {
            let id = cx.id(editing, out.len());
            let edges: Vec<EdgeSelector> = s.edges.iter().map(|e| e.sel.clone()).collect();
            let radius = s.radius.expr(None);
            let kind = if self.chamfer {
                FeatureKind::Chamfer {
                    body: self.body,
                    edges,
                    distance: radius,
                    auto_reduce: s.auto_reduce,
                }
            } else {
                FeatureKind::Fillet {
                    body: self.body,
                    edges,
                    radius,
                    auto_reduce: s.auto_reduce,
                }
            };
            let fname = match (&s.name, out.len()) {
                (Some(n), _) => n.clone(),
                (None, 0) => name.to_string(),
                (None, i) => format!("{name} {}", i + 1),
            };
            out.push(Feature {
                id,
                name: fname,
                suppressed: false,
                kind,
            });
        }
        if out.is_empty() {
            return Err("pick edges in the view".into());
        }
        Ok(out)
    }

    /// Map the preview's dress-up reports back onto the picked edges.
    pub fn on_preview(&mut self, out: &Output, model: Option<&Output>, editing: Option<FeatureId>) {
        let _ = model;
        let base = out.prefix_bodies.keys().min().copied().unwrap_or(0);
        let next = out.data.features.get(base).map(|f| f.id);
        let mut k = 0usize;
        for s in self.sets.iter_mut() {
            if s.edges.is_empty() {
                s.results.clear();
                continue;
            }
            let fid = out.data.features.get(base + k).map(|f| f.id);
            let _ = (next, editing);
            let input = out.prefix_bodies.get(&(base + k));
            k += 1;
            let Some(fid) = fid else { continue };
            let Some(rep) = out.result.report(fid) else {
                continue;
            };
            s.status = Some(rep.status.clone());
            s.results = vec![Vec::new(); s.edges.len()];
            let Some(dr) = &rep.dressup else { continue };
            s.strategy = Some(dr.strategy);
            s.healed = dr.healed;
            // input shape's edge infos
            let infos: Vec<EdgeInfo> = input
                .and_then(|bodies| {
                    bodies
                        .iter()
                        .find(|b| Some(b.id) == self.body)
                        .or_else(|| bodies.iter().rev().find(|b| !b.reference))
                })
                .and_then(|b| b.shape().edges().ok())
                .unwrap_or_default();
            for o in &dr.outcomes {
                let r = match &o.status {
                    EdgeStatus::Applied => EdgeResult::Applied,
                    EdgeStatus::AppliedReduced { requested, applied } => EdgeResult::Reduced {
                        requested: *requested,
                        applied: *applied,
                    },
                    EdgeStatus::Failed { reason, max_size } => EdgeResult::Failed {
                        reason: reason.clone(),
                        max: *max_size,
                    },
                };
                // nearest picked edge by midpoint (semantic selectors collect everything)
                let mid = infos.get(o.edge as usize).map(|e| e.midpoint);
                let idx = match mid {
                    Some(m) => s
                        .edges
                        .iter()
                        .enumerate()
                        .filter_map(|(i, e)| e.midpoint.map(|p| (i, p.distance(m))))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .filter(|(_, d)| *d < 1.0)
                        .map(|(i, _)| i)
                        .or_else(|| s.edges.iter().position(|e| e.midpoint.is_none())),
                    None => None,
                }
                .unwrap_or(0);
                if let Some(v) = s.results.get_mut(idx) {
                    v.push(r);
                }
            }
            if let FeatureStatus::Unresolved(u) = &rep.status {
                self.unresolved = Some((u.selector.clone(), u.best_candidate.clone(), u.score));
            }
        }
    }

    fn totals(&self) -> (usize, usize, usize, usize) {
        let (mut done, mut total, mut pending, mut skipped) = (0, 0, 0, 0);
        for s in &self.sets {
            for (i, _) in s.edges.iter().enumerate() {
                let rs = s.results.get(i).cloned().unwrap_or_default();
                let n = rs.len().max(1);
                total += n;
                done += rs
                    .iter()
                    .filter(|r| !matches!(r, EdgeResult::Failed { .. }))
                    .count();
                pending += rs
                    .iter()
                    .filter(|r| matches!(r, EdgeResult::Failed { .. }))
                    .count();
                if rs.is_empty() {
                    done += 0;
                }
            }
            skipped += s.skipped.len();
            total += s.skipped.len();
        }
        (done, total, pending, skipped)
    }

    pub fn ui(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx) -> (bool, Option<DialogEvent>) {
        let mut ch = false;
        let mut ev = None;
        let env = &cx.env;
        let n_edges: usize = self.sets.iter().map(|s| s.edges.len()).sum();
        let n_sets = self.sets.iter().filter(|s| !s.edges.is_empty()).count();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Edges").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                super::selected_chip(
                    ui,
                    &format!(
                        "{n_edges} selected · {n_sets} set{}",
                        if n_sets == 1 { "" } else { "s" }
                    ),
                );
            });
        });
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut self.tangent_chains, "Extend to tangent chains"),
        )
        .on_disabled_hover_text("The engine always extends to tangent chains.");
        // per-set radius rows
        let mut new_set = false;
        let mut remove_set = None;
        let n = self.sets.len();
        for (i, s) in self.sets.iter_mut().enumerate() {
            let active = i == self.active;
            ui.horizontal(|ui| {
                let label = s.label();
                let warn = s
                    .results
                    .iter()
                    .flatten()
                    .any(|r| matches!(r, EdgeResult::Failed { .. }));
                let col = if active {
                    theme::TEXT_STRONG
                } else {
                    theme::TEXT
                };
                let r = ui.add(
                    egui::Label::new(RichText::new(label).size(12.0).color(col))
                        .sense(egui::Sense::click())
                        .truncate(),
                );
                if r.on_hover_text("Click to add new picks to this set")
                    .clicked()
                {
                    self.active = i;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if n > 1 && ui.small_button("✕").on_hover_text("Remove set").clicked() {
                        remove_set = Some(i);
                    }
                    let fid = id.with(("r", i));
                    let o = ui
                        .scope(|ui| {
                            if warn {
                                ui.visuals_mut().widgets.inactive.bg_stroke =
                                    egui::Stroke::new(1.0, theme::ORANGE);
                            }
                            widgets::expr_field(ui, fid, &mut s.radius.text, 120.0, env, None)
                        })
                        .inner;
                    ch |= o.changed;
                    if active && n > 1 {
                        ui.label(RichText::new("●").size(8.0).color(theme::ACCENT));
                    }
                });
            });
        }
        if let Some(i) = remove_set {
            let s = self.sets.remove(i);
            if self.sets.is_empty() {
                self.sets.push(s);
            } else {
                self.active = self.active.min(self.sets.len() - 1);
                ch = true;
            }
        }
        ui.horizontal(|ui| {
            if ui
                .small_button("+ New edge set")
                .on_hover_text("Following picks go into a set with its own radius")
                .clicked()
            {
                new_set = true;
            }
            ui.label(widgets::muted(
                "Click edges in the view. Tab cycles hidden ones.",
            ));
        });
        if new_set {
            let r = self.sets[self.active].radius.text.clone();
            self.sets.push(EdgeSet::new(&r));
            self.active = self.sets.len() - 1;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Corner type").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled(
                    false,
                    egui::Button::new(RichText::new("Setback").size(12.0)).frame(false),
                )
                .on_disabled_hover_text("Setback corners need engine support (GUI_NEEDS).");
                widgets::seg(ui, "Rolling ball", true);
            });
        });

        // unresolved reference
        if let Some((_, best, score)) = self.unresolved.clone() {
            egui::Frame::new()
                .fill(theme::ORANGE_CARD)
                .stroke(egui::Stroke::new(1.0, theme::ORANGE))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.label(RichText::new("An edge reference no longer matches · re-pick").color(theme::ORANGE_SOFT));
                    ui.label(widgets::muted(format!("Closest candidate scores {score:.2} (accepted ≤ 1). Click the right edge, or:")));
                    if let Some(b) = best {
                        if widgets::button(ui, "Use the closest candidate").clicked() {
                            if let (Some((Selector::Edge(old), _, _)), Selector::Edge(new)) = (self.unresolved.take(), b) {
                                for s in &mut self.sets {
                                    for e in &mut s.edges {
                                        if e.sel == old {
                                            e.sel = new.clone();
                                            e.label = sel_label(&new);
                                            e.midpoint = hint_mid(&new);
                                        }
                                    }
                                }
                                ch = true;
                            }
                        }
                    }
                });
        }

        widgets::divider(ui);
        // ---- self-heal report
        let (done, total, pending, skipped) = self.totals();
        let have = self.sets.iter().any(|s| !s.results.is_empty());
        ui.horizontal(|ui| {
            ui.label(RichText::new("SELF-HEAL REPORT").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !have {
                    ui.label(widgets::muted(if cx.regenerating {
                        "running…"
                    } else {
                        "pick edges"
                    }));
                } else if pending > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{done} of {total} edges · {pending} need{} you",
                            if pending == 1 { "s" } else { "" }
                        ))
                        .size(12.0)
                        .color(theme::ORANGE_SOFT),
                    );
                } else if skipped > 0 {
                    ui.label(
                        RichText::new(format!("{done} of {total} edges · {skipped} skipped"))
                            .size(12.0)
                            .color(theme::GREEN),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("{done} of {total} edges"))
                            .size(12.0)
                            .color(theme::GREEN),
                    );
                }
            });
        });
        let word = if self.chamfer { "chamfer" } else { "fillet" };
        let mut actions: Vec<(usize, usize, Action)> = Vec::new();
        for (si, s) in self.sets.iter().enumerate() {
            if s.edges.is_empty() {
                continue;
            }
            let r_txt = match crate::widgets::eval(&s.radius.text, cx.env.values) {
                Ok(v) => crate::units::len(v),
                Err(_) => s.radius.text.clone(),
            };
            let failing: Vec<usize> = (0..s.edges.len())
                .filter(|i| {
                    s.results
                        .get(*i)
                        .is_some_and(|r| r.iter().any(|x| matches!(x, EdgeResult::Failed { .. })))
                })
                .collect();
            let reduced: Vec<(usize, f64)> = (0..s.edges.len())
                .filter_map(|i| {
                    s.results.get(i)?.iter().find_map(|x| {
                        if let EdgeResult::Reduced { applied, .. } = x {
                            Some((i, *applied))
                        } else {
                            None
                        }
                    })
                })
                .collect();
            let have_results = s.results.iter().any(|r| !r.is_empty());
            if let (Some(FeatureStatus::Error(e)), false) = (&s.status, have_results) {
                report_row(
                    ui,
                    Icon::Warn,
                    theme::ORANGE_SOFT,
                    &format!("{} · r {r_txt}", s.label()),
                    e,
                );
                continue;
            }
            // applied part of the set
            let ok_n = s.edges.len() - failing.len() - reduced.len();
            if ok_n > 0 && s.results.iter().any(|r| !r.is_empty()) {
                let note = match s.strategy {
                    Some(Strategy::AllAtOnce) => "applied in one pass".to_string(),
                    Some(Strategy::ByChains) => {
                        "failed together, so it was re-run chain by chain. Fixed automatically."
                            .to_string()
                    }
                    Some(Strategy::Individual) => {
                        "re-run edge by edge to find what fits. Fixed automatically.".to_string()
                    }
                    _ => "applied".to_string(),
                };
                let note = if s.healed {
                    format!("{note} · merged coplanar faces first")
                } else {
                    note
                };
                let title = if failing.is_empty() && reduced.is_empty() {
                    s.label()
                } else {
                    format!("{} other edge{}", ok_n, if ok_n == 1 { "" } else { "s" })
                };
                report_row(
                    ui,
                    Icon::Check,
                    theme::GREEN,
                    &format!("{title} · r {r_txt}"),
                    &note,
                );
            }
            for (ei, applied) in &reduced {
                let e = &s.edges[*ei];
                let nm = if s.edges.len() == 1 {
                    s.label()
                } else {
                    e.label.clone()
                };
                ui.horizontal_top(|ui| {
                    widgets::icon(ui, Icon::Check, 16.0, theme::GREEN);
                    ui.vertical(|ui| {
                        ui.label(format!("{nm} · r {applied:.2} (clamped)"));
                        ui.label(widgets::muted(format!("Saved as min({r_txt}, fits). It grows back to {r_txt} automatically if there is room again.")));
                        if widgets::button(ui, "Undo choice").clicked() {
                            actions.push((si, *ei, Action::UndoReduce));
                        }
                    });
                });
            }
            let groups: Vec<Vec<usize>> = if failing.len() > 1 {
                vec![failing.clone()]
            } else {
                failing.iter().map(|i| vec![*i]).collect()
            };
            for group in &groups {
                let ei = &group[0];
                let all_of_set = group.len() == s.edges.len();
                let e = &s.edges[*ei];
                let nm = if all_of_set || s.edges.len() == 1 {
                    s.label()
                } else if group.len() > 1 {
                    format!("{} edges", group.len())
                } else {
                    e.label.clone()
                };
                let fails: Vec<(String, Option<f64>)> = group
                    .iter()
                    .flat_map(|i| s.results[*i].iter())
                    .filter_map(|x| {
                        if let EdgeResult::Failed { reason, max } = x {
                            Some((reason.clone(), *max))
                        } else {
                            None
                        }
                    })
                    .collect();
                let reason = fails.first().map(|f| f.0.clone()).unwrap_or_default();
                let max = fails
                    .iter()
                    .filter_map(|f| f.1)
                    .reduce(f64::min)
                    .filter(|_| fails.iter().all(|f| f.1.is_some()));
                let others = total.saturating_sub(group.len());
                egui::Frame::new()
                    .fill(theme::ORANGE_CARD)
                    .stroke(egui::Stroke::new(1.0, theme::ORANGE))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            widgets::icon(ui, Icon::Warn, 16.0, theme::ORANGE_SOFT);
                            ui.vertical(|ui| {
                                ui.label(format!("{nm} · r {r_txt} doesn't fit"));
                                let rest = match others {
                                    0 => String::new(),
                                    1 => " The other edge is kept either way.".to_string(),
                                    n => format!(" The other {n} edges are kept either way."),
                                };
                                let msg = match max {
                                    Some(m) => format!("The largest {word} that fits is {m:.2}.{rest}"),
                                    None => format!("No {word} size works here ({reason}).{rest}"),
                                };
                                ui.label(RichText::new(msg).size(11.0).color(theme::hex(0xc9b8a8)));
                                ui.horizontal_wrapped(|ui| {
                                    if let Some(m) = max {
                                        if widgets::primary(ui, &format!("Use {m:.2}")).clicked() {
                                            actions.push((si, *ei, if group.len() > 1 { Action::UseMaxGroup } else { Action::UseMax }));
                                        }
                                    }
                                    if widgets::button(ui, if group.len() > 1 { "Skip edges" } else { "Skip edge" }).clicked() {
                                        actions.push((si, *ei, if group.len() > 1 { Action::SkipGroup } else { Action::Skip }));
                                    }
                                    if widgets::button(ui, "Zoom to").clicked() {
                                        actions.push((si, *ei, Action::Zoom));
                                    }
                                });
                            });
                        });
                    });
            }
            for (ki, e) in s.skipped.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    widgets::icon(ui, Icon::Check, 16.0, theme::TEXT_MUTED);
                    ui.vertical(|ui| {
                        ui.label(format!("{} · skipped", e.label));
                        ui.label(widgets::muted(
                            "Kept in this dialog's report; the edge is left sharp.",
                        ));
                        if widgets::button(ui, "Undo choice").clicked() {
                            actions.push((si, ki, Action::Unskip));
                        }
                    });
                });
            }
        }
        for (si, ei, a) in actions {
            ch = true;
            match a {
                Action::UseMax => {
                    let s = &mut self.sets[si];
                    if s.edges.len() == 1 {
                        s.auto_reduce = true;
                    } else {
                        let e = s.edges.remove(ei);
                        let mut ns = EdgeSet::new(&s.radius.text);
                        ns.auto_reduce = true;
                        ns.name = Some(e.label.clone());
                        ns.edges.push(e);
                        self.sets.push(ns);
                    }
                }
                Action::UndoReduce => self.sets[si].auto_reduce = false,
                Action::UseMaxGroup => {
                    // every failing edge of the set takes its largest workable size
                    let s = &mut self.sets[si];
                    let failing: Vec<usize> = (0..s.edges.len())
                        .filter(|i| {
                            s.results.get(*i).is_some_and(|r| {
                                r.iter().any(|x| matches!(x, EdgeResult::Failed { .. }))
                            })
                        })
                        .collect();
                    if failing.len() == s.edges.len() {
                        s.auto_reduce = true;
                    } else {
                        let mut ns = EdgeSet::new(&s.radius.text);
                        ns.auto_reduce = true;
                        ns.name = Some(format!("{} (clamped)", s.label()));
                        for i in failing.iter().rev() {
                            ns.edges.insert(0, s.edges.remove(*i));
                        }
                        self.sets.push(ns);
                    }
                }
                Action::SkipGroup => {
                    let s = &mut self.sets[si];
                    let failing: Vec<usize> = (0..s.edges.len())
                        .filter(|i| {
                            s.results.get(*i).is_some_and(|r| {
                                r.iter().any(|x| matches!(x, EdgeResult::Failed { .. }))
                            })
                        })
                        .collect();
                    for i in failing.iter().rev() {
                        let e = s.edges.remove(*i);
                        s.skipped.push(e);
                    }
                }
                Action::Skip => {
                    let e = self.sets[si].edges.remove(ei);
                    self.sets[si].skipped.push(e);
                    if self.sets[si].edges.is_empty() && self.sets.len() > 1 {
                        let sk = self.sets.remove(si).skipped;
                        self.sets[0].skipped.extend(sk);
                    }
                }
                Action::Unskip => {
                    let e = self.sets[si].skipped.remove(ei);
                    self.sets[si].edges.push(e);
                }
                Action::Zoom => {
                    ch = false;
                    if let Some(m) = self.sets[si].edges.get(ei).and_then(|e| e.midpoint) {
                        ev = Some(DialogEvent::ZoomTo(m.as_vec3()));
                    }
                }
            }
            for s in &mut self.sets {
                s.results.clear();
            }
        }
        widgets::divider(ui);
        ui.checkbox(&mut self.auto_heal, "Auto-heal");
        ui.label(widgets::muted(
            "Merges coplanar faces first, then retries by tangent chain, re-orders failed sets and finds the largest radius that fits. You always get a valid solid.",
        ));
        (ch, ev)
    }
}

#[derive(Clone, Copy)]
enum Action {
    UseMax,
    UseMaxGroup,
    SkipGroup,
    UndoReduce,
    Skip,
    Unskip,
    Zoom,
}

fn report_row(ui: &mut Ui, icon: Icon, color: egui::Color32, title: &str, note: &str) {
    ui.horizontal_top(|ui| {
        widgets::icon(ui, icon, 16.0, color);
        ui.vertical(|ui| {
            ui.label(title);
            ui.label(widgets::muted(note));
        });
    });
}

fn hint_mid(s: &EdgeSelector) -> Option<DVec3> {
    match s {
        EdgeSelector::Hint { midpoint, .. } => Some(*midpoint),
        _ => None,
    }
}

pub fn edge_ref(model: Option<&Output>, k: PickKey) -> Option<EdgeRef> {
    let PickKind::Edge(e) = k.kind else {
        return None;
    };
    let b = model?.meshed(k.body)?;
    if b.body.reference {
        return None;
    }
    let info = b.edges.get(e as usize)?;
    Some(EdgeRef {
        key: Some(k),
        sel: EdgeSelector::from_pick(info),
        label: edge_label(info),
        midpoint: Some(info.midpoint),
    })
}
