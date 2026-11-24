//! Small shared widgets: expression fields (with autocomplete and process-fit
//! chips), pills, cards, toolbar buttons and painter-drawn icons.

use easycad_model::params::{self, Expr, Values};
use easycad_model::process::{Fit, FitClass, FitRole, ProcessProfile};
use egui::{
    pos2, vec2, Color32, CornerRadius, Frame, Margin, Pos2, Rect, Response, RichText, Sense,
    Stroke, Ui,
};

use crate::theme;

/// Context for expression fields: parameter values and names plus the profile.
pub struct ExprEnv<'a> {
    pub values: &'a Values,
    pub names: &'a [String],
    pub profile: &'a ProcessProfile,
}

/// Functions the expression language knows (for autocomplete).
pub const FUNCTIONS: &[(&str, &str)] = &[
    (
        "hole(",
        "hole(d[, class]) · hole diameter under the active process",
    ),
    (
        "pin(",
        "pin(d[, class]) · pin / tongue size under the active process",
    ),
    (
        "gap(",
        "gap([class]) · one-side clearance between two faces",
    ),
    ("tol(", "tol(class) · per-side allowance of a fit class"),
    ("sqrt(", "square root"),
    ("abs(", "absolute value"),
    ("min(", "smaller of two values"),
    ("max(", "larger of two values"),
    ("sin(", "sine (degrees with `deg`)"),
    ("cos(", "cosine"),
    ("tan(", "tangent"),
];
pub const CLASSES: &[&str] = &["press", "snug", "slide", "loose"];

pub struct ExprOut {
    pub response: Response,
    pub changed: bool,
    /// Enter pressed or focus lost after an edit.
    pub committed: bool,
    pub value: Result<f64, String>,
    pub fit_changed: bool,
}

/// Evaluate an expression string against parameter values.
pub fn eval(src: &str, values: &Values) -> Result<f64, String> {
    params::eval(src, values).map_err(|e| e.to_string())
}

/// Identifier being typed at the end of `text`.
pub fn trailing_ident(text: &str) -> &str {
    let start = text
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let s = &text[start..];
    if s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        s
    } else {
        ""
    }
}

/// Autocomplete suggestions for the identifier at the end of `text`.
pub fn suggestions(text: &str, names: &[String]) -> Vec<(String, String)> {
    let id = trailing_ident(text);
    if id.is_empty() {
        return Vec::new();
    }
    let in_call = text[..text.len() - id.len()].trim_end().ends_with(',') || text.contains('(');
    let mut out: Vec<(String, String)> = Vec::new();
    for n in names {
        if n.starts_with(id) && n != id {
            out.push((n.clone(), "parameter".into()));
        }
    }
    for (f, help) in FUNCTIONS {
        if f.starts_with(id) {
            out.push((f.to_string(), help.to_string()));
        }
    }
    if in_call {
        for c in CLASSES {
            if c.starts_with(id) && *c != id {
                out.push((c.to_string(), "fit class".into()));
            }
        }
    }
    out.truncate(8);
    out
}

/// Replace the trailing identifier with `completion`.
pub fn apply_completion(text: &mut String, completion: &str) {
    let id_len = trailing_ident(text).len();
    text.truncate(text.len() - id_len);
    text.push_str(completion);
}

pub fn fit_label(f: Fit) -> String {
    match f.role {
        FitRole::Nominal => "nominal".into(),
        FitRole::Hole => format!("hole · {}", f.class.name()),
        FitRole::Pin => format!("pin · {}", f.class.name()),
        FitRole::Gap => format!("gap · {}", f.class.name()),
    }
}

/// "2.00 → 2.30 under FDM 0.4 (+0.15/side)".
pub fn fit_tooltip(nominal: f64, f: Fit, p: &ProcessProfile) -> String {
    let r = p.resolve(nominal, f);
    let a = p.allowance(f.class);
    if f.role == FitRole::Nominal {
        return format!("{nominal:.2} (nominal, no process allowance)");
    }
    format!("{nominal:.2} → {r:.2} under {} ({:+.2}/side)", p.name, a)
}

/// A profile that changes this fit's value.
pub fn fit_active(f: Fit, p: &ProcessProfile) -> bool {
    f.role != FitRole::Nominal && (p.resolve(10.0, f) - 10.0).abs() > 1e-9
}

