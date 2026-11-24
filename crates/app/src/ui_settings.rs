//! Settings window (navigation preset, default process, units, mesh quality,
//! autosave), the `?` keyboard cheat sheet and the crash-recovery offer.

use egui::{vec2, Margin, RichText, Ui};

use crate::app::EasyCadApp;
use crate::settings::{MeshPref, NavPreset};
use crate::theme;
use crate::widgets;

pub fn windows(app: &mut EasyCadApp, ui: &mut Ui) {
    settings_window(app, ui);
    cheat_sheet(app, ui);
    recovery_window(app, ui);
}

fn settings_window(app: &mut EasyCadApp, ui: &mut Ui) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut s = app.settings.clone();
    let mut inch = crate::units::inch();
    let mut cloud_act = None;
    let mut mcp_on = app.mcp.is_some();
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
            let profiles: Vec<String> = easycad_model::process::ProcessProfile::builtins()
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
            ui.add_space(6.0);
            theme::section_label(ui, "Updates");
            ui.checkbox(&mut s.auto_reload, "Auto-reload on new build");
            ui.add(egui::Label::new(widgets::muted(format!(
                "Running build {}. When a newer build is installed, easyCAD restarts into it and restores your session.",
                crate::reload::build_label()
            ))).wrap());
            ui.add_space(6.0);
            theme::section_label(ui, "Claude bridge");
            ui.checkbox(&mut mcp_on, "Let Claude model in this window (MCP, this computer only)");
            let note = match &app.mcp {
                Some(b) => format!(
                    "Listening on {} · {} calls so far. Claude's steps show up live and undo one at a time.",
                    b.url().unwrap_or_else(|| "(not listening)".into()),
                    b.calls
                ),
                None => "Claude Code connects through the easycad plugin (tools/claude-plugin, docs/MCP.md). Also: easycad --mcp.".into(),
            };
            ui.add(egui::Label::new(widgets::muted(&note)).wrap());
            ui.add_space(6.0);
            cloud_act = drive_section(app, ui);
        });
    match cloud_act {
        Some(CloudAct::SignIn) => app.cloud.sign_in(),
        Some(CloudAct::SignOut) => app.cloud.sign_out(),
        Some(CloudAct::SyncNow) => app.cloud.sync_now(),
        Some(CloudAct::Open(p)) => {
            if let Err(e) = app.open_path(&p) {
                app.warn(format!("Open failed: {e}"));
            }
        }
        Some(CloudAct::Dismiss(i)) if i < app.cloud.conflicts.len() => {
            app.cloud.conflicts.remove(i);
        }
        Some(CloudAct::Dismiss(_)) | None => {}
    }
    if mcp_on != app.mcp.is_some() {
        let port = app.settings.mcp_port.unwrap_or(easycad_mcp::DEFAULT_PORT);
        app.set_mcp(ui.ctx(), mcp_on, port);
        s.mcp = app.mcp.is_some();
    }
    if inch != crate::units::inch() {
        crate::units::set_inch(inch);
        s.inch = inch;
        app.cache.usage.clear();
    }
    if s.nav != app.settings.nav
        || s.mesh != app.settings.mesh
        || s.default_profile != app.settings.default_profile
        || s.autosave_min != app.settings.autosave_min
        || s.auto_reload != app.settings.auto_reload
        || s.inch != app.settings.inch
        || s.mcp != app.settings.mcp
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

enum CloudAct {
    SignIn,
    SignOut,
    SyncNow,
    Open(std::path::PathBuf),
    Dismiss(usize),
}

/// Google Drive: sign in / out, what syncs where, status and conflicts.
fn drive_section(app: &EasyCadApp, ui: &mut Ui) -> Option<CloudAct> {
    use crate::cloud::Indicator;
    let c = &app.cloud;
    let mut act = None;
    theme::section_label(ui, "Google Drive");
    if !c.available() {
        let why = c.unavailable.as_deref().unwrap_or("Sync is off in this session.");
        ui.add(egui::Label::new(widgets::muted(format!("Sync is not set up on this computer. {why}"))).wrap());
        return None;
    }
    let ind = c.indicator();
    match &ind {
        Indicator::Starting => {
            ui.horizontal(|ui| {
                crate::widgets::spinner(ui, 12.0, None);
                ui.label(widgets::muted("Checking the sign-in…"));
            });
        }
        Indicator::SigningIn => {
            ui.horizontal(|ui| {
                crate::widgets::spinner(ui, 12.0, None);
                ui.label("Finish signing in in your browser…");
            });
        }
        Indicator::SignedOut | Indicator::SignInNeeded => {
            if c.needs_sign_in.is_some() {
                ui.label(RichText::new("Your Google sign-in expired. Sign in again to keep syncing.").color(theme::ORANGE_SOFT));
            }
            ui.add(
                egui::Label::new(widgets::muted(
                    "Keep your designs, parts library and settings in easyCAD/ in your Google Drive, on every computer you sign in on. easyCAD can only see the files it creates.",
                ))
                .wrap(),
            );
            if widgets::primary(ui, "  Sign in with Google  ").clicked() {
                act = Some(CloudAct::SignIn);
            }
        }
        _ => {
            let who = c.account.as_deref().unwrap_or("your Google account");
            ui.label(format!("Signed in as {who}"));
            ui.horizontal(|ui| {
                if matches!(ind, Indicator::Syncing { .. }) {
                    crate::widgets::spinner(ui, 12.0, None);
                }
                ui.label(widgets::muted(ind.label()));
            });
            ui.add(
                egui::Label::new(widgets::muted(format!(
                    "Documents: {} ↔ My Drive ▸ easyCAD ▸ Documents",
                    crate::cloud::documents_dir().display()
                )))
                .wrap(),
            );
            ui.horizontal(|ui| {
                if widgets::button(ui, "Sync now").clicked() {
                    act = Some(CloudAct::SyncNow);
                }
                if widgets::button(ui, "Sign out").clicked() {
                    act = Some(CloudAct::SignOut);
                }
            });
        }
    }
    if let Some(e) = &c.last_error {
        ui.add(egui::Label::new(RichText::new(e).size(11.5).color(theme::ORANGE_SOFT)).wrap());
    }
    for (i, k) in c.conflicts.iter().enumerate() {
        let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Conflict: {} changed here and on {}. Theirs is kept as {}.",
                    name(&k.local_abs),
                    k.remote_device.as_deref().unwrap_or("another device"),
                    name(&k.conflict_copy_abs)
                ))
                .size(11.5)
                .color(theme::ORANGE_SOFT),
            )
            .wrap(),
        );
        ui.horizontal(|ui| {
            let is_doc = k.conflict_copy_abs.join("doc.json").is_file();
            if is_doc && widgets::button(ui, "Open their version").clicked() {
                act = Some(CloudAct::Open(k.conflict_copy_abs.clone()));
            }
            if widgets::button(ui, "Dismiss").clicked() {
                act = Some(CloudAct::Dismiss(i));
            }
        });
    }
    act
}

/// Every shortcut, grouped. Shown by `?`.
pub fn shortcuts(app: &EasyCadApp) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
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
                s("Ctrl+[ / Ctrl+]", "hide / show the left / right pane"),
                s("Ctrl+\\", "hide both panes (again: show them)"),
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
                s("X", "explode (slider fine-tunes)"),
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

fn cheat_sheet(app: &mut EasyCadApp, ui: &mut Ui) {
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

fn recovery_window(app: &mut EasyCadApp, ui: &mut Ui) {
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
            ui.label("easyCAD did not close cleanly. These documents had changes that were not saved:");
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
