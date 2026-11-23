//! Window layout: title bar, toolbar, browser, the pinned Parameters panel,
//! history strip and status bar around the viewport (or the active mode's screen).

use easycad_model::feature::{FeatureId, FeatureKind, SketchId};
use easycad_model::regen::FeatureStatus;
use egui::{vec2, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui};

use crate::app::{EasyCadApp, Mode};
use crate::dialogs::DialogKind;
use crate::pickstack::{PickKey, PickKind};
use crate::theme;
use crate::widgets::{self, Icon};

fn bar_frame(fill: Color32) -> Frame {
    Frame::new()
        .fill(fill)
        .inner_margin(Margin::symmetric(12, 0))
}

pub fn show(app: &mut EasyCadApp, ui: &mut Ui) {
    ui.painter().rect_filled(ui.max_rect(), 0.0, theme::GROUND);
    crate::panes::shortcuts(app, ui.ctx());
    title_bar(app, ui);
    match &app.mode {
        Mode::Model => model_screen(app, ui),
        Mode::Sketch(_) => crate::sketch_mode::screen(app, ui),
        Mode::Parts(_) => crate::ui_parts::screen(app, ui),
        Mode::Drawing(_) => crate::ui_drawing::screen(app, ui),
    }
    toasts(app, ui);
    if app.calibrate.is_some() {
        crate::ui_process::calibrate_window(app, ui);
    }
    new_doc_window(app, ui);
    crate::ui_welcome::wizard_window(app, ui);
    crate::ui_bom::window(app, ui);
}

/// New document: name + process profile (defaults to the last one used).
fn new_doc_window(app: &mut EasyCadApp, ui: &mut Ui) {
    let Some((mut name, mut profile)) = app.new_doc_prompt.take() else {
        return;
    };
    let profiles: Vec<String> = easycad_model::process::ProcessProfile::builtins()
        .into_iter()
        .map(|p| p.name)
        .collect();
    let mut open = true;
    let mut create = false;
    egui::Window::new("New document")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .frame(widgets::card_frame().inner_margin(Margin::same(14)))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut name).desired_width(220.0));
            });
            ui.label(RichText::new("Made by").color(theme::TEXT_DIM));
            for p in &profiles {
                ui.radio_value(&mut profile, p.clone(), p);
            }
            ui.label(widgets::muted(
                "Fits and teardrops follow this process; switch any time in the PROCESS panel.",
            ));
            ui.add_space(6.0);
            if widgets::primary(ui, "Create").clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                create = true;
            }
        });
    if create {
        app.create_doc(name.trim(), &profile);
    } else if open {
        app.new_doc_prompt = Some((name, profile));
    }
}

fn model_screen(app: &mut EasyCadApp, ui: &mut Ui) {
    let in_dialog = app.dialog.is_some();
    if !in_dialog {
        toolbar(app, ui);
    }
    status_bar(app, ui);
    if !in_dialog {
        history(app, ui);
    }
    // side panes fold to rails (crate::panes); the viewport takes what they free
    use crate::panes::{self, Pane, Side};
    let (left_was, right_was) = panes::shown(app, ui.ctx());
    let was = left_was;
    let mut open = was;
    panes::show(
        ui,
        Pane {
            id: "browser",
            side: Side::Left,
            name: "browser",
            default: 250.0,
            min: 220.0,
            max: 300.0,
            frame: Frame::new().fill(theme::PANEL),
        },
        &mut open,
        |ui| browser(app, ui),
    );
    panes::sync(app, Side::Left, was, open);
    let was = right_was;
    let mut open = was;
    panes::show(
        ui,
        Pane {
            id: "params",
            side: Side::Right,
            name: "parameters",
            default: if in_dialog { 280.0 } else { 300.0 },
            min: 260.0,
            max: 340.0,
            frame: Frame::new().fill(theme::PANEL),
        },
        &mut open,
        |ui| params_panel(app, ui),
    );
    panes::sync(app, Side::Right, was, open);
    egui::CentralPanel::no_frame().show(ui, |ui| {
        crate::ui_viewport::viewport(app, ui);
        let rect = app.viewport_rect;
        let (l, r) = panes::shown(app, ui.ctx());
        for (side, name, open) in [(Side::Left, "browser", l), (Side::Right, "parameters", r)] {
            if open && panes::edge_tab(ui, rect, side, name) {
                panes::set(app, side, false);
            }
        }
    });
}

// ---------------------------------------------------------------- title bar

