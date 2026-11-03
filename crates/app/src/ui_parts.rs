//! Parts library screen: metric/imperial, categories, size/length pills, hole
//! rules, click a hole edge (or press Enter) to place, "place at all matching
//! holes", and a BOM tab.

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::library::{hole_rules, Category, Fit, Head, HoleChoice, PartInfo, System};
use easycad_model::occt::CurveKind;
use easycad_model::select::EdgeSelector;
use egui::{vec2, Color32, Frame, Margin, RichText, Stroke, Ui};

use crate::app::EasyCadApp;
use crate::pickstack::{Filter, PickKey, PickKind};
use crate::theme;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Bom,
    Params,
}

pub struct PartsUi {
    pub source: usize,
    pub query: String,
    pub category: Category,
    pub system: System,
    pub item: usize,
    pub thread: String,
    pub length: Option<String>,
    pub tab: Tab,
    pub picked: Vec<PickKey>,
    pub last_placed: Option<FeatureId>,
}

pub fn categories() -> [(Category, &'static str); 7] {
    [
        (Category::Screw, "Screws"),
        (Category::Insert, "Inserts"),
        (Category::Nut, "Nuts"),
        (Category::Washer, "Washers"),
        (Category::Standoff, "Standoffs"),
        (Category::Magnet, "Magnets"),
        (Category::Foot, "Feet"),
    ]
}

/// Split "M3x10" / "#4-40x1/2" into (thread, length).
pub fn split_size(s: &str) -> (String, Option<String>) {
    match s.rfind('x') {
        Some(i) if i > 0 && !s[..i].ends_with('-') => {
            (s[..i].to_string(), Some(s[i + 1..].to_string()))
        }
        _ => (s.to_string(), None),
    }
}

impl PartsUi {
    pub fn new(_doc: &Document) -> Self {
        PartsUi {
            source: 0,
            query: String::new(),
            category: Category::Screw,
            system: System::Metric,
            item: 0,
            thread: "M3".into(),
            length: Some("10".into()),
            tab: Tab::Bom,
            picked: Vec::new(),
            last_placed: None,
        }
    }

    pub fn items(&self, doc: &Document) -> Vec<PartInfo> {
        let all: Vec<PartInfo> = if self.query.trim().is_empty() {
            doc.library.sources.iter().flat_map(|s| s.parts()).collect()
        } else {
            doc.library.search(&self.query)
        };
        let builtin = self.source == 0;
        all.into_iter()
            .filter(|p| (p.source == easycad_model::library::BUILTIN) == builtin)
            .filter(|p| !builtin || p.category == self.category)
            .filter(|p| !builtin || p.system.is_none_or(|s| s == self.system))
            .collect()
    }

    pub fn current(&self, doc: &Document) -> Option<PartInfo> {
        let items = self.items(doc);
        items
            .get(self.item.min(items.len().saturating_sub(1)))
            .cloned()
    }

    pub fn size(&self) -> String {
        match &self.length {
            Some(l) => format!("{}x{}", self.thread, l),
            None => self.thread.clone(),
        }
    }

    fn sizes(&self, doc: &Document, info: &PartInfo) -> Vec<String> {
        doc.library
            .source(&info.source)
            .map(|s| s.sizes(&info.id))
            .unwrap_or_default()
    }

    /// Make sure thread/length are valid for the current part.
    fn normalize(&mut self, doc: &Document) {
        let Some(info) = self.current(doc) else {
            return;
        };
        let sizes = self.sizes(doc, &info);
        if sizes.contains(&self.size()) {
            return;
        }
        let threads: Vec<String> = sizes.iter().map(|s| split_size(s).0).collect();
        if !threads.contains(&self.thread) {
            let want = if self.system == System::Metric {
                "M3"
            } else {
                "#4-40"
            };
            self.thread = threads
                .iter()
                .find(|t| *t == want)
                .or(threads.first())
                .cloned()
                .unwrap_or_default();
        }
        let lens: Vec<String> = sizes
            .iter()
            .filter(|s| split_size(s).0 == self.thread)
            .filter_map(|s| split_size(s).1)
            .collect();
        self.length = if lens.is_empty() {
            None
        } else if self.length.as_ref().is_some_and(|l| lens.contains(l)) {
            self.length.clone()
        } else {
            lens.iter()
                .find(|l| *l == "10" || *l == "3/8")
                .or(lens.first())
                .cloned()
        };
    }

    pub fn hole_choice(info: &PartInfo) -> Option<HoleChoice> {
        match info.category {
            Category::Screw => Some(HoleChoice::Counterbore(Fit::Normal)),
            Category::Insert => Some(HoleChoice::Insert),
            Category::Magnet | Category::Foot => Some(HoleChoice::Pocket),
            _ => None,
        }
    }
}

/// Circular edges with the same radius as `edge` on the same body (matching holes).
pub fn matching_holes(app: &EasyCadApp, key: PickKey) -> Vec<PickKey> {
    let PickKind::Edge(e) = key.kind else {
        return vec![];
    };
    let Some(m) = &app.model else { return vec![] };
    let Some(b) = m.meshed(key.body) else {
        return vec![];
    };
    let Some(r) = b
        .edges
        .get(e as usize)
        .and_then(|i| i.circle.map(|c| (c.2, c.1)))
    else {
        return vec![key];
    };
    let z = b.edges[e as usize].midpoint.z;
    b.edges
        .iter()
        .filter(|i| {
            i.kind == CurveKind::Circle
                && i.circle
                    .is_some_and(|c| (c.2 - r.0).abs() < 1e-3 && c.1.dot(r.1).abs() > 0.999)
                && (i.midpoint.z - z).abs() < 1e-3
        })
        .map(|i| PickKey {
            body: key.body,
            kind: PickKind::Edge(i.index),
        })
        .collect()
}

pub fn count_standoffs(app: &EasyCadApp, f: FeatureId) -> usize {
    let Some(feat) = app.doc.data.feature(f) else {
        return 0;
    };
    let FeatureKind::StandoffsFromPcb {
        source:
            PcbSource::Step {
                import,
                min_dia,
                max_dia,
            },
        ..
    } = &feat.kind
    else {
        return 0;
    };
    let Some(m) = &app.model else { return 0 };
    let Some(imp_f) = app
        .doc
        .data
        .features
        .iter()
        .find(|x| matches!(&x.kind, FeatureKind::ImportStep { import: i, .. } if i == import))
    else {
        return 0;
    };
    let Some(b) = m.bodies.iter().find(|b| b.body.id.feature == imp_f.id) else {
        return 0;
    };
    crate::dialogs::standoffs::board_holes(b.body.shape())
        .0
        .iter()
        .filter(|h| h.dia >= *min_dia && h.dia <= *max_dia)
        .count()
}

fn place(app: &mut EasyCadApp, keys: &[PickKey]) {
    let crate::app::Mode::Parts(p) = &app.mode else {
        return;
    };
    let Some(info) = p.current(&app.doc) else {
        return;
    };
    let size = p.size();
    let model = app.model.clone();
    let mut placements = Vec::new();
    let mut target = None;
    for k in keys {
        if let (PickKind::Edge(e), Some(m)) = (k.kind, &model) {
            if let Some(i) = m.meshed(k.body).and_then(|b| b.edges.get(e as usize)) {
                placements.push(PlacementRef::CircularEdge {
                    edge: EdgeSelector::from_pick(i),
                });
                target = Some(k.body);
            }
        }
    }
    let on_hole = !placements.is_empty();
    if placements.is_empty() {
        placements.push(PlacementRef::Explicit {
            origin: glam::DVec3::ZERO,
            axis: glam::DVec3::Z,
        });
    }
    let r = match app.doc.part_ref(&info, &size) {
        Ok(r) => r,
        Err(e) => {
            app.warn(e.to_string());
            return;
        }
    };
    let auto_hole = if on_hole {
        PartsUi::hole_choice(&info).map(|c| AutoHole {
            choice: c,
            depth: None,
            fit: None,
        })
    } else {
        None
    };
    let n = placements.len();
    let id = FeatureId(app.doc.data.next_id);
    let feat = Feature {
        id,
        name: format!("{} {size}", info.name),
        suppressed: false,
        kind: FeatureKind::InsertPart {
            part: r,
            placements,
            flip: false,
            offset: "0".into(),
            auto_hole,
            target,
        },
    };
    app.commit_features(&format!("Insert {size} ×{n}"), vec![feat], None);
    if let crate::app::Mode::Parts(p) = &mut app.mode {
        p.last_placed = Some(id);
        p.picked.clear();
    }
    app.toast(format!("Placed {size} ×{n}"));
}

pub fn screen(app: &mut EasyCadApp, ui: &mut Ui) {
    app.filter = Filter::EDGES_ONLY;
    egui::Panel::left("parts")
        .default_size(400.0)
        .min_size(320.0)
        .max_size(420.0)
        .frame(Frame::new().fill(theme::PANEL))
        .show(ui, |ui| left(app, ui));
    egui::Panel::right("bom")
        .default_size(300.0)
        .min_size(260.0)
        .max_size(320.0)
        .frame(Frame::new().fill(theme::PANEL))
        .show(ui, |ui| right(app, ui));
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let rect = ui.available_rect_before_wrap();
        crate::ui_viewport::viewport(app, ui);
        banner(app, ui, rect);
    });
    // Enter places at the picked hole edges (or the origin)
    if !ui.ctx().text_edit_focused()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        let keys: Vec<PickKey> = app
            .selection
            .iter()
            .copied()
            .filter(|k| matches!(k.kind, PickKind::Edge(_)))
            .collect();
        place(app, &keys);
    }
}

