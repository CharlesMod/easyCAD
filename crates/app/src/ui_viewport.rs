//! The 3D viewport: GPU paint callback, Fusion-style navigation, hover picking
//! with the pick stack popover, selection highlights, x-ray of the hidden
//! candidate, the view cube, filter bar, display modes and floating dialogs.

use std::sync::Arc;

use casework_model::occt::{CurveKind, EdgeInfo, FaceInfo, Mesh, SurfaceKind};
use casework_model::pick::EdgeTolerance;
use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
use glam::{Vec2, Vec3};

use crate::app::CaseworkApp;
use crate::dialogs::DialogEvent;
use crate::pickstack::{candidates, BodyFacts, Filter, PickKey, PickKind};
use crate::theme;
use crate::viewcube::{self, CubeAction, CubeState};
use crate::viewport::gpu::{
    srgba, DisplayMode, DrawBody, FaceHighlight, Frame3D, Grid, ViewportCallback,
};
use crate::widgets::{self, Icon};
use crate::worker::Output;

pub const HOVER_COLOR: [f32; 4] = [0.94, 0.53, 0.24, 0.45];
pub const XRAY_COLOR: [f32; 4] = [0.94, 0.53, 0.24, 0.55];
pub const SELECT_COLOR: [f32; 4] = [0.30, 0.61, 1.0, 0.40];

pub fn face_desc(f: &FaceInfo) -> String {
    face_label(f).0
}

/// (label, kind) for the pick stack.
pub fn face_label(f: &FaceInfo) -> (String, String) {
    match f.kind {
        SurfaceKind::Plane => {
            let dir = if f.normal.z > 0.99 {
                "facing up"
            } else if f.normal.z < -0.99 {
                "facing down"
            } else if f.normal.z.abs() < 0.01 {
                "wall"
            } else {
                "sloped"
            };
            (format!("Planar face · {dir}"), "planar".into())
        }
        SurfaceKind::Cylinder => (
            format!("Cylinder ⌀{:.2}", f.radius * 2.0),
            "cylinder".into(),
        ),
        SurfaceKind::Cone => ("Cone (drafted)".into(), "cone".into()),
        SurfaceKind::Torus => (format!("Fillet r{:.2}", f.radius), "torus".into()),
        SurfaceKind::Sphere => (format!("Sphere r{:.2}", f.radius), "sphere".into()),
        SurfaceKind::Other => ("Freeform face".into(), "surface".into()),
    }
}

pub fn edge_label(e: &EdgeInfo) -> (String, String) {
    match e.kind {
        CurveKind::Line => (format!("Line edge · {:.2}", e.length), "edge".into()),
        CurveKind::Circle => (
            format!(
                "Circular edge · ⌀{:.2}",
                e.circle.map(|c| c.2 * 2.0).unwrap_or(0.0)
            ),
            "edge".into(),
        ),
        _ => (format!("Curved edge · {:.2}", e.length), "edge".into()),
    }
}

pub fn pick_label(m: &Output, k: &PickKey) -> (String, String) {
    let Some(b) = m.meshed(k.body) else {
        return ("?".into(), String::new());
    };
    match k.kind {
        PickKind::Body => (
            b.body.name.clone(),
            if b.body.reference {
                "body · locked".into()
            } else {
                "body".into()
            },
        ),
        PickKind::Face(f) => b
            .faces
            .get(f as usize)
            .map(face_label)
            .unwrap_or_else(|| (format!("Face {f}"), "face".into())),
        PickKind::Edge(e) => b
            .edges
            .get(e as usize)
            .map(edge_label)
            .unwrap_or_else(|| (format!("Edge {e}"), "edge".into())),
    }
}

struct Facts<'a> {
    m: &'a Output,
    app: &'a CaseworkApp,
}
impl BodyFacts for Facts<'_> {
    fn is_reference(&self, b: casework_model::feature::BodyId) -> bool {
        self.m.body(b).is_some_and(|x| x.reference)
    }
    fn is_visible(&self, b: casework_model::feature::BodyId) -> bool {
        self.app.body_visible(b)
    }
}

fn body_color(reference: bool, part: bool) -> [f32; 4] {
    if part {
        [0.55, 0.60, 0.68, 0.75]
    } else if reference {
        let c = srgba(theme::GREEN_REF);
        [c[0], c[1], c[2], 0.42]
    } else {
        srgba(theme::BODY)
    }
}

