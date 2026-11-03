//! Interference tool: every pair of placed bodies that overlap, with the common
//! volume (computed off the UI thread on the shown, solved assembly). The selected
//! pair is drawn red and Zoom to frames it.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use easycad_model::assembly::{InterferenceOptions, InterferenceReport};
use egui::{pos2, vec2, Frame, Margin, Rect, RichText, Sense, Stroke, Ui};
use glam::Vec3;

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;
use crate::worker::{ctx_key, CtxBody, Output};

#[derive(Default)]
pub struct InterferenceUi {
    pub report: Option<InterferenceReport>,
    /// The overlap solid of each pair (GUI_NEEDS #19), meshed off the UI thread.
    pub overlaps: Vec<Option<Arc<easycad_model::occt::Mesh>>>,
    pub job: Option<Receiver<Job>>,
    /// Highlighted pair (index into `report.pairs`); `None` highlights every pair.
    pub sel: Option<usize>,
    /// Model id the report was computed on.
    pub model_id: u64,
    pub include_reference: bool,
}

impl InterferenceUi {
    /// Overlap solids to draw (world frame): the selected pair's, or all.
    pub fn shown_overlaps(&self) -> Vec<Arc<easycad_model::occt::Mesh>> {
        self.overlaps
            .iter()
            .enumerate()
            .filter(|(i, _)| self.sel.is_none_or(|s| s == *i))
            .filter_map(|(_, m)| m.clone())
            .collect()
    }

    pub fn highlighted(&self, c: &CtxBody) -> bool {
        let Some(r) = &self.report else { return false };
        let hit = |p: &easycad_model::assembly::Interference| {
            [&p.a, &p.b].iter().any(|i| ctx_key(&i.path, i.body) == c.key)
        };
        match self.sel {
            Some(i) => r.pairs.get(i).is_some_and(hit),
            None => r.pairs.iter().any(hit),
        }
    }
}

impl EasyCadApp {
    /// Run the interference check on the assembly as shown (mates solved).
    pub fn check_interference(&mut self) {
        let Some(m) = self.model.clone() else { return };
        if m.asm.is_none() {
            self.warn("Interference needs an assembly (two or more components)");
            return;
        }
        let include_reference = self.asm.interference.as_ref().map(|i| i.include_reference).unwrap_or(true);
        self.asm.interference = Some(InterferenceUi {
            job: Some(spawn(m.clone(), include_reference)),
            model_id: m.id,
            include_reference,
            ..Default::default()
        });
    }

    pub fn poll_interference(&mut self) {
        let Some(i) = self.asm.interference.as_mut() else { return };
        let Some(rx) = &i.job else { return };
        if let Ok(r) = rx.try_recv() {
            i.job = None;
            let (r, meshes) = r;
            let n = r.pairs.len();
            i.overlaps = meshes;
            i.report = Some(r);
            i.sel = None;
            if n == 0 {
                self.toast("No interference");
            }
        }
    }
}

pub type Job = (InterferenceReport, Vec<Option<Arc<easycad_model::occt::Mesh>>>);

fn spawn(m: Arc<Output>, include_reference: bool) -> Receiver<Job> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Some(a) = &m.asm else { return };
        let opts = InterferenceOptions { include_reference, ..Default::default() };
        let r = easycad_model::assembly::interference(&m.data, &a.result, &opts);
        let meshes = r
            .pairs
            .iter()
            .map(|p| {
                p.common
                    .as_ref()
                    .and_then(|o| o.0.mesh_quality(easycad_model::regen::MeshQuality::Display).ok())
            })
            .collect();
        let _ = tx.send((r, meshes));
    });
    rx
}

/// View-frame bounds of a placed body (sampled vertices).
fn body_bounds(m: &Output, key: easycad_model::feature::BodyId) -> Option<(Vec3, Vec3)> {
    let c = m.ctx_body(key)?;
    let mesh = c.mb.mesh.as_ref()?;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for p in mesh.positions.iter().step_by((mesh.positions.len() / 600).max(1)) {
        let q = c.xf.transform_point3(Vec3::from_array(*p).as_dvec3()).as_vec3();
        lo = lo.min(q);
        hi = hi.max(q);
    }
    Some((lo, hi))
}