/// Expression text field. `fit` shows a chip after the field (role + class picker).
pub fn expr_field(
    ui: &mut Ui,
    id: egui::Id,
    text: &mut String,
    width: f32,
    env: &ExprEnv,
    fit: Option<&mut Fit>,
) -> ExprOut {
    let value = eval(text, env.values);
    let bad = value.is_err() && !text.trim().is_empty();
    let border = if bad {
        theme::ORANGE
    } else {
        theme::BORDER_INPUT
    };
    let mut fit_changed = false;
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let te = egui::TextEdit::singleline(text)
            .id(id)
            .font(theme::mono(12.0))
            .desired_width(width)
            .margin(Margin::symmetric(6, 4))
            .background_color(theme::INPUT);
        let resp = ui.scope(|ui| {
            let v = ui.visuals_mut();
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
            v.widgets.hovered.bg_stroke = Stroke::new(
                1.0,
                if bad {
                    theme::ORANGE
                } else {
                    theme::BORDER_CARD
                },
            );
            v.selection.stroke = Stroke::new(1.5, theme::ACCENT);
            ui.add(te)
        });
        let resp = resp.inner;
        if let Some(f) = fit {
            let nominal = value.clone().unwrap_or(0.0);
            fit_changed = fit_chip(ui, id.with("fit"), f, nominal, env.profile);
        }
        resp
    });
    let mut resp = inner.inner;
    // autocomplete popup
    if resp.has_focus() {
        let sugg = suggestions(text, env.names);
        if !sugg.is_empty() {
            let accept_tab = ui.input(|i| i.key_pressed(egui::Key::Tab));
            if accept_tab {
                apply_completion(text, &sugg[0].0);
                resp.mark_changed();
                ui.memory_mut(|m| m.request_focus(id));
            } else {
                let pos = resp.rect.left_bottom() + vec2(0.0, 2.0);
                egui::Area::new(id.with("ac"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(pos)
                    .show(ui.ctx(), |ui| {
                        card_frame().inner_margin(Margin::same(4)).show(ui, |ui| {
                            ui.set_min_width(resp.rect.width().max(180.0));
                            for (i, (s, help)) in sugg.iter().enumerate() {
                                let r = ui.horizontal(|ui| {
                                    ui.label(RichText::new(s).font(theme::mono(12.0)).color(
                                        if i == 0 {
                                            theme::TEXT_STRONG
                                        } else {
                                            theme::TEXT
                                        },
                                    ));
                                    ui.label(RichText::new(help).small().color(theme::TEXT_MUTED));
                                });
                                let r = ui.interact(
                                    r.response.rect,
                                    id.with(("ac", i)),
                                    Sense::click(),
                                );
                                if r.clicked() {
                                    apply_completion(text, s);
                                    ui.memory_mut(|m| m.request_focus(id));
                                }
                            }
                            ui.label(
                                RichText::new("Tab completes")
                                    .small()
                                    .color(theme::TEXT_FAINT),
                            );
                        });
                    });
            }
        }
    }
    let tip = match &value {
        Ok(v) if text.trim().parse::<f64>().is_err() || crate::units::inch() => {
            format!("= {}", crate::units::len_u(*v))
        }
        Ok(_) => String::new(),
        Err(e) => e.clone(),
    };
    if !tip.is_empty() {
        resp = resp.on_hover_text(tip);
    }
    let committed =
        resp.lost_focus() || (resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
    ExprOut {
        changed: resp.changed(),
        committed,
        value,
        response: resp,
        fit_changed,
    }
}

/// The small `hole · slide` chip; click opens a role/class picker.
pub fn fit_chip(
    ui: &mut Ui,
    id: egui::Id,
    fit: &mut Fit,
    nominal: f64,
    profile: &ProcessProfile,
) -> bool {
    let active = fit_active(*fit, profile);
    let (bg, fg, border) = if active {
        (theme::ORANGE_BG, theme::ORANGE_SOFT, theme::hex(0x6a4a30))
    } else {
        (Color32::TRANSPARENT, theme::TEXT_MUTED, theme::BORDER_INPUT)
    };
    let label = fit_label(*fit);
    let resp = ui.add(
        egui::Button::new(RichText::new(label).font(theme::prop(10.5)).color(fg))
            .fill(bg)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(9))
            .min_size(vec2(0.0, 18.0)),
    );
    let resp = resp.on_hover_text(fit_tooltip(nominal, *fit, profile));
    let mut changed = false;
    egui::Popup::from_toggle_button_response(&resp)
        .id(id)
        .show(|ui| {
            ui.set_min_width(170.0);
            ui.label(
                RichText::new("PROCESS FIT")
                    .small()
                    .color(theme::TEXT_LABEL),
            );
            ui.horizontal(|ui| {
                for (role, name) in [
                    (FitRole::Hole, "Hole"),
                    (FitRole::Pin, "Pin"),
                    (FitRole::Gap, "Gap"),
                    (FitRole::Nominal, "Nominal"),
                ] {
                    if pill(ui, name, fit.role == role).clicked() {
                        fit.role = role;
                        changed = true;
                    }
                }
            });
            if fit.role != FitRole::Nominal {
                ui.horizontal(|ui| {
                    for c in FitClass::ALL {
                        if pill(ui, c.name(), fit.class == c).clicked() {
                            fit.class = c;
                            changed = true;
                        }
                    }
                });
            }
            ui.label(
                RichText::new(fit_tooltip(nominal, *fit, profile))
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        });
    let resolved = profile.resolve(nominal, *fit);
    if (resolved - nominal).abs() > 1e-9 {
        ui.label(
            RichText::new(format!("→ {}", crate::units::len(resolved)))
                .font(theme::mono(11.0))
                .color(theme::TEXT_MUTED),
        )
        .on_hover_text(fit_tooltip(nominal, *fit, profile));
    }
    changed
}

/// Read the fit of an expression (or a default).
pub fn expr_fit(e: &Expr) -> Option<Fit> {
    e.1
}

pub fn card_frame() -> Frame {
    Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER_CARD))
        .corner_radius(CornerRadius::same(10))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 30,
            spread: 0,
            color: Color32::from_black_alpha(128),
        })
}