pub fn viewport(app: &mut CaseworkApp, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.interact(
        rect,
        ui.scope_id().with("viewport"),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::VIEWPORT);
    app.viewport_size = rect.size();
    app.viewport_rect = rect;
    let size = Vec2::new(rect.width(), rect.height());

    // ------------------------------------------------ navigation
    let mods = ui.input(|i| i.modifiers);
    if resp.dragged_by(egui::PointerButton::Middle) {
        let d = resp.drag_delta();
        if mods.shift {
            app.camera.orbit(Vec2::new(d.x, d.y));
        } else {
            app.camera.pan(Vec2::new(d.x, d.y), size.y);
        }
    }
    let hover = resp.hover_pos();
    if let Some(p) = hover {
        let (scroll, raw) = ui.input(|i| {
            let raw = i.events.iter().fold(egui::Vec2::ZERO, |a, e| match e {
                egui::Event::MouseWheel { delta, .. } => a + *delta,
                _ => a,
            });
            (i.smooth_scroll_delta, raw)
        });
        if mods.alt {
            // Alt+wheel cycles the pick stack (one step per notch)
            let s = if raw.y.abs() > 0.0 { raw.y } else { raw.x };
            if s.abs() > 0.0 {
                app.stack.cycle(if s < 0.0 { 1 } else { -1 });
            }
        } else if scroll.y.abs() > 0.0 {
            let f = (-scroll.y * 0.0018).exp();
            let local = p - rect.min;
            app.camera.zoom_at(Vec2::new(local.x, local.y), size, f);
        }
    }

    // ------------------------------------------------ hover picking
    let model = app.model.clone();
    let filter = app.filter;
    if let (Some(m), Some(p)) = (&model, hover) {
        if !resp.dragged() {
            let local = p - rect.min;
            let (o, d) = app.camera.ray(Vec2::new(local.x, local.y), size);
            let tol = if app.camera.ortho {
                EdgeTolerance::ortho(app.camera.world_per_point(size.y) * 5.0)
            } else {
                EdgeTolerance::perspective(5.0, app.camera.fov_y, size.y)
            };
            let hits = m.picker.pick_all(o, d, tol);
            let facts = Facts { m, app };
            let c = candidates(&hits, filter, &facts);
            if app.hover_pos.is_none_or(|h| h.distance(p) > 3.0) {
                app.stack_since = app.now;
            }
            app.hover_pos = Some(p);
            app.stack.update(c);
        }
    } else if hover.is_none() {
        app.stack.clear();
        app.hover_pos = None;
    }
    // Tab cycles while hovering the viewport (and no text field has focus)
    if app.pending_cycle != 0 {
        let n = std::mem::take(&mut app.pending_cycle);
        app.stack.cycle(n);
    }
    if hover.is_some() && !app.stack.is_empty() && !ui.ctx().text_edit_focused() {
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)) {
            app.stack.cycle(-1);
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            app.stack.cycle(1);
        }
    }

    // ------------------------------------------------ click selection
    let mut popover_click = None;
    if resp.clicked_by(egui::PointerButton::Primary) {
        let cur = app.stack.current().map(|c| c.key);
        select(app, cur, mods.command || mods.shift);
    }
    resp.context_menu(|ui| context_menu(app, ui));

    // ------------------------------------------------ GPU frame
    let shown: Option<Arc<Output>> = if app.dialog.is_some() {
        app.preview.clone().or(model.clone())
    } else {
        model.clone()
    };
    if let Some(shown) = &shown {
        if app.gpu {
            let ppp = ui.ctx().pixels_per_point();
            let frame = build_frame(app, shown, model.as_deref(), rect, ppp);
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                ViewportCallback {
                    frame: Arc::new(frame),
                },
            ));
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No GPU adapter: 3D view unavailable",
                theme::prop(14.0),
                theme::TEXT_MUTED,
            );
        }
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if app.regenerating() {
                "Regenerating…"
            } else {
                "Empty document: press S to sketch, or File ▸ Open sample enclosure"
            },
            theme::prop(13.0),
            theme::TEXT_MUTED,
        );
    }

    // edge highlights (thick lines over the image)
    if let Some(m) = &model {
        let mut sel: Vec<PickKey> = app.selection.clone();
        if let Some(d) = &app.dialog {
            sel.extend(d.picked());
        }
        for k in &sel {
            if let PickKind::Edge(e) = k.kind {
                draw_edge(&painter, app, m, rect, k.body, e, theme::ACCENT, 3.0, true);
            }
        }
        if let Some(c) = app.stack.current() {
            if let PickKind::Edge(e) = c.key.kind {
                // only the hidden pick candidate is drawn x-ray; everything else is depth-tested
                let hidden = app.stack.current_is_hidden();
                let col = if hidden {
                    theme::ORANGE_LIGHT
                } else {
                    theme::ORANGE
                };
                draw_edge(&painter, app, m, rect, c.key.body, e, col, 2.5, !hidden);
            }
        }
    }

    // ------------------------------------------------ overlays
    filter_bar(app, ui, rect);
    display_bar(app, ui, rect);
    let dialog_w = if app.dialog.is_some() {
        app.dialog_card_w.max(300.0) + 12.0
    } else {
        0.0
    };
    cube(app, ui, rect, dialog_w);
    if app.regenerating() && app.model.is_some() {
        let r = Rect::from_center_size(pos2(rect.center().x, rect.top() + 26.0), vec2(150.0, 26.0));
        painter.rect(
            r,
            13.0,
            theme::CARD.gamma_multiply(0.92),
            Stroke::new(1.0, theme::BORDER_INPUT),
            egui::StrokeKind::Inside,
        );
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            "Regenerating…",
            theme::prop(12.0),
            theme::TEXT_MUTED,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    if let Some(m) = &model {
        if let Some(i) = pick_popover(app, ui, rect, m) {
            popover_click = Some(i);
        }
    }
    if let Some(i) = popover_click {
        app.stack.select_index(i);
        let cur = app.stack.current().map(|c| c.key);
        select(app, cur, mods.command || mods.shift);
    }
    joint_plane_overlay(app, ui, rect);
    dialog_overlay(app, ui, rect);
    if let Some(mut ex) = app.export.take() {
        let r = Rect::from_center_size(rect.center(), vec2(370.0, 240.0));
        let mut keep = true;
        egui::Area::new(egui::Id::unique("export"))
            .order(egui::Order::Foreground)
            .fixed_pos(r.min)
            .show(ui.ctx(), |ui| match ex.ui(ui, &app.doc.data) {
                crate::dialogs::export::ExportEvent::Export => {
                    let mut data = app.doc.data.clone();
                    crate::adapters::apply_bindings(&mut data);
                    let req = ex.request(&data.name);
                    let (blobs, lib) = (app.doc.blobs.clone(), app.doc.library.clone());
                    let ex2 = crate::dialogs::export::ExportDialog {
                        format: ex.format,
                        profile: ex.profile.clone(),
                        orient: ex.orient,
                        include_reference: ex.include_reference,
                    };
                    app.ask_path(req, move |app, path| {
                        app.export_job = Some(ex2.start(path, data, blobs, lib));
                        app.toast("Exporting in the background…");
                    });
                    keep = false;
                }
                crate::dialogs::export::ExportEvent::Cancel => keep = false,
                crate::dialogs::export::ExportEvent::None => {}
            });
        if keep {
            app.export = Some(ex);
        }
    }
    if app.camera.animating() {
        ui.ctx().request_repaint();
    }
}

