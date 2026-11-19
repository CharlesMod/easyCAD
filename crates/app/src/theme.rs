//! Dark theme tokens from the approved mock, applied to egui's style.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Window ground.
pub const GROUND: Color32 = hex(0x17191d);
/// Title / status bars.
pub const BAR: Color32 = hex(0x111316);
/// Side panels and toolbar.
pub const PANEL: Color32 = hex(0x1c1f24);
/// Floating cards (dialogs, popovers).
pub const CARD: Color32 = hex(0x22262c);
/// 3D viewport background.
pub const VIEWPORT: Color32 = hex(0x1d2025);
/// Sketch canvas background.
pub const SKETCH_BG: Color32 = hex(0x1a1d21);
/// Text input background.
pub const INPUT: Color32 = hex(0x141619);

pub const BORDER: Color32 = hex(0x262a30);
pub const BORDER_SOFT: Color32 = hex(0x24282e);
pub const BORDER_CARD: Color32 = hex(0x3a3f47);
pub const BORDER_INPUT: Color32 = hex(0x333840);
pub const DIVIDER: Color32 = hex(0x30353c);
pub const HOVER: Color32 = hex(0x2b2f36);
pub const BUTTON: Color32 = hex(0x262a30);

pub const TEXT: Color32 = hex(0xe6e8eb);
pub const TEXT_STRONG: Color32 = Color32::WHITE;
pub const TEXT_DIM: Color32 = hex(0xc3c8cf);
pub const TEXT_MUTED: Color32 = hex(0x9aa1ab);
pub const TEXT_LABEL: Color32 = hex(0x8b929c);
pub const TEXT_FAINT: Color32 = hex(0x5b616b);

pub const ACCENT: Color32 = hex(0x4c9bff);
pub const ACCENT_LINK: Color32 = hex(0x7db4ff);
pub const ACCENT_BG: Color32 = hex(0x2f4e78);
pub const ACCENT_DEEP: Color32 = hex(0x1e3a60);
pub const PRIMARY: Color32 = hex(0x2f6fd0);
pub const ROW_SELECTED: Color32 = hex(0x24364f);
/// Parameter rows that feed the open dialog.
pub const ROW_HOT: Color32 = hex(0x22303f);

pub const ORANGE: Color32 = hex(0xf0883e);
pub const ORANGE_SOFT: Color32 = hex(0xf0a46a);
pub const ORANGE_LIGHT: Color32 = hex(0xffb27a);
pub const ORANGE_BG: Color32 = hex(0x3a2a1c);
pub const ORANGE_CARD: Color32 = hex(0x2c2219);
pub const GREEN: Color32 = hex(0x6fd29a);
pub const GREEN_REF: Color32 = hex(0x3a9a5c);
pub const RED: Color32 = hex(0xe5534b);
pub const PURPLE: Color32 = hex(0xb48cff);
pub const SILK: Color32 = hex(0xd6c36a);

/// Body display colour (light grey like the mock's top faces).
pub const BODY: Color32 = hex(0xb9bfc8);
pub const EDGE_DARK: Color32 = hex(0x23272e);
pub const EDGE_LIGHT: Color32 = hex(0xc7ccd4);

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}
pub fn prop(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Font files tried in order (local system fonts only; nothing is downloaded).
/// Inter / JetBrains Mono match the mock; DejaVu covers symbols (⌀ ∥ ⊥ ⟲ …).
const PROP_FONTS: &[&str] = &[
    "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
    "/Library/Fonts/Inter-Regular.ttf",
    "C:\\Windows\\Fonts\\Inter-Regular.ttf",
];
const MONO_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/opentype/jetbrains-mono/JetBrainsMono-Regular.otf",
    "/Library/Fonts/JetBrainsMono-Regular.ttf",
    "C:\\Windows\\Fonts\\JetBrainsMono-Regular.ttf",
];
const SYMBOL_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/gdouros-symbola/Symbola.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "C:\\Windows\\Fonts\\seguisym.ttf",
];

fn first_font(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Install Inter / JetBrains Mono / a symbol fallback when present on the system.
pub fn install_fonts(ctx: &egui::Context) {
    use std::sync::Arc;
    let mut defs = egui::FontDefinitions::default();
    let mut put = |name: &str, data: Option<Vec<u8>>, family: FontFamily, primary: bool| {
        if let Some(d) = data {
            defs.font_data
                .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(d)));
            let list = defs.families.entry(family).or_default();
            if primary {
                list.insert(0, name.to_owned());
            } else {
                list.push(name.to_owned());
            }
        }
    };
    let sym = first_font(SYMBOL_FONTS);
    put(
        "inter",
        first_font(PROP_FONTS),
        FontFamily::Proportional,
        true,
    );
    put(
        "jetbrains",
        first_font(MONO_FONTS),
        FontFamily::Monospace,
        true,
    );
    put("symbols", sym.clone(), FontFamily::Proportional, false);
    put("symbols_m", sym, FontFamily::Monospace, false);
    ctx.set_fonts(defs);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.extreme_bg_color = INPUT;
    v.faint_bg_color = hex(0x1f2227);
    v.code_bg_color = INPUT;
    v.window_stroke = Stroke::new(1.0, BORDER_CARD);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = ACCENT_LINK;
    v.selection.bg_fill = ACCENT_BG;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.override_text_color = Some(TEXT);
    v.warn_fg_color = ORANGE_SOFT;
    v.error_fg_color = RED;
    let r = CornerRadius::same(5);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.bg_fill = BUTTON;
    v.widgets.inactive.weak_bg_fill = BUTTON;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_CARD);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.bg_fill = hex(0x30353c);
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_CARD);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.bg_fill = ACCENT_BG;
    v.widgets.active.weak_bg_fill = ACCENT_BG;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    v.widgets.active.corner_radius = r;
    v.widgets.open = v.widgets.active;
    v.widgets.noninteractive.corner_radius = r;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, prop(15.0)),
            (TextStyle::Body, prop(13.0)),
            (TextStyle::Button, prop(12.5)),
            (TextStyle::Small, prop(11.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size = egui::vec2(28.0, 26.0);
        s.spacing.window_margin = Margin::same(0);
        s.spacing.menu_margin = Margin::same(6);
        s.interaction.tooltip_delay = 0.35;
    });
}

/// Small all-caps section label like "PARAMETERS".
pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(spaced(text))
            .font(prop(10.5))
            .color(TEXT_LABEL),
    );
}

/// Letter-spaced caps approximation (egui has no letter-spacing).
pub fn spaced(text: &str) -> String {
    text.to_uppercase()
}
