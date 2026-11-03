//! 2D drawing sheet.
//!
//! PLACEHOLDER ADAPTER: the engine's 2D drawings (phase 2: projected views with
//! hidden-line removal, dimensions bound to model edges, PDF/DXF/SVG export) have
//! not landed. This screen lays out the sheet, views, title block and view
//! properties from the mock and draws each view by projecting the display mesh's
//! edge polylines (visible and hidden alike, no hidden-line removal). Annotation
//! tools and export are disabled until the API arrives.

use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
use glam::{Quat, Vec3};

use crate::app::CaseworkApp;
use crate::camera::{look_rot, CubeRegion};
use crate::theme;
use crate::widgets;

pub const PLACEHOLDER: bool = true;

#[derive(Clone)]
pub struct View {
    pub id: char,
    pub orient: String,
    pub rot: [f32; 3],
    pub paper_rot: f32,
    pub scale: f32,
}

pub struct DrawingUi {
    pub views: Vec<View>,
    pub selected: usize,
    pub tool: usize,
    pub hidden_lines: bool,
}

pub const TOOLS: [(&str, &str, &str); 11] = [
    (
        "Place view",
        "▣",
        "Place view: pick a body, then a face of the cube or Custom, click to drop",
    ),
    (
        "Dimension",
        "↔",
        "Dimension: pick an edge, two points or a circle · Shift locks horizontal/vertical",
    ),
    (
        "Ordinate",
        "⊢",
        "Ordinate: pick the zero edge, then features",
    ),
    (
        "Radius / ⌀",
        "⌀",
        "Radius or diameter: pick an arc or circle · Tab switches R/⌀",
    ),
    ("Angle", "∠", "Angle: pick two lines"),
    (
        "Note",
        "A",
        "Note: click to place · use {param} to insert live values",
    ),
    (
        "Leader",
        "↗",
        "Leader: pick a point on geometry, then the text position",
    ),
    (
        "Balloon",
        "①",
        "Balloon: pick a part · numbers follow the BOM",
    ),
    (
        "Center mark",
        "+",
        "Center mark: pick circles (multi-select ok)",
    ),
    ("Centerline", "—", "Centerline: pick two parallel edges"),
    (
        "Parting line",
        "PL",
        "Parting line: shows the PL from the joint feature in this view",
    ),
];

impl Default for DrawingUi {
    fn default() -> Self {
        DrawingUi {
            views: vec![
                View {
                    id: 'A',
                    orient: "Top".into(),
                    rot: [0.0; 3],
                    paper_rot: 0.0,
                    scale: 1.0,
                },
                View {
                    id: 'B',
                    orient: "Front".into(),
                    rot: [0.0; 3],
                    paper_rot: 0.0,
                    scale: 1.0,
                },
                View {
                    id: 'C',
                    orient: "Right".into(),
                    rot: [0.0; 3],
                    paper_rot: 0.0,
                    scale: 1.0,
                },
                View {
                    id: 'D',
                    orient: "Iso NE".into(),
                    rot: [35.26, 0.0, 45.0],
                    paper_rot: 15.0,
                    scale: 0.5,
                },
            ],
            selected: 3,
            tool: 1,
            hidden_lines: false,
        }
    }
}

impl DrawingUi {
    pub fn sheet_label(&self) -> &'static str {
        "A3 landscape"
    }
}

/// Camera orientation for a named drawing view.
pub fn view_rot(v: &View) -> Quat {
    let r = |x: i8, y: i8, z: i8| {
        look_rot(
            CubeRegion([x, y, z]).eye_dir(),
            if x == 0 && y == 0 {
                Vec3::Y * if z > 0 { 1.0 } else { -1.0 }
            } else {
                Vec3::Z
            },
        )
    };
    let base = match v.orient.as_str() {
        "Front" => r(0, -1, 0),
        "Back" => r(0, 1, 0),
        "Top" => r(0, 0, 1),
        "Bottom" => r(0, 0, -1),
        "Left" => r(-1, 0, 0),
        "Right" => r(1, 0, 0),
        "Iso NE" => r(1, 1, 1),
        "Iso NW" => r(-1, 1, 1),
        "Iso SE" => r(1, -1, 1),
        "Iso SW" => r(-1, -1, 1),
        "Iso NE ↓" => r(1, 1, -1),
        "Iso NW ↓" => r(-1, 1, -1),
        "Iso SE ↓" => r(1, -1, -1),
        "Iso SW ↓" => r(-1, -1, -1),
        _ => Quat::from_euler(
            glam::EulerRot::ZXY,
            v.rot[2].to_radians(),
            v.rot[0].to_radians(),
            v.rot[1].to_radians(),
        ),
    };
    base * Quat::from_rotation_z(-v.paper_rot.to_radians())
}