fn select(app: &mut CaseworkApp, key: Option<PickKey>, additive: bool) {
    let model = app.model.clone();
    if let (Some(d), Some(k)) = (&mut app.dialog, key) {
        if d.on_pick(k, model.as_deref()) {
            app.request_preview();
            return;
        }
    }
    match key {
        Some(k) => {
            if additive {
                if let Some(i) = app.selection.iter().position(|s| *s == k) {
                    app.selection.remove(i);
                } else {
                    app.selection.push(k);
                }
            } else {
                app.selection = vec![k];
            }
            app.orientation_body = Some(k.body);
        }
        None => {
            if !additive {
                app.selection.clear();
            }
        }
    }
}

fn mesh_of(m: &Output, b: casework_model::feature::BodyId) -> Option<&Arc<Mesh>> {
    m.mesh(b)
}

/// The upper half created by the previewed joint, if the preview is a joint.
pub fn explode_body(shown: &Output) -> Option<casework_model::feature::BodyId> {
    let i = *shown.prefix_bodies.keys().min()?;
    let f = shown.data.features.get(i)?;
    matches!(
        f.kind,
        casework_model::feature::FeatureKind::PartingJoint { .. }
    )
    .then(|| casework_model::feature::BodyId::of(f.id))
}

/// The joint dialog's exploded preview: (upper half, translation).
pub fn explode_offset(
    app: &CaseworkApp,
    shown: &Output,
) -> Option<(casework_model::feature::BodyId, Vec3)> {
    let d = app.dialog.as_ref()?;
    let crate::dialogs::Body::Joint(j) = &d.body else {
        return None;
    };
    if !j.explode {
        return None;
    }
    let i = *shown.prefix_bodies.keys().min()?;
    let f = shown.data.features.get(i)?;
    if !matches!(
        f.kind,
        casework_model::feature::FeatureKind::PartingJoint { .. }
    ) {
        return None;
    }
    let h = shown.bbox.map(|b| b.size().z as f32).unwrap_or(30.0);
    Some((
        casework_model::feature::BodyId::of(f.id),
        Vec3::Z * (h * 0.35).max(8.0),
    ))
}