pub fn title_bar(app: &mut EasyCadApp, ui: &mut Ui) {
    egui::Panel::top("title")
        .exact_size(40.0)
        .frame(bar_frame(theme::BAR))
        .show(ui, |ui| {
            // narrow windows (down to 480): drop the words first, never the controls
            let w = ui.available_width();
            let (narrow, tiny) = (w < 720.0, w < 560.0);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = if narrow { 6.0 } else { 10.0 };
                file_menu(app, ui);
                widgets::icon(ui, Icon::Logo, 20.0, theme::ACCENT);
                if !narrow {
                    ui.label(RichText::new("easyCAD").strong());
                    ui.label(RichText::new("/").color(theme::TEXT_FAINT));
                }
                ui.label(RichText::new(app.title()).strong());
                if !app.active.is_empty() && app.mode.is_model() {
                    ui.label(RichText::new("·").color(theme::TEXT_FAINT));
                    crate::ui_assembly::breadcrumb(app, ui);
                }
                match &app.mode {
                    Mode::Sketch(s) => {
                        let text = if tiny {
                            format!("Sketch · {}", s.plane_name())
                        } else {
                            format!("Sketch · {} · {}", s.sketch.name, s.plane_name())
                        };
                        chip(ui, &text, theme::ACCENT_DEEP);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::primary(ui, "Finish sketch")
                                .on_hover_text("Enter on an empty tool")
                                .clicked()
                            {
                                app.finish_sketch();
                            }
                            if widgets::button(ui, "Cancel").clicked() {
                                app.cancel_sketch();
                            }
                        });
                        return;
                    }
                    Mode::Parts(_) => {
                        ui.label(
                            RichText::new(if tiny { "Parts library" } else { "Parts library · press P" })
                                .size(12.0)
                                .color(theme::TEXT_MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::button(ui, "Back to model").clicked() {
                                app.mode = Mode::Model;
                            }
                        });
                        return;
                    }
                    Mode::Drawing(d) => {
                        let name = d
                            .draft
                            .as_ref()
                            .map(|x| x.name.clone())
                            .unwrap_or_else(|| "Sheet 1".into());
                        let text = if tiny {
                            format!("Drawing · {name}")
                        } else {
                            format!("Drawing · {name} · {}", d.sheet_label())
                        };
                        chip(ui, &text, theme::hex(0x2a2f37));
                        let mut back = false;
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            back = widgets::button(ui, "Back to model").clicked();
                        });
                        if back {
                            let _ = crate::ui_drawing::commit_now(app);
                            app.mode = Mode::Model;
                        }
                        return;
                    }
                    Mode::Model => {}
                }
                if let Some(d) = &app.dialog {
                    ui.label(
                        RichText::new(format!("editing: {}", d.kind.title()))
                            .size(12.0)
                            .color(d.kind.accent()),
                    );
                    let mut back = false;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        back = widgets::button(ui, "Back to model")
                            .on_hover_text("Cancel the dialog (Esc)")
                            .clicked();
                    });
                    if back {
                        app.close_dialog(false);
                    }
                    return;
                }
                let (state, col) = if app.regenerating() {
                    ("Regenerating…", theme::TEXT_MUTED)
                } else if app.is_dirty()
                    || (app.path.is_none() && !crate::ui_welcome::is_empty(app))
                {
                    (
                        if app.path.is_some() {
                            "Unsaved changes"
                        } else {
                            "Not saved yet"
                        },
                        theme::TEXT_MUTED,
                    )
                } else if app.path.is_none() {
                    ("New document", theme::TEXT_MUTED)
                } else {
                    ("Saved locally", theme::TEXT_MUTED)
                };
                if tiny {
                    // the state as a dot, the words in its tooltip
                    let (r, resp) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    let dot = if state == "Saved locally" { theme::hex(0x4caf72) } else { col };
                    ui.painter().circle_filled(r.center(), 3.0, dot);
                    resp.on_hover_text(state);
                } else {
                    ui.label(RichText::new(state).size(12.0).color(col));
                    ui.add_space(if narrow { 2.0 } else { 8.0 });
                }
                let undo_tip = match app.doc.undo_label() {
                    Some(l) => format!("Undo {l} (Ctrl+Z)"),
                    None => "Undo (Ctrl+Z)".into(),
                };
                if widgets::icon_button(ui, Icon::Undo, 32.0, &undo_tip, app.doc.can_undo())
                    .clicked()
                {
                    app.undo();
                }
                let redo_tip = match app.doc.redo_label() {
                    Some(l) => format!("Redo {l} (Ctrl+Shift+Z)"),
                    None => "Redo (Ctrl+Shift+Z)".into(),
                };
                if widgets::icon_button(ui, Icon::Redo, 32.0, &redo_tip, app.doc.can_redo())
                    .clicked()
                {
                    app.redo();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, resp) = ui.allocate_exact_size(vec2(34.0, 22.0), Sense::click());
                    let hov = resp.hovered();
                    ui.painter().rect(
                        r,
                        4.0,
                        if hov { theme::HOVER } else { Color32::TRANSPARENT },
                        Stroke::new(1.0, if hov { theme::BORDER_CARD } else { theme::BORDER_INPUT }),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::units::label(), theme::mono(12.0), theme::TEXT);
                    if resp
                        .on_hover_text("Display units (click to switch mm / in). The document stays in mm; fields accept 0.125in, 1/8\", 3mm.")
                        .clicked()
                    {
                        crate::units::set_inch(!crate::units::inch());
                        let mut st = crate::settings::Settings::load();
                        st.inch = crate::units::inch();
                        st.save();
                        app.cache.usage.clear();
                    }
                    if !narrow {
                        ui.label(RichText::new("Units").size(12.0).color(theme::TEXT_MUTED));
                    }
                });
            });
        });
}

fn chip(ui: &mut Ui, text: &str, fill: Color32) {
    Frame::new()
        .fill(fill)
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(theme::TEXT_STRONG));
        });
}

fn file_menu(app: &mut EasyCadApp, ui: &mut Ui) {
    let r = widgets::icon_button(ui, Icon::Menu, 28.0, "File", true);
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(220.0);
        let item = |ui: &mut Ui, label: &str, key: &str| {
            ui.add(egui::Button::new(label).shortcut_text(key))
        };
        if item(ui, "New", "Ctrl+N").clicked() {
            app.new_doc();
        }
        if item(ui, "New enclosure from PCB…", "").clicked() {
            app.wizard = Some(crate::ui_welcome::WizardUi::new());
        }
        if item(ui, "Open…", "Ctrl+O").clicked() {
            app.open_dialog_file();
        }
        if item(ui, "Save", "Ctrl+S").clicked() {
            app.save(false);
        }
        if item(ui, "Save as…", "Ctrl+Shift+S").clicked() {
            app.save(true);
        }
        ui.separator();
        if item(ui, "Import STEP / DXF…", "Ctrl+I").clicked() {
            app.import_file();
        }
        if item(ui, "Export…", "").clicked() {
            app.export = Some(crate::dialogs::export::ExportDialog::new(&app.doc.data));
        }
        ui.separator();
        if item(ui, "Settings…", "Ctrl+,").clicked() {
            app.show_settings = true;
        }
        if item(ui, "Keyboard shortcuts", "?").clicked() {
            app.show_keys = true;
        }
        ui.separator();
        if item(ui, "Open sample enclosure", "").clicked() {
            app.load(crate::sample::sensor_node(), None);
        }
        ui.separator();
        if item(ui, "Quit", "").clicked() {
            app.quit = true;
        }
    });
}

// ---------------------------------------------------------------- toolbar

