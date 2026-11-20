//! The history strip's extras: the golden head cursor, the branch strip, and the
//! diff preview (what an undo/redo or a stored branch would change).

use std::collections::HashMap;

use easycad_model::branch::{history_diff, HeadAt, HistDiff};
use easycad_model::document::DocData;
use easycad_model::feature::{Feature, FeatureId, SketchId};
use egui::{pos2, vec2, Color32, CornerRadius, Id, Rect, RichText, Sense, Stroke, Ui};

use crate::app::EasyCadApp;
use crate::theme;

/// Which toolbar button the pointer is over (set by the toolbar, read by the strip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistPeek {
    Undo,
    Redo,
}

/// A preview of another document state against the live one.
pub struct Peek {
    pub diff: HistDiff,
    /// Features that would appear, in the other state's order.
    pub ghosts: Vec<Feature>,
    /// Feature order of the other state.
    pub order: Vec<FeatureId>,
    pub sketch_names: HashMap<SketchId, String>,
    pub label: String,
}

impl Peek {
    pub fn new(cur: &DocData, other: &DocData, label: String) -> Peek {
        let diff = history_diff(cur, other);
        let ghosts = diff
            .added_features
            .iter()
            .filter_map(|id| other.features.iter().find(|f| f.id == *id).cloned())
            .collect();
        Peek {
            order: other.features.iter().map(|f| f.id).collect(),
            sketch_names: other.sketches.iter().map(|s| (s.id, s.sketch.name.clone())).collect(),
            ghosts,
            diff,
            label,
        }
    }
}

/// The preview wanted this frame: a hovered toolbar button, else a hovered branch tab.
pub fn peek_for(app: &EasyCadApp, toolbar: Option<HistPeek>, branch: Option<usize>) -> Option<Peek> {
    if !app.active.is_empty() || matches!(app.mode, crate::app::Mode::Sketch(_)) {
        return None;
    }
    let cur = &app.doc.data;
    match (toolbar, branch) {
        (Some(HistPeek::Undo), _) => app
            .doc
            .peek_undo()
            .map(|o| Peek::new(cur, o, format!("Undo {}", app.doc.undo_label().unwrap_or("")))),
        (Some(HistPeek::Redo), _) => app
            .doc
            .peek_redo()
            .map(|o| Peek::new(cur, o, format!("Redo {}", app.doc.redo_label().unwrap_or("")))),
        (None, Some(i)) => cur.branches.get(i).map(|b| Peek::new(cur, &b.data, format!("Branch {}", b.name))),
        _ => None,
    }
}

/// One slot of the strip: a live feature (`real` is its index) or a ghost.
pub struct Disp {
    pub id: FeatureId,
    pub real: Option<usize>,
    pub ghost: Option<usize>,
}

/// The live features in order with the previewed ghosts slotted in after their predecessor.
pub fn merged(ids: &[FeatureId], peek: Option<&Peek>) -> Vec<Disp> {
    let mut out: Vec<Disp> = ids.iter().enumerate().map(|(i, id)| Disp { id: *id, real: Some(i), ghost: None }).collect();
    if let Some(p) = peek {
        for (gi, g) in p.ghosts.iter().enumerate() {
            let pos = p.order.iter().position(|x| *x == g.id).unwrap_or(0);
            let at = if pos == 0 {
                0
            } else {
                let pred = p.order[pos - 1];
                out.iter().position(|d| d.id == pred).map_or(out.len(), |k| k + 1)
            };
            out.insert(at, Disp { id: g.id, real: None, ghost: Some(gi) });
        }
    }
    out
}

// ------------------------------------------------------------------ head cursor

fn drag_key() -> Id {
    Id::new("history_head_drag_preview")
}

/// The gap (1..=n) the head is being dragged to, while a drag is going.
pub fn drag_preview(ui: &Ui) -> Option<usize> {
    ui.data(|d| d.get_temp::<usize>(drag_key()))
}