fn build_frame(
    app: &CaseworkApp,
    shown: &Output,
    model: Option<&Output>,
    rect: Rect,
    ppp: f32,
) -> Frame3D {
    let size = Vec2::new(rect.width(), rect.height());
    let cam = &app.camera;
    let mut bodies = Vec::new();
    let explode = explode_offset(app, shown);
    for b in &shown.bodies {
        if !app.body_visible(b.body.id) {
            continue;
        }
        if let Some(m) = &b.mesh {
            let mut color = body_color(b.body.reference, b.body.part.is_some());
            if b.colors.is_some() {
                // imported appearance: vertex colours, keep the reference translucency
                color = [1.0, 1.0, 1.0, if b.body.reference { 0.6 } else { 1.0 }];
            }
            let offset = match explode {
                Some((id, off)) if id == b.body.id => off,
                _ => Vec3::ZERO,
            };
            bodies.push(DrawBody {
                mesh: m.clone(),
                color,
                translucent: b.body.reference,
                colors: b.colors.clone(),
                offset,
            });
        }
    }
    let mut highlights = Vec::new();
    if let Some(m) = model {
        let mut sel = app.selection.clone();
        if let Some(d) = &app.dialog {
            sel.extend(d.picked());
        }
        for k in &sel {
            if let Some(mesh) = mesh_of(m, k.body) {
                match k.kind {
                    PickKind::Face(f) => {
                        // a selected face behind other geometry (picked through the
                        // stack) is drawn x-ray, or the selection would be invisible
                        let hidden = m
                            .meshed(k.body)
                            .and_then(|b| b.faces.get(f as usize))
                            .is_some_and(|info| !point_visible(app, m, info.point.as_vec3()));
                        highlights.push(FaceHighlight {
                            mesh: mesh.clone(),
                            face: f,
                            color: SELECT_COLOR,
                            xray: hidden,
                        })
                    }
                    PickKind::Body => highlights.push(FaceHighlight {
                        mesh: mesh.clone(),
                        face: u32::MAX,
                        color: [0.30, 0.61, 1.0, 0.22],
                        xray: false,
                    }),
                    PickKind::Edge(_) => {}
                }
            }
        }
        if let Some(c) = app.stack.current() {
            let hidden = app.stack.current_is_hidden();
            if let Some(mesh) = mesh_of(m, c.key.body) {
                let color = if hidden { XRAY_COLOR } else { HOVER_COLOR };
                match c.key.kind {
                    PickKind::Face(f) => highlights.push(FaceHighlight {
                        mesh: mesh.clone(),
                        face: f,
                        color,
                        xray: hidden,
                    }),
                    PickKind::Body => highlights.push(FaceHighlight {
                        mesh: mesh.clone(),
                        face: u32::MAX,
                        color: [0.94, 0.53, 0.24, 0.30],
                        xray: hidden,
                    }),
                    PickKind::Edge(_) => {}
                }
            }
        }
    }
    let grid = shown.bbox.map(|bb| {
        let ext = (bb.size().x.max(bb.size().y) as f32 * 1.5).max(50.0);
        Grid {
            z: bb.min.z as f32,
            spacing: 10.0,
            extent: ext,
            center: [bb.center().x as f32, bb.center().y as f32],
        }
    });
    let light = (cam.up() * 0.8 + cam.right() * -0.35 - cam.forward() * 0.6).normalize();
    Frame3D {
        size_px: [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ],
        view_proj: cam.view_proj(size),
        eye: cam.eye(),
        forward: cam.forward(),
        ortho: cam.ortho,
        light,
        background: srgba(theme::VIEWPORT),
        mode: app.display,
        edge_color: srgba(theme::EDGE_DARK),
        bodies,
        highlights,
        grid,
    }
}

#[allow(clippy::too_many_arguments)]
/// Is a point on the model surface visible from the camera (nothing in front of it)?
pub fn point_visible(app: &CaseworkApp, m: &Output, p: Vec3) -> bool {
    let cam = &app.camera;
    let (o, d, dist) = if cam.ortho {
        let back = cam.distance + 4.0 * cam.scene_radius;
        (p - cam.forward() * back, cam.forward(), back)
    } else {
        let v = p - cam.eye();
        (cam.eye(), v.normalize(), v.length())
    };
    let eps = 0.03_f32.max(dist * 2e-3);
    let hits = m.picker.pick_all(o, d, EdgeTolerance::ortho(0.0));
    !hits.iter().any(|h| {
        matches!(h.kind, casework_model::pick::HitKind::Face(_))
            && h.t < dist - eps
            && app.body_visible(h.body)
            && !m.body(h.body).is_some_and(|b| b.reference)
    })
}