fn toolbar(app: &mut EasyCadApp, ui: &mut Ui) {
    egui::Panel::top("toolbar")
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(8, 6)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                type Item = (Icon, &'static str, &'static str, Cmd);
                let groups: [(&str, Vec<Item>); 6] = [
                    (
                        "CREATE",
                        vec![
                            (
                                Icon::Sketch,
                                "Sketch",
                                "Sketch on a face or XY (S)",
                                Cmd::Sketch,
                            ),
                            (
                                Icon::Extrude,
                                "Extrude",
                                "Extrude a sketch profile (E)",
                                Cmd::Dialog(DialogKind::Extrude),
                            ),
                            (
                                Icon::Revolve,
                                "Revolve",
                                "Revolve a profile (R)",
                                Cmd::Dialog(DialogKind::Revolve),
                            ),
                        ],
                    ),
                    (
                        "MODIFY",
                        vec![
                            (
                                Icon::Fillet,
                                "Fillet",
                                "Fillet edges, self-healing (F with edges selected)",
                                Cmd::Dialog(DialogKind::Fillet),
                            ),
                            (
                                Icon::Chamfer,
                                "Chamfer",
                                "Chamfer edges",
                                Cmd::Dialog(DialogKind::Chamfer),
                            ),
                            (
                                Icon::Shell,
                                "Shell",
                                "Hollow out a body (H)",
                                Cmd::Dialog(DialogKind::Shell),
                            ),
                            (
                                Icon::Draft,
                                "Draft",
                                "Draft faces for molding",
                                Cmd::Dialog(DialogKind::Draft),
                            ),
                            (
                                Icon::Split,
                                "Split",
                                "Split a body at a plane",
                                Cmd::Dialog(DialogKind::Split),
                            ),
                        ],
                    ),
                    (
                        "PATTERN",
                        vec![
                            (
                                Icon::Pattern,
                                "Pattern",
                                "Linear or circular pattern of features",
                                Cmd::Dialog(DialogKind::Pattern),
                            ),
                            (
                                Icon::Mirror,
                                "Mirror",
                                "Mirror features (M)",
                                Cmd::Dialog(DialogKind::Mirror),
                            ),
                        ],
                    ),
                    (
                        "ENCLOSURE",
                        vec![
                            (
                                Icon::Boss,
                                "Boss",
                                "Screw bosses (B)",
                                Cmd::Dialog(DialogKind::Boss),
                            ),
                            (
                                Icon::Rib,
                                "Rib",
                                "Rib from an open sketch line",
                                Cmd::Dialog(DialogKind::Rib),
                            ),
                            (
                                Icon::Standoffs,
                                "Standoffs",
                                "Standoffs from an imported PCB",
                                Cmd::Dialog(DialogKind::Standoffs),
                            ),
                            (
                                Icon::Joint,
                                "Joint",
                                "Parting joint between halves (J)",
                                Cmd::Dialog(DialogKind::Joint),
                            ),
                            (
                                Icon::Parts,
                                "Parts",
                                "Parts library: screws, inserts, nuts… (P)",
                                Cmd::Parts,
                            ),
                        ],
                    ),
                    (
                        "ASSEMBLE",
                        vec![
                            (
                                Icon::Parts,
                                "Component",
                                "New empty component in the one being edited",
                                Cmd::NewComponent,
                            ),
                            (
                                Icon::Import,
                                "Insert",
                                "Insert a .ecad as a linked component",
                                Cmd::InsertEcad,
                            ),
                            (
                                Icon::Joint,
                                "Mate",
                                "Mate two components: rigid, hinge, slider… (A)",
                                Cmd::Mate,
                            ),
                            (
                                Icon::Fit,
                                "Move",
                                "Move the selected component with the gizmo (G)",
                                Cmd::Move,
                            ),
                            (
                                Icon::Warn,
                                "Interfere",
                                "Check every pair of parts for overlap (I)",
                                Cmd::Interference,
                            ),
                            (
                                Icon::Drawing,
                                "BOM",
                                "Bill of materials: indented / flat, CSV",
                                Cmd::Bom,
                            ),
                        ],
                    ),
                    (
                        "FILE",
                        vec![
                            (
                                Icon::Import,
                                "Import",
                                "Import STEP or DXF (Ctrl+I)",
                                Cmd::Import,
                            ),
                            (
                                Icon::Export,
                                "Export",
                                "Export STEP / STL / 3MF",
                                Cmd::Export,
                            ),
                            (
                                Icon::Drawing,
                                "Drawing",
                                "2D drawing sheet (D)",
                                Cmd::Drawing,
                            ),
                        ],
                    ),
                ];
                if let Some(cmd) = tool_rows(ui, &groups) {
                    run(app, cmd);
                }
            });
        });
}

type Tool = (Icon, &'static str, &'static str, Cmd);

/// Width of a group of labelled buttons (same rule as `widgets::tool_button`).
fn group_w_labelled(ui: &Ui, items: &[Tool]) -> f32 {
    items
        .iter()
        .map(|(_, l, _, _)| {
            (ui.painter()
                .layout_no_wrap(l.to_string(), theme::prop(11.0), theme::TEXT)
                .size()
                .x
                + 12.0)
                .max(52.0)
        })
        .sum::<f32>()
        + 2.0 * (items.len() as f32 - 1.0)
        + 16.0
}

/// Icon-only buttons (the narrow toolbar) and their gaps.
const ICON_W: f32 = 34.0;
const ICON_GAP: f32 = 2.0;
const GROUP_SEP: f32 = 11.0;

/// The toolbar's tools, laid out for the width there is: labelled groups when
/// they fit, else one row of icons (name in the tooltip), and what still does
/// not fit goes into the » menu at the end. Returns the tool clicked.
fn tool_rows(ui: &mut Ui, groups: &[(&'static str, Vec<Tool>)]) -> Option<Cmd> {
    let avail = ui.available_width();
    let labelled: f32 = groups.iter().map(|(_, it)| group_w_labelled(ui, it)).sum();
    let mut hit = None;
    if labelled <= avail {
        let n = groups.len();
        for (gi, (name, items)) in groups.iter().enumerate() {
            let bw = group_w_labelled(ui, items);
            let (rect, _) = ui.allocate_exact_size(vec2(bw, 72.0), Sense::hover());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Min)),
            );
            child.spacing_mut().item_spacing = vec2(2.0, 0.0);
            child.add_space(8.0);
            for &(ic, label, tip, cmd) in items {
                if widgets::tool_button(&mut child, ic, label, tip, false).clicked() {
                    hit = Some(cmd);
                }
            }
            ui.painter().text(
                egui::pos2(rect.center().x, rect.bottom() - 7.0),
                egui::Align2::CENTER_CENTER,
                *name,
                theme::prop(10.0),
                theme::TEXT_LABEL,
            );
            if gi + 1 < n {
                ui.painter().vline(rect.right(), rect.y_range(), Stroke::new(1.0, theme::hex(0x2c3037)));
            }
        }
        return hit;
    }
    // icons: how many fit, keeping room for the » button when some do not
    let all: Vec<(usize, Tool)> = groups
        .iter()
        .enumerate()
        .flat_map(|(gi, (_, it))| it.iter().map(move |t| (gi, *t)))
        .collect();
    let width_of = |k: usize| -> f32 {
        // the first k tools, with separators between groups
        let mut w = 0.0;
        for i in 0..k {
            if i > 0 {
                w += if all[i].0 != all[i - 1].0 { GROUP_SEP } else { ICON_GAP };
            }
            w += ICON_W;
        }
        w
    };
    let fits = if width_of(all.len()) <= avail {
        all.len()
    } else {
        (0..all.len())
            .rev()
            .find(|&k| width_of(k) + GROUP_SEP + ICON_W <= avail)
            .unwrap_or(0)
    };
    ui.spacing_mut().item_spacing.x = 0.0;
    for (i, &(gi, (ic, label, tip, cmd))) in all.iter().take(fits).enumerate() {
        if i > 0 {
            if gi != all[i - 1].0 {
                let (r, _) = ui.allocate_exact_size(vec2(GROUP_SEP, ICON_W), Sense::hover());
                ui.painter().vline(r.center().x, r.shrink2(vec2(0.0, 6.0)).y_range(), Stroke::new(1.0, theme::hex(0x2c3037)));
            } else {
                ui.add_space(ICON_GAP);
            }
        }
        let r = widgets::tool_icon(ui, ic, label, &format!("{label} · {tip}"), ICON_W);
        if r.clicked() {
            hit = Some(cmd);
        }
    }
    if fits < all.len() {
        if fits > 0 {
            ui.add_space(GROUP_SEP);
        }
        let r = widgets::more_button(ui, "More tools", ICON_W);
        egui::Popup::menu(&r).show(|ui| {
            ui.set_min_width(190.0);
            let mut group = usize::MAX;
            for &(gi, (ic, label, tip, cmd)) in &all[fits..] {
                if gi != group {
                    if group != usize::MAX {
                        ui.separator();
                    }
                    ui.label(RichText::new(groups[gi].0).size(10.0).color(theme::TEXT_LABEL));
                    group = gi;
                }
                if widgets::menu_tool(ui, ic, label).on_hover_text(tip).clicked() {
                    hit = Some(cmd);
                    ui.close();
                }
            }
        });
    }
    hit
}

