//! 2D drawing sheet on the engine's `drawing` module: views by cube face / iso /
//! custom rotation with in-sheet rotation and scale, HLR edges (computed in the
//! worker), associative annotations (dangling ones in orange) and SVG/DXF/PDF
//! export.
//!
//! Edits go to a draft `Drawing` that is recomputed immediately and committed to
//! the document (one undo step) after a short pause.

use std::sync::Arc;

use easycad_model::document::{DocError, Document};
use easycad_model::drawing::*;
use easycad_model::feature::{BodyId, FeatureKind};
use easycad_model::occt::CurveKind;
use easycad_model::process::CubeFace;
use easycad_model::select::EdgeSelector;
use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
use glam::{DQuat, DVec2};

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;

pub const TOOLS: [(&str, &str, &str); 11] = [
    ("Place view", "▣", "Place view: click on the sheet to drop a copy of the selected view (Iso by default)"),
    ("Dimension", "↔", "Dimension: pick a straight edge then empty paper (length), or two edges (distance) · Shift: vertical"),
    ("Ordinate", "⊢", "Ordinate: pick the zero edge, then a feature · Shift: Y axis"),
    ("Radius / ⌀", "⌀", "Radius or diameter: pick a circle seen face-on · Shift: radius"),
    ("Angle", "∠", "Angle: pick two straight edges"),
    ("Note", "A", "Note: click to place · use {param} to insert live values"),
    ("Leader", "↗", "Leader: pick an edge, then click the text position"),
    ("Balloon", "①", "Balloon: pick a library part's edge · numbers follow the BOM"),
    ("Center mark", "+", "Center mark: pick circles"),
    ("Centerline", "—", "Centerline: pick two parallel edges"),
    ("Parting line", "PL", "Parting line: click a view to show the split / joint plane edge-on"),
];

pub struct DrawingUi {
    pub id: Option<DrawingId>,
    pub draft: Option<Drawing>,
    pub result: Option<Arc<DrawingResult>>,
    pub selected: Option<ViewId>,
    pub tool: usize,
    pub picks: Vec<(ViewId, DimRef, DVec2)>,
    pub dirty_at: Option<f64>,
    pub requested: Option<(u64, u64)>,
    pub edit_gen: u64,
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub rot: [f64; 3],
}

impl Default for DrawingUi {
    fn default() -> Self {
        DrawingUi {
            id: None,
            draft: None,
            result: None,
            selected: None,
            tool: 1,
            picks: Vec::new(),
            dirty_at: None,
            requested: None,
            edit_gen: 0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            rot: [35.26, 0.0, 45.0],
        }
    }
}

impl DrawingUi {
    pub fn sheet_label(&self) -> String {
        match &self.draft {
            Some(d) => format!(
                "{:?} {}",
                d.sheet.size,
                if d.sheet.orientation == PaperOrientation::Landscape {
                    "landscape"
                } else {
                    "portrait"
                }
            ),
            None => "A3 landscape".into(),
        }
    }

    fn changed(&mut self, now: f64) {
        self.dirty_at = Some(now);
        self.edit_gen += 1;
    }
}

pub fn orientation_label(o: &ViewOrientation) -> String {
    match o {
        ViewOrientation::Face(f) => match f {
            CubeFace::NegY => "Front",
            CubeFace::PosY => "Back",
            CubeFace::PosZ => "Top",
            CubeFace::NegZ => "Bottom",
            CubeFace::NegX => "Left",
            CubeFace::PosX => "Right",
        }
        .into(),
        ViewOrientation::Iso(c) => {
            let ns = if c.y > 0 { "N" } else { "S" };
            let ew = if c.x > 0 { "E" } else { "W" };
            format!("Iso {ns}{ew}{}", if c.z < 0 { " ↓" } else { "" })
        }
        ViewOrientation::Rotation(_) => "Custom".into(),
    }
}

