//! Collapsible side panes: the browser (left) and Parameters (right) on the model
//! screen, and the sketch screen's two panels, slide between full size and a thin
//! rail so the viewport can take the window (e.g. while Claude models through the
//! bridge). Open / closed is remembered in the user's settings.
//!
//! Ways to toggle: the tab on the viewport's edge (pane open), the rail (pane
//! closed), Cmd/Ctrl+[ and Cmd/Ctrl+], Cmd/Ctrl+\ for both, dragging the pane's
//! edge past its minimum (or the rail's edge outward), double-clicking the edge.

use egui::{pos2, vec2, Color32, CornerRadius, Frame, Key, Modifiers, Rect, Sense, Stroke, Ui};

use crate::app::{EasyCadApp, Mode};
use crate::theme;

/// Width of a closed pane's rail.
pub const RAIL: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// The chord that toggles this side, as the cheat sheet and tooltips write it.
    pub fn keys(self) -> &'static str {
        match self {
            Side::Left => "Ctrl+[",
            Side::Right => "Ctrl+]",
        }
    }
}

/// One side pane: its panel id, sizes and frame when open.
pub struct Pane {
    pub id: &'static str,
    pub side: Side,
    /// What the pane holds, for tooltips ("browser", "parameters").
    pub name: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub frame: Frame,
}

/// Show `p` open (`*open`) or as its rail, sliding between the two. The rail, a
/// drag past the edge's limits and a double-click on the edge flip `*open`.
pub fn show(ui: &mut Ui, p: Pane, open: &mut bool, add: impl FnOnce(&mut Ui)) {
    // the open panel keeps the id the app always used, so its saved width holds
    let (rail, full) = match p.side {
        Side::Left => (egui::Panel::left((p.id, "rail")), egui::Panel::left(p.id)),
        Side::Right => (egui::Panel::right((p.id, "rail")), egui::Panel::right(p.id)),
    };
    let rail = rail
        .exact_size(RAIL)
        .resizable(true)
        .frame(Frame::new().fill(theme::PANEL));
    let full = full
        .default_size(p.default)
        .min_size(p.min)
        .max_size(p.max)
        .resizable(true)
        .frame(p.frame);
    let (side, name) = (p.side, p.name);
    let clicked = egui::Panel::show_switched(ui, open, rail, full, |ui, expanded| {
        // fill the panel: egui reserves the contents' width while it slides, and
        // contents narrower than the panel (the rail's, shown in the sliding
        // panel past halfway) would let the viewport's edge lag the panel's
        ui.set_min_width(ui.max_rect().width());
        if expanded {
            add(ui);
            false
        } else {
            rail_button(ui, side, name)
        }
    })
    .inner;
    if clicked {
        *open = true;
    }
}

/// The closed pane: the whole rail is the button that opens it.
fn rail_button(ui: &mut Ui, side: Side, name: &str) -> bool {
    let r = ui.max_rect();
    let resp = ui
        .interact(r, ui.scope_id().with("rail_btn"), Sense::click())
        .on_hover_text(format!("Show {name} ({})", side.keys()));
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(r, 0.0, theme::HOVER);
    }
    // points into the window: the way the pane will open
    let c = pos2(r.center().x, r.top() + 24.0);
    chevron(ui, c, side == Side::Left, if hot { theme::TEXT_STRONG } else { theme::TEXT_DIM });
    resp.clicked()
}

/// The tab on the viewport's edge next to an open pane; clicked → close it.
/// Call after the viewport has drawn, inside its panel, so it sits on top.
pub fn edge_tab(ui: &mut Ui, viewport: Rect, side: Side, name: &str) -> bool {
    let size = vec2(12.0, 40.0);
    let y = viewport.center().y - size.y / 2.0;
    let (r, cr) = match side {
        Side::Left => (
            Rect::from_min_size(pos2(viewport.left(), y), size),
            CornerRadius { nw: 0, sw: 0, ne: 6, se: 6 },
        ),
        Side::Right => (
            Rect::from_min_size(pos2(viewport.right() - size.x, y), size),
            CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 },
        ),
    };
    let resp = ui
        .interact(r, ui.scope_id().with(("pane_tab", side == Side::Left)), Sense::click())
        .on_hover_text(format!("Hide {name} ({})", side.keys()));
    let hot = resp.hovered();
    let p = ui.painter();
    p.rect(
        r,
        cr,
        if hot { theme::HOVER } else { theme::PANEL.gamma_multiply(0.9) },
        Stroke::new(1.0, theme::BORDER_CARD),
        egui::StrokeKind::Inside,
    );
    // points out of the window: the way the pane will close
    chevron(ui, r.center(), side == Side::Right, if hot { theme::TEXT_STRONG } else { theme::TEXT_DIM });
    resp.clicked()
}