#[allow(clippy::too_many_arguments)]
fn draw_edge(
    p: &egui::Painter,
    app: &CaseworkApp,
    m: &Output,
    rect: Rect,
    body: casework_model::feature::BodyId,
    e: u32,
    col: Color32,
    w: f32,
    depth_test: bool,
) {
    let Some(mesh) = m.mesh(body) else { return };
    let size = Vec2::new(rect.width(), rect.height());
    let to_screen = |q: Vec3| {
        app.camera
            .project(q, size)
            .map(|s| pos2(rect.left() + s.x, rect.top() + s.y))
    };
    for pl in mesh.edges.iter().filter(|pl| pl.edge == e) {
        // sample the polyline finely enough that partly hidden segments split correctly
        let mut samples: Vec<Vec3> = Vec::new();
        for w2 in pl.points.windows(2) {
            let (a, b) = (Vec3::from_array(w2[0]), Vec3::from_array(w2[1]));
            let len_px = match (to_screen(a), to_screen(b)) {
                (Some(x), Some(y)) => x.distance(y),
                _ => 0.0,
            };
            let n = if depth_test {
                ((len_px / 8.0).ceil() as usize).clamp(1, 64)
            } else {
                1
            };
            for k in 0..n {
                samples.push(a.lerp(b, k as f32 / n as f32));
            }
        }
        if let Some(last) = pl.points.last() {
            samples.push(Vec3::from_array(*last));
        }
        let mut run: Vec<Pos2> = Vec::new();
        for q in samples {
            let vis = !depth_test || point_visible(app, m, q);
            match (vis, to_screen(q)) {
                (true, Some(s)) => run.push(s),
                _ => {
                    if run.len() >= 2 {
                        p.add(egui::Shape::line(
                            std::mem::take(&mut run),
                            Stroke::new(w, col),
                        ));
                    }
                    run.clear();
                }
            }
        }
        if run.len() >= 2 {
            p.add(egui::Shape::line(run, Stroke::new(w, col)));
        }
    }
}

fn overlay_frame() -> Frame {
    Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER_INPUT))
        .corner_radius(8.0)
        .inner_margin(Margin::same(4))
}

fn filter_bar(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    egui::Area::new(egui::Id::unique("filters"))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min + vec2(12.0, 12.0))
        .show(ui.ctx(), |ui| {
            overlay_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if let Some(note) = app.dialog.as_ref().and_then(|d| d.filter_note()) {
                        let f = app.filter;
                        let label = match (f.bodies, f.faces, f.edges) {
                            (false, false, true) => "Edges only",
                            (false, true, false) => "Faces only",
                            _ => "Custom filter",
                        };
                        widgets::pill(ui, label, true);
                        ui.label(RichText::new(note).size(12.0).color(theme::TEXT_MUTED));
                        ui.add_space(6.0);
                        return;
                    }
                    let f = &mut app.filter;
                    for (label, on, tip) in [
                        ("Bodies", &mut f.bodies, "Pick whole bodies"),
                        ("Faces", &mut f.faces, "Pick faces"),
                        ("Edges", &mut f.edges, "Pick edges"),
                        (
                            "Sketches",
                            &mut f.sketches,
                            "Pick sketch geometry (in sketch mode)",
                        ),
                    ] {
                        if widgets::pill(ui, label, *on).on_hover_text(tip).clicked() {
                            *on = !*on;
                        }
                    }
                });
            });
        });
}

fn display_bar(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    let w = 380.0;
    let pos = pos2(rect.center().x - w / 2.0, rect.bottom() - 54.0);
    egui::Area::new(egui::Id::unique("display"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            overlay_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if widgets::icon_button(ui, Icon::Fit, 30.0, "Fit all (F)", true).clicked() {
                        app.fit_all();
                    }
                    widgets::icon_button(
                        ui,
                        Icon::Section,
                        30.0,
                        "Section view (needs engine section API)",
                        false,
                    );
                    let (r, _) = ui.allocate_exact_size(vec2(9.0, 22.0), Sense::hover());
                    ui.painter().vline(
                        r.center().x,
                        r.y_range(),
                        Stroke::new(1.0, theme::BORDER_CARD),
                    );
                    for (m, label, key) in [
                        (DisplayMode::Shaded, "Shaded", "1"),
                        (DisplayMode::ShadedEdges, "Shaded + edges", "2"),
                        (DisplayMode::Wireframe, "Wireframe", "3"),
                    ] {
                        if widgets::seg(ui, label, app.display == m)
                            .on_hover_text(format!("{label} ({key})"))
                            .clicked()
                        {
                            app.display = m;
                        }
                    }
                });
            });
        });
}