/// A first sheet: top / front / right at 1:1, an iso at 1:2, and associative
/// annotations on the model's own edges.
pub fn default_drawing(app: &mut EasyCadApp) -> Option<DrawingId> {
    let name = format!("Sheet {}", app.doc.data.drawings.len() + 1);
    let id = app.doc.add_drawing(
        &name,
        Sheet::new(PaperSize::A3, PaperOrientation::Landscape),
    );
    let model = app.model.clone();
    let split = app
        .doc
        .data
        .features
        .iter()
        .find(|f| {
            matches!(
                f.kind,
                FeatureKind::Split { .. } | FeatureKind::PartingJoint { .. }
            )
        })
        .map(|f| f.id);
    // model extents (solids) → a standard scale that fills the sheet
    let ext = model
        .as_ref()
        .and_then(|m| {
            let mut bb: Option<easycad_model::occt::BBox> = None;
            for b in m.bodies.iter().filter(|b| !b.body.reference) {
                if let Ok(x) = b.body.shape().bbox() {
                    bb = Some(match bb {
                        None => x,
                        Some(o) => easycad_model::occt::BBox {
                            min: o.min.min(x.min),
                            max: o.max.max(x.max),
                        },
                    });
                }
            }
            bb.map(|b| b.size())
        })
        .unwrap_or(glam::DVec3::new(100.0, 60.0, 40.0));
    let sheet_size = Sheet::new(PaperSize::A3, PaperOrientation::Landscape).size_mm();
    let layout = layout_for(ext, sheet_size);
    // the revision is a user field, not the internal save counter
    if !app.doc.data.meta.contains_key("revision") {
        let _ = app.doc.edit("Revision", |d| {
            d.meta.insert("revision".into(), "A".into());
            Ok::<_, DocError>(())
        });
    }
    let _ = app.doc.edit_drawing(id, "Default views", |dr| {
        for f in &mut dr.sheet.title_block.fields {
            if f.template.contains("{doc.revision}") {
                f.template = "{meta.revision}".into();
            }
        }
        let s = layout.scale;
        let top = dr.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), layout.top, s);
        let front = dr.add_view(
            "Front",
            ViewOrientation::Face(CubeFace::NegY),
            layout.front,
            s,
        );
        dr.add_view(
            "Right",
            ViewOrientation::Face(CubeFace::PosX),
            layout.right,
            s,
        );
        dr.add_view(
            "Iso",
            ViewOrientation::Iso(IsoCorner { x: 1, y: -1, z: 1 }),
            layout.iso,
            layout.iso_scale,
        );
        // notes block (live values)
        for (i, line) in [
            "NOTES",
            "1. Material: {meta.material}",
            "2. Wall {wall} mm, draft {draft} on walls",
            "3. Unless noted: dimensions in mm, scale {sheet.scale}",
        ]
        .iter()
        .enumerate()
        {
            dr.annotate(AnnotationKind::Note {
                position: DVec2::new(22.0, 42.0 - 6.0 * i as f64),
                text: (*line).into(),
                height: if i == 0 { 4.0 } else { 3.2 },
            });
        }
        let Some(m) = &model else { return };
        let Some(base) = m.bodies.iter().find(|b| !b.body.reference) else {
            return;
        };
        let bid = base.body.id;
        let edge = |pred: &dyn Fn(&easycad_model::occt::EdgeInfo) -> bool| {
            base.edges.iter().find(|e| pred(e)).map(|e| DimRef::Edge {
                body: bid,
                edge: EdgeSelector::from_pick(e),
            })
        };
        if let Ok(bb) = base.body.shape().bbox() {
            // bottom front edge (straight, along X) in the front view
            if let Some(a) = edge(&|e| {
                e.kind == CurveKind::Line
                    && e.tangent.x.abs() > 0.99
                    && (e.midpoint.z - bb.min.z).abs() < 1e-3
                    && (e.midpoint.y - bb.min.y).abs() < 1e-3
            }) {
                dr.annotate(AnnotationKind::Linear {
                    view: front,
                    a,
                    b: None,
                    kind: LinearKind::Horizontal,
                    offset: -12.0,
                });
            }
            // a sharp vertical edge of the front face (USB-C slot side) for its height
            if let Some(a) = edge(&|e| {
                e.kind == CurveKind::Line
                    && e.tangent.z.abs() > 0.99
                    && (e.midpoint.y - bb.min.y).abs() < 1e-3
                    && e.midpoint.x.abs() < 10.0
            }) {
                dr.annotate(AnnotationKind::Linear {
                    view: front,
                    a,
                    b: None,
                    kind: LinearKind::Vertical,
                    offset: 10.0,
                });
            }
        }
        // a standoff bore, face-on in the top view
        if let Some(c) = edge(&|e| {
            e.kind == CurveKind::Circle
                && e.circle.is_some_and(|c| c.1.z.abs() > 0.99 && c.2 < 2.0)
                && e.midpoint.x < 0.0
                && e.midpoint.y > 0.0
        }) {
            dr.annotate(AnnotationKind::Radius {
                view: top,
                circle: c.clone(),
                diameter: true,
                leader_deg: 135.0,
            });
            dr.annotate(AnnotationKind::CenterMark {
                view: top,
                circle: c,
            });
        }
        if let Some(f) = split {
            dr.annotate(AnnotationKind::PartingLine {
                view: front,
                feature: f,
            });
        }
    });
    Some(id)
}

/// Make sure the screen has a drawing, commit paused drafts, and keep the
/// worker's result current.
pub fn sync(app: &mut EasyCadApp) {
    let need_new = match &app.mode {
        crate::app::Mode::Drawing(d) => d.id.is_none_or(|id| app.doc.data.drawing(id).is_none()),
        _ => return,
    };
    if need_new {
        let existing =
            match &app.mode {
                crate::app::Mode::Drawing(d) if d.id.is_some() => None,
                _ => None,
            }
            .or_else(|| {
                app.doc.data.drawings.first().map(|d| d.id).filter(
                    |_| matches!(&app.mode, crate::app::Mode::Drawing(d) if d.edit_gen == 0),
                )
            });
        let id = existing.or_else(|| default_drawing(app));
        if let crate::app::Mode::Drawing(d) = &mut app.mode {
            d.id = id;
            d.draft = id.and_then(|i| app.doc.data.drawing(i).cloned());
            d.selected = None;
            d.requested = None;
            d.edit_gen += 1;
        }
    }
    let now = app.now;
    let crate::app::Mode::Drawing(d) = &mut app.mode else {
        return;
    };
    if let (Some(t), Some(id), Some(draft)) = (d.dirty_at, d.id, d.draft.clone()) {
        if now - t > 0.6 {
            d.dirty_at = None;
            let _ = app
                .doc
                .edit_drawing(id, "Edit drawing", move |dr| *dr = draft);
        }
    }
    let crate::app::Mode::Drawing(d) = &mut app.mode else {
        return;
    };
    if d.dirty_at.is_none() {
        // follow undo / redo
        if let Some(id) = d.id {
            let cur = app.doc.data.drawing(id).cloned();
            if cur != d.draft {
                d.draft = cur;
                d.edit_gen += 1;
            }
        }
    }
    let key = (app.doc.data.revision, d.edit_gen);
    if d.requested != Some(key) {
        if let Some(draft) = d.draft.clone() {
            d.requested = Some(key);
            let data = crate::adapters::effective(&app.doc.data, None);
            app.worker.submit_full(crate::worker::Request {
                id: 0,
                purpose: crate::worker::Purpose::Drawing,
                data,
                blobs: app.doc.blobs.clone(),
                library: app.doc.library.clone(),
                prefixes: Vec::new(),
                drawing: Some(draft),
                quality: easycad_model::regen::MeshQuality::Display,
                section: None,
                active: Vec::new(),
                explode: 0.0,
                cut: None,
            });
        }
    }
}

