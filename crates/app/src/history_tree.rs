//! The history panel: a pan/zoom canvas holding the live trunk (a row of chips with
//! the golden head) and, below it, one row per stored branch forking off its
//! parent at the split gap, like a git graph growing downward.
//!
//! Gaps. The head sits in one of `n + 1` gaps (0 = before every feature, k = after
//! feature k); the gap is the `DocData.rollback` marker. A sketch chip is drawn
//! before the first feature that uses it, and gap k sits *after* those sketch chips,
//! so "right after sketch S" is the gap before S's first consumer: that feature and
//! all later ones are rolled back while S stays live. A sketch nothing uses yet is
//! drawn after the last feature.

use std::collections::{HashMap, HashSet};

use easycad_model::document::DocData;
use easycad_model::feature::{FeatureId, FeatureKind, SketchId};
use easycad_model::regen::FeatureStatus;
use egui::{pos2, vec2, Color32, CornerRadius, Id, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use crate::app::{EasyCadApp, Mode};
use crate::hist_sel::HistItem;
use crate::history_ui::{BranchAct, Peek};
use crate::theme;
use crate::ui_main::feature_code;

pub const ROW_H: f32 = 44.0;
const CHIP_H: f32 = 32.0;
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 4.0;
/// Panel height with a single trunk.
pub const BASE_H: f32 = 76.0;
pub const Z_MIN: f32 = 0.5;
pub const Z_MAX: f32 = 2.0;

/// Pan, zoom and panel height of the history canvas.
#[derive(Clone, Debug)]
pub struct HistView {
    /// Screen offset of the canvas origin from the canvas' top-left.
    pub pan: Vec2,
    pub zoom: f32,
    /// What the canvas last auto-scrolled for: (branch, head, features, rows).
    seen: Option<(String, usize, usize, usize)>,
    /// A height the user dragged the panel to, and the row count it was set for.
    pub user_h: Option<f32>,
    pub user_rows: usize,
    /// Panel height used last frame.
    pub height: f32,
    /// Asks for the recentre on the next frame.
    pub recentre: bool,
}

impl Default for HistView {
    fn default() -> Self {
        HistView { pan: Vec2::ZERO, zoom: 1.0, seen: None, user_h: None, user_rows: 1, height: BASE_H, recentre: false }
    }
}

impl HistView {
    pub fn from_settings(s: &crate::settings::Settings) -> HistView {
        HistView { user_h: s.history_height, user_rows: s.history_rows.max(1), ..Default::default() }
    }
}

/// Rows the panel draws: the trunk plus one per stored branch.
pub fn rows_count(app: &EasyCadApp) -> usize {
    if app.active.is_empty() {
        1 + app.doc.data.branches.len()
    } else {
        1
    }
}

/// The height that fits the tree.
pub fn auto_height(rows: usize) -> f32 {
    if rows <= 1 {
        BASE_H
    } else {
        (36.0 + rows as f32 * ROW_H).max(BASE_H)
    }
}

/// The panel height now: the user's, until the branch count changes; else the tree's.
pub fn panel_height(app: &EasyCadApp, screen_h: f32) -> f32 {
    let rows = rows_count(app);
    let v = &app.hist_view;
    let h = match v.user_h {
        Some(h) if v.user_rows == rows => h,
        _ => auto_height(rows).min(screen_h * 0.6).max(BASE_H),
    };
    h.clamp(48.0, (screen_h * 0.85).max(48.0))
}

// ------------------------------------------------------------------ layout

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChipMark {
    None,
    /// would disappear
    Removed,
    /// would appear (not part of the live history yet)
    Ghost,
    /// parameters change
    Changed,
}

#[derive(Clone)]
enum Kind {
    Sk(SketchId),
    /// index in the row's feature list
    Feat(usize),
    /// index in the peek's ghosts
    Ghost(usize),
}

#[derive(Clone)]
struct Item {
    kind: Kind,
    x: f32,
    w: f32,
}

struct Row {
    name: String,
    /// index in `doc.data.branches`; None for the live trunk
    bidx: Option<usize>,
    y: f32,
    label_x: f32,
    label_w: f32,
    label: String,
    items: Vec<Item>,
    /// x of gap k (0..=features); gaps before `from` sit at the row's start
    gaps: Vec<f32>,
    /// connectors (world): from parent point to child point
    links: Vec<(Pos2, Pos2)>,
}

/// One entry of a row before it is laid out.
enum Entry {
    Real(usize),
    Ghost(usize),
}

fn measure(ui: &Ui, code: &str, label: &str) -> f32 {
    let lw = ui.painter().layout_no_wrap(label.to_owned(), theme::prop(12.0), theme::TEXT).size().x;
    let cw = ui.painter().layout_no_wrap(code.to_owned(), theme::mono(10.0), theme::TEXT).size().x;
    lw + cw + 26.0
}

fn label_width(ui: &Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), theme::prop(11.0), theme::TEXT).size().x + 18.0
}