fn cube(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect, right_offset: f32) {
    let w = 88.0;
    let h = viewcube::widget_height(w);
    let r = Rect::from_min_size(
        pos2(rect.right() - 16.0 - w - right_offset, rect.top() + 12.0),
        vec2(w, h),
    );
    let mut action = None;
    egui::Area::new(egui::Id::unique("cube"))
        .order(egui::Order::Middle)
        .fixed_pos(r.min)
        .show(ui.ctx(), |ui| {
            let (rr, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            let st = CubeState {
                rot: app.camera.rot,
                ortho: app.camera.ortho,
                fit_on_change: app.camera.fit_on_change,
                step_frame: app.camera.step_frame,
                force_hover: app.cube_hover,
                id: "viewcube",
            };
            action = viewcube::view_cube(ui, rr, &st);
        });
    let aspect = rect.width() / rect.height().max(1.0);
    let now = app.now;
    match action {
        Some(CubeAction::Region(reg)) => app.camera.go_region(reg, now, aspect),
        Some(CubeAction::Orbit(d)) => app.camera.orbit(Vec2::new(d.x, d.y)),
        Some(CubeAction::Step(a, s)) => app.camera.go_step(a, s, now),
        Some(CubeAction::Home) => app.camera.go_home(now, aspect),
        Some(CubeAction::SetHome) => {
            app.camera.set_home();
            app.toast("Home view saved");
        }
        Some(CubeAction::ResetHome) => app.camera.reset_home(),
        Some(CubeAction::SetOrtho(o)) => app.camera.ortho = o,
        Some(CubeAction::SetFitOnChange(f)) => app.camera.fit_on_change = f,
        Some(CubeAction::SetStepFrame(f)) => app.camera.step_frame = f,
        None => {}
    }
}

/// The "UNDER CURSOR" popover. Returns a clicked row.
fn pick_popover(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect, m: &Output) -> Option<usize> {
    let p = app.hover_pos?;
    if app.stack.len() < 2 || !rect.contains(p) {
        return None;
    }
    // appear after a short rest so plain moves stay uncluttered
    if app.now - app.stack_since < 0.25 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(260));
        return None;
    }
    let mut clicked = None;
    let mut pos = p + vec2(18.0, 18.0);
    let n = app.stack.len().min(8);
    let h = 52.0 + n as f32 * 34.0;
    // stay clear of the floating dialog card on the right
    let right = if app.dialog.is_some() {
        rect.right() - 16.0 - app.dialog_card_w.max(300.0) - 8.0
    } else {
        rect.right()
    };
    if pos.x + 270.0 > right {
        pos.x = (p.x - 288.0).max(rect.left() + 4.0);
    }
    if pos.y + h > rect.bottom() {
        pos.y = p.y - h - 12.0;
    }
    egui::Area::new(egui::Id::unique("pickstack")).order(egui::Order::Foreground).fixed_pos(pos).interactable(true).show(ui.ctx(), |ui| {
        Frame::new()
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::BORDER_CARD))
            .corner_radius(8.0)
            .inner_margin(Margin::same(6))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(115) })
            .show(ui, |ui| {
                ui.set_width(252.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("UNDER CURSOR · {} hits", app.stack.len())).size(11.0).color(theme::TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Tab ↹").font(theme::mono(11.0)).color(theme::TEXT_MUTED));
                    });
                });
                for (i, c) in app.stack.items.iter().take(8).enumerate() {
                    let cur = i == app.stack.index;
                    let (label, kind) = pick_label(m, &c.key);
                    let (r, resp) = ui.allocate_exact_size(vec2(252.0, 32.0), Sense::click());
                    let (bg, border) = if cur { (theme::ORANGE_BG, theme::ORANGE) } else if resp.hovered() { (theme::HOVER, Color32::TRANSPARENT) } else { (Color32::TRANSPARENT, Color32::TRANSPARENT) };
                    ui.painter().rect(r, 5.0, bg, Stroke::new(1.0, border), egui::StrokeKind::Inside);
                    ui.painter().text(r.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, (i + 1).to_string(), theme::mono(11.0), theme::TEXT_LABEL);
                    ui.painter().text(r.left_center() + vec2(30.0, 0.0), egui::Align2::LEFT_CENTER, label, theme::prop(12.0), theme::TEXT);
                    ui.painter().text(r.right_center() - vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, kind, theme::prop(11.0), theme::TEXT_MUTED);
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                }
                ui.add(
                    egui::Label::new(RichText::new("Hover shows everything the ray hits, front to back. Tab or Alt+wheel cycles, click selects. No press-and-hold.").size(11.0).color(theme::TEXT_MUTED))
                        .wrap(),
                );
            });
    });
    clicked
}