fn chevron(ui: &Ui, c: egui::Pos2, right: bool, color: Color32) {
    let (dx, dy) = (if right { 2.5 } else { -2.5 }, 5.0);
    ui.painter().line(
        vec![pos2(c.x - dx, c.y - dy), pos2(c.x + dx, c.y), pos2(c.x - dx, c.y + dy)],
        Stroke::new(1.6, color),
    );
}

/// Is the left pane open on this screen? The browser also folds away while a
/// feature dialog is open, until the user opens it again (`browser_open`).
pub fn left_open(app: &EasyCadApp) -> bool {
    !app.settings.left_pane_collapsed
        && (app.dialog.is_none() || !app.mode.is_model() || app.browser_open)
}

pub fn right_open(app: &EasyCadApp) -> bool {
    !app.settings.right_pane_collapsed
}

/// Below this window width both panes open would leave the viewport too little
/// room: only one shows (the one opened last), the other keeps its rail.
pub const BOTH_PANES_MIN_W: f32 = 900.0;

/// Which side the user opened last (true = left): the one that shows when the
/// window is too narrow for both.
static LEFT_LAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// The same for the sketch screen, where the palette (right) comes first.
static SK_LEFT_LAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn last(sketch: bool) -> &'static std::sync::atomic::AtomicBool {
    if sketch {
        &SK_LEFT_LAST
    } else {
        &LEFT_LAST
    }
}

/// The panes actually shown, from the ones wanted open (`left_open` /
/// `right_open`, or the sketch screen's) and the window's width.
/// `sketch`: the sketch screen's panes (else the model screen's).
pub fn fit(ctx: &egui::Context, sketch: bool, (l, r): (bool, bool)) -> (bool, bool) {
    if l && r && ctx.content_rect().width() < BOTH_PANES_MIN_W {
        if last(sketch).load(std::sync::atomic::Ordering::Relaxed) {
            (true, false)
        } else {
            (false, true)
        }
    } else {
        (l, r)
    }
}

/// The user opened Parameters during this dialog: it stays, narrow or not.
static RIGHT_FOR_DIALOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The model screen's panes as shown this frame.
pub fn shown(app: &EasyCadApp, ctx: &egui::Context) -> (bool, bool) {
    let (l, mut r) = fit(ctx, false, (left_open(app), right_open(app)));
    // a feature dialog's card needs the viewport's width: in a narrow window
    // Parameters folds for the dialog too, like the browser does
    if app.dialog.is_none() {
        RIGHT_FOR_DIALOG.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    if r && app.dialog.is_some()
        && app.mode.is_model()
        && !RIGHT_FOR_DIALOG.load(std::sync::atomic::Ordering::Relaxed)
    {
        let left = if l { 250.0 } else { RAIL };
        if ctx.content_rect().width() - left - 300.0 < app.dialog_card_w.max(300.0) + 40.0 {
            r = false;
        }
    }
    (l, r)
}

/// Open or close a side: the user's choice, saved for the next start. Closing
/// the browser during a dialog only folds it for that dialog, as before.
pub fn set(app: &mut EasyCadApp, side: Side, open: bool) {
    if open {
        last(!app.mode.is_model()).store(side == Side::Left, std::sync::atomic::Ordering::Relaxed);
        if side == Side::Right && app.dialog.is_some() {
            RIGHT_FOR_DIALOG.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let folding_for_dialog =
        side == Side::Left && !open && app.dialog.is_some() && app.mode.is_model();
    match side {
        Side::Left if folding_for_dialog => app.browser_open = false,
        Side::Left => {
            app.settings.left_pane_collapsed = !open;
            if open {
                app.browser_open = true;
            }
        }
        Side::Right => app.settings.right_pane_collapsed = !open,
    }
    app.settings.save();
}

/// Bring `open` (what the panel did this frame) back into the app's state.
pub fn sync(app: &mut EasyCadApp, side: Side, was: bool, now: bool) {
    if was != now {
        set(app, side, now);
    }
}

/// Cmd/Ctrl+[ / ] toggle the left / right pane, Cmd/Ctrl+\ both (both close
/// unless both are closed already). Works in text fields too: these chords
/// type nothing.
pub fn shortcuts(app: &mut EasyCadApp, ctx: &egui::Context) {
    if app.wizard.is_some() || app.path_prompt.is_some() || !app.recovery_offer.is_empty() {
        return;
    }
    if !matches!(app.mode, Mode::Model | Mode::Sketch(_)) {
        return;
    }
    let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, k));
    let (l, r) = if app.mode.is_model() {
        shown(app, ctx)
    } else {
        fit(ctx, true, (!app.settings.left_pane_collapsed, !app.settings.right_pane_collapsed))
    };
    if pressed(Key::OpenBracket) {
        set(app, Side::Left, !l);
    }
    if pressed(Key::CloseBracket) {
        set(app, Side::Right, !r);
    }
    if pressed(Key::Backslash) {
        let open = !l && !r;
        set(app, Side::Left, open);
        set(app, Side::Right, open);
    }
}