fn sketch_of(k: &FeatureKind) -> Option<SketchId> {
    k.sketch()
}

/// Lay out one row's chips from `start`. `from` = first feature drawn.
#[allow(clippy::too_many_arguments)]
fn lay_row(
    ui: &Ui,
    data: &DocData,
    labels: &dyn Fn(usize) -> String,
    entries: &[Entry],
    peek: Option<&Peek>,
    from: usize,
    start: f32,
    orphans: &[SketchId],
    sketch_label: &dyn Fn(SketchId) -> String,
) -> (Vec<Item>, Vec<f32>) {
    let n = data.features.len();
    let mut items: Vec<Item> = Vec::new();
    let mut gaps = vec![start - 3.0; n + 1];
    let mut x = start;
    let mut shown: HashSet<SketchId> = HashSet::new();
    fn push(items: &mut Vec<Item>, x: &mut f32, kind: Kind, w: f32) {
        items.push(Item { kind, x: *x, w });
        *x += w + 6.0;
    }
    for e in entries {
        match e {
            Entry::Real(i) => {
                let f = &data.features[*i];
                if let Some(s) = sketch_of(&f.kind) {
                    if shown.insert(s) && *i >= from {
                        let w = measure(ui, "SK", &sketch_label(s));
                        push(&mut items, &mut x, Kind::Sk(s), w);
                    }
                }
                if *i < from {
                    continue;
                }
                gaps[*i] = x - 3.0;
                let (code, _) = feature_code(&f.kind);
                let w = measure(ui, code, &labels(*i));
                push(&mut items, &mut x, Kind::Feat(*i), w);
            }
            Entry::Ghost(g) => {
                let Some(p) = peek else { continue };
                let gf = &p.ghosts[*g];
                if let Some(s) = sketch_of(&gf.kind) {
                    if p.diff.added_sketches.contains(&s) && shown.insert(s) {
                        let w = measure(ui, "SK", p.sketch_names.get(&s).map_or("", |s| s.as_str()));
                        push(&mut items, &mut x, Kind::Sk(s), w);
                    }
                }
                let (code, _) = feature_code(&gf.kind);
                let w = measure(ui, code, &gf.name);
                push(&mut items, &mut x, Kind::Ghost(*g), w);
            }
        }
    }
    for s in orphans {
        let w = measure(ui, "SK", &sketch_label(*s));
        push(&mut items, &mut x, Kind::Sk(*s), w);
    }
    gaps[n] = x - 3.0;
    (items, gaps)
}

// ------------------------------------------------------------------ the panel

/// The whole history panel (bottom).
pub fn panel(app: &mut EasyCadApp, ui: &mut Ui) {
    let screen_h = ui.ctx().content_rect().height();
    // a user's height holds until the branch count changes
    let rows_n = rows_count(app);
    if app.hist_view.user_h.is_some() && app.hist_view.user_rows != rows_n {
        app.hist_view.user_h = None;
        if app.settings.history_height.take().is_some() {
            app.settings.save();
        }
    }
    let h = panel_height(app, screen_h);
    app.hist_view.height = h;
    egui::Panel::bottom("history")
        .exact_size(h)
        .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 0)))
        .show(ui, |ui| body(app, ui));
}