pub fn note_frame() -> Frame {
    Frame::new()
        .stroke(Stroke::new(1.0, theme::hex(0x2c3037)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
}

pub fn note(ui: &mut Ui, text: &str) {
    note_frame().show(ui, |ui| {
        ui.label(RichText::new(text).size(12.0).color(theme::TEXT_MUTED));
    });
}

/// Selected/unselected pill button (filters, tabs, sizes).
pub fn pill(ui: &mut Ui, label: &str, on: bool) -> Response {
    let (bg, border, fg) = if on {
        (theme::ACCENT_BG, theme::ACCENT, theme::TEXT_STRONG)
    } else {
        (Color32::TRANSPARENT, theme::BORDER_INPUT, theme::TEXT_DIM)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(fg))
            .fill(bg)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(5.0)
            .min_size(vec2(0.0, 26.0)),
    )
}

/// Pill without border when off (segmented controls like the display modes).
pub fn seg(ui: &mut Ui, label: &str, on: bool) -> Response {
    let (bg, fg) = if on {
        (theme::ACCENT_BG, theme::TEXT_STRONG)
    } else {
        (Color32::TRANSPARENT, theme::TEXT_DIM)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(fg))
            .fill(bg)
            .stroke(Stroke::NONE)
            .corner_radius(5.0)
            .min_size(vec2(0.0, 28.0)),
    )
}

pub fn primary(ui: &mut Ui, label: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(theme::TEXT_STRONG))
            .fill(theme::PRIMARY)
            .stroke(Stroke::new(1.0, theme::ACCENT))
            .corner_radius(5.0)
            .min_size(vec2(0.0, 30.0)),
    )
}

pub fn button(ui: &mut Ui, label: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(theme::TEXT))
            .fill(theme::BUTTON)
            .stroke(Stroke::new(1.0, theme::BORDER_CARD))
            .corner_radius(5.0)
            .min_size(vec2(0.0, 30.0)),
    )
}

/// Dialog section divider (full width line).
/// A rule inside a frame with a 14 px horizontal margin, bleeding to the card edge.
pub fn divider(ui: &mut Ui) {
    let r = ui.available_rect_before_wrap();
    let y = ui.cursor().top();
    ui.painter().hline(
        r.left() - 14.0..=r.right() + 14.0,
        y,
        Stroke::new(1.0, theme::DIVIDER),
    );
    ui.add_space(1.0);
}