pub fn screen(app: &mut EasyCadApp, ui: &mut Ui) {
    sync(app);
    let model = app.model.clone();
    let busy = app.worker.busy_with(crate::worker::Purpose::Drawing);
    let now = app.now;
    if let crate::app::Mode::Drawing(d) = &app.mode {
        if d.dirty_at.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
    let mut export: Option<&'static str> = None;
    let mut meta_edit: Option<(String, String)> = None;
    let crate::app::Mode::Drawing(d) = &mut app.mode else {
        return;
    };
    egui::Panel::top("draw_tools")
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(8, 6)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, (label, glyph, hint)) in TOOLS.iter().enumerate() {
                    let on = d.tool == i;
                    let w = (ui
                        .painter()
                        .layout_no_wrap(label.to_string(), theme::prop(11.0), theme::TEXT)
                        .size()
                        .x
                        + 12.0)
                        .max(52.0);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 56.0), Sense::click());
                    if on {
                        ui.painter().rect(
                            rect,
                            6.0,
                            theme::ACCENT_BG,
                            Stroke::new(1.0, theme::ACCENT),
                            egui::StrokeKind::Inside,
                        );
                    } else if resp.hovered() {
                        ui.painter().rect(
                            rect,
                            6.0,
                            theme::HOVER,
                            Stroke::new(1.0, theme::BORDER_CARD),
                            egui::StrokeKind::Inside,
                        );
                    }
                    ui.painter().text(
                        rect.center() - vec2(0.0, 9.0),
                        egui::Align2::CENTER_CENTER,
                        *glyph,
                        theme::mono(13.0),
                        theme::TEXT_DIM,
                    );
                    ui.painter().text(
                        rect.center() + vec2(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        *label,
                        theme::prop(11.0),
                        theme::TEXT,
                    );
                    if resp.on_hover_text(*hint).clicked() {
                        d.tool = i;
                        d.picks.clear();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (f, ext) in [("SVG", "svg"), ("DXF", "dxf"), ("Export PDF", "pdf")] {
                        if widgets::button(ui, f).clicked() {
                            export = Some(ext);
                        }
                    }
                });
            });
        });
    let dangling = d
        .result
        .as_ref()
        .map(|r| {
            r.annotations
                .iter()
                .filter(|a| matches!(a.status, AnnotationStatus::Dangling(_)))
                .count()
        })
        .unwrap_or(0);
    egui::Panel::bottom("draw_status")
        .exact_size(28.0)
        .frame(
            Frame::new()
                .fill(theme::BAR)
                .inner_margin(Margin::symmetric(12, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    RichText::new(TOOLS[d.tool].2)
                        .size(11.0)
                        .color(theme::TEXT_MUTED),
                );
                if !d.picks.is_empty() {
                    ui.label(
                        RichText::new(format!("· {} picked", d.picks.len()))
                            .size(11.0)
                            .color(theme::ACCENT_LINK),
                    );
                }
                if dangling > 0 {
                    ui.label(
                        RichText::new(format!("· {dangling} dangling (kept, shown orange)"))
                            .size(11.0)
                            .color(theme::ORANGE_SOFT),
                    );
                }
                if busy {
                    ui.add(egui::Spinner::new().size(10.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Pan MMB · Zoom wheel · Delete removes the selected view")
                            .size(11.0)
                            .color(theme::TEXT_MUTED),
                    )
                });
            });
        });
    egui::Panel::left("sheets")
        .default_size(240.0)
        .min_size(200.0)
        .max_size(260.0)
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(0, 12)),
        )
        .show(ui, |ui| {
            meta_edit = left_panel(d, ui, &app.doc, now);
        });
    egui::Panel::right("view_props")
        .default_size(300.0)
        .min_size(260.0)
        .max_size(320.0)
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(14, 12)),
        )
        .show(ui, |ui| {
            props(d, ui, now);
        });
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(theme::hex(0x2a2d33))
                .inner_margin(Margin::same(20)),
        )
        .show(ui, |ui| {
            sheet(d, ui, model.as_deref(), now);
        });
    if let Some((k, v)) = meta_edit {
        let _ = app.doc.edit(&format!("Set {k}"), |dd| {
            dd.meta.insert(k.clone(), v);
            Ok::<_, DocError>(())
        });
    }
    if let Some(ext) = export {
        if let (Some(r), Some(dr)) = (d.result.clone(), d.draft.clone()) {
            let req = crate::files::Request::save(
                &format!("Export drawing as {}", ext.to_uppercase()),
                &ext.to_uppercase(),
                ext,
                &format!("{} - {}.{ext}", app.doc.data.name, dr.name),
            );
            app.ask_path(req, move |app, path| {
                let bytes = export_bytes(&r, &dr, ext);
                match std::fs::write(&path, bytes) {
                    Ok(()) => app.toast(format!("Exported {}", path.display())),
                    Err(e) => app.warn(format!("Export failed: {e}")),
                }
            });
        }
    }
}

