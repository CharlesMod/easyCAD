//! Settings window (navigation preset, default process, units, mesh quality,
//! autosave), the `?` keyboard cheat sheet and the crash-recovery offer.

use egui::{vec2, Margin, RichText, Ui};

use crate::app::CaseworkApp;
use crate::settings::{MeshPref, NavPreset};
use crate::theme;
use crate::widgets;

pub fn windows(app: &mut CaseworkApp, ui: &mut Ui) {
    settings_window(app, ui);
    cheat_sheet(app, ui);
    recovery_window(app, ui);
}

fn settings_window(app: &mut CaseworkApp, ui: &mut Ui) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut s = app.settings.clone();
    let mut inch = crate::units::inch();
    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(app.viewport_rect.center() - vec2(200.0, 220.0))
        .frame(widgets::card_frame().inner_margin(Margin::same(16)))
        .show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            theme::section_label(ui, "Navigation");
            egui::ComboBox::from_id_salt("nav_preset")
                .width(260.0)
                .selected_text(s.nav.label())
                .show_ui(ui, |ui| {
                    for p in NavPreset::ALL {
                        ui.selectable_value(&mut s.nav, p, p.label());
                    }
                });
            let (o, p) = s.nav.hints();
            ui.label(widgets::muted(format!("Orbit {o} · pan {p} · zoom wheel")));
            ui.add_space(6.0);
            theme::section_label(ui, "New documents");
            let profiles: Vec<String> = casework_model::process::ProcessProfile::builtins()
                .into_iter()
                .map(|p| p.name)
                .collect();
            let cur = s
                .default_profile
                .clone()
                .unwrap_or_else(|| "Last used".to_string());
            ui.horizontal(|ui| {
                ui.label("Process");
                egui::ComboBox::from_id_salt("default_profile")
                    .width(240.0)
                    .selected_text(cur)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.default_profile, None, "Last used");
                        for p in &profiles {
                            ui.selectable_value(&mut s.default_profile, Some(p.clone()), p);
                        }
                    });
            });
            ui.add_space(6.0);
            theme::section_label(ui, "Display");
            ui.horizontal(|ui| {
                ui.label("Units");
                ui.radio_value(&mut inch, false, "mm");
                ui.radio_value(&mut inch, true, "inch");
            });
            ui.horizontal(|ui| {
                ui.label("Mesh quality");
                egui::ComboBox::from_id_salt("mesh_pref")
                    .width(200.0)
                    .selected_text(s.mesh.label())
                    .show_ui(ui, |ui| {
                        for m in MeshPref::ALL {
                            ui.selectable_value(&mut s.mesh, m, m.label());
                        }
                    });
            });
            ui.add_space(6.0);
            theme::section_label(ui, "Autosave");
            ui.horizontal(|ui| {
                ui.label("Recovery copy every");
                ui.add(egui::DragValue::new(&mut s.autosave_min).range(0..=60).suffix(" min"));
                if s.autosave_min == 0 {
                    ui.label(widgets::muted("off"));
                }
            });
            let note = match (&app.autosave.last_error, &app.autosave.last_path) {
                (Some(e), _) => format!("Last recovery copy failed: {e}"),
                (None, Some(p)) => format!("Last copy: {}", p.display()),
                (None, None) => "Unsaved work is copied to the recovery folder and offered again after a crash.".into(),
            };
            ui.add(egui::Label::new(widgets::muted(&note)).wrap());
        });
    if inch != crate::units::inch() {
        crate::units::set_inch(inch);
        s.inch = inch;
        app.cache.usage.clear();
    }
    if s.nav != app.settings.nav
        || s.mesh != app.settings.mesh
        || s.default_profile != app.settings.default_profile
        || s.autosave_min != app.settings.autosave_min
        || s.inch != app.settings.inch
    {
        let remesh = s.mesh != app.settings.mesh;
        app.settings = s;
        app.settings.save();
        if remesh {
            app.request_model();
        }
    }
    if !open {
        app.show_settings = false;
    }
}