// ---------------------------------------------------------------- icons

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Logo,
    Undo,
    Redo,
    Sketch,
    Extrude,
    Revolve,
    Fillet,
    Chamfer,
    Shell,
    Draft,
    Split,
    Pattern,
    Mirror,
    Boss,
    Rib,
    Standoffs,
    Joint,
    Parts,
    Import,
    Export,
    Drawing,
    Eye,
    EyeOff,
    Fit,
    Section,
    Lock,
    Menu,
    Check,
    Warn,
    Cross,
    Plane,
    Construct,
    Axis,
    Point,
}

/// Paint an icon in a `size`² box (24-unit design grid like the mock's SVGs).
pub fn paint_icon(p: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width() / 24.0;
    let o = rect.min;
    let pt = |x: f32, y: f32| pos2(o.x + x * s, o.y + y * s);
    let st = Stroke::new(1.6 * s.max(0.75), color);
    let line = |pts: &[(f32, f32)]| {
        p.add(egui::Shape::line(
            pts.iter().map(|(x, y)| pt(*x, *y)).collect(),
            st,
        ));
    };
    let closed = |pts: &[(f32, f32)]| {
        p.add(egui::Shape::closed_line(
            pts.iter().map(|(x, y)| pt(*x, *y)).collect(),
            st,
        ));
    };
    let circle = |x: f32, y: f32, r: f32| {
        p.circle_stroke(pt(x, y), r * s, st);
    };
    let arc = |cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32| {
        let n = 16;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
                pt(cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, st));
    };
    match icon {
        Icon::Logo => {
            closed(&[
                (3.0, 8.0),
                (12.0, 3.0),
                (21.0, 8.0),
                (21.0, 16.0),
                (12.0, 21.0),
                (3.0, 16.0),
            ]);
            line(&[(3.0, 8.0), (12.0, 13.0), (21.0, 8.0)]);
            line(&[(12.0, 13.0), (12.0, 21.0)]);
        }
        Icon::Undo => {
            line(&[(9.0, 6.0), (4.0, 11.0), (9.0, 16.0)]);
            line(&[(4.0, 11.0), (14.0, 11.0)]);
            arc(14.0, 15.5, 4.5, 4.5, -90.0, 90.0);
            line(&[(14.0, 20.0), (11.0, 20.0)]);
        }
        Icon::Redo => {
            line(&[(15.0, 6.0), (20.0, 11.0), (15.0, 16.0)]);
            line(&[(20.0, 11.0), (10.0, 11.0)]);
            arc(10.0, 15.5, 4.5, 4.5, 90.0, 270.0);
            line(&[(10.0, 20.0), (13.0, 20.0)]);
        }
        Icon::Sketch => {
            closed(&[
                (4.0, 20.0),
                (8.0, 19.0),
                (19.0, 8.0),
                (16.0, 5.0),
                (5.0, 16.0),
            ]);
            line(&[(14.0, 7.0), (17.0, 10.0)]);
        }
        Icon::Extrude => {
            closed(&[(4.0, 13.0), (16.0, 13.0), (16.0, 20.0), (4.0, 20.0)]);
            line(&[(10.0, 10.0), (10.0, 3.0)]);
            line(&[(7.0, 6.0), (10.0, 3.0), (13.0, 6.0)]);
        }
        Icon::Revolve => {
            arc(12.0, 15.0, 8.0, 3.5, 0.0, 360.0);
            line(&[(12.0, 3.0), (12.0, 15.0)]);
            line(&[(4.0, 15.0), (4.0, 9.0)]);
            line(&[(20.0, 15.0), (20.0, 9.0)]);
        }
        Icon::Fillet => {
            line(&[(4.0, 20.0), (4.0, 11.0)]);
            arc(11.0, 11.0, 7.0, 7.0, 180.0, 270.0);
            line(&[(11.0, 4.0), (20.0, 4.0)]);
        }
        Icon::Chamfer => line(&[(4.0, 20.0), (4.0, 10.0), (10.0, 4.0), (20.0, 4.0)]),
        Icon::Shell => {
            line(&[(4.0, 6.0), (4.0, 20.0), (20.0, 20.0), (20.0, 6.0)]);
            line(&[(7.0, 6.0), (7.0, 17.0), (17.0, 17.0), (17.0, 6.0)]);
        }
        Icon::Draft => closed(&[(6.0, 20.0), (9.0, 4.0), (15.0, 4.0), (18.0, 20.0)]),
        Icon::Split => {
            closed(&[(5.0, 4.0), (19.0, 4.0), (19.0, 20.0), (5.0, 20.0)]);
            for i in 0..5 {
                let x = 2.0 + i as f32 * 4.4;
                line(&[(x, 12.0), (x + 2.2, 12.0)]);
            }
        }
        Icon::Pattern => {
            for (x, y) in [(3.0, 3.0), (15.0, 3.0), (3.0, 15.0), (15.0, 15.0)] {
                closed(&[(x, y), (x + 6.0, y), (x + 6.0, y + 6.0), (x, y + 6.0)]);
            }
        }
        Icon::Mirror => {
            for i in 0..5 {
                let y = 3.0 + i as f32 * 4.0;
                line(&[(12.0, y), (12.0, y + 2.0)]);
            }
            closed(&[(9.0, 7.0), (4.0, 17.0), (9.0, 17.0)]);
            closed(&[(15.0, 7.0), (20.0, 17.0), (15.0, 17.0)]);
        }
        Icon::Boss => {
            arc(12.0, 6.0, 6.0, 2.5, 0.0, 360.0);
            line(&[(6.0, 6.0), (6.0, 18.0)]);
            line(&[(18.0, 6.0), (18.0, 18.0)]);
            arc(12.0, 18.0, 6.0, 2.5, 0.0, 180.0);
            arc(12.0, 6.0, 2.0, 0.9, 0.0, 360.0);
        }
        Icon::Rib => {
            line(&[(3.0, 20.0), (21.0, 20.0)]);
            line(&[(7.0, 20.0), (7.0, 9.0)]);
            line(&[(12.0, 20.0), (12.0, 5.0)]);
            line(&[(17.0, 20.0), (17.0, 9.0)]);
        }
        Icon::Standoffs => {
            closed(&[(4.0, 4.0), (20.0, 4.0), (20.0, 20.0), (4.0, 20.0)]);
            for (x, y) in [(8.0, 8.0), (16.0, 8.0), (8.0, 16.0), (16.0, 16.0)] {
                circle(x, y, 1.4);
            }
        }
        Icon::Joint => {
            line(&[
                (3.0, 10.0),
                (9.0, 10.0),
                (9.0, 13.0),
                (15.0, 13.0),
                (15.0, 10.0),
                (21.0, 10.0),
            ]);
            line(&[
                (3.0, 14.0),
                (8.0, 14.0),
                (8.0, 17.0),
                (16.0, 17.0),
                (16.0, 14.0),
                (21.0, 14.0),
            ]);
            line(&[(3.0, 5.0), (21.0, 5.0)]);
            line(&[(3.0, 20.0), (21.0, 20.0)]);
        }
        Icon::Parts => {
            closed(&[(8.0, 4.0), (16.0, 4.0), (16.0, 8.0), (8.0, 8.0)]);
            line(&[(10.0, 8.0), (10.0, 20.0), (14.0, 20.0), (14.0, 8.0)]);
            line(&[(10.0, 12.0), (14.0, 12.0)]);
            line(&[(10.0, 16.0), (14.0, 16.0)]);
        }
        Icon::Import => {
            line(&[(12.0, 3.0), (12.0, 15.0)]);
            line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            line(&[(4.0, 20.0), (20.0, 20.0)]);
        }
        Icon::Export => {
            line(&[(12.0, 15.0), (12.0, 3.0)]);
            line(&[(7.0, 8.0), (12.0, 3.0), (17.0, 8.0)]);
            line(&[(4.0, 20.0), (20.0, 20.0)]);
        }
        Icon::Drawing => {
            closed(&[(3.0, 4.0), (21.0, 4.0), (21.0, 20.0), (3.0, 20.0)]);
            line(&[(13.0, 15.0), (21.0, 15.0)]);
            line(&[(13.0, 15.0), (13.0, 20.0)]);
            closed(&[(6.0, 8.0), (11.0, 8.0), (11.0, 12.0), (6.0, 12.0)]);
        }
        Icon::Eye | Icon::EyeOff => {
            arc(12.0, 12.0, 10.0, 7.0, 180.0, 360.0);
            arc(12.0, 12.0, 10.0, 7.0, 0.0, 180.0);
            circle(12.0, 12.0, 3.0);
            if icon == Icon::EyeOff {
                line(&[(4.0, 4.0), (20.0, 20.0)]);
            }
        }
        Icon::Fit => {
            line(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)]);
            line(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)]);
            line(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)]);
            line(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)]);
        }
        Icon::Section => {
            closed(&[(4.0, 4.0), (20.0, 4.0), (20.0, 20.0), (4.0, 20.0)]);
            line(&[(4.0, 20.0), (20.0, 4.0)]);
        }
        Icon::Lock => {
            closed(&[(6.0, 11.0), (18.0, 11.0), (18.0, 20.0), (6.0, 20.0)]);
            arc(12.0, 11.0, 4.0, 5.0, 180.0, 360.0);
        }
        Icon::Menu => {
            line(&[(4.0, 6.0), (20.0, 6.0)]);
            line(&[(4.0, 12.0), (14.0, 12.0)]);
            line(&[(4.0, 18.0), (17.0, 18.0)]);
        }
        Icon::Check => {
            let st2 = Stroke::new(2.2 * s.max(0.75), color);
            p.add(egui::Shape::line(
                vec![pt(5.0, 12.0), pt(9.0, 16.0), pt(19.0, 6.0)],
                st2,
            ));
        }
        Icon::Warn => {
            closed(&[(12.0, 3.0), (22.0, 21.0), (2.0, 21.0)]);
            line(&[(12.0, 10.0), (12.0, 15.0)]);
            line(&[(12.0, 18.0), (12.0, 18.5)]);
        }
        Icon::Cross => {
            line(&[(6.0, 6.0), (18.0, 18.0)]);
            line(&[(18.0, 6.0), (6.0, 18.0)]);
        }
        Icon::Plane => {
            closed(&[(3.0, 16.0), (9.0, 6.0), (21.0, 6.0), (15.0, 16.0)]);
            line(&[(12.0, 11.0), (12.0, 21.0)]);
        }
        Icon::Construct => {
            closed(&[(3.0, 17.0), (9.0, 8.0), (21.0, 8.0), (15.0, 17.0)]);
            circle(12.0, 12.5, 2.0);
            line(&[(12.0, 3.0), (12.0, 7.0)]);
        }
        Icon::Axis => {
            line(&[(3.0, 20.0), (7.0, 16.0)]);
            line(&[(9.5, 13.5), (10.5, 12.5)]);
            line(&[(13.0, 10.0), (16.0, 7.0)]);
            line(&[(18.0, 5.0), (21.0, 2.5)]);
        }
        Icon::Point => {
            circle(12.0, 12.0, 2.5);
            line(&[(12.0, 3.0), (12.0, 7.0)]);
            line(&[(12.0, 17.0), (12.0, 21.0)]);
            line(&[(3.0, 12.0), (7.0, 12.0)]);
            line(&[(17.0, 12.0), (21.0, 12.0)]);
        }
    }
}