/// Returns an edited document field (meta key, value) to commit.
fn left_panel(
    d: &mut DrawingUi,
    ui: &mut Ui,
    doc: &Document,
    now: f64,
) -> Option<(String, String)> {
    ui.spacing_mut().item_spacing.y = 2.0;
    Frame::new()
        .inner_margin(Margin::symmetric(14, 4))
        .show(ui, |ui| theme::section_label(ui, "Sheets"));
    let row = |ui: &mut Ui, text: &str, on: bool, muted: bool| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        if on {
            ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
        }
        ui.painter().text(
            rect.left_center() + vec2(14.0, 0.0),
            egui::Align2::LEFT_CENTER,
            text,
            theme::prop(13.0),
            if muted {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            },
        );
        resp
    };
    let mut switch = None;
    for dr in &doc.data.drawings {
        let label = format!("{} · {:?}", dr.name, dr.sheet.size);
        if row(ui, &label, Some(dr.id) == d.id, false).clicked() {
            switch = Some(dr.id);
        }
    }
    if let Some(id) = switch {
        d.id = Some(id);
        d.draft = doc.data.drawing(id).cloned();
        d.selected = None;
        d.edit_gen += 1;
    }
    if row(ui, "+ Add sheet", false, true)
        .on_hover_text("New sheet with the standard views")
        .clicked()
    {
        d.id = None;
        d.edit_gen += 1;
    }
    Frame::new()
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 14,
            bottom: 6,
        })
        .show(ui, |ui| theme::section_label(ui, "Views"));
    let Some(draft) = &mut d.draft else {
        return None;
    };
    for (i, v) in draft.views.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        if Some(v.id) == d.selected {
            ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
        }
        let letter = (b'A' + (i as u8 % 26)) as char;
        ui.painter().text(
            rect.left_center() + vec2(14.0, 0.0),
            egui::Align2::LEFT_CENTER,
            letter,
            theme::mono(12.0),
            theme::TEXT,
        );
        let label = if v.rotation_deg != 0.0 {
            format!(
                "{} · {}°",
                orientation_label(&v.orientation),
                v.rotation_deg
            )
        } else {
            orientation_label(&v.orientation)
        };
        ui.painter().text(
            rect.left_center() + vec2(36.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            theme::prop(13.0),
            theme::TEXT,
        );
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            scale_label(v.scale),
            theme::prop(11.0),
            theme::TEXT_MUTED,
        );
        if resp.clicked() {
            d.selected = Some(v.id);
        }
    }
    Frame::new()
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 14,
            bottom: 6,
        })
        .show(ui, |ui| {
            theme::section_label(ui, "Title block");
            ui.label(widgets::muted(
                "Templates: {param}, {meta.key}, {doc.name}, {doc.revision}, {sheet.scale}.",
            ));
        });
    let mut changed = false;
    Frame::new()
        .inner_margin(Margin::symmetric(14, 0))
        .show(ui, |ui| {
            for f in &mut draft.sheet.title_block.fields {
                ui.horizontal(|ui| {
                    ui.add_sized(vec2(64.0, 20.0), egui::Label::new(widgets::muted(&f.label)));
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut f.template)
                                .font(theme::mono(11.0))
                                .desired_width(140.0),
                        )
                        .changed();
                });
            }
        });
    if changed {
        d.changed(now);
    }
    // document fields used by the title block ({meta.*}); committed on Enter / focus loss
    let mut edit = None;
    Frame::new()
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 12,
            bottom: 4,
        })
        .show(ui, |ui| {
            theme::section_label(ui, "Document fields");
            for (key, label) in [
                ("revision", "Revision"),
                ("material", "Material"),
                ("author", "Drawn by"),
                ("part_number", "Part no."),
            ] {
                let id = ui.scope_id().with(("meta", key));
                let mut v = ui
                    .data_mut(|m| m.get_temp::<String>(id))
                    .unwrap_or_else(|| doc.data.meta.get(key).cloned().unwrap_or_default());
                ui.horizontal(|ui| {
                    ui.add_sized(vec2(64.0, 20.0), egui::Label::new(widgets::muted(label)));
                    let r = ui.add(egui::TextEdit::singleline(&mut v).desired_width(140.0));
                    if r.changed() {
                        ui.data_mut(|m| m.insert_temp(id, v.clone()));
                    }
                    if r.lost_focus() {
                        ui.data_mut(|m| m.remove::<String>(id));
                        if doc.data.meta.get(key).map(|s| s.as_str()).unwrap_or("") != v {
                            edit = Some((key.to_string(), v.clone()));
                        }
                    }
                });
            }
        });
    edit
}

fn scale_label(s: f64) -> String {
    if s >= 1.0 {
        format!("{}:1", s.round() as i32)
    } else {
        format!("1:{}", (1.0 / s).round() as i32)
    }
}

fn props(d: &mut DrawingUi, ui: &mut Ui, now: f64) {
    let Some(draft) = &mut d.draft else { return };
    let idx = d
        .selected
        .and_then(|id| draft.views.iter().position(|v| v.id == id));
    let Some(idx) = idx else {
        theme::section_label(ui, "Sheet");
        let mut ch = false;
        egui::ComboBox::from_id_salt("paper")
            .selected_text(format!("{:?}", draft.sheet.size))
            .show_ui(ui, |ui| {
                for p in [
                    PaperSize::A4,
                    PaperSize::A3,
                    PaperSize::A2,
                    PaperSize::A1,
                    PaperSize::A0,
                    PaperSize::AnsiA,
                    PaperSize::AnsiB,
                    PaperSize::AnsiC,
                    PaperSize::AnsiD,
                    PaperSize::AnsiE,
                ] {
                    ch |= ui
                        .selectable_value(&mut draft.sheet.size, p, format!("{p:?}"))
                        .changed();
                }
            });
        ui.horizontal(|ui| {
            ch |= ui
                .radio_value(
                    &mut draft.sheet.orientation,
                    PaperOrientation::Landscape,
                    "Landscape",
                )
                .changed();
            ch |= ui
                .radio_value(
                    &mut draft.sheet.orientation,
                    PaperOrientation::Portrait,
                    "Portrait",
                )
                .changed();
        });
        ui.label(widgets::muted("Click a view on the sheet to edit it."));
        if ch {
            d.changed(now);
        }
        return;
    };
    let letter = (b'A' + idx as u8) as char;
    let v = &mut draft.views[idx];
    let mut ch = false;
    ui.spacing_mut().item_spacing.y = 8.0;
    theme::section_label(ui, &format!("View {letter} · properties"));
    ui.label(
        RichText::new("Orientation")
            .size(12.0)
            .color(theme::TEXT_DIM),
    );
    let pill = |ui: &mut Ui, w: f32, h: f32, label: &str, on: bool| {
        ui.add_sized(
            vec2(w, h),
            egui::Button::new(RichText::new(label).size(12.0))
                .fill(if on {
                    theme::ACCENT_BG
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::new(
                    1.0,
                    if on {
                        theme::ACCENT
                    } else {
                        theme::BORDER_INPUT
                    },
                )),
        )
    };
    egui::Grid::new("faces")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            let w = (ui.available_width() - 12.0) / 3.0;
            for (i, (f, l)) in [
                (CubeFace::NegY, "Front"),
                (CubeFace::PosY, "Back"),
                (CubeFace::PosZ, "Top"),
                (CubeFace::NegZ, "Bottom"),
                (CubeFace::NegX, "Left"),
                (CubeFace::PosX, "Right"),
            ]
            .iter()
            .enumerate()
            {
                if pill(ui, w, 34.0, l, v.orientation == ViewOrientation::Face(*f)).clicked() {
                    v.orientation = ViewOrientation::Face(*f);
                    ch = true;
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    egui::Grid::new("isos")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            let w = (ui.available_width() - 18.0) / 4.0;
            let corners = [
                (1, 1, 1),
                (-1, 1, 1),
                (1, -1, 1),
                (-1, -1, 1),
                (1, 1, -1),
                (-1, 1, -1),
                (1, -1, -1),
                (-1, -1, -1),
            ];
            for (i, (x, y, z)) in corners.iter().enumerate() {
                let o = ViewOrientation::Iso(IsoCorner {
                    x: *x,
                    y: *y,
                    z: *z,
                });
                if pill(ui, w, 30.0, &orientation_label(&o), v.orientation == o).clicked() {
                    v.orientation = o;
                    ch = true;
                }
                if i % 4 == 3 {
                    ui.end_row();
                }
            }
        });
    ui.label(
        RichText::new("Custom model rotation")
            .size(12.0)
            .color(theme::TEXT_DIM),
    );
    let mut rot_ch = false;
    ui.horizontal(|ui| {
        for (i, n) in ["Rx", "Ry", "Rz"].iter().enumerate() {
            ui.vertical(|ui| {
                ui.label(widgets::muted(*n));
                rot_ch |= ui
                    .add(egui::DragValue::new(&mut d.rot[i]).speed(0.5).suffix("°"))
                    .changed();
            });
        }
    });
    if rot_ch {
        // model → view: turn about Z, tilt about X (Rx 35.26 / Rz 45 = an iso)
        let q = DQuat::from_rotation_x(-(90.0 - d.rot[0]).to_radians())
            * DQuat::from_rotation_y(d.rot[1].to_radians())
            * DQuat::from_rotation_z(-d.rot[2].to_radians());
        v.orientation = ViewOrientation::Rotation(q);
        ch = true;
    }
    ui.label(widgets::muted(
        "Or pick a face / iso above. Drag-orbit inside the view is not wired yet.",
    ));
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Rotation on paper")
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(widgets::mono_text(format!("{}°", v.rotation_deg)))
        });
    });
    ch |= ui
        .add(
            egui::Slider::new(&mut v.rotation_deg, -180.0..=180.0)
                .step_by(1.0)
                .show_value(false),
        )
        .changed();
    ui.horizontal(|ui| {
        let wrap = |a: f64| ((a + 180.0).rem_euclid(360.0)) - 180.0;
        if widgets::button(ui, "−90°").clicked() {
            v.rotation_deg = wrap(v.rotation_deg - 90.0);
            ch = true;
        }
        if widgets::button(ui, "0°").clicked() {
            v.rotation_deg = 0.0;
            ch = true;
        }
        if widgets::button(ui, "+90°").clicked() {
            v.rotation_deg = wrap(v.rotation_deg + 90.0);
            ch = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Scale").size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt("scale")
                .selected_text(scale_label(v.scale))
                .width(100.0)
                .show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 1.0, 2.0, 5.0] {
                        if ui.selectable_label(v.scale == s, scale_label(s)).clicked() {
                            v.scale = s;
                            ch = true;
                        }
                    }
                });
        });
    });
    ch |= ui.checkbox(&mut v.hidden_lines, "Hidden lines").changed();
    ch |= ui.checkbox(&mut v.smooth_edges, "Tangent edges").changed();
    ch |= ui
        .checkbox(&mut v.exact, "Exact HLR (falls back automatically)")
        .changed();
    if ch {
        d.changed(now);
    }
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        widgets::note(ui, "Views stay linked to the model. If a dimensioned edge disappears, the dimension turns orange as \"dangling\" and is kept, not deleted.");
    });
}