fn banner(app: &mut EasyCadApp, ui: &mut Ui, rect: egui::Rect) {
    let crate::app::Mode::Parts(p) = &app.mode else {
        return;
    };
    let Some(info) = p.current(&app.doc) else {
        return;
    };
    let sel: Vec<PickKey> = app
        .selection
        .iter()
        .copied()
        .filter(|k| matches!(k.kind, PickKind::Edge(_)))
        .collect();
    let rules = hole_rules(&split_size(&p.size()).0);
    let choice = PartsUi::hole_choice(&info);
    let cut = match (choice, &rules) {
        (Some(HoleChoice::Counterbore(_)), Some(r)) => r
            .counterbore
            .iter()
            .find(|(h, _)| *h == Head::Socket)
            .map(|(_, b)| {
                format!(
                    "auto-cut counterbore ⌀{:.2} × {:.2}",
                    r.mm(b.d),
                    r.mm(b.depth)
                )
            }),
        (Some(HoleChoice::Insert), Some(r)) => r
            .insert
            .map(|(d, depth)| format!("auto-cut insert hole ⌀{:.2} × {:.2}", r.mm(d), r.mm(depth))),
        _ => None,
    };
    let size = p.size();
    let last_placed = p
        .last_placed
        .filter(|id| app.doc.data.feature(*id).is_some());
    let all = sel
        .first()
        .map(|k| matching_holes(app, *k))
        .unwrap_or_default();
    let mut go: Option<Vec<PickKey>> = None;
    let mut undo = false;
    let mut as_component = false;
    egui::Area::new(egui::Id::unique("parts_banner"))
        .order(egui::Order::Middle)
        .fixed_pos(egui::pos2(rect.center().x - 260.0, rect.top() + 64.0))
        .show(ui.ctx(), |ui| {
            widgets::card_frame()
                .inner_margin(Margin {
                    left: 14,
                    right: 10,
                    top: 8,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if sel.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "Click a hole edge to place {} {size}",
                                    info.name.to_lowercase()
                                ))
                                .size(12.0),
                            );
                            if widgets::button(ui, "Place at origin").clicked() {
                                go = Some(Vec::new());
                            }
                            if widgets::button(ui, "Insert as component")
                                .on_hover_text("A locked, purchased component (one BOM line per placement); move it with G or mate it")
                                .clicked()
                            {
                                as_component = true;
                            }
                        } else {
                            ui.label(RichText::new("Snapped to hole edge").size(12.0));
                            if let Some(c) = &cut {
                                ui.label(
                                    RichText::new(format!("· {c}"))
                                        .size(12.0)
                                        .color(theme::ORANGE_SOFT),
                                );
                            }
                            if widgets::button(ui, "Place (Enter)").clicked() {
                                go = Some(sel.clone());
                            }
                            if all.len() > 1
                                && widgets::primary(
                                    ui,
                                    &format!("Place at all {} matching holes", all.len()),
                                )
                                .clicked()
                            {
                                go = Some(all.clone());
                            }
                        }
                        if last_placed.is_some() && widgets::button(ui, "Undo").clicked() {
                            undo = true;
                        }
                    });
                });
        });
    if undo {
        app.undo();
    } else if as_component {
        insert_component(app, &info, &size);
    } else if let Some(k) = go {
        place(app, &k);
    }
}