pub fn icon(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_icon(ui.painter(), rect, icon, color);
    resp
}

/// Square icon button (title bar undo/redo, eyes).
pub fn icon_button(ui: &mut Ui, ic: Icon, size: f32, tooltip: &str, enabled: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(size, size),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = resp.hovered() && enabled;
    if hovered {
        ui.painter().rect_filled(rect, 6.0, theme::HOVER);
    }
    let col = if !enabled {
        theme::TEXT_FAINT
    } else if hovered {
        theme::TEXT_STRONG
    } else {
        theme::TEXT_DIM
    };
    let inner = rect.shrink(rect.width() * 0.22);
    paint_icon(ui.painter(), inner, ic, col);
    resp.on_hover_text(tooltip)
}

/// Toolbar button: icon above a label, 52×56 like the mock.
pub fn tool_button(ui: &mut Ui, ic: Icon, label: &str, tooltip: &str, active: bool) -> Response {
    let font = theme::prop(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), theme::TEXT);
    let w = (galley.size().x + 12.0).max(52.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 56.0), Sense::click());
    let hovered = resp.hovered();
    if active {
        ui.painter().rect(
            rect,
            6.0,
            theme::ACCENT_BG,
            Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter().rect(
            rect,
            6.0,
            theme::HOVER,
            Stroke::new(1.0, theme::BORDER_CARD),
            egui::StrokeKind::Inside,
        );
    }
    let col = if hovered || active {
        theme::TEXT_STRONG
    } else {
        theme::hex(0xd7dbe0)
    };
    let ir = Rect::from_center_size(pos2(rect.center().x, rect.top() + 18.0), vec2(20.0, 20.0));
    paint_icon(ui.painter(), ir, ic, col);
    ui.painter().text(
        pos2(rect.center().x, rect.top() + 40.0),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        col,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp.on_hover_text(tooltip)
}

/// A toolbar tool as an icon alone (narrow windows): its name is the tooltip's
/// first word and what screen readers and tests find.
pub fn tool_icon(ui: &mut Ui, ic: Icon, label: &str, tooltip: &str, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect(rect, 6.0, theme::HOVER, Stroke::new(1.0, theme::BORDER_CARD), egui::StrokeKind::Inside);
    }
    let col = if hovered { theme::TEXT_STRONG } else { theme::hex(0xd7dbe0) };
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), ic, col);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp.on_hover_text(tooltip)
}