struct Xf {
    origin: Pos2,
    k: f32,
}

impl Xf {
    fn p(&self, s: DVec2) -> Pos2 {
        pos2(
            self.origin.x + s.x as f32 * self.k,
            self.origin.y - s.y as f32 * self.k,
        )
    }
    fn inv(&self, p: Pos2) -> DVec2 {
        DVec2::new(
            ((p.x - self.origin.x) / self.k) as f64,
            ((self.origin.y - p.y) / self.k) as f64,
        )
    }
}

fn ink(style: LineStyle) -> (f32, Option<(f32, f32)>) {
    match style {
        LineStyle::Visible => (0.35, None),
        LineStyle::Hidden => (0.25, Some((1.5, 0.75))),
        LineStyle::Thin => (0.18, None),
        LineStyle::Center => (0.18, Some((6.0, 1.5))),
        LineStyle::Phantom => (0.25, Some((8.0, 1.5))),
        LineStyle::Frame => (0.5, None),
    }
}

fn draw_prim(p: &egui::Painter, x: &Xf, prim: &Prim, col: Color32) {
    match prim {
        Prim::Polyline { pts, style } => {
            let (w, dash) = ink(*style);
            let st = Stroke::new((w * x.k * 1.6).max(0.8), col);
            let pts: Vec<Pos2> = pts.iter().map(|q| x.p(*q)).collect();
            match dash {
                None => {
                    p.add(egui::Shape::line(pts, st));
                }
                Some((a, b)) => {
                    p.extend(egui::Shape::dashed_line(&pts, st, a * x.k, b * x.k));
                }
            }
        }
        Prim::Circle { c, r, style } => {
            let (w, _) = ink(*style);
            p.circle_stroke(
                x.p(*c),
                *r as f32 * x.k,
                Stroke::new((w * x.k * 1.6).max(0.8), col),
            );
        }
        Prim::Arrow { tip, dir, size } => {
            let back = *tip - *dir * *size;
            let w = dir.perp() * *size * 0.3;
            p.add(egui::Shape::convex_polygon(
                vec![x.p(*tip), x.p(back + w), x.p(back - w)],
                col,
                Stroke::NONE,
            ));
        }
        Prim::Text {
            pos,
            height,
            angle_deg,
            text,
            anchor,
        } => {
            let size = (*height as f32 * x.k * 1.35).max(5.0);
            let galley = p.layout_no_wrap(text.clone(), egui::FontId::proportional(size), col);
            let w = galley.size().x;
            let h = galley.size().y;
            let a = -(*angle_deg as f32).to_radians();
            let dx = match anchor {
                Anchor::Start => 0.0,
                Anchor::Middle => -w / 2.0,
                Anchor::End => -w,
            };
            // galley origin is its top-left; put the baseline (≈ 0.8 h) on `pos`
            let rot = egui::emath::Rot2::from_angle(a);
            let at = x.p(*pos) + rot * vec2(dx, -h * 0.8);
            p.add(egui::epaint::TextShape::new(at, galley, col).with_angle(a));
        }
    }
}