/// Joint dialog: the translucent parting plane over the model and a handle that
/// drags it along Z (coarse previews while dragging, full quality on release).
fn joint_plane_overlay(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    let Some(bb) = app.model.as_ref().and_then(|m| m.bbox) else {
        return;
    };
    let vals = crate::adapters::values(&app.doc.data);
    let size = Vec2::new(rect.width(), rect.height());
    let cam = app.camera.clone();
    let Some(d) = app.dialog.as_mut() else { return };
    let crate::dialogs::Body::Joint(j) = &mut d.body else {
        return;
    };
    let Ok(z) = crate::widgets::eval(&j.z.text, &vals) else {
        return;
    };
    let pad = 0.12 * bb.size().x.max(bb.size().y);
    let (x0, x1, y0, y1) = (
        bb.min.x - pad,
        bb.max.x + pad,
        bb.min.y - pad,
        bb.max.y + pad,
    );
    let w = |x: f64, y: f64| Vec3::new(x as f32, y as f32, z as f32);
    let corners = [w(x0, y0), w(x1, y0), w(x1, y1), w(x0, y1)];
    let to = |p: Vec3| {
        cam.project(p, size)
            .map(|s| pos2(rect.left() + s.x, rect.top() + s.y))
    };
    let pts: Vec<Pos2> = corners.iter().filter_map(|c| to(*c)).collect();
    if pts.len() == 4 {
        let p = ui.painter_at(rect);
        p.add(egui::Shape::convex_polygon(
            pts.clone(),
            Color32::from_rgba_unmultiplied(76, 155, 255, 34),
            Stroke::new(1.2, theme::ACCENT),
        ));
    }
    // handle on the plane corner furthest left on screen (the dialog sits on the right)
    let anchor = corners
        .iter()
        .copied()
        .filter_map(|c| to(c).map(|s| (c, s)))
        .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map(|x| x.0)
        .unwrap_or(w(x1, y0));
    let (Some(h), Some(hz)) = (to(anchor), to(anchor + Vec3::Z)) else {
        return;
    };
    let dir = hz - h; // screen px per mm of Z
    let hr = Rect::from_center_size(h, vec2(26.0, 26.0));
    let id = egui::Id::unique("joint_handle");
    let mut dz = 0.0f64;
    let mut dragging = false;
    let mut released = false;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(hr.min)
        .show(ui.ctx(), |ui| {
            let (r, resp) = ui.allocate_exact_size(hr.size(), Sense::drag());
            let hot = resp.hovered() || resp.dragged();
            let p = ui.painter();
            p.circle(
                r.center(),
                9.0,
                if hot { theme::ACCENT } else { theme::ACCENT_BG },
                Stroke::new(1.5, theme::ACCENT),
            );
            let n = dir.normalized();
            for s in [1.0f32, -1.0] {
                let tip = r.center() + n * (24.0 * s);
                p.arrow(
                    r.center() + n * (11.0 * s),
                    n * (13.0 * s),
                    Stroke::new(1.6, theme::ACCENT),
                );
                let _ = tip;
            }
            let resp = resp.on_hover_text(format!(
                "Drag to move the parting plane (z = {})",
                crate::units::len_u(z)
            ));
            if resp.dragged() {
                dragging = true;
                let dd = resp.drag_delta();
                let l2 = dir.length_sq();
                if l2 > 1e-6 {
                    dz = (dd.dot(dir) / l2) as f64;
                }
            }
            released = resp.drag_stopped();
        });
    if dragging && dz != 0.0 {
        let nz = (z + dz).clamp(j.z_min + 0.5, j.z_max - 0.5);
        j.z.text = crate::dialogs::fmt((nz * 10.0).round() / 10.0);
    }
    let slider = j.dragging;
    let was = app.dragging;
    app.dragging = dragging || slider;
    if (dragging && dz != 0.0) || (released || (was && !app.dragging)) {
        app.request_preview();
    }
    let p = ui.painter_at(rect);
    p.text(
        h + vec2(16.0, -16.0),
        egui::Align2::LEFT_BOTTOM,
        format!("z {}", crate::units::len(z)),
        theme::mono(11.0),
        theme::ACCENT_LINK,
    );
}