/// The » button that opens an overflow menu.
pub fn more_button(ui: &mut Ui, label: &str, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let hot = resp.hovered() || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    if hot {
        ui.painter().rect(rect, 6.0, theme::HOVER, Stroke::new(1.0, theme::BORDER_CARD), egui::StrokeKind::Inside);
    }
    let col = if hot { theme::TEXT_STRONG } else { theme::TEXT_DIM };
    let st = Stroke::new(1.6, col);
    let c = rect.center();
    for dx in [-3.5, 2.5] {
        ui.painter().line(vec![pos2(c.x + dx - 2.5, c.y - 5.0), pos2(c.x + dx + 2.5, c.y), pos2(c.x + dx - 2.5, c.y + 5.0)], st);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp.on_hover_text(label)
}

/// A row of an overflow menu: icon and name.
pub fn menu_tool(ui: &mut Ui, ic: Icon, label: &str) -> Response {
    let h = 28.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 4.0, theme::HOVER);
    }
    let col = if hovered { theme::TEXT_STRONG } else { theme::hex(0xd7dbe0) };
    paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(18.0, 18.0)), ic, col);
    ui.painter().text(pos2(rect.left() + 32.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::prop(12.5), col);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp
}

/// Status dot.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(r.center(), 4.0, color);
}