#[derive(Clone, Copy)]
enum Cmd {
    Sketch,
    Dialog(DialogKind),
    Parts,
    Import,
    Export,
    Drawing,
    NewComponent,
    InsertEcad,
    Mate,
    Move,
    Interference,
    Bom,
}

fn run(app: &mut EasyCadApp, c: Cmd) {
    match c {
        Cmd::Sketch => app.open_sketch(None),
        Cmd::Dialog(k) => app.open_dialog(k),
        Cmd::Parts => app.mode = Mode::Parts(Box::new(crate::ui_parts::PartsUi::new(&app.doc))),
        Cmd::Import => app.import_file(),
        Cmd::Export => app.export = Some(crate::dialogs::export::ExportDialog::new(&app.doc.data)),
        Cmd::Drawing => app.mode = Mode::Drawing(Box::default()),
        Cmd::NewComponent => {
            let c = app.scope();
            app.new_component(c);
        }
        Cmd::InsertEcad => app.insert_ecad(),
        Cmd::Mate => app.open_mate(None),
        Cmd::Move => app.toggle_gizmo(),
        Cmd::Interference => app.check_interference(),
        Cmd::Bom => app.open_bom(),
    }
}

// ---------------------------------------------------------------- browser

pub(crate) fn row(ui: &mut Ui, indent: f32, height: f32, selected: bool) -> (egui::Rect, egui::Response) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme::hex(0x20242a));
    }
    (rect.with_min_x(rect.left() + indent), resp)
}

pub fn browser(app: &mut EasyCadApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        theme::section_label(ui, "Browser");
        if app.dialog.is_some() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("«").on_hover_text("Collapse").clicked() {
                    crate::panes::set(app, crate::panes::Side::Left, false);
                }
            });
        }
    });
    ui.add_space(4.0);
    let p = ui.painter().clone();
    let assembly = crate::ui_assembly::is_assembly(&app.doc.data);
    if assembly {
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() * 0.55)
            .auto_shrink([false, true])
            .show(ui, |ui| crate::ui_assembly::tree(app, ui));
        let sep = ui.cursor().top();
        p.hline(ui.max_rect().x_range(), sep, Stroke::new(1.0, theme::BORDER_SOFT));
        ui.add_space(4.0);
        let c = crate::ui_assembly::active_component(app);
        let (r, _) = row(ui, 14.0, 26.0, false);
        p.text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            if app.active.is_empty() {
                format!("{} (top level)", app.doc.data.name)
            } else {
                app.doc.data.component_name(c)
            },
            theme::prop(12.0),
            theme::TEXT_LABEL,
        );
    }
    let (r, _) = if assembly { (egui::Rect::NOTHING, ui.allocate_response(vec2(0.0, 0.0), Sense::hover())) } else { row(ui, 14.0, 30.0, false) };
    if !assembly {
    p.text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        &app.doc.data.name,
        theme::prop(13.0),
        theme::TEXT,
    );
    let w = p
        .layout_no_wrap(app.doc.data.name.clone(), theme::prop(13.0), theme::TEXT)
        .size()
        .x;
    p.text(
        r.left_center() + vec2(w + 8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("v{}", app.doc.data.revision),
        theme::prop(12.0),
        theme::TEXT_LABEL,
    );
    }
    let (r, _) = row(ui, 26.0, 28.0, false);
    p.text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        "Origin",
        theme::prop(13.0),
        theme::TEXT_MUTED,
    );

    let Some(model) = app.model.clone() else {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            crate::widgets::spinner(ui, 12.0, None);
            ui.label(widgets::muted("regenerating…"));
        });
        return;
    };
    let (r, _) = row(ui, 26.0, 28.0, false);
    p.text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        "Bodies",
        theme::prop(13.0),
        theme::TEXT_DIM,
    );
    for b in model.bodies.iter().filter(|b| !b.body.reference) {
        let id = b.body.id;
        let key = PickKey {
            body: id,
            kind: PickKind::Body,
        };
        let sel = app.selection.contains(&key);
        let vis = app.body_visible(id);
        let (r, resp) = row(ui, 40.0, 32.0, sel);
        p.rect_filled(
            egui::Rect::from_center_size(r.left_center() + vec2(5.0, 0.0), vec2(10.0, 10.0)),
            2.0,
            theme::hex(0xa3aab4),
        );
        let col = if vis {
            theme::TEXT
        } else {
            theme::TEXT.gamma_multiply(0.45)
        };
        p.text(
            r.left_center() + vec2(18.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &b.body.name,
            theme::prop(13.0),
            col,
        );
        let eye = egui::Rect::from_center_size(
            egui::pos2(r.right() - 26.0, r.center().y),
            vec2(28.0, 28.0),
        );
        let er = ui.interact(eye, ui.scope_id().with(("eye", id)), Sense::click());
        if er.hovered() {
            p.rect_filled(eye, 6.0, theme::HOVER);
        }
        widgets::paint_icon(
            &p,
            eye.shrink(6.0),
            if vis { Icon::Eye } else { Icon::EyeOff },
            if er.hovered() {
                theme::TEXT_STRONG
            } else {
                theme::TEXT_DIM
            },
        );
        if er
            .on_hover_text(if vis {
                format!("Hide {}", b.body.name)
            } else {
                format!("Show {}", b.body.name)
            })
            .clicked()
        {
            if vis {
                app.hidden.insert(id);
            } else {
                app.hidden.remove(&id);
            }
        } else if resp.clicked() {
            if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                if sel {
                    app.selection.retain(|k| *k != key);
                } else {
                    app.selection.push(key);
                }
            } else {
                app.selection = vec![key];
            }
            app.orientation_body = Some(id);
        }
    }
    let (r, _) = row(ui, 26.0, 28.0, false);
    p.text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        "Sketches",
        theme::prop(13.0),
        theme::TEXT_DIM,
    );
    let sketches: Vec<(SketchId, String, bool)> = app
        .view()
        .sketches
        .iter()
        .map(|s| {
            (
                s.id,
                s.sketch.name.clone(),
                app.doc
                    .data
                    .meta
                    .get(&format!("gui.sketch_src.{}", s.id.0))
                    .is_some_and(|v| v == "DXF"),
            )
        })
        .collect();
    let mut edit = None;
    for (sid, name, dxf) in sketches {
        let (r, resp) = row(ui, 40.0, 28.0, false);
        p.text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            &name,
            theme::prop(13.0),
            theme::TEXT_DIM,
        );
        if dxf {
            let w = p
                .layout_no_wrap(name.clone(), theme::prop(13.0), theme::TEXT)
                .size()
                .x;
            p.text(
                r.left_center() + vec2(w + 8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "DXF",
                theme::prop(11.0),
                theme::TEXT_LABEL,
            );
        }
        let resp = resp.on_hover_text("Double-click to edit");
        if resp.double_clicked() {
            edit = Some(sid);
        }
        resp.context_menu(|ui| {
            if ui.button("Edit sketch").clicked() {
                edit = Some(sid);
            }
            if ui.button("Extrude…").clicked() {
                app.open_dialog(DialogKind::Extrude);
                if let Some(crate::dialogs::Dialog {
                    body: crate::dialogs::Body::Extrude(e),
                    ..
                }) = &mut app.dialog
                {
                    e.sketch = Some(sid);
                }
            }
        });
    }
    if let Some(s) = edit {
        app.open_sketch(Some(s));
    }
    let refs: Vec<_> = model
        .bodies
        .iter()
        .filter(|b| b.body.reference)
        .map(|b| (b.body.id, b.body.name.clone(), b.body.part.is_some()))
        .collect();
    if !refs.is_empty() {
        let (r, _) = row(ui, 26.0, 28.0, false);
        p.text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            "References",
            theme::prop(13.0),
            theme::TEXT_DIM,
        );
        for (id, name, part) in refs {
            let key = PickKey {
                body: id,
                kind: PickKind::Body,
            };
            let sel = app.selection.contains(&key);
            let (r, resp) = row(ui, 40.0, 32.0, sel);
            p.rect_filled(
                egui::Rect::from_center_size(r.left_center() + vec2(5.0, 0.0), vec2(10.0, 10.0)),
                2.0,
                if part {
                    theme::hex(0x7d848e)
                } else {
                    theme::GREEN_REF
                },
            );
            let label = if part {
                name.clone()
            } else {
                format!("{name}.step")
            };
            let vis = app.body_visible(id);
            p.text(
                r.left_center() + vec2(18.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                theme::prop(13.0),
                if vis { theme::TEXT } else { theme::TEXT_MUTED },
            );
            p.text(
                r.right_center() - vec2(14.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                "locked",
                theme::prop(11.0),
                theme::TEXT_LABEL,
            );
            if resp.clicked() {
                app.selection = vec![key];
            }
            resp.context_menu(|ui| {
                if ui.button(if vis { "Hide" } else { "Show" }).clicked() {
                    if vis {
                        app.hidden.insert(id);
                    } else {
                        app.hidden.remove(&id);
                    }
                }
            });
        }
    }
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        ui.add_space(12.0);
        Frame::new().outer_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
            widgets::note(ui, "Imported PCBs land here as locked reference bodies. They're used for clearance checks and standoffs, never boolean'd.");
        });
    });
}