/// The library part as a component placed in the component being edited (at its
/// origin), selected with the move gizmo on.
pub fn insert_component(app: &mut EasyCadApp, info: &easycad_model::library::PartInfo, size: &str) {
    let parent = app.scope();
    let comp = match app.doc.add_part_component(info, size) {
        Ok(c) => c,
        Err(e) => {
            app.warn(e.to_string());
            return;
        }
    };
    let name = app.doc.data.component_name(comp);
    let n = crate::ui_assembly::instance_count(&app.doc.data, comp) + 1;
    match app.doc.add_occurrence(parent, comp, &format!("{name}:{n}"), easycad_model::assembly::Transform::IDENTITY) {
        Ok(occ) => {
            let mut path = app.active.clone();
            path.push(occ);
            app.mode = crate::app::Mode::Model;
            app.asm.sel = Some(path);
            app.asm.gizmo = true;
            app.toast(format!("Inserted {name} · drag the gizmo or mate it (A)"));
            app.request_model();
        }
        Err(e) => app.warn(e.to_string()),
    }
}

fn left(app: &mut EasyCadApp, ui: &mut Ui) {
    let crate::app::Mode::Parts(p) = &mut app.mode else {
        return;
    };
    let doc = &app.doc;
    p.normalize(doc);
    Frame::new()
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                theme::section_label(ui, "Parts library");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (i, l) in ["My STEP", "Built-in"]
                        .iter()
                        .enumerate()
                        .rev()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                    {
                        let idx = 1 - i;
                        let _ = idx;
                        let si = if *l == "Built-in" { 0 } else { 1 };
                        if widgets::pill(ui, l, p.source == si).clicked() {
                            p.source = si;
                            p.item = 0;
                        }
                    }
                });
            });
            let n = p.items(doc).len();
            Frame::new()
                .fill(theme::INPUT)
                .stroke(Stroke::new(1.0, theme::BORDER_CARD))
                .corner_radius(6.0)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut p.query)
                                    .hint_text("search (m3, insert, magnet…)")
                                    .frame(egui::Frame::NONE)
                                    .desired_width(ui.available_width() - 80.0),
                            )
                            .changed()
                        {
                            p.item = 0;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(widgets::muted(format!("{n} results")))
                        });
                    });
                });
            if p.source == 0 {
                ui.horizontal_wrapped(|ui| {
                    for (c, label) in categories() {
                        if widgets::pill(ui, label, p.category == c).clicked() {
                            p.category = c;
                            p.item = 0;
                        }
                    }
                });
                Frame::new()
                    .fill(theme::INPUT)
                    .stroke(Stroke::new(1.0, theme::BORDER_INPUT))
                    .corner_radius(6.0)
                    .inner_margin(Margin::same(3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (s, l) in
                                [(System::Metric, "Metric"), (System::Imperial, "Imperial")]
                            {
                                if widgets::seg(ui, l, p.system == s).clicked() && p.system != s {
                                    p.system = s;
                                    p.item = 0;
                                    p.thread = if s == System::Metric {
                                        "M3".into()
                                    } else {
                                        "#4-40".into()
                                    };
                                    p.length = Some(if s == System::Metric {
                                        "10".into()
                                    } else {
                                        "3/8".into()
                                    });
                                }
                            }
                        });
                    });
            }
        });
    widgets::divider(ui);
    let items = p.items(doc);
    if p.source == 1 && items.is_empty() {
        Frame::new().inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
            ui.label(widgets::muted("No local STEP library yet. Point easyCAD at a folder of STEP files described by library.json."));
            ui.add_enabled(false, egui::Button::new("Add STEP files…")).on_disabled_hover_text("Folder picker for LocalStepSource: coming next");
        });
        return;
    }
    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            for (i, it) in items.iter().enumerate() {
                let on = i == p.item.min(items.len() - 1);
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 48.0), egui::Sense::click());
                if on {
                    ui.painter().rect_filled(rect, 0.0, theme::ROW_SELECTED);
                    ui.painter().vline(
                        rect.left() + 1.5,
                        rect.y_range(),
                        Stroke::new(3.0, theme::ACCENT),
                    );
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 0.0, theme::hex(0x20242a));
                }
                let g = egui::Rect::from_min_size(
                    rect.left_center() + vec2(14.0, -17.0),
                    vec2(34.0, 34.0),
                );
                ui.painter().rect(
                    g,
                    6.0,
                    theme::BUTTON,
                    Stroke::new(1.0, theme::BORDER_INPUT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    g.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph(it),
                    theme::mono(9.0),
                    theme::TEXT_DIM,
                );
                ui.painter().text(
                    rect.left_center() + vec2(60.0, -8.0),
                    egui::Align2::LEFT_CENTER,
                    &it.name,
                    theme::prop(13.0),
                    theme::TEXT,
                );
                let std = it
                    .standard
                    .clone()
                    .unwrap_or_else(|| it.vendor.clone().unwrap_or_default());
                ui.painter().text(
                    rect.left_center() + vec2(60.0, 9.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{std} · parametric"),
                    theme::prop(11.0),
                    theme::TEXT_MUTED,
                );
                let sizes = doc
                    .library
                    .source(&it.source)
                    .map(|s| s.sizes(&it.id))
                    .unwrap_or_default();
                let range = match (sizes.first(), sizes.last()) {
                    (Some(a), Some(b)) => {
                        let (a, b) = (split_size(a).0, split_size(b).0);
                        if a == b {
                            a
                        } else {
                            format!("{a}–{b}")
                        }
                    }
                    _ => String::new(),
                };
                ui.painter().text(
                    rect.right_center() - vec2(14.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    range,
                    theme::prop(11.0),
                    theme::TEXT_MUTED,
                );
                if resp.clicked() {
                    p.item = i;
                }
            }
        });
    widgets::divider(ui);
    let Some(info) = p.current(doc) else { return };
    let sizes = p.sizes(doc, &info);
    let mut threads: Vec<String> = Vec::new();
    for s in &sizes {
        let t = split_size(s).0;
        if !threads.contains(&t) {
            threads.push(t);
        }
    }
    let lens: Vec<String> = sizes
        .iter()
        .filter(|s| split_size(s).0 == p.thread)
        .filter_map(|s| split_size(s).1)
        .collect();
    let mut place_now = false;
    let mut save_step = false;
    egui::ScrollArea::vertical()
        .id_salt("part_detail")
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.label(RichText::new(&info.name).strong());
                    ui.label(widgets::label_caps("Size"));
                    ui.horizontal_wrapped(|ui| {
                        for t in threads.iter().take(14) {
                            if size_pill(ui, t, *t == p.thread).clicked() {
                                p.thread = t.clone();
                            }
                        }
                    });
                    if !lens.is_empty() {
                        ui.label(widgets::label_caps("Length"));
                        ui.horizontal_wrapped(|ui| {
                            for l in lens.iter().take(16) {
                                let label = if p.system == System::Imperial {
                                    format!("{l}\"")
                                } else {
                                    l.clone()
                                };
                                if size_pill(ui, &label, p.length.as_deref() == Some(l)).clicked() {
                                    p.length = Some(l.clone());
                                }
                            }
                        });
                    }
                    let rules = hole_rules(&p.thread);
                    widgets::note_frame().show(ui, |ui| {
                        egui::Grid::new("rules")
                            .num_columns(2)
                            .spacing(vec2(10.0, 6.0))
                            .show(ui, |ui| {
                                let row = |ui: &mut Ui, k: &str, v: String| {
                                    ui.label(RichText::new(k).size(12.0).color(theme::TEXT_MUTED));
                                    ui.label(RichText::new(v).font(theme::mono(12.0)));
                                    ui.end_row();
                                };
                                match &rules {
                                    Some(r) => {
                                        let u = |v: f64| {
                                            if r.units == easycad_model::library::Units::In {
                                                format!("{v:.4}")
                                            } else {
                                                format!("{v:.2}")
                                            }
                                        };
                                        let (n, c) = if p.system == System::Imperial {
                                            ("free", "close")
                                        } else {
                                            ("normal", "close")
                                        };
                                        row(
                                            ui,
                                            "Clearance hole",
                                            format!("⌀{} {n} · ⌀{} {c}", u(r.normal), u(r.close)),
                                        );
                                        if let Some((_, b)) = r
                                            .counterbore
                                            .iter()
                                            .find(|(h, _)| *h == Head::Socket)
                                            .or(r.counterbore.first())
                                        {
                                            row(
                                                ui,
                                                "Counterbore",
                                                format!(
                                                    "⌀{} × {}{}",
                                                    u(b.d),
                                                    u(b.depth),
                                                    if b.derived { " (derived)" } else { "" }
                                                ),
                                            );
                                        }
                                        row(
                                            ui,
                                            "Into plastic",
                                            format!("pilot ⌀{}", u(r.pilot_plastic)),
                                        );
                                        row(ui, "Tap drill", format!("⌀{}", u(r.tap_drill)));
                                        if let Some((d, depth)) = r.insert {
                                            row(
                                                ui,
                                                "Insert hole",
                                                format!("⌀{} × {}", u(d), u(depth)),
                                            );
                                        }
                                        ui.label(
                                            RichText::new("Source table")
                                                .size(12.0)
                                                .color(theme::TEXT_MUTED),
                                        );
                                        ui.label(
                                            RichText::new(format!(
                                                "{} · {}",
                                                info.standard.clone().unwrap_or_default(),
                                                r.source
                                            ))
                                            .size(12.0)
                                            .color(theme::TEXT_MUTED),
                                        );
                                        ui.end_row();
                                    }
                                    None => row(ui, "Hole rules", "—".into()),
                                }
                            });
                    });
                    for n in &info.notes {
                        ui.label(
                            RichText::new(format!("Note: {n}"))
                                .size(11.0)
                                .color(theme::ORANGE_SOFT),
                        );
                    }
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - 130.0;
                        if ui
                            .add_sized(
                                vec2(w, 36.0),
                                egui::Button::new(
                                    RichText::new("Click a hole edge or press Enter")
                                        .size(12.0)
                                        .color(theme::TEXT_STRONG),
                                )
                                .fill(theme::PRIMARY)
                                .stroke(Stroke::new(1.0, theme::ACCENT)),
                            )
                            .on_hover_text("Places at the selected hole edges, or at the origin")
                            .clicked()
                        {
                            place_now = true;
                        }
                        if ui
                            .add_sized(
                                vec2(120.0, 36.0),
                                egui::Button::new(RichText::new("Save STEP…").size(12.0)),
                            )
                            .clicked()
                        {
                            save_step = true;
                        }
                    });
                });
        });
    if save_step {
        if let Ok(inst) = doc.library.build(&info.source, &info.id, &p.size()) {
            let name = format!("{}-{}.step", info.id, p.size().replace('/', "_"));
            let req = crate::files::Request::save("Save part STEP", "STEP", "step", &name);
            app.ask_path(req, move |app, path| match inst.shape.write_step(&path) {
                Ok(()) => app.toast(format!("Saved {}", path.display())),
                Err(e) => app.warn(format!("STEP export failed: {}", e.0)),
            });
        }
    }
    if place_now {
        let keys: Vec<PickKey> = app
            .selection
            .iter()
            .copied()
            .filter(|k| matches!(k.kind, PickKind::Edge(_)))
            .collect();
        place(app, &keys);
    }
}