/// Colour swatch square.
pub fn swatch(ui: &mut Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, color);
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(11.0)
        .color(theme::TEXT_MUTED)
}
pub fn label_caps(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(10.5)
        .color(theme::TEXT_LABEL)
}
pub fn mono_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).font(theme::mono(12.0))
}

/// Whether the window can be seen at all (not minimized, not covered).
pub fn window_visible(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        let v = i.viewport();
        v.minimized != Some(true) && v.occluded != Some(true)
    })
}

/// A busy spinner that animates at full rate only in the focused window, at
/// 2 frames/s when it is unfocused, and not at all when it cannot be seen
/// (egui's own spinner asks for every frame, even in the background).
pub fn spinner(ui: &mut Ui, size: f32, color: Option<Color32>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let focused = ui.input(|i| i.focused);
    let color = color.unwrap_or_else(|| ui.visuals().strong_text_color());
    let t = ui.input(|i| i.time);
    let t = if focused { t } else { (t * 2.0).floor() / 2.0 };
    let radius = rect.height() / 2.0 - 2.0;
    let start = t * std::f64::consts::TAU;
    let end = start + 240f64.to_radians() * t.sin();
    let points: Vec<egui::Pos2> = (0..=20)
        .map(|i| {
            let a = start + (end - start) * i as f64 / 20.0;
            rect.center() + radius * egui::vec2(a.cos() as f32, a.sin() as f32)
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, Stroke::new(3.0, color)));
    if window_visible(ui.ctx()) {
        if focused {
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
    resp
}