fn dialog_overlay(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    let Some(mut d) = app.dialog.take() else {
        return;
    };
    let vals = crate::adapters::values(&app.doc.data);
    let names: Vec<String> = app
        .doc
        .data
        .params
        .list
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let profile = app.doc.data.profile();
    let model = app.model.clone();
    let preview = app.preview.clone();
    let regenerating =
        app.worker.busy_with(crate::worker::Purpose::Preview) || app.preview_dirty.is_some();
    let mut ev = DialogEvent::None;
    let card_w = app.dialog_card_w.max(300.0);
    let pos = pos2(rect.right() - 16.0 - card_w, rect.top() + 12.0);
    let shown = egui::Area::new(egui::Id::unique("dialog"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(362.0);
            {
                let cx = crate::dialogs::DialogCx {
                    env: widgets::ExprEnv {
                        values: &vals,
                        names: &names,
                        profile: &profile,
                    },
                    data: &app.doc.data,
                    model: model.as_deref(),
                    preview: preview.as_deref(),
                    regenerating,
                    next_id: app.doc.data.next_id,
                };
                ev = d.ui(ui, &cx);
            }
        });
    let w = shown.response.rect.width();
    if (w - app.dialog_card_w).abs() > 0.5 {
        app.dialog_card_w = w;
        ui.ctx().request_repaint();
    }
    // joint section card / standoffs clearance banner
    match &d.body {
        crate::dialogs::Body::Joint(j) => {
            egui::Area::new(egui::Id::unique("joint_section"))
                .order(egui::Order::Middle)
                .fixed_pos(pos2(rect.left() + 12.0, rect.bottom() - 330.0))
                .show(ui.ctx(), |ui| {
                    ui.set_max_width(320.0);
                    Frame::new()
                        .fill(theme::CARD)
                        .stroke(Stroke::new(1.0, theme::BORDER_CARD))
                        .corner_radius(10.0)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("SECTION THROUGH WALL · LIVE")
                                        .size(11.0)
                                        .color(theme::TEXT_MUTED),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(widgets::muted(if j.explode {
                                            "exploded"
                                        } else {
                                            "assembled"
                                        }));
                                    },
                                );
                            });
                            let upper = preview.as_ref().and_then(|o| explode_body(o));
                            let z = crate::widgets::eval(&j.z.text, &vals).unwrap_or(0.0);
                            let cuts: &[crate::worker::SectionCut] = preview
                                .as_ref()
                                .map(|o| o.sections.as_slice())
                                .unwrap_or(&[]);
                            j.section(ui, cuts, upper, z);
                        });
                });
        }
        crate::dialogs::Body::Standoffs(s) => {
            if let Some(c) = s.clearance_mm {
                let min = vals.get("clearance").copied().unwrap_or(0.0);
                egui::Area::new(egui::Id::unique("clearance")).order(egui::Order::Middle).fixed_pos(pos2(rect.left() + 12.0, rect.bottom() - 110.0)).show(ui.ctx(), |ui| {
                    overlay_frame().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            widgets::icon(ui, if c >= min { Icon::Check } else { Icon::Warn }, 15.0, if c >= min { theme::GREEN } else { theme::ORANGE_SOFT });
                            ui.label(RichText::new(format!("Clearance check: board to inner wall is {c:.2} mm (bounding boxes). Minimum is clearance = {min:.2}.")).size(12.0));
                        });
                    });
                });
            }
        }
        _ => {}
    }
    match ev {
        DialogEvent::Ok => {
            app.dialog = Some(d);
            app.close_dialog(true);
            return;
        }
        DialogEvent::Cancel => {
            app.dialog = Some(d);
            app.close_dialog(false);
            return;
        }
        DialogEvent::Changed => {
            app.dialog = Some(d);
            app.request_preview();
            return;
        }
        DialogEvent::ZoomTo(p) => {
            let mut c = app.camera.clone();
            c.target = p;
            c.distance = (app.camera.scene_radius * 0.6).max(10.0);
            app.camera.animate_to(c.pose(), app.now);
        }
        DialogEvent::None => {}
    }
    app.dialog = Some(d);
}

fn context_menu(app: &mut CaseworkApp, ui: &mut Ui) {
    let sel = app.selection.clone();
    if sel.is_empty() {
        if ui.button("Fit all").clicked() {
            app.fit_all();
            ui.close();
        }
        if ui.button("Home view").clicked() {
            let a = app.viewport_size.x / app.viewport_size.y.max(1.0);
            app.camera.go_home(app.now, a);
            ui.close();
        }
        if ui.button("Show all bodies").clicked() {
            app.hidden.clear();
            ui.close();
        }
        return;
    }
    let edges = sel.iter().any(|k| matches!(k.kind, PickKind::Edge(_)));
    let faces = sel.iter().any(|k| matches!(k.kind, PickKind::Face(_)));
    if edges {
        if ui.button("Fillet…   F").clicked() {
            app.open_dialog(crate::dialogs::DialogKind::Fillet);
            ui.close();
        }
        if ui.button("Chamfer…").clicked() {
            app.open_dialog(crate::dialogs::DialogKind::Chamfer);
            ui.close();
        }
    }
    if faces {
        if ui.button("Sketch on face   S").clicked() {
            app.open_sketch(None);
            ui.close();
        }
        if ui.button("Shell (remove faces)…").clicked() {
            app.open_dialog(crate::dialogs::DialogKind::Shell);
            ui.close();
        }
        crate::ui_process::teardrop_menu(app, ui);
    }
    let body = sel[0].body;
    if ui.button("Hide body").clicked() {
        app.hidden.insert(body);
        app.selection.clear();
        ui.close();
    }
    if ui.button("Build orientation…").clicked() {
        app.orientation_body = Some(body);
        ui.close();
    }
    let _ = Filter::default();
}