pub fn screen(app: &mut CaseworkApp, ui: &mut Ui) {
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
                    }
                }
            });
        });
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
                ui.label(
                    RichText::new("· placeholder: annotations need the engine's 2D drawings")
                        .size(11.0)
                        .color(theme::ORANGE_SOFT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Pan MMB · Zoom wheel")
                            .size(11.0)
                            .color(theme::TEXT_MUTED),
                    )
                });
            });
        });
    egui::Panel::left("sheets").default_size(240.0).min_size(200.0).max_size(260.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 12))).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        Frame::new().inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| theme::section_label(ui, "Sheets"));
        let row = |ui: &mut Ui, text: &str, on: bool, muted: bool| {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
            if on {
                ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
            }
            ui.painter().text(rect.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, text, theme::prop(13.0), if muted { theme::TEXT_MUTED } else { theme::TEXT });
            resp
        };
        row(ui, "Sheet 1 · A3", true, false);
        row(ui, "+ Add sheet", false, true).on_hover_text("More sheets come with the engine's drawing API");
        Frame::new().inner_margin(Margin { left: 14, right: 14, top: 14, bottom: 6 }).show(ui, |ui| theme::section_label(ui, "Views"));
        for (i, v) in d.views.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
            if i == d.selected {
                ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
            }
            ui.painter().text(rect.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, v.id, theme::mono(12.0), theme::TEXT);
            let label = if v.paper_rot != 0.0 { format!("{} · {}°", v.orient, v.paper_rot) } else { v.orient.clone() };
            ui.painter().text(rect.left_center() + vec2(36.0, 0.0), egui::Align2::LEFT_CENTER, label, theme::prop(13.0), theme::TEXT);
            ui.painter().text(rect.right_center() - vec2(14.0, 0.0), egui::Align2::RIGHT_CENTER, scale_label(v.scale), theme::prop(11.0), theme::TEXT_MUTED);
            if resp.clicked() {
                d.selected = i;
            }
        }
        Frame::new().inner_margin(Margin { left: 14, right: 14, top: 14, bottom: 6 }).show(ui, |ui| {
            theme::section_label(ui, "Title block");
            ui.label(widgets::muted("Fields come from the document and its parameters. Edit them here or on the sheet."));
        });
        let meta = &mut app.doc.data.meta;
        Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
            for k in ["title", "author", "part_number"] {
                let mut v = meta.get(k).cloned().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.add_sized(vec2(70.0, 20.0), egui::Label::new(widgets::muted(k.replace('_', " "))));
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(130.0)).changed() {
                        meta.insert(k.into(), v);
                    }
                });
            }
        });
    });
    let model = app.model.clone();
    let crate::app::Mode::Drawing(d) = &mut app.mode else {
        return;
    };
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
            props(d, ui);
        });
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(theme::hex(0x2a2d33))
                .inner_margin(Margin::same(20)),
        )
        .show(ui, |ui| {
            sheet(d, ui, model.as_deref(), &app.doc.data);
        });
}

fn scale_label(s: f32) -> String {
    if s >= 1.0 {
        format!("{}:1", s.round() as i32)
    } else {
        format!("1:{}", (1.0 / s).round() as i32)
    }
}