// ---------------------------------------------------------------- parameters

pub fn params_panel(app: &mut EasyCadApp, ui: &mut Ui) {
    let hot: std::collections::BTreeSet<String> = if app.dialog.is_some() {
        app.hot_params()
    } else {
        Default::default()
    };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        crate::ui_process::process_section(app, ui);
        ui.add_space(4.0);
        Frame::new().inner_margin(Margin { left: 14, right: 14, top: 12, bottom: 8 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_label(ui, "Parameters");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tip = if app.active.is_empty() { "New document parameter" } else { "New parameter of this component (shadows a document one of the same name)" };
                    if ui.add(egui::Button::new(RichText::new("+ Add").size(12.0)).fill(Color32::TRANSPARENT).min_size(vec2(0.0, 26.0))).on_hover_text(tip).clicked() {
                        app.params.adding = Some((format!("p{}", app.view().params.list.len() + 1), "0".into()));
                    }
                });
            });
        });
        let usage = app.cache.usage.clone();
        let view = app.view().clone();
        let vals = crate::adapters::values(&view);
        let names: Vec<String> = view.params.list.iter().map(|p| p.name.clone()).collect();
        let profile = view.profile();
        let env = widgets::ExprEnv { values: &vals, names: &names, profile: &profile };
        Frame::new().inner_margin(Margin { left: 14, right: 14, top: 0, bottom: 4 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width();
                ui.add_sized(vec2(w - 150.0, 14.0), egui::Label::new(widgets::muted("Name")));
                ui.add_sized(vec2(110.0, 14.0), egui::Label::new(widgets::muted("Expression")));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(widgets::muted("Used")));
            });
        });
        // a component being edited: its own params first, then the document's
        let scope = app.scope();
        let local: Vec<_> = app
            .doc
            .data
            .component(scope)
            .map(|c| c.params.list.clone())
            .unwrap_or_default();
        let mut groups: Vec<(Option<String>, Vec<easycad_model::params::Param>)> = Vec::new();
        if scope != easycad_model::assembly::ComponentId::ROOT {
            groups.push((Some(format!("{} · local", app.doc.data.component_name(scope))), local.clone()));
            groups.push((Some("Document".into()), app.doc.data.params.list.clone()));
        } else {
            groups.push((None, app.doc.data.params.list.clone()));
        }
        for (title, list) in &groups {
        if let Some(t) = title {
            Frame::new().inner_margin(Margin { left: 14, right: 14, top: 8, bottom: 4 }).show(ui, |ui| {
                ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::TEXT_LABEL));
            });
            if list.is_empty() {
                Frame::new().inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| {
                    ui.label(widgets::muted("None yet: + Add makes one here"));
                });
            }
        }
        let shadowed = |n: &str| title.as_deref() == Some("Document") && local.iter().any(|l| l.name == n);
        for p in list {
            let is_hot = hot.contains(&p.name);
            let used = usage.get(&p.name).copied().unwrap_or(0);
            let r = Frame::new().fill(if is_hot { theme::ROW_HOT } else { Color32::TRANSPARENT }).inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| {
                ui.set_min_height(28.0);
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(w - 150.0, 30.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_min_width(w - 150.0);
                        ui.label(RichText::new(&p.name).font(theme::mono(12.0)).color(theme::TEXT));
                        let literal = p.expr.0.trim().parse::<f64>().is_ok();
                        let note = if shadowed(&p.name) {
                            "shadowed by the component's own".to_string()
                        } else if is_hot {
                            "used here".to_string()
                        } else if !p.comment.is_empty() {
                            p.comment.clone()
                        } else if !literal {
                            vals.get(&p.name).map(|v| format!("= {}", crate::units::len(*v))).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        if !note.is_empty() {
                            ui.label(RichText::new(note).size(11.0).color(theme::TEXT_LABEL));
                        }
                    });
                    let buf = app.params.buffers.entry(p.name.clone()).or_insert_with(|| display_expr(&p.expr.0));
                    let id = ui.scope_id().with(("param", &p.name));
                    let o = widgets::expr_field(ui, id, buf, 98.0, &env, None);
                    let escaped = o.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if escaped {
                        // Esc abandons the edit: back to the stored expression
                        *buf = display_expr(&p.expr.0);
                        if app.params.pending.as_ref().is_some_and(|x| x.0 == p.name) {
                            app.params.pending = None;
                        }
                        if app.params.error.as_ref().is_some_and(|x| x.0 == p.name) {
                            app.params.error = None;
                        }
                    } else if o.changed {
                        let text = buf.clone();
                        app.params.pending = Some((p.name.clone(), text, app.now + crate::app::DEBOUNCE));
                    }
                    if o.committed && !escaped {
                        if let Some((n, t, _)) = app.params.pending.take() {
                            app.params.pending = Some((n, t, app.now));
                        }
                    }
                    if !o.response.has_focus() && app.params.pending.as_ref().is_none_or(|x| x.0 != p.name) {
                        let canon = display_expr(&p.expr.0);
                        if *buf != canon && o.value.is_ok() {
                            *buf = canon;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let tip = app.cache.users.get(&p.name).cloned().unwrap_or_default();
                        ui.label(RichText::new(used.to_string()).font(theme::mono(11.0)).color(theme::TEXT_MUTED)).on_hover_text(tip);
                    });
                });
                if let Some((n, e)) = &app.params.error {
                    if *n == p.name {
                        ui.label(RichText::new(e).size(11.0).color(theme::ORANGE_SOFT));
                    }
                }
            });
            ui.painter().hline(r.response.rect.x_range(), r.response.rect.top(), Stroke::new(1.0, theme::BORDER_SOFT));
        }
        }
        if let Some((name, expr)) = &mut app.params.adding {
            let mut done = None;
            Frame::new().inner_margin(Margin::symmetric(14, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(name).font(theme::mono(12.0)).desired_width(90.0));
                    ui.add(egui::TextEdit::singleline(expr).font(theme::mono(12.0)).desired_width(80.0));
                    if ui.small_button("✓").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.small_button("✕").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    let (n, e) = app.params.adding.take().unwrap();
                    let r = if app.active.is_empty() { app.set_param(&n, &e) } else { app.set_local_param(&n, &e) };
                    if let Err(err) = r {
                        app.warn(err.to_string());
                    }
                }
                Some(false) => app.params.adding = None,
                None => {}
            }
        }
        Frame::new().outer_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if app.dialog.is_some() {
                widgets::note(ui, "Highlighted rows feed this dialog. Change one here and the preview updates.");
            } else {
                widgets::note(ui, "Edits regenerate live. Any dimension field accepts an expression like pcb_w + 2*(clearance + wall).");
            }
        });
    });
}