fn body(app: &mut EasyCadApp, ui: &mut Ui) {
    // drag handles on the panel's top and bottom edges
    {
        let r = ui.max_rect();
        let full = Rect::from_min_max(pos2(r.left() - 12.0, r.top()), pos2(r.right() + 12.0, r.bottom()));
        let top = Rect::from_min_size(full.min, vec2(full.width(), 4.0));
        let bot = Rect::from_min_max(pos2(full.left(), full.bottom() - 3.0), full.max);
        let mut dy = 0.0;
        let t = ui.interact(top, Id::NULL.with("history_resize_top"), Sense::drag());
        let b = ui.interact(bot, Id::NULL.with("history_resize_bottom"), Sense::drag());
        for (resp, sign) in [(&t, -1.0), (&b, 1.0)] {
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if resp.dragged() {
                dy += sign * resp.drag_delta().y;
            }
        }
        let stopped = t.drag_stopped() || b.drag_stopped();
        if dy != 0.0 {
            let screen_h = ui.ctx().content_rect().height();
            let nh = (app.hist_view.height + dy).clamp(48.0, (screen_h * 0.85).max(48.0));
            app.hist_view.user_h = Some(nh);
            app.hist_view.user_rows = rows_count(app);
            app.hist_view.height = nh;
            ui.ctx().request_repaint();
        }
        if stopped {
            app.settings.history_height = app.hist_view.user_h;
            app.settings.history_rows = app.hist_view.user_rows;
            app.settings.save();
        }
    }
    ui.add_space(5.0);
    let toolbar_peek = app.hist_peek.take();
    if toolbar_peek.is_some() {
        ui.ctx().request_repaint();
    }
    // the preview wanted this frame comes from a hover; the branch hover is from last frame
    let hover_key = Id::NULL.with("history_branch_hover");
    let branch_hover: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(hover_key)).flatten();
    let peek = crate::history_ui::peek_for(app, toolbar_peek, branch_hover);
    app.hist_diff = peek.as_ref().map(|p| p.diff.clone());
    let root = app.active.is_empty() && !matches!(app.mode, Mode::Sketch(_));
    let mut branch_act: Option<BranchAct> = None;
    let mut branch_at: Option<usize> = None;
    let mut recentre = std::mem::take(&mut app.hist_view.recentre);

    // ---- header
    let has_parent = app.doc.data.branch_parent.is_some();
    let in_root = app.active.is_empty();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new("HISTORY").size(10.5).color(theme::TEXT_LABEL));
        if !app.active.is_empty() {
            let c = app.scope();
            ui.label(RichText::new(app.doc.data.component_name(c)).size(11.0).color(theme::ACCENT_LINK))
                .on_hover_text("History of the component being edited");
        }
        let small = |t: &str| egui::Button::new(RichText::new(t.to_owned()).size(11.0)).corner_radius(9).min_size(vec2(0.0, 18.0));
        let n = app.view().features.len();
        if ui
            .add_enabled(in_root, small("+ Branch here"))
            .on_hover_text("Save this history and continue on a new branch from the head, to try an idea. Right-click a step to branch there")
            .on_disabled_hover_text("Branches apply to the whole document: leave the component first")
            .clicked()
        {
            branch_act = Some(BranchAct::Here(app.rollback().unwrap_or(n)));
        }
        if has_parent
            && ui
                .add_enabled(in_root, small("Back to split"))
                .on_hover_text("Return to the parent branch with the head at the step this one started from")
                .clicked()
        {
            branch_act = Some(BranchAct::BackToSplit);
        }
        if ui.add(small("Fit")).on_hover_text("Centre on head: zoom 1, default spacing, scrolled so the head is in the middle").clicked() {
            recentre = true;
        }
        if let Some(hint) = crate::ui_main::history_hint(app) {
            ui.label(RichText::new(hint).size(12.0).color(theme::TEXT_MUTED));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let msg = match &peek {
                Some(p) if p.diff.is_empty() => format!("{}: no change to the history", p.label),
                Some(p) => format!("{}: red = goes away · green = comes back · amber dot = changes", p.label),
                None => "Click to select · Del deletes · drag the head to roll back · drag empty space to pan · Ctrl+wheel zooms".to_string(),
            };
            ui.label(RichText::new(msg).size(11.0).color(if peek.is_some() { theme::GREEN } else { theme::TEXT_MUTED }));
        });
    });
    ui.add_space(2.0);

    // ---- data
    let (feats, n, marker) = {
        let v = app.view();
        let feats: Vec<(FeatureId, String, FeatureKind)> = v
            .features
            .iter()
            .map(|f| (f.id, app.cache.chips.get(&f.id).cloned().unwrap_or_else(|| f.name.clone()), f.kind.clone()))
            .collect();
        let n = feats.len();
        (feats, n, app.rollback().unwrap_or(n).min(n))
    };
    // drag preview of the golden head
    let view_marker = crate::history_ui::drag_preview(ui).unwrap_or(marker).min(n);

    let ids: Vec<FeatureId> = feats.iter().map(|f| f.0).collect();
    let disp = crate::history_ui::merged(&ids, peek.as_ref());
    let entries: Vec<Entry> = disp
        .iter()
        .map(|d| match d.real {
            Some(i) => Entry::Real(i),
            None => Entry::Ghost(d.ghost.unwrap_or(0)),
        })
        .collect();

    // ---- rows
    let rows_total = rows_count(app);
    let multi = rows_total > 1;
    let sk_names: HashMap<SketchId, String> = app.view().sketches.iter().map(|e| (e.id, e.sketch.name.clone())).collect();
    let sk_name = |s: SketchId| sk_names.get(&s).cloned().unwrap_or_default();
    let live_label = format!("{}  {}", app.doc.data.branch, app.doc.data.features.len());
    let mut rows: Vec<Row> = Vec::new();
    {
        let d = &app.doc.data;
        // the order of stored rows: ancestors of the live branch (full rows), then forks
        let stored = &d.branches;
        let mut placed: Vec<String> = vec![d.branch.clone()];
        let mut order: Vec<(usize, bool)> = Vec::new(); // (branch index, ancestor)
        if app.active.is_empty() {
            let mut cur = d.branch_parent.clone();
            while let Some(p) = cur {
                match stored.iter().position(|b| b.name == p) {
                    Some(i) if !placed.contains(&p) => {
                        placed.push(p);
                        order.push((i, true));
                        cur = stored[i].parent.clone();
                    }
                    _ => break,
                }
            }
            loop {
                let mut grew = false;
                for (i, b) in stored.iter().enumerate() {
                    if order.iter().any(|(j, _)| *j == i) {
                        continue;
                    }
                    if b.parent.as_ref().is_some_and(|p| placed.contains(p)) {
                        placed.push(b.name.clone());
                        order.push((i, false));
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            for i in 0..stored.len() {
                if !order.iter().any(|(j, _)| *j == i) {
                    order.push((i, true));
                }
            }
        }
        // widest label of a full row, so shared prefixes line up
        let mut lw_max = label_width(ui, &live_label);
        for (i, anc) in &order {
            if *anc {
                let b = &stored[*i];
                lw_max = lw_max.max(label_width(ui, &format!("{}  {}", b.name, b.data.features.len())));
            }
        }
        let full_start = if multi { PAD_X + lw_max + 8.0 } else { PAD_X };
        let orphans: Vec<SketchId> = app.view().sketches.iter().map(|e| e.id).filter(|s| app.view().first_consumer(*s).is_none()).collect();
        let labels_live = |i: usize| feats[i].1.clone();
        let (items, gaps) = lay_row(ui, app.view(), &labels_live, &entries, peek.as_ref(), 0, full_start, &orphans, &sk_name);
        rows.push(Row {
            name: d.branch.clone(),
            bidx: None,
            y: PAD_Y,
            label_x: PAD_X,
            label_w: lw_max,
            label: live_label.clone(),
            items,
            gaps,
            links: Vec::new(),
        });
        for (i, anc) in &order {
            let b = &stored[*i];
            let bd = &b.data;
            let names: Vec<&str> = bd.features.iter().map(|f| f.name.as_str()).collect();
            let labels = |k: usize| names[k].to_string();
            let ents: Vec<Entry> = (0..bd.features.len()).map(Entry::Real).collect();
            let bsk = |s: SketchId| bd.sketch(s).map(|s| s.name.clone()).unwrap_or_default();
            let y = PAD_Y + rows.len() as f32 * ROW_H;
            let label = format!("{}  {}", b.name, bd.features.len());
            let mut links = Vec::new();
            let row = if *anc {
                let (items, gaps) = lay_row(ui, bd, &labels, &ents, None, 0, full_start, &[], &bsk);
                // the live branch (or a descendant) started from this one at the split of its child
                let child = rows.iter().position(|r| {
                    let (parent, split) = match r.bidx {
                        None => (d.branch_parent.as_deref(), d.branch_split),
                        Some(j) => (stored[j].parent.as_deref(), stored[j].split),
                    };
                    parent == Some(b.name.as_str()) && split <= bd.features.len()
                });
                if let Some(ci) = child {
                    let split = match rows[ci].bidx {
                        None => d.branch_split,
                        Some(j) => stored[j].split,
                    };
                    let cx = rows[ci].gaps.get(split).copied().unwrap_or(full_start);
                    links.push((pos2(cx, rows[ci].y + CHIP_H / 2.0), pos2(gaps[split.min(bd.features.len())], y + CHIP_H / 2.0)));
                }
                Row { name: b.name.clone(), bidx: Some(*i), y, label_x: PAD_X, label_w: lw_max, label, items, gaps, links }
            } else {
                let parent = rows.iter().find(|r| Some(r.name.as_str()) == b.parent.as_deref()).expect("placed parent");
                let from = b.split.min(bd.features.len());
                let ax = parent.gaps.get(b.split).copied().unwrap_or(full_start);
                let ay = parent.y + CHIP_H / 2.0;
                let lx = ax + 30.0;
                let lwid = label_width(ui, &label);
                let (items, gaps) = lay_row(ui, bd, &labels, &ents, None, from, lx + lwid + 6.0, &[], &bsk);
                links.push((pos2(ax, ay), pos2(lx - 2.0, y + CHIP_H / 2.0)));
                Row { name: b.name.clone(), bidx: Some(*i), y, label_x: lx, label_w: lwid, label, items, gaps, links }
            };
            rows.push(row);
        }
    }
    let live_gaps = rows[0].gaps.clone();

    // ---- canvas
    let avail = ui.available_size();
    let (canvas, bg) = ui.allocate_exact_size(vec2(avail.x, avail.y.max(20.0)), Sense::click_and_drag());
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(canvas));
    cui.set_clip_rect(canvas.intersect(ui.clip_rect()));
    let ui = &mut cui;
    let key = (app.doc.data.branch.clone(), marker, n, rows_total);
    {
        let v = &mut app.hist_view;
        // pan by dragging empty space
        if bg.dragged_by(egui::PointerButton::Primary) || bg.dragged_by(egui::PointerButton::Middle) {
            v.pan += bg.drag_delta();
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if bg.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        // wheel: Ctrl+wheel zooms about the pointer, plain wheel scrolls sideways
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p));
        if let Some(p) = hover {
            let (zd, sc) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if (zd - 1.0).abs() > 1e-4 {
                let nz = (v.zoom * zd).clamp(Z_MIN, Z_MAX);
                let world = (p - canvas.min - v.pan) / v.zoom;
                v.pan = p - canvas.min - world * nz;
                v.zoom = nz;
            } else if sc != Vec2::ZERO {
                v.pan.x += sc.x + sc.y;
            }
        }
        let head_x = live_gaps[view_marker.min(n)];
        let centre_x = canvas.width() / 2.0 - head_x * v.zoom;
        if recentre {
            v.zoom = 1.0;
            v.pan = vec2(canvas.width() / 2.0 - head_x, 0.0);
            v.seen = Some(key.clone());
        } else if v.seen.as_ref() != Some(&key) {
            // first show, or the head moved by an action: keep it in view
            let first = v.seen.is_none();
            v.seen = Some(key.clone());
            let sx = v.pan.x + head_x * v.zoom;
            if sx < 24.0 || sx > canvas.width() - 24.0 {
                v.pan.x = if first && head_x * v.zoom < canvas.width() - 24.0 { 0.0 } else { centre_x };
            }
            let top = v.pan.y;
            if top < -(ROW_H - 8.0) || top > canvas.height() - 20.0 {
                v.pan.y = 0.0;
            }
        }
    }
    let z = app.hist_view.zoom;
    let origin = canvas.min + app.hist_view.pan;
    let to_s = |p: Pos2| origin + p.to_vec2() * z;
    let sx = |x: f32| origin.x + x * z;
    let painter = ui.painter().clone();

    // connectors
    for r in &rows {
        for (a, b) in &r.links {
            let (a, b) = (to_s(*a), to_s(*b));
            let col = theme::ACCENT_LINK.gamma_multiply(0.7);
            let c1 = pos2(a.x, b.y);
            let c2 = pos2(a.x + (b.x - a.x) * 0.5, b.y);
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke([a, c1, c2, b], false, Color32::TRANSPARENT, Stroke::new(1.6 * z.max(0.7), col)));
            painter.circle_filled(a, 3.0 * z.max(0.8), col);
        }
    }

    let mods = ui.input(|i| i.modifiers);
    let add = mods.ctrl || mods.command || mods.shift;
    let dif = peek.as_ref().map(|p| &p.diff);
    let mut xs: HashMap<FeatureId, f32> = HashMap::new();
    let mut head_resp: Option<egui::Response> = None;
    let mut click: Option<(HistItem, bool)> = None;
    app.hist_rects.clear();
    app.hist_row_rects.clear();
    let mut roll_to: Option<usize> = None;
    let app_root = app.active.is_empty();
    let mut del_one: Option<HistItem> = None;
    let mut dbl: Option<FeatureId> = None;
    let mut sketch_edit = None;
    let mut accept: Option<FeatureId> = None;
    let mut hovered_branch: Option<usize> = None;
    let mut rows_top = f32::MAX;
    let mut rows_bot = f32::MIN;

    for (ri, row) in rows.iter().enumerate() {
        let live = row.bidx.is_none();
        let yy = origin.y + row.y * z;
        if live {
            rows_top = yy;
            rows_bot = yy + CHIP_H * z;
        }
        // row label
        if multi {
            let rect = Rect::from_min_size(pos2(sx(row.label_x), yy + 5.0 * z), vec2(row.label_w * z, (CHIP_H - 10.0) * z));
            app.hist_row_rects.push((row.name.clone(), rect));
            if rect.intersects(canvas) {
                let resp = ui.interact(rect.intersect(canvas), Id::NULL.with(("history_row_label", ri)), Sense::click());
                let hot = resp.hovered();
                painter.rect(
                    rect,
                    CornerRadius::same((9.0 * z) as u8),
                    if live { theme::ACCENT_BG } else if hot { theme::HOVER } else { theme::CARD },
                    Stroke::new(1.0, if live { theme::ACCENT } else { theme::hex(0x33373e) }),
                    egui::StrokeKind::Inside,
                );
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, &row.label, theme::prop(11.0 * z), if live { theme::TEXT } else { theme::TEXT_MUTED });
                if !live && hot {
                    hovered_branch = row.bidx;
                }
                let name = row.name.clone();
                let resp = resp.on_hover_text(if live {
                    match &app.doc.data.branch_parent {
                        Some(p) => format!("You are on \"{}\" (started from \"{p}\" after {} features)", name, app.doc.data.branch_split),
                        None => format!("You are on \"{name}\""),
                    }
                } else {
                    format!("Click to switch to \"{name}\" · hover shows what would change")
                });
                if !live && resp.clicked() {
                    branch_act = Some(BranchAct::Switch(name.clone()));
                }
                resp.context_menu(|ui| {
                    if !live && ui.button("Switch to this branch").clicked() {
                        branch_act = Some(BranchAct::Switch(name.clone()));
                        ui.close();
                    }
                    if live && has_parent && ui.add_enabled(app_root, egui::Button::new("Back to split")).clicked() {
                        branch_act = Some(BranchAct::BackToSplit);
                        ui.close();
                    }
                    if live && ui.add_enabled(app_root, egui::Button::new("Keep this branch (delete the others)")).clicked() {
                        branch_act = Some(BranchAct::Keep);
                        ui.close();
                    }
                    if ui.add_enabled(app_root, egui::Button::new("Discard this branch")).clicked() {
                        branch_act = Some(BranchAct::Discard(name.clone()));
                        ui.close();
                    }
                });
            }
        }
        // chips
        for it in &row.items {
            let rect = Rect::from_min_size(pos2(sx(it.x), yy), vec2(it.w * z, CHIP_H * z));
            let vis = rect.intersects(canvas);
            if !live {
                // a stored branch's chip: dimmed, click switches to it
                let bidx = row.bidx.unwrap();
                let d = &app.doc.data.branches[bidx].data;
                let (code, col, label, active_kind) = match &it.kind {
                    Kind::Sk(s) => ("SK", theme::ACCENT_LINK, d.sketch(*s).map(|s| s.name.clone()).unwrap_or_default(), true),
                    Kind::Feat(i) => {
                        let (c, col) = feature_code(&d.features[*i].kind);
                        (c, col, d.features[*i].name.clone(), true)
                    }
                    Kind::Ghost(_) => continue,
                };
                let _ = active_kind;
                if !vis {
                    continue;
                }
                let st = ChipStyle { active: false, head: false, selected: false, warn: None, mark: ChipMark::None, drag: false };
                if let Some(r) = draw_chip(ui, canvas, rect, Id::NULL.with(("history_chip", ri, it.x as i32)), z, code, col, &label, &st) {
                    if r.hovered() {
                        hovered_branch = row.bidx;
                    }
                    let r = r.on_hover_text(format!("Click to switch to \"{}\" · hover shows what would change", row.name));
                    if r.clicked() {
                        branch_act = Some(BranchAct::Switch(row.name.clone()));
                    }
                }
                continue;
            }
            // ---- the live trunk
            match &it.kind {
                Kind::Ghost(g) => {
                    let p = peek.as_ref().expect("ghosts come from a peek");
                    let gf = &p.ghosts[*g];
                    let (code, col) = feature_code(&gf.kind);
                    xs.insert(gf.id, it.x + it.w + 3.0);
                    if vis {
                        let st = ChipStyle { active: true, head: false, selected: false, warn: None, mark: ChipMark::Ghost, drag: false };
                        if let Some(r) = draw_chip(ui, canvas, rect, Id::NULL.with(("history_ghost", *g)), z, code, col, &gf.name, &st) {
                            r.on_hover_text(format!("{} · {}: would appear", gf.kind.label(), p.label));
                        }
                    }
                }
                Kind::Sk(s) => {
                    let s = *s;
                    let consumer = app.view().first_consumer(s);
                    let ghost_sk = consumer.is_none() && peek.as_ref().is_some_and(|p| p.diff.added_sketches.contains(&s)) && app.view().sketch(s).is_none();
                    let name = if ghost_sk {
                        peek.as_ref().and_then(|p| p.sketch_names.get(&s).cloned()).unwrap_or_default()
                    } else {
                        sk_name(s)
                    };
                    // a sketch is live while its consumer is at or before the head
                    let active = consumer.is_none_or(|j| j <= view_marker);
                    let mark = if ghost_sk {
                        ChipMark::Ghost
                    } else {
                        match dif {
                            Some(d) if d.removed_sketches.contains(&s) => ChipMark::Removed,
                            Some(d) if d.changed_sketches.contains(&s) => ChipMark::Changed,
                            _ => ChipMark::None,
                        }
                    };
                    if ghost_sk {
                        if vis {
                            let st = ChipStyle { active: true, head: false, selected: false, warn: None, mark, drag: false };
                            draw_chip(ui, canvas, rect, Id::NULL.with(("history_sk_ghost", s.0)), z, "SK", theme::ACCENT_LINK, &name, &st);
                        }
                        continue;
                    }
                    if !vis {
                        // still tracked for selection rectangles
                        app.hist_rects.push((HistItem::Sketch(s), rect));
                        continue;
                    }
                    let st = ChipStyle { active, head: false, selected: app.is_hist_selected(HistItem::Sketch(s)), warn: None, mark, drag: false };
                    app.hist_rects.push((HistItem::Sketch(s), rect));
                    if let Some(r) = draw_chip(ui, canvas, rect, Id::NULL.with(("history_sk", s.0)), z, "SK", theme::ACCENT_LINK, &name, &st) {
                        let r = r.on_hover_text("Click to select (Del deletes) · double-click to edit the sketch · right-click to roll back to it");
                        if r.double_clicked() {
                            sketch_edit = Some(s);
                        } else if r.clicked() {
                            click = Some((HistItem::Sketch(s), add));
                        }
                        let at = consumer.unwrap_or(n);
                        r.context_menu(|ui| {
                            if ui.button("Edit sketch").clicked() {
                                sketch_edit = Some(s);
                                ui.close();
                            }
                            if ui.button("Roll back to here").on_hover_text("Roll back the sketch's feature and everything after it; the sketch stays").clicked() {
                                roll_to = Some(at);
                                ui.close();
                            }
                            if ui.add_enabled(app_root, egui::Button::new("Branch here")).clicked() {
                                branch_at = Some(at);
                                ui.close();
                            }
                            if ui.button("Delete").clicked() {
                                del_one = Some(HistItem::Sketch(s));
                                ui.close();
                            }
                        });
                    }
                }
                Kind::Feat(i) => {
                    let i = *i;
                    let (id, label, kind) = &feats[i];
                    xs.insert(*id, it.x + it.w + 3.0);
                    let status = app.feature_status(*id).cloned();
                    let warn = match &status {
                        Some(FeatureStatus::Error(e)) => Some((theme::RED, format!("Error: {e}"))),
                        Some(FeatureStatus::Warning(w)) => Some((theme::ORANGE_SOFT, format!("Warning: {w}"))),
                        Some(FeatureStatus::Unresolved(u)) => {
                            Some((theme::ORANGE, format!("A reference no longer matches (score {:.2}) · re-pick", u.score)))
                        }
                        _ => None,
                    };
                    app.hist_rects.push((HistItem::Feature(*id), rect));
                    if !vis {
                        if i + 1 == marker {
                            // keep the head chip's role even when scrolled away
                        }
                        continue;
                    }
                    let (code, col) = feature_code(kind);
                    let st = ChipStyle {
                        active: i < view_marker,
                        head: i + 1 == view_marker,
                        selected: app.is_hist_selected(HistItem::Feature(*id)),
                        warn: warn.as_ref().map(|w| w.0),
                        mark: match dif {
                            Some(d) if d.removed_features.contains(id) => ChipMark::Removed,
                            Some(d) if d.changed_features.contains(id) => ChipMark::Changed,
                            _ => ChipMark::None,
                        },
                        // only the real head chip is a drag handle for the golden head
                        drag: i + 1 == marker,
                    };
                    let Some(r) = draw_chip(ui, canvas, rect, Id::NULL.with(("history_feat", id.0)), z, code, col, label, &st) else { continue };
                    let tip = match &warn {
                        Some((_, t)) => format!("{}\n{t}\nClick to select · double-click to edit", kind.label()),
                        None => format!("{} · click to select (Del deletes) · double-click to edit", kind.label()),
                    };
                    let timing = match app.model.as_ref().and_then(|m| m.result.report(*id)) {
                        Some(rep) if rep.cached => format!("\nRegen: cached (last computed in {:.0} ms)", rep.elapsed_ms),
                        Some(rep) => format!("\nRegen: {:.0} ms", rep.elapsed_ms),
                        None => String::new(),
                    };
                    let r = r.on_hover_text(format!("{tip}{timing}"));
                    if r.double_clicked() {
                        dbl = Some(*id);
                    } else if r.clicked() {
                        click = Some((HistItem::Feature(*id), add));
                    }
                    if i + 1 == marker {
                        head_resp = Some(r.clone());
                    }
                    let unresolved = matches!(status, Some(FeatureStatus::Unresolved(_)));
                    r.context_menu(|ui| {
                        if unresolved {
                            if ui.button("Accept the closest candidate").clicked() {
                                accept = Some(*id);
                                ui.close();
                            }
                            if ui.button("Re-pick…").clicked() {
                                dbl = Some(*id);
                                ui.close();
                            }
                            ui.separator();
                        }
                        if ui.button("Roll back to here").clicked() {
                            roll_to = Some(i + 1);
                            ui.close();
                        }
                        if ui
                            .add_enabled(app_root, egui::Button::new("Branch here"))
                            .on_hover_text("Save this history and continue on a new branch from this step")
                            .on_disabled_hover_text("Branches apply to the whole document")
                            .clicked()
                        {
                            branch_at = Some(i + 1);
                            ui.close();
                        }
                        if ui.button("Edit").clicked() {
                            dbl = Some(*id);
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            del_one = Some(HistItem::Feature(*id));
                            ui.close();
                        }
                    });
                }
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(hover_key, hovered_branch));
    // the golden head, on the live row
    if rows_top < rows_bot {
        let strip = Rect::from_min_max(pos2(canvas.left(), rows_top), pos2(canvas.right(), rows_bot));
        let tf = |x: f32| origin.x + x * z;
        if let Some(k) = crate::history_ui::golden_cursor(ui, &live_gaps, &tf, strip, marker, head_resp.as_ref()) {
            roll_to = Some(k);
        }
        if let Some(hd) = peek.as_ref().and_then(|p| p.diff.head) {
            if let Some(x) = crate::history_ui::head_x(hd, &xs, live_gaps[0]) {
                crate::history_ui::ghost_cursor(ui, tf(x), rows_top, rows_bot);
            }
        }
    }
    let ui_outer_ctx = ui.ctx().clone();
    // ---- actions
    if let Some((it, add)) = click {
        app.hist_click(it, add);
    } else if bg.clicked() {
        app.hist_sel.clear();
    }
    // Ctrl+Alt+Left / Right nudge the golden head one gap (Alt+arrows alone step the camera)
    if app.active.is_empty() && !matches!(app.mode, Mode::Sketch(_)) && !ui_outer_ctx.text_edit_focused() && roll_to.is_none() && n > 0 {
        let (l, r) = ui_outer_ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::ArrowRight),
            )
        });
        if l && marker > 0 {
            roll_to = Some(marker - 1);
        } else if r && marker < n {
            roll_to = Some(marker + 1);
        }
    }
    let _ = root;
    if let Some(k) = roll_to {
        app.set_rollback(if k >= n { None } else { Some(k) });
    }
    if let Some(at) = branch_at {
        branch_act = Some(BranchAct::Here(at));
    }
    if let Some(a) = branch_act {
        crate::history_ui::apply_branch(app, a);
    }
    if let Some(it) = del_one {
        if app.is_hist_selected(it) && app.hist_sel.len() > 1 {
            app.hist_delete_selected();
        } else {
            app.hist_delete_request(vec![it]);
        }
    }
    crate::hist_sel::confirm_ui(app, &ui_outer_ctx);
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
}