/// Kind and offset of a one-edge length dimension whose line passes through `q`
/// (sheet mm, y up), matching the engine's layout: Horizontal / Vertical offsets
/// are measured from the edge's top / right end, Aligned along the edge's left
/// normal. Axis-aligned edges get Horizontal / Vertical; Shift forces Vertical.
fn single_edge_dim(p1: DVec2, p2: DVec2, q: DVec2, shift: bool) -> (LinearKind, f64) {
    let d = p2 - p1;
    let len = d.length().max(1e-9);
    if shift || d.x.abs() / len < 1e-3 {
        (LinearKind::Vertical, q.x - p1.x.max(p2.x))
    } else if d.y.abs() / len < 1e-3 {
        (LinearKind::Horizontal, q.y - p1.y.max(p2.y))
    } else {
        (LinearKind::Aligned, (q - p1).dot((d / len).perp()))
    }
}

fn nearest_edge(r: &DrawingResult, q: DVec2, tol: f64) -> Option<(ViewId, &ViewEdge, f64)> {
    let mut best: Option<(ViewId, &ViewEdge, f64)> = None;
    for v in &r.views {
        for e in v.edges.iter().filter(|e| e.visible && e.source.is_some()) {
            for w in e.points.windows(2) {
                let ab = w[1] - w[0];
                let t = ((q - w[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
                let dist = q.distance(w[0] + ab * t);
                if dist < tol && best.is_none_or(|b| dist < b.2) {
                    best = Some((v.id, e, dist));
                }
            }
        }
    }
    best
}

fn sheet(d: &mut DrawingUi, ui: &mut Ui, model: Option<&crate::worker::Output>, now: f64) {
    let avail = ui.available_rect_before_wrap();
    let resp = ui.interact(avail, ui.scope_id().with("sheet"), Sense::click_and_drag());
    let Some(draft) = d.draft.clone() else {
        ui.painter().text(
            avail.center(),
            egui::Align2::CENTER_CENTER,
            "No drawing",
            theme::prop(14.0),
            theme::TEXT_MUTED,
        );
        return;
    };
    if resp.dragged_by(egui::PointerButton::Middle) {
        d.pan += resp.drag_delta();
    }
    if resp.hovered() {
        let s = ui.input(|i| i.smooth_scroll_delta.y);
        if s.abs() > 0.0 {
            d.zoom = (d.zoom * (s * 0.0018).exp()).clamp(0.3, 8.0);
        }
    }
    let size = draft.sheet.size_mm();
    let fit = (avail.width() / size.x as f32).min(avail.height() / size.y as f32) * 0.98;
    let k = fit * d.zoom;
    let paper = Rect::from_center_size(
        avail.center() + d.pan,
        vec2(size.x as f32 * k, size.y as f32 * k),
    );
    let p = ui.painter_at(avail);
    p.rect_filled(
        paper.translate(vec2(0.0, 6.0)),
        0.0,
        Color32::from_black_alpha(90),
    );
    p.rect_filled(paper, 0.0, Color32::WHITE);
    let x = Xf {
        origin: paper.left_bottom(),
        k,
    };
    let black = theme::hex(0x15181c);
    let Some(r) = d.result.clone() else {
        p.text(
            paper.center(),
            egui::Align2::CENTER_CENTER,
            "Computing views…",
            theme::prop(14.0),
            theme::TEXT_FAINT,
        );
        return;
    };
    for prim in &r.frame {
        draw_prim(&p, &x, prim, black);
    }
    for vr in &r.views {
        let view = draft.views.iter().find(|v| v.id == vr.id);
        let hidden = view.is_some_and(|v| v.hidden_lines);
        let smooth = view.is_some_and(|v| v.smooth_edges);
        let sel = Some(vr.id) == d.selected;
        let col = if sel { theme::hex(0x1e5bb8) } else { black };
        for e in &vr.edges {
            if (!e.visible && !hidden) || (e.kind == HlrKindS::Smooth && !smooth) {
                continue;
            }
            draw_prim(
                &p,
                &x,
                &Prim::Polyline {
                    pts: e.points.clone(),
                    style: if e.visible {
                        LineStyle::Visible
                    } else {
                        LineStyle::Hidden
                    },
                },
                col,
            );
        }
        if let Some(err) = &vr.error {
            p.text(
                x.p((vr.min + vr.max) / 2.0),
                egui::Align2::CENTER_CENTER,
                err,
                theme::prop(11.0),
                theme::ORANGE,
            );
        }
        if let Some(v) = view {
            let label = format!(
                "{} · {}{}",
                v.name.to_uppercase(),
                scale_label(v.scale),
                if v.rotation_deg != 0.0 {
                    format!(" · ROTATED {}°", v.rotation_deg)
                } else {
                    String::new()
                }
            );
            p.text(
                x.p(DVec2::new((vr.min.x + vr.max.x) / 2.0, vr.min.y - 24.0)),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional((2.6 * k).max(7.0)),
                black,
            );
        }
        if sel {
            let r0 = Rect::from_two_pos(x.p(vr.min), x.p(vr.max)).expand(6.0);
            p.rect_stroke(
                r0,
                3.0,
                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)),
                egui::StrokeKind::Outside,
            );
        }
    }
    let mut dangling_n = 0;
    for a in &r.annotations {
        let col = match a.status {
            AnnotationStatus::Ok => black,
            AnnotationStatus::Dangling(_) => theme::ORANGE,
        };
        for prim in &a.prims {
            draw_prim(&p, &x, prim, col);
        }
        if let AnnotationStatus::Dangling(why) = &a.status {
            let text_at = a.prims.iter().find_map(|p| {
                if let Prim::Text { pos, .. } = p {
                    Some(*pos)
                } else {
                    None
                }
            });
            let at = match text_at {
                Some(t) => x.p(t) + vec2(0.0, 6.0),
                None => {
                    // nothing left to draw: tag it at its view
                    let view = draft
                        .annotations
                        .iter()
                        .find(|x| x.id == a.id)
                        .and_then(|x| annotation_view(&x.kind));
                    let vr = view.and_then(|v| r.view(v));
                    match vr {
                        Some(vr) => {
                            x.p(DVec2::new(vr.min.x, vr.max.y + 6.0))
                                + vec2(0.0, 14.0 * dangling_n as f32)
                        }
                        None => paper.left_top() + vec2(12.0, 12.0 + 14.0 * dangling_n as f32),
                    }
                }
            };
            dangling_n += 1;
            let why: String = match why.find("): ") {
                Some(i) => why[..=i].to_string(),
                None => why.chars().take(70).collect(),
            };
            p.text(
                at,
                if text_at.is_some() {
                    egui::Align2::CENTER_TOP
                } else {
                    egui::Align2::LEFT_BOTTOM
                },
                format!("⚠ {} · dangling: {why}", a.text),
                theme::prop(10.5),
                theme::ORANGE,
            );
        }
    }
    for (_, _, at) in &d.picks {
        p.circle_stroke(x.p(*at), 5.0, Stroke::new(1.5, theme::ACCENT));
    }
    let tol = (6.0 / k) as f64;
    if let Some(q) = resp.hover_pos().map(|h| x.inv(h)) {
        if d.tool != 0 && d.tool != 5 {
            if let Some((_, e, _)) = nearest_edge(&r, q, tol) {
                let pts: Vec<Pos2> = e.points.iter().map(|s| x.p(*s)).collect();
                p.add(egui::Shape::line(pts, Stroke::new(2.0, theme::ORANGE)));
            }
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Delete)) && !ui.ctx().text_edit_focused() {
        if let (Some(sel), Some(dr)) = (d.selected, &mut d.draft) {
            dr.views.retain(|v| v.id != sel);
            d.selected = None;
            d.changed(now);
        }
    }
    if !resp.clicked() {
        return;
    }
    let Some(q) = resp.interact_pointer_pos().map(|h| x.inv(h)) else {
        return;
    };
    click(
        d,
        &r,
        &draft,
        model,
        q,
        tol,
        ui.input(|i| i.modifiers.shift),
        now,
    );
}

#[allow(clippy::too_many_arguments)]
fn click(
    d: &mut DrawingUi,
    r: &DrawingResult,
    draft: &Drawing,
    model: Option<&crate::worker::Output>,
    q: DVec2,
    tol: f64,
    shift: bool,
    now: f64,
) {
    let view_at = r
        .views
        .iter()
        .find(|v| {
            q.x >= v.min.x - 3.0
                && q.x <= v.max.x + 3.0
                && q.y >= v.min.y - 3.0
                && q.y <= v.max.y + 3.0
        })
        .map(|v| v.id);
    // the engine's reference for the picked view edge (GUI_NEEDS #16: works on
    // placed instances of assembly views too); the curve kind when the model has it
    let hit = nearest_edge(r, q, tol).and_then(|(vid, e, _)| {
        let rf = DimRef::from_view_edge(e)?;
        let kind = e.source.and_then(|(body, idx)| {
            Some(model?.meshed(body)?.edges.get(idx as usize)?.kind)
        });
        Some(Hit { vid, rf, kind, body: e.source.map(|s| s.0) })
    });
    let had_hit = hit.is_some();
    let mut add: Option<AnnotationKind> = None;
    match d.tool {
        0 => {
            let o = d
                .selected
                .and_then(|s| draft.views.iter().find(|v| v.id == s))
                .map(|v| v.orientation)
                .unwrap_or(ViewOrientation::Iso(IsoCorner { x: 1, y: -1, z: 1 }));
            if let Some(dr) = &mut d.draft {
                let id = dr.add_view(&orientation_label(&o), o, q, 0.5);
                d.selected = Some(id);
                d.changed(now);
            }
            return;
        }
        1 => match (d.picks.pop(), hit) {
            (Some((pv, a, at)), Some(Hit { vid, rf, .. })) if pv == vid => {
                let dx = (q - at).abs();
                let kind = if shift || dx.y > dx.x {
                    LinearKind::Vertical
                } else {
                    LinearKind::Horizontal
                };
                add = Some(AnnotationKind::Linear {
                    view: vid,
                    a,
                    b: Some(rf.clone()),
                    kind,
                    offset: 10.0,
                });
            }
            (Some((vid, a, at)), None) => {
                // the dimension line goes where the paper was clicked
                let line = nearest_edge(r, at, tol)
                    .and_then(|(_, e, _)| Some((*e.points.first()?, *e.points.last()?)));
                let (kind, offset) = match line {
                    Some((p1, p2)) => single_edge_dim(p1, p2, q, shift),
                    None if shift => (LinearKind::Vertical, 8.0),
                    None => (LinearKind::Aligned, 8.0),
                };
                add = Some(AnnotationKind::Linear {
                    view: vid,
                    a,
                    b: None,
                    kind,
                    offset,
                });
            }
            (_, Some(Hit { vid, rf, kind: hk, .. })) if hk.is_none_or(|k| k == CurveKind::Line) => {
                d.picks.push((vid, rf.clone(), q))
            }
            _ => {}
        },
        2 => {
            if let Some(Hit { vid, rf, .. }) = hit {
                match d.picks.pop() {
                    Some((pv, origin, _)) if pv == vid => {
                        let axis = if shift { SheetAxis::Y } else { SheetAxis::X };
                        add = Some(AnnotationKind::Ordinate {
                            view: vid,
                            origin,
                            target: rf.clone(),
                            axis,
                            offset: 10.0,
                        });
                    }
                    _ => d.picks.push((vid, rf.clone(), q)),
                }
            }
        }
        3 | 8 => {
            if let Some(Hit { vid, rf, .. }) = hit.filter(|h| h.kind.is_none_or(|k| k == CurveKind::Circle)) {
                add = Some(if d.tool == 3 {
                    AnnotationKind::Radius {
                        view: vid,
                        circle: rf.clone(),
                        diameter: !shift,
                        leader_deg: 45.0,
                    }
                } else {
                    AnnotationKind::CenterMark {
                        view: vid,
                        circle: rf.clone(),
                    }
                });
            }
        }
        4 | 9 => {
            if let Some(Hit { vid, rf, .. }) = hit.filter(|h| h.kind.is_none_or(|k| k == CurveKind::Line)) {
                match d.picks.pop() {
                    Some((pv, a, _)) if pv == vid => {
                        let b = rf.clone();
                        add = Some(if d.tool == 4 {
                            AnnotationKind::Angle {
                                view: vid,
                                a,
                                b,
                                radius: 15.0,
                            }
                        } else {
                            AnnotationKind::Centerline { view: vid, a, b }
                        });
                    }
                    _ => d.picks.push((vid, rf.clone(), q)),
                }
            }
        }
        5 => {
            add = Some(AnnotationKind::Note {
                position: q,
                text: "Note · wall {wall} mm".into(),
                height: 3.5,
            })
        }
        6 => match (d.picks.pop(), hit) {
            (Some((vid, target, _)), None) => {
                add = Some(AnnotationKind::Leader {
                    view: vid,
                    target,
                    text: "See note".into(),
                    position: q,
                })
            }
            (_, Some(Hit { vid, rf, .. })) => d.picks.push((vid, rf.clone(), q)),
            _ => {}
        },
        7 => {
            if let Some(Hit { vid, body: Some(body), .. }) = hit {
                add = Some(AnnotationKind::Balloon {
                    view: vid,
                    body,
                    position: q + DVec2::new(15.0, 15.0),
                });
            }
        }
        10 => {
            let f = model.and_then(|m| {
                m.data
                    .features
                    .iter()
                    .find(|f| {
                        matches!(
                            f.kind,
                            FeatureKind::Split { .. } | FeatureKind::PartingJoint { .. }
                        )
                    })
                    .map(|f| f.id)
            });
            if let (Some(vid), Some(f)) = (view_at, f) {
                add = Some(AnnotationKind::PartingLine {
                    view: vid,
                    feature: f,
                });
            }
        }
        _ => {}
    }
    if let Some(kind) = add {
        if let Some(dr) = &mut d.draft {
            dr.annotate(kind);
            d.picks.clear();
            d.changed(now);
        }
    } else if !had_hit && d.picks.is_empty() {
        d.selected = view_at;
    }
}

/// Commit the draft right away (tests, leaving the screen).
pub fn commit_now(app: &mut EasyCadApp) -> Result<(), DocError> {
    let crate::app::Mode::Drawing(d) = &mut app.mode else {
        return Ok(());
    };
    if let (Some(id), Some(draft)) = (d.id, d.draft.clone()) {
        d.dirty_at = None;
        app.doc
            .edit_drawing(id, "Edit drawing", move |dr| *dr = draft)?;
    }
    Ok(())
}

/// The view an annotation belongs to (notes have none).
pub fn annotation_view(k: &AnnotationKind) -> Option<ViewId> {
    k.view()
}

/// A view edge under the cursor.
struct Hit {
    vid: ViewId,
    rf: DimRef,
    kind: Option<CurveKind>,
    body: Option<BodyId>,
}

/// Standard-scale third-angle layout for model extents on a sheet.
pub struct Layout {
    pub scale: f64,
    pub iso_scale: f64,
    pub top: DVec2,
    pub front: DVec2,
    pub right: DVec2,
    pub iso: DVec2,
}

pub const STANDARD_SCALES: [f64; 11] = [10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.4, 0.25, 0.2, 0.1, 0.05];

pub fn layout_for(ext: glam::DVec3, sheet: DVec2) -> Layout {
    let (sx, sy, sz) = (ext.x.max(1.0), ext.y.max(1.0), ext.z.max(1.0));
    let gap = 36.0; // room for dimensions and view labels between views
    let (left, bottom) = (22.0, 62.0); // notes block below
    let avail_w = sheet.x * 0.68 - left; // the right third holds the iso and title block
    let avail_h = sheet.y - bottom - 22.0;
    let scale = STANDARD_SCALES
        .iter()
        .copied()
        .find(|s| (sx + sy) * s + 2.0 * gap <= avail_w && (sy + sz) * s + 2.0 * gap <= avail_h)
        .unwrap_or(0.05);
    let fx = left + gap + sx * scale / 2.0;
    let fy = bottom + gap * 0.5 + sz * scale / 2.0;
    let iso_scale = STANDARD_SCALES
        .iter()
        .copied()
        .find(|s| *s <= scale + 1e-9 && (sx + sy) * s * 0.85 < sheet.x * 0.3)
        .unwrap_or(scale / 2.0);
    Layout {
        scale,
        iso_scale,
        front: DVec2::new(fx, fy),
        top: DVec2::new(fx, fy + sz * scale / 2.0 + gap + sy * scale / 2.0),
        right: DVec2::new(fx + sx * scale / 2.0 + gap + sy * scale / 2.0, fy),
        iso: DVec2::new(sheet.x * 0.82, sheet.y * 0.6),
    }
}

/// The bytes of a drawing export (`svg`, `dxf` or `pdf`): what the toolbar buttons write.
pub fn export_bytes(r: &DrawingResult, d: &Drawing, ext: &str) -> Vec<u8> {
    match ext {
        "svg" => to_svg(r, d).into_bytes(),
        "dxf" => to_dxf(r, d).into_bytes(),
        _ => to_pdf(r, d),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn single_edge_dim_follows_the_click() {
        use glam::DVec2 as V;
        let (k, o) = super::single_edge_dim(V::new(0.0, 0.0), V::new(10.0, 0.0), V::new(5.0, -7.0), false);
        assert!(matches!(k, super::LinearKind::Horizontal) && (o + 7.0).abs() < 1e-9);
        let (k, o) = super::single_edge_dim(V::new(2.0, 0.0), V::new(2.0, 9.0), V::new(9.0, 4.0), false);
        assert!(matches!(k, super::LinearKind::Vertical) && (o - 7.0).abs() < 1e-9);
        let (k, o) = super::single_edge_dim(V::new(0.0, 0.0), V::new(3.0, 4.0), V::new(-4.0, 3.0), false);
        assert!(matches!(k, super::LinearKind::Aligned) && (o - 5.0).abs() < 1e-9);
    }
    use super::*;

    #[test]
    fn layout_picks_a_standard_scale_that_fits() {
        let a3 = DVec2::new(420.0, 297.0);
        let l = layout_for(glam::DVec3::new(90.6, 54.6, 32.0), a3);
        assert_eq!(l.scale, 1.0);
        assert!(l.top.y + 54.6 / 2.0 < 297.0 - 10.0);
        let big = layout_for(glam::DVec3::new(400.0, 300.0, 200.0), a3);
        assert_eq!(big.scale, 0.25);
        let tiny = layout_for(glam::DVec3::new(12.0, 8.0, 5.0), a3);
        assert_eq!(tiny.scale, 5.0);
        assert!(STANDARD_SCALES.contains(&tiny.iso_scale));
    }
}