fn props(d: &mut DrawingUi, ui: &mut Ui) {
    let v = &mut d.views[d.selected];
    ui.spacing_mut().item_spacing.y = 8.0;
    theme::section_label(ui, &format!("View {} · properties", v.id));
    ui.label(
        RichText::new("Orientation")
            .size(12.0)
            .color(theme::TEXT_DIM),
    );
    egui::Grid::new("faces")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            let w = (ui.available_width() - 12.0) / 3.0;
            for (i, f) in ["Front", "Back", "Top", "Bottom", "Left", "Right"]
                .iter()
                .enumerate()
            {
                if ui
                    .add_sized(
                        vec2(w, 34.0),
                        egui::Button::new(RichText::new(*f).size(12.0))
                            .fill(if v.orient == *f {
                                theme::ACCENT_BG
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(Stroke::new(
                                1.0,
                                if v.orient == *f {
                                    theme::ACCENT
                                } else {
                                    theme::BORDER_INPUT
                                },
                            )),
                    )
                    .clicked()
                {
                    v.orient = f.to_string();
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
            for (i, f) in [
                "Iso NE",
                "Iso NW",
                "Iso SE",
                "Iso SW",
                "Iso NE ↓",
                "Iso NW ↓",
                "Iso SE ↓",
                "Iso SW ↓",
            ]
            .iter()
            .enumerate()
            {
                if ui
                    .add_sized(
                        vec2(w, 30.0),
                        egui::Button::new(RichText::new(*f).size(11.0))
                            .fill(if v.orient == *f {
                                theme::ACCENT_BG
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(Stroke::new(
                                1.0,
                                if v.orient == *f {
                                    theme::ACCENT
                                } else {
                                    theme::BORDER_INPUT
                                },
                            )),
                    )
                    .clicked()
                {
                    v.orient = f.to_string();
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
    ui.horizontal(|ui| {
        for (i, n) in ["Rx", "Ry", "Rz"].iter().enumerate() {
            ui.vertical(|ui| {
                ui.label(widgets::muted(*n));
                if ui
                    .add(egui::DragValue::new(&mut v.rot[i]).speed(0.5).suffix("°"))
                    .changed()
                {
                    v.orient = "Custom".into();
                }
            });
        }
    });
    ui.label(widgets::muted(
        "Or drag inside the view with Shift+MMB to orbit it, like the 3D viewport.",
    ));
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Rotation on paper")
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(widgets::mono_text(format!("{}°", v.paper_rot)))
        });
    });
    ui.add(
        egui::Slider::new(&mut v.paper_rot, -180.0..=180.0)
            .step_by(1.0)
            .show_value(false),
    );
    ui.horizontal(|ui| {
        let wrap = |a: f32| ((a + 180.0).rem_euclid(360.0)) - 180.0;
        if widgets::button(ui, "−90°").clicked() {
            v.paper_rot = wrap(v.paper_rot - 90.0);
        }
        if widgets::button(ui, "0°").clicked() {
            v.paper_rot = 0.0;
        }
        if widgets::button(ui, "+90°").clicked() {
            v.paper_rot = wrap(v.paper_rot + 90.0);
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Scale").size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt("scale")
                .selected_text(scale_label(v.scale))
                .width(100.0)
                .show_ui(ui, |ui| {
                    for s in [0.5f32, 1.0, 2.0] {
                        if ui.selectable_label(v.scale == s, scale_label(s)).clicked() {
                            v.scale = s;
                        }
                    }
                });
        });
    });
    ui.add_enabled(
        false,
        egui::Checkbox::new(&mut d.hidden_lines, "Hidden lines"),
    )
    .on_disabled_hover_text("Needs hidden-line removal from the engine");
    let mut t = true;
    ui.add_enabled(false, egui::Checkbox::new(&mut t, "Tangent edges"));
    ui.add_enabled(false, egui::Checkbox::new(&mut t, "Show parting line"));
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        widgets::note(ui, "Placeholder until the engine's 2D drawings land: views are projected mesh edges without hidden-line removal; dimensions and export are disabled.");
    });
}

fn sheet(
    d: &DrawingUi,
    ui: &mut Ui,
    model: Option<&crate::worker::Output>,
    data: &casework_model::document::DocData,
) {
    let avail = ui.available_rect_before_wrap();
    let aspect = 420.0 / 297.0;
    let w = avail.width().min(avail.height() * aspect).min(980.0);
    let rect = Rect::from_min_size(
        pos2(avail.center().x - w / 2.0, avail.top()),
        vec2(w, w / aspect),
    );
    let p = ui.painter_at(avail);
    p.rect_filled(
        rect.translate(vec2(0.0, 6.0)),
        0.0,
        Color32::from_black_alpha(90),
    );
    p.rect_filled(rect, 0.0, Color32::WHITE);
    let ink = theme::hex(0x1b1f24);
    let k = w / 420.0; // px per sheet mm
    let frame = rect.shrink(10.0 * k);
    p.rect_stroke(frame, 0.0, Stroke::new(1.2, ink), egui::StrokeKind::Inside);
    // title block
    let tb = Rect::from_min_max(
        pos2(frame.right() - 180.0 * k, frame.bottom() - 36.0 * k),
        frame.right_bottom(),
    );
    p.rect_stroke(tb, 0.0, Stroke::new(1.0, ink), egui::StrokeKind::Inside);
    p.hline(tb.x_range(), tb.top() + 12.0 * k, Stroke::new(0.8, ink));
    p.vline(
        tb.left() + 110.0 * k,
        (tb.top() + 12.0 * k)..=tb.bottom(),
        Stroke::new(0.8, ink),
    );
    let title = data
        .meta
        .get("title")
        .cloned()
        .unwrap_or_else(|| data.name.clone());
    p.text(
        tb.left_top() + vec2(4.0 * k, 6.0 * k),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        egui::FontId::proportional(5.0 * k),
        ink,
    );
    p.text(
        tb.left_top() + vec2(4.0 * k, 20.0 * k),
        egui::Align2::LEFT_CENTER,
        format!("{} · rev {}", data.name, data.revision),
        egui::FontId::proportional(3.6 * k),
        ink,
    );
    p.text(
        tb.left_top() + vec2(4.0 * k, 28.0 * k),
        egui::Align2::LEFT_CENTER,
        format!("profile: {}", data.active_profile),
        egui::FontId::proportional(3.0 * k),
        ink,
    );
    p.text(
        tb.left_top() + vec2(114.0 * k, 20.0 * k),
        egui::Align2::LEFT_CENTER,
        "SCALE 1:1 · mm",
        egui::FontId::proportional(3.4 * k),
        ink,
    );
    p.text(
        tb.left_top() + vec2(114.0 * k, 28.0 * k),
        egui::Align2::LEFT_CENTER,
        "SHEET 1/1 · A3",
        egui::FontId::proportional(3.4 * k),
        ink,
    );
    let Some(m) = model else {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No model yet",
            theme::prop(14.0),
            ink,
        );
        return;
    };
    // view cells: Top (A) above Front (B), Right (C) beside Front, D custom top-right
    let cells = [
        Rect::from_min_size(
            frame.min + vec2(15.0 * k, 12.0 * k),
            vec2(150.0 * k, 105.0 * k),
        ),
        Rect::from_min_size(
            frame.min + vec2(15.0 * k, 125.0 * k),
            vec2(150.0 * k, 105.0 * k),
        ),
        Rect::from_min_size(
            frame.min + vec2(175.0 * k, 125.0 * k),
            vec2(100.0 * k, 105.0 * k),
        ),
        Rect::from_min_size(
            frame.min + vec2(260.0 * k, 12.0 * k),
            vec2(130.0 * k, 105.0 * k),
        ),
    ];
    let bb = m.bbox;
    let center = bb.map(|b| b.center().as_vec3()).unwrap_or(Vec3::ZERO);
    let size = bb.map(|b| b.size().length() as f32).unwrap_or(100.0);
    for (i, v) in d.views.iter().enumerate() {
        let cell = cells[i.min(3)];
        let q = view_rot(v).inverse();
        let s = (cell.width().min(cell.height()) / size * 1.15) * v.scale.clamp(0.5, 1.0);
        let to = |x: &[f32; 3]| -> Pos2 {
            let c = q * (Vec3::from_array(*x) - center);
            pos2(cell.center().x + c.x * s, cell.center().y - c.y * s)
        };
        let line = Stroke::new(
            if i == d.selected { 1.0 } else { 0.8 },
            if i == d.selected {
                theme::hex(0x1e5bb8)
            } else {
                ink
            },
        );
        for b in &m.bodies {
            let Some(mesh) = &b.mesh else { continue };
            if b.body.reference {
                continue;
            }
            for e in &mesh.edges {
                let pts: Vec<Pos2> = e.points.iter().map(to).collect();
                p.add(egui::Shape::line(pts, line));
            }
        }
        let label = format!(
            "VIEW {} · {}{} · {}",
            v.id,
            v.orient.to_uppercase(),
            if v.paper_rot != 0.0 {
                format!(" · ROTATED {}°", v.paper_rot)
            } else {
                String::new()
            },
            scale_label(v.scale)
        );
        p.text(
            pos2(cell.center().x, cell.bottom() + 3.0 * k),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(3.2 * k),
            ink,
        );
    }
    let _ = Frame::new();
}