/// The golden head cursor. `bounds[k-1]` is the x of the gap after feature k.
/// Pressing and dragging it, its grip, or the head chip itself (`head_chip`)
/// moves the head to the nearest gap (live preview through `drag_preview`);
/// returns the new head position (1..=n) when the drag is released on another gap.
pub fn golden_cursor(
    ui: &mut Ui,
    bounds: &[f32],
    strip: Rect,
    marker: usize,
    head_chip: Option<&egui::Response>,
) -> Option<usize> {
    let n = bounds.len();
    if n == 0 {
        return None;
    }
    let cur = marker.clamp(1, n);
    let (top, bot) = (strip.top(), strip.bottom());
    let x0 = bounds[cur - 1];
    let hit = Rect::from_min_max(pos2(x0 - 5.0, top), pos2(x0 + 5.0, bot));
    let bar = ui.interact(hit, Id::new("history_head_bar"), Sense::drag());
    let chip = head_chip;
    let dragging = bar.dragged() || chip.is_some_and(|c| c.dragged());
    let stopped = bar.drag_stopped() || chip.is_some_and(|c| c.drag_stopped());
    let hovered = bar.hovered() || chip.is_some_and(|c| c.hovered());
    let mut show = ui.data(|d| d.get_temp::<usize>(drag_key())).unwrap_or(cur);
    if dragging || stopped {
        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            let mut best = (f32::MAX, cur);
            for (k, x) in bounds.iter().enumerate() {
                let d = (x - p.x).abs();
                if d < best.0 {
                    best = (d, k + 1);
                }
            }
            show = best.1;
        }
    }
    let mut commit = None;
    if dragging {
        ui.data_mut(|d| d.insert_temp(drag_key(), show));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        ui.ctx().request_repaint();
    } else {
        ui.data_mut(|d| d.remove_temp::<usize>(drag_key()));
        if stopped {
            commit = Some(show).filter(|k| *k != cur);
        } else {
            show = cur;
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
    }
    let active = dragging || hovered;
    let hot = ui.ctx().animate_bool_with_time(Id::new("history_head_hot"), active, 0.12);
    let t = if dragging { 0.06 } else { 0.14 };
    let x = ui.ctx().animate_value_with_time(Id::new("history_head_x"), bounds[show.clamp(1, n) - 1], t);
    paint_cursor(ui, x, top, bot, hot, dragging);
    if hovered && !dragging {
        bar.on_hover_text("Drag the golden head to a step to roll back (or forward) · Ctrl+Alt+←/→ nudges it");
    }
    commit
}

fn paint_cursor(ui: &Ui, x: f32, top: f32, bot: f32, hot: f32, dragging: bool) {
    let p = ui.painter();
    let w = 7.0 + 2.0 * hot;
    let body = Rect::from_min_max(pos2(x - w / 2.0, top - 1.0), pos2(x + w / 2.0, bot + 1.0));
    // soft glow: a few widening, fading outlines
    let glow = if dragging { 46 } else { 22 } + (24.0 * hot) as u8;
    for (k, a) in [(2.0, glow), (4.5, glow / 2), (7.0, glow / 4)] {
        p.rect_filled(body.expand(k), CornerRadius::same((w / 2.0 + k) as u8), theme::HEAD.gamma_multiply(a as f32 / 255.0));
    }
    // the gold pill, lit from the top
    p.rect(
        body,
        CornerRadius::same((w / 2.0) as u8),
        Color32::from_rgb(0xe6, 0xb0, 0x12),
        Stroke::new(1.0, Color32::from_rgb(0xff, 0xe0, 0x6a)),
        egui::StrokeKind::Inside,
    );
    let hi = Rect::from_min_max(pos2(x - w / 2.0 + 1.5, top + 2.0), pos2(x - w / 2.0 + 3.0, bot - 2.0));
    p.rect_filled(hi, CornerRadius::same(1), Color32::from_rgba_unmultiplied(255, 255, 255, 70));
    // grip dots
    let cy = (top + bot) / 2.0;
    for dy in [-5.0, 0.0, 5.0] {
        p.circle_filled(pos2(x, cy + dy), 1.3, Color32::from_rgb(0x6b, 0x4c, 0x00));
    }
    // caret above and below, so it reads as a cursor even between two tall chips
    for (sign, y) in [(1.0, top - 1.0), (-1.0, bot + 1.0)] {
        p.add(egui::Shape::convex_polygon(
            vec![pos2(x - 4.0, y - sign * 3.0), pos2(x + 4.0, y - sign * 3.0), pos2(x, y + sign * 2.0)],
            theme::HEAD,
            Stroke::NONE,
        ));
    }
}

/// A faint cursor where a previewed state's head would land.
pub fn ghost_cursor(ui: &Ui, x: f32, top: f32, bot: f32) {
    let p = ui.painter();
    let c = theme::GREEN;
    p.rect_filled(Rect::from_min_max(pos2(x - 2.0, top - 1.0), pos2(x + 2.0, bot + 1.0)), CornerRadius::same(2), c.gamma_multiply(0.55));
    for (sign, y) in [(1.0, top - 1.0), (-1.0, bot + 1.0)] {
        p.add(egui::Shape::convex_polygon(
            vec![pos2(x - 3.5, y - sign * 3.0), pos2(x + 3.5, y - sign * 3.0), pos2(x, y + sign * 2.0)],
            c.gamma_multiply(0.7),
            Stroke::NONE,
        ));
    }
}

/// Where a previewed head lands, as an x: after chip `id`, or at the strip start.
pub fn head_x(head: HeadAt, xs: &HashMap<FeatureId, f32>, start: f32) -> Option<f32> {
    match head {
        HeadAt::Start => Some(start),
        HeadAt::After(id) => xs.get(&id).copied(),
    }
}

