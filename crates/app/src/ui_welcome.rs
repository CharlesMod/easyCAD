//! First run: the empty-document welcome card in the viewport, and the
//! "New enclosure from PCB" wizard window (the model side is `wizard.rs`).

use egui::{pos2, vec2, Align2, Color32, Margin, Rect, RichText, Sense, Stroke, Ui};
use glam::DVec2;

use crate::app::CaseworkApp;
use crate::dialogs::joint::Kind as JointKind;
use crate::theme;
use crate::widgets;
use crate::wizard::{Board, Settings};

/// The wizard's state between frames.
pub struct WizardUi {
    pub path: String,
    pub board: Option<Board>,
    pub settings: Settings,
    pub error: Option<String>,
    pub profiles: Vec<String>,
}

impl WizardUi {
    pub fn new() -> WizardUi {
        let st = crate::settings::Settings::load();
        let profiles: Vec<String> = casework_model::process::ProcessProfile::builtins()
            .into_iter()
            .map(|p| p.name)
            .collect();
        let profile = st
            .default_profile
            .clone()
            .or(st.last_profile.clone())
            .filter(|p| profiles.contains(p))
            .unwrap_or_else(|| profiles.first().cloned().unwrap_or_default());
        WizardUi {
            path: String::new(),
            board: None,
            settings: Settings::new(&profile),
            error: None,
            profiles,
        }
    }

    pub fn load(&mut self, p: &std::path::Path) {
        self.path = p.display().to_string();
        match Board::read(p) {
            Ok(b) => {
                if self.settings.name == "enclosure" || self.board.is_some() {
                    self.settings.name = b.name.clone();
                }
                self.board = Some(b);
                self.error = None;
            }
            Err(e) => {
                self.board = None;
                self.error = Some(e);
            }
        }
    }
}

impl Default for WizardUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Is the document empty enough to greet the user?
pub fn is_empty(app: &CaseworkApp) -> bool {
    let d = &app.doc.data;
    d.features.is_empty() && d.sketches.is_empty() && d.imports.is_empty() && app.dialog.is_none()
}