/// The report card (bottom left of the viewport).
pub fn card(app: &mut EasyCadApp, ui: &mut Ui, rect: Rect) {
    app.poll_interference();
    let Some(mut st) = app.asm.interference.take() else { return };
    let model = app.model.clone();
    let mut close = false;
    let mut rerun = false;
    let mut zoom: Option<usize> = None;
    let stale = model.as_ref().is_some_and(|m| m.id != st.model_id) && st.job.is_none();
    egui::Area::new(egui::Id::unique("interference"))
        .order(egui::Order::Middle)
        .fixed_pos(pos2(rect.left() + 12.0, rect.top() + 56.0))
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::CARD)
                .stroke(Stroke::new(1.0, theme::BORDER_CARD))
                .corner_radius(10.0)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(330.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("INTERFERENCE").size(11.0).color(theme::TEXT_LABEL));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Close").clicked() {
                                close = true;
                            }
                            if ui.small_button("Run again").clicked() {
                                rerun = true;
                            }
                        });
                    });
                    if st.job.is_some() {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(widgets::muted("Intersecting every overlapping pair…"));
                        });
                        return;
                    }
                    let Some(r) = &st.report else { return };
                    if stale {
                        ui.label(RichText::new("The assembly changed since this check").size(11.0).color(theme::ORANGE_SOFT));
                    }
                    if r.pairs.is_empty() {
                        ui.horizontal(|ui| {
                            widgets::icon(ui, widgets::Icon::Check, 15.0, theme::GREEN);
                            ui.label(RichText::new("No interference").color(theme::GREEN));
                        });
                    } else {
                        let total: f64 = r.pairs.iter().map(|p| p.volume).sum();
                        ui.label(RichText::new(format!("{} pair{} · {:.1} mm³ in all", r.pairs.len(), if r.pairs.len() == 1 { "" } else { "s" }, total)).color(theme::RED));
                        for (i, p) in r.pairs.iter().enumerate().take(12) {
                            let sel = st.sel == Some(i);
                            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                            let pa = ui.painter();
                            if sel {
                                pa.rect_filled(row, 5.0, theme::ROW_SELECTED);
                            } else if resp.hovered() {
                                pa.rect_filled(row, 5.0, theme::HOVER);
                            }
                            pa.circle_filled(row.left_center() + vec2(8.0, 0.0), 4.0, theme::RED);
                            pa.text(row.left_top() + vec2(18.0, 4.0), egui::Align2::LEFT_TOP, &p.a.label, theme::prop(12.0), theme::TEXT);
                            pa.text(row.left_bottom() + vec2(18.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("↔ {}", p.b.label), theme::prop(12.0), theme::TEXT_DIM);
                            pa.text(row.right_center() - vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{:.2} mm³", p.volume), theme::mono(11.0), theme::RED);
                            let resp = resp.on_hover_text("Click to highlight · double-click to zoom to it");
                            if resp.clicked() {
                                st.sel = if sel { None } else { Some(i) };
                            }
                            if resp.double_clicked() {
                                st.sel = Some(i);
                                zoom = Some(i);
                            }
                        }
                        if let Some(i) = st.sel {
                            if widgets::button(ui, "Zoom to").clicked() {
                                zoom = Some(i);
                            }
                        }
                    }
                    let tested = format!("{} pair{} intersected, {} skipped by bounds", r.tested, if r.tested == 1 { "" } else { "s" }, r.culled);
                    ui.label(widgets::muted(tested));
                    for (a, b, e) in r.failed.iter().take(3) {
                        ui.label(RichText::new(format!("{} ↔ {}: {e}", a.label, b.label)).size(11.0).color(theme::ORANGE_SOFT));
                    }
                    if ui.checkbox(&mut st.include_reference, "Include references (PCB, parts)").changed() {
                        rerun = true;
                    }
                });
        });
    if let (Some(i), Some(m), Some(r)) = (zoom, &model, &st.report) {
        if let Some(p) = r.pairs.get(i) {
            let ka = ctx_key(&p.a.path, p.a.body);
            let kb = ctx_key(&p.b.path, p.b.body);
            if let (Some(a), Some(b)) = (body_bounds(m, ka), body_bounds(m, kb)) {
                // the overlap of the two boxes holds the common volume
                let lo = a.0.max(b.0);
                let hi = a.1.min(b.1);
                let c = (lo + hi) * 0.5;
                let r = ((hi - lo).max(Vec3::splat(1.0))).length() * 0.5;
                let mut cam = app.camera.clone();
                cam.target = c;
                cam.distance = (r * 4.0).max(15.0);
                app.camera.animate_to(cam.pose(), app.now);
            }
        }
    }
    if close {
        return;
    }
    app.asm.interference = Some(st);
    if rerun {
        app.check_interference();
    }
}