/// A dashed split line with a small diamond where a branch starts.
pub fn split_mark(ui: &Ui, x: f32, top: f32, bot: f32, col: Color32) {
    let p = ui.painter();
    let mut y = top + 8.0;
    while y < bot - 2.0 {
        p.line_segment([pos2(x, y), pos2(x, (y + 3.0).min(bot - 2.0))], Stroke::new(1.2, col.gamma_multiply(0.8)));
        y += 6.0;
    }
    let c = pos2(x, top + 3.5);
    p.add(egui::Shape::convex_polygon(
        vec![pos2(c.x, c.y - 3.5), pos2(c.x + 3.0, c.y), pos2(c.x, c.y + 3.5), pos2(c.x - 3.0, c.y)],
        col,
        Stroke::NONE,
    ));
}

// ------------------------------------------------------------------ branch strip

#[derive(Clone, Debug, PartialEq)]
pub enum BranchAct {
    Here(usize),
    Switch(String),
    BackToSplit,
    Keep,
    Discard(String),
}

/// The branch tabs above the timeline. Returns the action chosen and the index (in
/// `doc.data.branches`) of the stored branch tab the pointer is over.
pub fn branch_strip(app: &EasyCadApp, ui: &mut Ui) -> (Option<BranchAct>, Option<usize>) {
    let d = &app.doc.data;
    let mut act = None;
    let mut hov = None;
    let root = app.active.is_empty();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(RichText::new("BRANCH").size(10.5).color(theme::TEXT_LABEL));
        let cur = format!("{}  {}", d.branch, d.features.len());
        ui.add(
            egui::Button::new(RichText::new(cur).size(11.0).color(theme::TEXT))
                .fill(theme::ACCENT_BG)
                .stroke(Stroke::new(1.0, theme::ACCENT))
                .corner_radius(9)
                .min_size(vec2(0.0, 18.0)),
        )
        .on_hover_text(match &d.branch_parent {
            Some(p) => format!("You are on \"{}\" (started from \"{p}\" after {} features)", d.branch, d.branch_split),
            None => format!("You are on \"{}\"", d.branch),
        });
        for (i, b) in d.branches.iter().enumerate() {
            let r = ui.add(
                egui::Button::new(RichText::new(format!("{}  {}", b.name, b.data.features.len())).size(11.0).color(theme::TEXT_MUTED))
                    .fill(theme::CARD)
                    .stroke(Stroke::new(1.0, theme::hex(0x33373e)))
                    .corner_radius(9)
                    .min_size(vec2(0.0, 18.0)),
            );
            if r.hovered() {
                hov = Some(i);
            }
            let r = r.on_hover_text(format!("Click to switch to \"{}\" · hover shows what would change", b.name));
            if r.clicked() {
                act = Some(BranchAct::Switch(b.name.clone()));
            }
            r.context_menu(|ui| {
                if ui.button("Switch to this branch").clicked() {
                    act = Some(BranchAct::Switch(b.name.clone()));
                    ui.close();
                }
                if ui.button("Discard this branch").clicked() {
                    act = Some(BranchAct::Discard(b.name.clone()));
                    ui.close();
                }
            });
        }
        let n = d.features.len();
        let here = ui
            .add_enabled(root, egui::Button::new(RichText::new("+ Branch here").size(11.0)).corner_radius(9).min_size(vec2(0.0, 18.0)))
            .on_hover_text("Save this history and continue on a new branch from the head, to try an idea. Right-click a step to branch there")
            .on_disabled_hover_text("Branches apply to the whole document: leave the component first");
        if here.clicked() {
            act = Some(BranchAct::Here(app.rollback().unwrap_or(n)));
        }
        if d.branch_parent.is_some()
            && ui
                .add_enabled(root, egui::Button::new(RichText::new("Back to split").size(11.0)).corner_radius(9).min_size(vec2(0.0, 18.0)))
                .on_hover_text("Return to the parent branch with the head at the step this one started from")
                .clicked()
        {
            act = Some(BranchAct::BackToSplit);
        }
        if !d.branches.is_empty() {
            ui.menu_button(RichText::new("···").size(11.0), |ui| {
                if ui.button("Keep this branch (delete the others)").clicked() {
                    act = Some(BranchAct::Keep);
                    ui.close();
                }
                if ui.button("Discard this branch").clicked() {
                    act = Some(BranchAct::Discard(d.branch.clone()));
                    ui.close();
                }
            });
        }
    });
    (act, hov)
}

/// Apply a branch action to the document; the caller regenerates afterwards.
pub fn apply_branch(app: &mut EasyCadApp, act: BranchAct) {
    let r = match act {
        BranchAct::Here(at) => app.doc.branch_here(at).map(|n| format!("Started branch {n}")),
        BranchAct::Switch(n) => app.doc.switch_branch(&n).map(|_| format!("Switched to {n}")),
        BranchAct::BackToSplit => app.doc.back_to_split().map(|_| "Back at the split".to_string()),
        BranchAct::Keep => app.doc.keep_branch().map(|_| "Kept this branch".to_string()),
        BranchAct::Discard(n) => app.doc.discard_branch(&n).map(|_| format!("Discarded {n}")),
    };
    match r {
        Ok(m) => {
            app.toast(m);
            app.hist_sel.clear();
            app.after_edit();
        }
        Err(e) => app.toast(e),
    }
}