fn size_pill(ui: &mut Ui, label: &str, on: bool) -> egui::Response {
    let (bg, border) = if on {
        (theme::ACCENT_BG, theme::ACCENT)
    } else {
        (Color32::TRANSPARENT, theme::BORDER_INPUT)
    };
    ui.add(
        egui::Button::new(RichText::new(label).font(theme::mono(12.0)))
            .fill(bg)
            .stroke(Stroke::new(1.0, border))
            .min_size(vec2(44.0, 30.0)),
    )
}

fn glyph(p: &PartInfo) -> String {
    let id = p.id.to_lowercase();
    let g = match p.category {
        Category::Screw if id.contains("7380") || id.contains("button") => "BHCS",
        Category::Screw if id.contains("10642") || id.contains("flat") => "FHCS",
        Category::Screw if id.contains("4762") || id.contains("shcs") => "SHCS",
        Category::Screw => "TF",
        Category::Insert => "INS",
        Category::Nut if id.contains("557") || id.contains("562") => "SQ",
        Category::Nut => "HEX",
        Category::Washer => "W",
        Category::Standoff => "SO",
        Category::Magnet => "MAG",
        Category::Foot => "FT",
        Category::Other => "STEP",
    };
    g.into()
}

fn right(app: &mut EasyCadApp, ui: &mut Ui) {
    let crate::app::Mode::Parts(p) = &mut app.mode else {
        return;
    };
    Frame::new()
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if widgets::seg(ui, "BOM", p.tab == Tab::Bom).clicked() {
                    p.tab = Tab::Bom;
                }
                if widgets::seg(ui, "Parameters", p.tab == Tab::Params).clicked() {
                    p.tab = Tab::Params;
                }
            });
        });
    if p.tab == Tab::Params {
        crate::ui_main::params_panel(app, ui);
        return;
    }
    let bom = app.doc.bom();
    Frame::new()
        .inner_margin(Margin::symmetric(14, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::muted("Part"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(widgets::muted("Qty"))
                });
            });
        });
    let mut rows: Vec<(String, String, String)> = bom
        .iter()
        .map(|l| {
            (
                format!("{} {}", l.size, l.name.to_lowercase()),
                [l.part.clone(), l.vendor.clone().unwrap_or_default()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · "),
                l.qty.to_string(),
            )
        })
        .collect();
    if let Some(m) = &app.model {
        for b in m
            .bodies
            .iter()
            .filter(|b| b.body.reference && b.body.part.is_none())
        {
            rows.push((
                format!("{} PCB", b.body.name),
                "reference · not purchased".into(),
                "1".into(),
            ));
        }
    }
    for (name, note, qty) in &rows {
        let r = Frame::new()
            .inner_margin(Margin::symmetric(14, 6))
            .show(ui, |ui| {
                ui.set_min_height(32.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).size(12.0));
                        ui.label(widgets::muted(note));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(widgets::mono_text(qty))
                    });
                });
            });
        ui.painter().hline(
            r.response.rect.x_range(),
            r.response.rect.top(),
            Stroke::new(1.0, theme::BORDER_SOFT),
        );
    }
    if rows.is_empty() {
        Frame::new()
            .inner_margin(Margin::symmetric(14, 6))
            .show(ui, |ui| ui.label(widgets::muted("Nothing placed yet.")));
    }
    let mut save_csv = false;
    let csv: String = std::iter::once("part,note,qty".to_string())
        .chain(rows.iter().map(|(a, b, c)| format!("\"{a}\",\"{b}\",{c}")))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Frame::new()
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if widgets::button(ui, "Export CSV").clicked() {
                    save_csv = true;
                }
                if widgets::button(ui, "Copy").clicked() {
                    ui.ctx().copy_text(csv.clone());
                }
            });
        });
    if save_csv {
        let req = crate::files::Request::save("Export BOM", "CSV", "csv", "bom.csv");
        app.ask_path(req, move |app, path| match std::fs::write(&path, &csv) {
            Ok(()) => app.toast(format!("Saved {}", path.display())),
            Err(e) => app.warn(format!("BOM export failed: {e}")),
        });
    }
}