/// Every shortcut, grouped. Shown by `?`.
pub fn shortcuts(app: &CaseworkApp) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let (orbit, pan) = app.settings.nav.hints();
    let s = |k: &str, v: &'static str| (k.to_string(), v);
    vec![
        (
            "View",
            vec![
                s(orbit, "orbit"),
                s(pan, "pan"),
                s("Wheel", "zoom at the cursor"),
                s("F", "fit all (fillet with edges selected)"),
                s("Home", "home view"),
                s("Alt+arrows, PgUp/Dn", "15° steps about X / Y / Z"),
                s("1 / 2 / 3", "shaded / + edges / wireframe"),
            ],
        ),
        (
            "Select",
            vec![
                s("Click", "select (Ctrl / Shift adds)"),
                s("Tab, Alt+wheel", "cycle what is under the cursor"),
                s("Esc", "clear selection / leave component"),
            ],
        ),
        (
            "Model",
            vec![
                s("S", "sketch"),
                s("E", "extrude"),
                s("R", "revolve"),
                s("H", "shell"),
                s("B", "boss"),
                s("J", "parting joint"),
                s("M", "mirror"),
                s("P", "parts library"),
                s("D", "drawing"),
                s("G", "move (gizmo) the selected component"),
                s("Enter / Esc", "dialog OK / cancel"),
            ],
        ),
        (
            "Assembly",
            vec![
                s("Double-click", "activate a component (browser)"),
                s("Esc", "up one level"),
                s("A", "new mate"),
                s("X", "explode slider"),
                s("I", "interference check"),
            ],
        ),
        (
            "File",
            vec![
                s("Ctrl+N / O / S", "new / open / save"),
                s("Ctrl+Shift+S", "save as"),
                s("Ctrl+I", "import STEP / DXF"),
                s("Ctrl+Z / Ctrl+Y", "undo / redo (also Ctrl+Shift+Z)"),
                s("Ctrl+,", "settings"),
                s("?", "this sheet"),
            ],
        ),
        (
            "Sketch",
            vec![
                s("L R C A", "line, rectangle, circle, arc"),
                s("F O P T D", "fillet, offset, project, trim, dimension"),
                s("123 / =expr", "exact length while drawing"),
            ],
        ),
    ]
}

fn cheat_sheet(app: &mut CaseworkApp, ui: &mut Ui) {
    if !app.show_keys {
        return;
    }
    let mut open = true;
    let groups = shortcuts(app);
    egui::Window::new("Keyboard shortcuts")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(app.viewport_rect.center())
        .frame(widgets::card_frame().inner_margin(Margin::same(16)))
        .show(ui.ctx(), |ui| {
            ui.set_width(900.0);
            ui.horizontal_top(|ui| {
                for col in 0..3 {
                    ui.allocate_ui_with_layout(vec2(290.0, 10.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(290.0);
                        for (name, keys) in groups.iter().skip(col).step_by(3) {
                            theme::section_label(ui, name);
                            ui.add_space(2.0);
                            for (k, v) in keys {
                                ui.horizontal_top(|ui| {
                                    ui.allocate_ui_with_layout(vec2(130.0, 16.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                                        ui.set_width(130.0);
                                        ui.add(egui::Label::new(RichText::new(k).font(theme::mono(11.0)).color(theme::TEXT_STRONG)).wrap());
                                    });
                                    ui.add(egui::Label::new(RichText::new(*v).size(12.0).color(theme::TEXT_DIM)).wrap());
                                });
                            }
                            ui.add_space(12.0);
                        }
                    });
                }
            });
            ui.label(widgets::muted("Press ? or Esc to close."));
        });
    if !open {
        app.show_keys = false;
    }
}

fn recovery_window(app: &mut CaseworkApp, ui: &mut Ui) {
    if app.recovery_offer.is_empty() {
        return;
    }
    let mut act: Option<(usize, bool)> = None;
    let mut dismiss = false;
    egui::Window::new("Recover unsaved work")
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ui.ctx().content_rect().center())
        .frame(widgets::card_frame().inner_margin(Margin::same(16)))
        .show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.label("Casework did not close cleanly. These documents had changes that were not saved:");
            ui.add_space(6.0);
            for (i, e) in app.recovery_offer.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&e.info.name).strong());
                        let from = e
                            .info
                            .original
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "never saved".into());
                        ui.label(widgets::muted(format!("{} · v{} · {from}", e.age_text(), e.info.revision)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Discard").clicked() {
                            act = Some((i, false));
                        }
                        if widgets::primary(ui, "Restore").on_hover_text(if i == 0 { "Enter" } else { "" }).clicked() {
                            act = Some((i, true));
                        }
                    });
                });
                ui.separator();
            }
            if widgets::button(ui, "Decide later").on_hover_text("Keep the copies; ask again next launch").clicked() {
                dismiss = true;
            }
        });
    if act.is_none() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        act = Some((0, true));
    }
    if let Some((i, restore)) = act {
        let e = app.recovery_offer.remove(i);
        if restore {
            match crate::recovery::load(&e) {
                Ok(doc) => {
                    let name = doc.data.name.clone();
                    app.load(doc, e.info.original.clone());
                    // restored work is unsaved until saved again
                    app.saved_revision = u64::MAX;
                    crate::recovery::discard(&e);
                    app.autosave.saved_rev = None;
                    app.autosave.last_at = f64::MIN;
                    app.toast(format!("Restored {name} · save it to keep it"));
                }
                Err(err) => app.warn(format!("Cannot restore {}: {err}", e.info.name)),
            }
        } else {
            crate::recovery::discard(&e);
        }
    }
    if dismiss {
        app.recovery_offer.clear();
    }
}