/// Literals shown with two decimals like the mock ("86.00"); expressions as typed.
pub fn display_expr(e: &str) -> String {
    match e.trim().parse::<f64>() {
        // literals are mm; in inch display mode say so (the note shows inches)
        Ok(v) if crate::units::inch() => format!("{v:.2} mm"),
        Ok(v) => format!("{v:.2}"),
        Err(_) => e.to_string(),
    }
}

// ---------------------------------------------------------------- history

pub fn feature_code(k: &FeatureKind) -> (&'static str, Color32) {
    match k {
        FeatureKind::Extrude { .. } => ("EX", theme::TEXT_DIM),
        FeatureKind::Revolve { .. } => ("RV", theme::TEXT_DIM),
        FeatureKind::Fillet { .. } => ("FL", theme::ORANGE_SOFT),
        FeatureKind::Chamfer { .. } => ("CH", theme::ORANGE_SOFT),
        FeatureKind::Shell { .. } => ("SH", theme::ORANGE_SOFT),
        FeatureKind::Draft { .. } => ("DR", theme::ORANGE_SOFT),
        FeatureKind::Split { .. } => ("SP", theme::ORANGE_SOFT),
        FeatureKind::Rib { .. } => ("RB", theme::GREEN),
        FeatureKind::ScrewBoss { .. } | FeatureKind::StandoffsFromPcb { .. } => {
            ("BS", theme::GREEN)
        }
        FeatureKind::Pattern { .. } => ("PT", theme::TEXT_DIM),
        FeatureKind::Mirror { .. } => ("MR", theme::TEXT_DIM),
        FeatureKind::PartingJoint { .. } => ("JT", theme::GREEN),
        FeatureKind::ImportStep { .. } => ("IM", theme::GREEN),
        FeatureKind::InsertPart { .. } => ("PR", theme::GREEN),
        FeatureKind::ContextRef { .. } => ("CX", theme::GREEN),
    }
}

/// Chip label like "Fillet r5.3", "Split @ lid_split", "Standoffs ×4".
pub fn chip_label(app: &EasyCadApp, f: &easycad_model::feature::Feature) -> String {
    let vals = crate::adapters::values(app.view());
    let ev = |e: &easycad_model::params::Expr| {
        e.eval(&vals)
            .ok()
            .map(crate::units::short)
            .unwrap_or_else(|| e.0.clone())
    };
    let name = &f.name;
    match &f.kind {
        FeatureKind::Extrude { distance, .. } => format!("{name} {}", ev(distance)),
        FeatureKind::Fillet { radius, .. } => format!("{name} r{}", ev(radius)),
        FeatureKind::Chamfer { distance, .. } => format!("{name} {}", ev(distance)),
        FeatureKind::Shell { thickness, .. } => format!("{name} {}", ev(thickness)),
        FeatureKind::Split { plane, offset, .. } => {
            format!("{name} @ {}", crate::dialogs::plane_text(plane, offset))
        }
        FeatureKind::StandoffsFromPcb { .. } => {
            let n = app
                .model
                .as_ref()
                .and_then(|m| m.result.report(f.id))
                .map(|_| crate::ui_parts::count_standoffs(app, f.id))
                .unwrap_or(0);
            if n > 0 {
                format!("{name} ×{n}")
            } else {
                name.clone()
            }
        }
        FeatureKind::ScrewBoss { points, .. } => format!("{name} ×{}", points.len()),
        FeatureKind::Draft { angle_deg, .. } => format!("{name} {}°", ev(angle_deg)),
        _ => name.clone(),
    }
}