/// Welcome card over an empty viewport.
pub fn welcome(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    if !is_empty(app) || app.wizard.is_some() {
        return;
    }
    let recent: Vec<std::path::PathBuf> = crate::settings::Settings::load()
        .recent
        .into_iter()
        .filter(|p| p.exists())
        .take(6)
        .collect();
    egui::Area::new(egui::Id::unique("welcome"))
        .order(egui::Order::Middle)
        .pivot(Align2::CENTER_CENTER)
        .fixed_pos(rect.center())
        .show(ui.ctx(), |ui| {
            widgets::card_frame()
                .inner_margin(Margin::same(22))
                .show(ui, |ui| {
                    ui.set_width(420.0);
                    ui.horizontal(|ui| {
                        widgets::icon(ui, widgets::Icon::Logo, 26.0, theme::ACCENT);
                        ui.label(RichText::new("Casework").size(20.0).strong());
                    });
                    ui.label(widgets::muted(
                        "Parametric enclosures for 3D printing and injection molding.",
                    ));
                    ui.add_space(14.0);
                    let big = |ui: &mut Ui, label: &str, sub: &str, primary: bool| {
                        let (r, resp) =
                            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
                        let fill = match (primary, resp.hovered()) {
                            (true, false) => theme::PRIMARY,
                            (true, true) => theme::ACCENT,
                            (false, false) => theme::BUTTON,
                            (false, true) => theme::HOVER,
                        };
                        let p = ui.painter();
                        p.rect_filled(r, 6.0, fill);
                        if !primary {
                            p.rect_stroke(r, 6.0, Stroke::new(1.0, theme::BORDER_INPUT), egui::StrokeKind::Inside);
                        }
                        p.text(r.left_top() + vec2(12.0, 9.0), Align2::LEFT_TOP, label, theme::prop(14.0), theme::TEXT_STRONG);
                        p.text(
                            r.left_top() + vec2(12.0, 29.0),
                            Align2::LEFT_TOP,
                            sub,
                            theme::prop(11.0),
                            if primary { theme::TEXT_DIM } else { theme::TEXT_MUTED },
                        );
                        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                    };
                    if big(
                        ui,
                        "New enclosure from PCB…",
                        "EasyEDA STEP or DXF → walls, standoffs, lid joint",
                        true,
                    )
                    .clicked()
                    {
                        app.wizard = Some(WizardUi::new());
                    }
                    ui.add_space(6.0);
                    ui.columns(2, |c| {
                        if big(&mut c[0], "Open…", "a .cwk document", false).clicked() {
                            app.open_dialog_file();
                        }
                        if big(&mut c[1], "Sample", "the sensor-node box", false).clicked() {
                            app.load(crate::sample::sensor_node(), None);
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(widgets::muted(
                        "Or start from scratch: S sketches on the XY plane. ? shows every shortcut.",
                    ));
                    if !recent.is_empty() {
                        ui.add_space(12.0);
                        theme::section_label(ui, "Recent");
                        for p in &recent {
                            let name = p
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            let dir = p
                                .parent()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default();
                            let (r, resp) =
                                ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, 4.0, theme::HOVER);
                            }
                            ui.painter().text(
                                r.left_top() + vec2(8.0, 5.0),
                                Align2::LEFT_TOP,
                                &name,
                                theme::prop(13.0),
                                theme::TEXT,
                            );
                            ui.painter().text(
                                r.left_top() + vec2(8.0, 20.0),
                                Align2::LEFT_TOP,
                                &dir,
                                theme::prop(10.5),
                                theme::TEXT_MUTED,
                            );
                            if resp.on_hover_text(p.display().to_string()).clicked() {
                                if let Err(e) = app.open_path(p) {
                                    app.warn(format!("Open failed: {e}"));
                                }
                            }
                        }
                    }
                });
        });
}

/// The "New enclosure from PCB" window.
pub fn wizard_window(app: &mut CaseworkApp, ui: &mut Ui) {
    let Some(mut w) = app.wizard.take() else {
        return;
    };
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    let mut close = false;
    let mut create = false;
    let mut browse = false;
    egui::Area::new(egui::Id::unique("wizard_dim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(&ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            ui.allocate_rect(screen, Sense::click());
        });
    egui::Area::new(egui::Id::unique("wizard"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_CENTER)
        .fixed_pos(screen.center())
        .show(&ctx, |ui| {
            widgets::card_frame()
                .inner_margin(Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(640.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("New enclosure from PCB").size(16.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(widgets::muted("Enter ✓ · Esc ✕"));
                        });
                    });
                    ui.label(widgets::muted(
                        "Builds ordinary features (profile, extrude, fillet, shell, standoffs, joint) driven by named parameters: edit anything afterwards.",
                    ));
                    ui.add_space(10.0);
                    // board file
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Board").color(theme::TEXT_DIM));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut w.path)
                                .hint_text("EasyEDA STEP (.step) or board outline DXF")
                                .font(theme::mono(12.0))
                                .desired_width(440.0),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if let Some(p) = crate::files::resolve(&w.path) {
                                w.load(&p);
                            }
                        }
                        if widgets::button(ui, "Browse…").clicked() {
                            browse = true;
                        }
                    });
                    match (&w.board, &w.error) {
                        (Some(b), _) => {
                            let what = match &b.kind {
                                crate::wizard::BoardKind::Step => "STEP".to_string(),
                                crate::wizard::BoardKind::Dxf { outline, .. } => {
                                    format!("DXF · outline layer {outline}")
                                }
                            };
                            let holes = if b.holes.is_empty() {
                                "no mounting holes (no standoffs)".to_string()
                            } else {
                                format!(
                                    "{} mounting hole{} Ø{:.1}",
                                    b.holes.len(),
                                    if b.holes.len() == 1 { "" } else { "s" },
                                    b.holes[0].1
                                )
                            };
                            ui.label(
                                RichText::new(format!(
                                    "{what} · {:.1} × {:.1} mm · {holes}",
                                    b.size.x, b.size.y
                                ))
                                .size(12.0)
                                .color(theme::GREEN),
                            );
                        }
                        (None, Some(e)) => {
                            ui.label(RichText::new(e).size(12.0).color(theme::ORANGE_SOFT));
                        }
                        (None, None) => {
                            ui.label(widgets::muted("Pick the board to start."));
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal_top(|ui| {
                        // preview
                        let (r, _) = ui.allocate_exact_size(vec2(270.0, 210.0), Sense::hover());
                        preview(ui, r, &w);
                        ui.add_space(14.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            let s = &mut w.settings;
                            egui::Grid::new("wiz_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                for (label, v, tip) in [
                                    ("Name", &mut s.name, "Document name"),
                                    ("Clearance", &mut s.clearance, "Board edge to inner wall, per side (mm)"),
                                    ("Wall", &mut s.wall, "Wall, floor and lid thickness (mm)"),
                                    ("Height", &mut s.height, "Outside height, floor to top (mm)"),
                                    ("Corner radius", &mut s.corner_r, "Outline corners (round board corners stay concentric)"),
                                    ("Standoffs", &mut s.standoff_h, "Board bottom above the floor (mm)"),
                                ] {
                                    ui.label(RichText::new(label).color(theme::TEXT_DIM)).on_hover_text(tip);
                                    ui.add(egui::TextEdit::singleline(v).font(theme::mono(12.0)).desired_width(150.0));
                                    ui.end_row();
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(RichText::new("Lid joint").color(theme::TEXT_DIM));
                            ui.horizontal_wrapped(|ui| {
                                for k in JointKind::ALL {
                                    if widgets::pill(ui, k.label(), s.joint == k).clicked() {
                                        s.joint = k;
                                    }
                                }
                            });
                            ui.label(RichText::new("Made by").color(theme::TEXT_DIM));
                            egui::ComboBox::from_id_salt("wiz_profile")
                                .selected_text(&s.profile)
                                .width(250.0)
                                .show_ui(ui, |ui| {
                                    for p in &w.profiles {
                                        ui.selectable_value(&mut s.profile, p.clone(), p);
                                    }
                                });
                        });
                    });
                    if let (Some(e), Some(_)) = (&w.error, &w.board) {
                        ui.add_space(6.0);
                        ui.label(RichText::new(e).size(12.0).color(theme::ORANGE_SOFT));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ok = ui.add_enabled(
                                w.board.is_some(),
                                egui::Button::new(
                                    RichText::new("  Create enclosure  ").color(theme::TEXT_STRONG),
                                )
                                .fill(theme::PRIMARY),
                            );
                            if ok.clicked() {
                                create = true;
                            }
                            if widgets::button(ui, "Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                });
        });
    if !ctx.text_edit_focused() || ctx.input(|i| i.modifiers.command) {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && w.board.is_some() {
            create = true;
        }
    }
    if browse {
        app.wizard = Some(w);
        let req = crate::files::Request::open(
            "Board file (EasyEDA STEP or DXF)",
            "STEP / DXF",
            &["step", "stp", "dxf"],
        );
        app.ask_path(req, |app, p| {
            if let Some(w) = app.wizard.as_mut() {
                w.load(&p);
            }
        });
        return;
    }
    if create {
        if let Some(b) = &w.board {
            match crate::wizard::build(b, &w.settings) {
                Ok(doc) => {
                    let src = b
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut st = crate::settings::Settings::load();
                    st.last_profile = Some(w.settings.profile.clone());
                    st.save();
                    app.load(doc, None);
                    app.toast(format!(
                        "Enclosure from {src}: every size is a parameter on the right"
                    ));
                    return;
                }
                Err(e) => w.error = Some(e),
            }
        }
    }
    if !close {
        app.wizard = Some(w);
    }
}

/// Board outline (grey), enclosure outline (blue, approximate), holes and bosses.
fn preview(ui: &mut Ui, r: Rect, w: &WizardUi) {
    let p = ui.painter_at(r);
    p.rect_filled(r, 6.0, theme::VIEWPORT);
    p.rect_stroke(r, 6.0, Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
    let Some(b) = &w.board else {
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            "top view",
            theme::prop(12.0),
            theme::TEXT_FAINT,
        );
        return;
    };
    let vals = |e: &str| {
        let mut v = casework_model::params::Values::new();
        for (n, x) in [("wall", &w.settings.wall), ("clearance", &w.settings.clearance)] {
            if let Ok(x) = crate::widgets::eval(x, &v) {
                v.insert(n.to_string(), x);
            }
        }
        crate::widgets::eval(e, &v).ok()
    };
    let off = vals(&format!("({}) + ({})", w.settings.clearance, w.settings.wall)).unwrap_or(2.5);
    let cr = vals(&w.settings.corner_r).unwrap_or(5.0);
    let outer = b.size + DVec2::splat(2.0 * off);
    let k = ((r.width() - 30.0) as f64 / outer.x).min((r.height() - 40.0) as f64 / outer.y);
    let c = r.center() - vec2(0.0, 6.0);
    let to = |q: DVec2| pos2(c.x + ((q.x - b.center.x) * k) as f32, c.y - ((q.y - b.center.y) * k) as f32);
    // enclosure: rounded rectangle around the board's bbox
    let er = Rect::from_center_size(c, vec2((outer.x * k) as f32, (outer.y * k) as f32));
    p.rect_stroke(
        er,
        (cr.max(0.0) * k) as f32,
        Stroke::new(1.5, theme::ACCENT),
        egui::StrokeKind::Middle,
    );
    let pts: Vec<egui::Pos2> = b.outline.iter().map(|q| to(*q)).collect();
    if pts.len() > 2 {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, theme::GREEN_REF)));
    }
    let boss = 3.5 * k as f32;
    for (h, d) in &b.holes {
        p.circle_stroke(to(*h), boss, Stroke::new(1.0, theme::TEXT_MUTED));
        p.circle_stroke(to(*h), (d / 2.0 * k) as f32, Stroke::new(1.0, theme::GREEN));
    }
    p.text(
        r.center_bottom() - vec2(0.0, 10.0),
        Align2::CENTER_CENTER,
        format!("outside {:.1} × {:.1} mm", outer.x, outer.y),
        theme::mono(11.0),
        theme::TEXT_MUTED,
    );
}