// ------------------------------------------------------------------ chips

pub struct ChipStyle {
    pub active: bool,
    pub head: bool,
    pub selected: bool,
    pub warn: Option<Color32>,
    pub mark: ChipMark,
    pub drag: bool,
}

/// Draw one chip at `rect` (screen, already zoomed) and return its response; None
/// when it is entirely outside the canvas.
#[allow(clippy::too_many_arguments)]
fn draw_chip(ui: &mut Ui, canvas: Rect, rect: Rect, id: Id, z: f32, code: &str, code_col: Color32, label: &str, st: &ChipStyle) -> Option<egui::Response> {
    if !rect.intersects(canvas) {
        return None;
    }
    let sense = if st.mark == ChipMark::Ghost { Sense::hover() } else { Sense::click_and_drag() };
    let resp = ui.interact(rect.intersect(canvas), id, sense);
    if st.drag && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let (mark, head, selected, warn) = (st.mark, st.head, st.selected, st.warn);
    let border = if mark == ChipMark::Removed {
        theme::RED
    } else if mark == ChipMark::Ghost {
        theme::GREEN
    } else if selected {
        theme::ACCENT
    } else if head {
        theme::HEAD
    } else if let Some(w) = warn {
        w
    } else {
        theme::hex(0x33373e)
    };
    let fill = if mark == ChipMark::Removed {
        theme::hex(0x3a1e1e)
    } else if mark == ChipMark::Ghost {
        theme::hex(0x1b3a2a)
    } else if selected {
        theme::ACCENT_BG
    } else if head {
        theme::HEAD_BG
    } else if resp.hovered() {
        theme::HOVER
    } else {
        theme::CARD
    };
    let op: f32 = if st.active { 1.0 } else { 0.4 };
    let op = if mark == ChipMark::Ghost { 0.85 } else { op };
    let p = ui.painter_at(canvas);
    p.rect(
        rect,
        CornerRadius::same((16.0 * z) as u8),
        fill.gamma_multiply(op.max(0.6)),
        Stroke::new(if head || selected { 2.0 } else { 1.0 }, border.gamma_multiply(op)),
        egui::StrokeKind::Inside,
    );
    let cw = p.layout_no_wrap(code.to_owned(), theme::mono(10.0), theme::TEXT).size().x;
    p.text(rect.left_center() + vec2(10.0 * z, 0.0), egui::Align2::LEFT_CENTER, code, theme::mono(10.0 * z), warn.unwrap_or(code_col).gamma_multiply(op));
    p.text(
        rect.left_center() + vec2((16.0 + cw) * z, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        theme::prop(12.0 * z),
        if mark == ChipMark::Ghost { theme::GREEN } else { theme::TEXT }.gamma_multiply(op),
    );
    match mark {
        ChipMark::Removed => {
            let y = rect.center().y;
            p.line_segment([pos2(rect.left() + 12.0 * z, y), pos2(rect.right() - 12.0 * z, y)], Stroke::new(1.5, theme::RED));
        }
        ChipMark::Changed => {
            p.circle_filled(pos2(rect.right() - 7.0 * z, rect.top() + 7.0 * z), 3.5 * z, theme::ORANGE);
        }
        _ => {}
    }
    Some(resp)
}