fn history(app: &mut EasyCadApp, ui: &mut Ui) {
    egui::Panel::bottom("history")
        .exact_size(52.0)
        .frame(
            Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(12, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("HISTORY").size(10.5).color(theme::TEXT_LABEL));
                if !app.active.is_empty() {
                    let c = app.scope();
                    ui.label(
                        RichText::new(app.doc.data.component_name(c))
                            .size(11.0)
                            .color(theme::ACCENT_LINK),
                    )
                    .on_hover_text("History of the component being edited");
                }
                ui.add_space(6.0);
                let feats: Vec<(FeatureId, String, FeatureKind)> = app
                    .view()
                    .features
                    .iter()
                    .map(|f| {
                        (
                            f.id,
                            app.cache
                                .chips
                                .get(&f.id)
                                .cloned()
                                .unwrap_or_else(|| f.name.clone()),
                            f.kind.clone(),
                        )
                    })
                    .collect();
                // sketches appear before the first feature that uses them
                let mut shown_sketches = std::collections::HashSet::new();
                let n = feats.len();
                let marker = app.rollback().unwrap_or(n);
                let mut click: Option<usize> = None;
                let mut dbl: Option<FeatureId> = None;
                let mut sketch_edit = None;
                let mut accept: Option<FeatureId> = None;
                if let Some(hint) = history_hint(app) {
                    ui.label(RichText::new(hint).size(12.0).color(theme::TEXT_MUTED));
                }
                // the right-hand hint only where there is room (the chips' tooltips say it too)
                let hint = ui.ctx().content_rect().width() >= 860.0;
                egui::ScrollArea::horizontal()
                    .max_width(ui.available_width() - if hint { 210.0 } else { 0.0 })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, (id, label, kind)) in feats.iter().enumerate() {
                                let sk = match kind {
                                    FeatureKind::Extrude { sketch, .. }
                                    | FeatureKind::Revolve { sketch, .. }
                                    | FeatureKind::Rib { sketch, .. } => Some(*sketch),
                                    _ => None,
                                };
                                if let Some(s) = sk {
                                    if shown_sketches.insert(s) {
                                        let name = app
                                            .view()
                                            .sketch(s)
                                            .map(|s| s.name.clone())
                                            .unwrap_or_default();
                                        let r = history_chip(
                                            ui,
                                            "SK",
                                            theme::ACCENT_LINK,
                                            &name,
                                            i < marker,
                                            false,
                                            None,
                                        );
                                        if r.on_hover_text("Double-click to edit the sketch")
                                            .double_clicked()
                                        {
                                            sketch_edit = Some(s);
                                        }
                                    }
                                }
                                let (code, col) = feature_code(kind);
                                let status = app.feature_status(*id).cloned();
                                let warn = match &status {
                                    Some(FeatureStatus::Error(e)) => {
                                        Some((theme::RED, format!("Error: {e}")))
                                    }
                                    Some(FeatureStatus::Warning(w)) => {
                                        Some((theme::ORANGE_SOFT, format!("Warning: {w}")))
                                    }
                                    Some(FeatureStatus::Unresolved(u)) => Some((
                                        theme::ORANGE,
                                        format!(
                                            "A reference no longer matches (score {:.2}) · re-pick",
                                            u.score
                                        ),
                                    )),
                                    _ => None,
                                };
                                let r = history_chip(
                                    ui,
                                    code,
                                    col,
                                    label,
                                    i < marker,
                                    i + 1 == marker && app.rollback().is_some(),
                                    warn.as_ref().map(|w| w.0),
                                );
                                let tip = match &warn {
                                    Some((_, t)) => format!(
                                        "{}\n{t}\nClick to roll back here · double-click to edit",
                                        kind.label()
                                    ),
                                    None => format!(
                                        "{} · click to roll back here · double-click to edit",
                                        kind.label()
                                    ),
                                };
                                let timing =
                                    match app.model.as_ref().and_then(|m| m.result.report(*id)) {
                                        Some(rep) if rep.cached => format!(
                                            "\nRegen: cached (last computed in {:.0} ms)",
                                            rep.elapsed_ms
                                        ),
                                        Some(rep) => format!("\nRegen: {:.0} ms", rep.elapsed_ms),
                                        None => String::new(),
                                    };
                                let r = r.on_hover_text(format!("{tip}{timing}"));
                                if r.double_clicked() {
                                    dbl = Some(*id);
                                } else if r.clicked() {
                                    click = Some(i);
                                }
                                if matches!(status, Some(FeatureStatus::Unresolved(_))) {
                                    r.context_menu(|ui| {
                                        if ui.button("Accept the closest candidate").clicked() {
                                            accept = Some(*id);
                                        }
                                        if ui.button("Re-pick…").clicked() {
                                            dbl = Some(*id);
                                        }
                                    });
                                }
                            }
                            // a feature was added (or the file changed): show the
                            // newest chip instead of leaving it off to the right
                            let key = egui::Id::unique("history_len");
                            let last = ui.data(|d| d.get_temp::<usize>(key));
                            if last != Some(n) {
                                ui.data_mut(|d| d.insert_temp(key, n));
                                if last.is_some_and(|l| n > l) {
                                    ui.scroll_to_cursor(Some(egui::Align::Max));
                                }
                            }
                        });
                    });
                if let Some(i) = click {
                    app.set_rollback(if i + 1 >= n { None } else { Some(i + 1) });
                }
                if let Some(id) = dbl {
                    app.edit_feature(id);
                }
                if let Some(s) = sketch_edit {
                    app.open_sketch(Some(s));
                }
                if let Some(id) = accept {
                    if let Some(FeatureStatus::Unresolved(u)) = app.feature_status(id).cloned() {
                        if let Some(b) = &u.best_candidate {
                            if app.doc.accept_candidate(id, &u.selector, b) {
                                app.toast("Re-picked reference");
                                app.request_model();
                            }
                        }
                    }
                }
                if hint {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Click to roll back · double-click to edit")
                                .size(11.0)
                                .color(theme::TEXT_MUTED),
                        );
                    });
                }
            });
        });
}

/// What the history strip says when the level being edited has no features of
/// its own (an assembly's root: its history lives in the components).
pub fn history_hint(app: &EasyCadApp) -> Option<String> {
    if !app.view().features.is_empty() {
        return None;
    }
    let d = &app.doc.data;
    Some(if !app.active.is_empty() {
        format!("{} has no features yet · Sketch (S) to start", d.component_name(app.scope()))
    } else if crate::ui_assembly::is_assembly(d) {
        let first = d.occurrences.iter().find(|o| d.component(o.component).is_some_and(|c| !c.locked && !c.features.is_empty()));
        match first {
            Some(o) => format!("The assembly has no features of its own · double-click a component (e.g. {}) to edit its history", o.name),
            None => "The assembly has no features of its own · its components carry the history".into(),
        }
    } else {
        "No features yet · start with a Sketch (S)".into()
    })
}

fn history_chip(
    ui: &mut Ui,
    code: &str,
    code_col: Color32,
    label: &str,
    active: bool,
    marker: bool,
    warn: Option<Color32>,
) -> egui::Response {
    let font = theme::prop(12.0);
    let code_font = theme::mono(10.0);
    let lw = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let cw = ui
        .painter()
        .layout_no_wrap(code.to_owned(), code_font.clone(), theme::TEXT)
        .size()
        .x;
    let (rect, resp) = ui.allocate_exact_size(vec2(lw + cw + 26.0, 32.0), Sense::click());
    let border = if marker {
        theme::ACCENT
    } else if let Some(w) = warn {
        w
    } else {
        theme::hex(0x33373e)
    };
    let fill = if resp.hovered() {
        theme::HOVER
    } else {
        theme::CARD
    };
    let op: f32 = if active { 1.0 } else { 0.4 };
    ui.painter().rect(
        rect,
        CornerRadius::same(16),
        fill.gamma_multiply(op.max(0.6)),
        Stroke::new(1.0, border.gamma_multiply(op)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        code,
        code_font,
        warn.unwrap_or(code_col).gamma_multiply(op),
    );
    ui.painter().text(
        rect.left_center() + vec2(16.0 + cw, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        theme::TEXT.gamma_multiply(op),
    );
    resp
}

// ---------------------------------------------------------------- status

pub fn status_text(app: &EasyCadApp) -> String {
    if let Some(n) = app.rollback() {
        if let Some(f) = app.view().features.get(n.saturating_sub(1)) {
            let l = app
                .cache
                .chips
                .get(&f.id)
                .cloned()
                .unwrap_or_else(|| f.name.clone());
            return format!("Rolled back to \"{l}\" · later features suppressed");
        }
    }
    let Some(m) = &app.model else {
        return "Regenerating…".into();
    };
    let solids = m.bodies.iter().filter(|b| !b.body.reference).count();
    let errs = m.result.errors().len();
    let n = app.view().features.len();
    if let Some(a) = &m.asm {
        let occ = app.doc.data.all_occurrences().len();
        let issues = a.result.issues.len();
        let mut s = match crate::ui_assembly::active_label(app) {
            Some(l) => format!("{l} · {n} feature{}", if n == 1 { "" } else { "s" }),
            None => format!(
                "Ready · {occ} occurrence{} · {} placed bod{}",
                if occ == 1 { "" } else { "s" },
                m.ctx.len() + solids,
                if m.ctx.len() + solids == 1 { "y" } else { "ies" }
            ),
        };
        let errs: usize = a.result.components.iter().map(|c| c.regen.errors().len()).sum::<usize>() + issues;
        if errs > 0 {
            s += &format!(" · {errs} with errors");
        }
        if let Some(sv) = &a.solve {
            let bad = sv.mates.iter().filter(|r| matches!(r.status, easycad_model::assembly::MateStatus::OverConstrained(_) | easycad_model::assembly::MateStatus::Unsolved(_))).count();
            if bad > 0 {
                s += &format!(" · {bad} mate{} not solved", if bad == 1 { "" } else { "s" });
            }
        }
        return s;
    }
    let mut s = format!(
        "Ready · {solids} bod{} · {n} feature{}",
        if solids == 1 { "y" } else { "ies" },
        if n == 1 { "" } else { "s" }
    );
    if errs > 0 {
        s += &format!(" · {errs} with errors");
    }
    s
}

fn status_bar(app: &mut EasyCadApp, ui: &mut Ui) {
    egui::Panel::bottom("status")
        .exact_size(28.0)
        .frame(bar_frame(theme::BAR))
        .show(ui, |ui| {
            let w = ui.available_width();
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = if w < 720.0 { 10.0 } else { 16.0 };
                let small = |t: &str| RichText::new(t).size(11.0).color(theme::TEXT_MUTED);
                ui.label(small(&status_text(app)));
                if app.regenerating() {
                    crate::widgets::spinner(ui, 10.0, Some(theme::TEXT_MUTED));
                }
                if let Some(m) = app.model.as_ref().filter(|_| w >= 640.0) {
                    ui.label(small(&format!(
                        "regen {:.0} ms · {} tris · ui {:.1} ms",
                        app.last_regen_ms,
                        m.triangles(),
                        app.frame_ms
                    )));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    sync_indicator(app, ui);
                    let (orbit, pan) = app.settings.nav.hints();
                    if ui
                        .add(egui::Label::new(small("? shortcuts")).sense(Sense::click()))
                        .on_hover_text("Keyboard cheat sheet (?)")
                        .clicked()
                    {
                        app.show_keys = true;
                    }
                    // the mouse hints are in the cheat sheet too
                    if w >= 860.0 {
                        ui.label(small("Zoom wheel"));
                        ui.label(small(&format!("Pan {pan}")));
                        ui.label(small(&format!("Orbit {orbit}")));
                    }
                    if !app.gpu {
                        ui.label(
                            RichText::new("no GPU: viewport disabled")
                                .size(11.0)
                                .color(theme::ORANGE_SOFT),
                        );
                    }
                });
            });
        });
}

/// Drive sync state at the right end of the status bar; a click opens Settings.
fn sync_indicator(app: &mut EasyCadApp, ui: &mut Ui) {
    use crate::cloud::Indicator;
    let ind = app.cloud.indicator();
    if ind == Indicator::Hidden {
        return;
    }
    let dot = match ind {
        Indicator::Synced => theme::hex(0x4caf72),
        Indicator::Syncing { .. } | Indicator::SigningIn | Indicator::Starting => theme::ACCENT,
        Indicator::Conflict | Indicator::SignInNeeded => theme::ORANGE,
        _ => theme::TEXT_MUTED,
    };
    let text = RichText::new(ind.label()).size(11.0).color(match ind {
        Indicator::Conflict | Indicator::SignInNeeded => theme::ORANGE_SOFT,
        _ => theme::TEXT_MUTED,
    });
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let l = ui.add(egui::Label::new(text).sense(Sense::click()));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, dot);
            l
        })
        .inner;
    let tip = match ind {
        Indicator::SignedOut => "Google Drive sync: sign in in Settings",
        Indicator::SignInNeeded => "The Google sign-in expired: sign in again in Settings",
        Indicator::Offline => "No connection to Google Drive: changes sync when it is back",
        Indicator::Conflict => "A file changed on two computers: both versions are kept (see Settings)",
        _ => "Google Drive sync (Settings)",
    };
    if r.on_hover_text(tip).clicked() {
        app.show_settings = true;
    }
}

fn toasts(app: &mut EasyCadApp, ui: &mut Ui) {
    if app.toasts.is_empty() {
        return;
    }
    // stacked upward from just above the viewport's display-mode bar
    let screen = ui.ctx().content_rect();
    let vp = app.viewport_rect;
    let anchor = if vp.is_positive() && matches!(app.mode, crate::app::Mode::Model) {
        egui::pos2(vp.center().x, vp.bottom() - 62.0)
    } else {
        egui::pos2(screen.center().x, screen.bottom() - 70.0)
    };
    egui::Area::new(egui::Id::unique("toasts"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            for t in &app.toasts {
                Frame::new()
                    .fill(if t.warn {
                        theme::ORANGE_CARD
                    } else {
                        theme::CARD
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if t.warn {
                            theme::ORANGE
                        } else {
                            theme::BORDER_CARD
                        },
                    ))
                    .corner_radius(6.0)
                    .inner_margin(Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.set_max_width(320.0);
                        ui.label(RichText::new(&t.text).size(12.0));
                    });
            }
        });
}
