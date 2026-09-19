//! The design system: tokens, typography, and the egui `Style` that turns the
//! stock look into a native-feeling one.
//!
//! Everything visual that isn't a widget's *content* is decided here — color
//! roles (`bg` → `surface` → `elevated`), the text tiers, rounding, spacing and
//! the Inter type scale. Views only ever reach for `Tokens` and the `RichText`
//! helpers; they never hard-code a hex value.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Rounding,
    Shadow, Stroke, TextStyle, Visuals,
};

// ---- color roles ------------------------------------------------------------

/// Semantic colors for the current appearance. Views read these through
/// [`tokens`] every frame, so a theme switch is instantaneous.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    /// Window background (the deepest layer).
    pub bg: Color32,
    /// Sidebar / panels — one step up.
    pub side: Color32,
    /// Cards and grouped content.
    pub surface: Color32,
    /// Controls resting on a card.
    pub elevated: Color32,
    /// Sunken wells: slider rails, text fields, preview troughs.
    pub well: Color32,
    /// 1 px hairlines.
    pub stroke: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

const DARK: Tokens = Tokens {
    dark: true,
    bg: Color32::from_rgb(0x0e, 0x0f, 0x13),
    side: Color32::from_rgb(0x14, 0x15, 0x1b),
    surface: Color32::from_rgb(0x1b, 0x1d, 0x25),
    elevated: Color32::from_rgb(0x25, 0x28, 0x32),
    well: Color32::from_rgb(0x0a, 0x0b, 0x0f),
    stroke: Color32::from_rgb(0x2b, 0x2e, 0x39),
    text: Color32::from_rgb(0xf2, 0xf3, 0xf7),
    text2: Color32::from_rgb(0x9c, 0xa0, 0xad),
    text3: Color32::from_rgb(0x66, 0x6a, 0x77),
    accent: Color32::from_rgb(0x0a, 0x84, 0xff),
    success: Color32::from_rgb(0x30, 0xd1, 0x58),
    warn: Color32::from_rgb(0xff, 0x9f, 0x0a),
    danger: Color32::from_rgb(0xff, 0x45, 0x3a),
};

const LIGHT: Tokens = Tokens {
    dark: false,
    bg: Color32::from_rgb(0xf2, 0xf2, 0xf7),
    side: Color32::from_rgb(0xe9, 0xe9, 0xef),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    elevated: Color32::from_rgb(0xf0, 0xf0, 0xf5),
    well: Color32::from_rgb(0xe3, 0xe3, 0xe9),
    stroke: Color32::from_rgb(0xd9, 0xd9, 0xe0),
    text: Color32::from_rgb(0x1c, 0x1c, 0x1e),
    text2: Color32::from_rgb(0x6c, 0x6c, 0x70),
    text3: Color32::from_rgb(0xa8, 0xa8, 0xad),
    accent: Color32::from_rgb(0x00, 0x7a, 0xff),
    success: Color32::from_rgb(0x34, 0xc7, 0x59),
    warn: Color32::from_rgb(0xff, 0x9f, 0x0a),
    danger: Color32::from_rgb(0xff, 0x3b, 0x30),
};

/// Tokens for whatever appearance the context is currently in.
pub fn tokens(ui: &egui::Ui) -> Tokens {
    tokens_for(ui.visuals().dark_mode)
}

pub fn tokens_for(dark: bool) -> Tokens {
    if dark {
        DARK
    } else {
        LIGHT
    }
}

// ---- geometry -----------------------------------------------------------------

pub const R_CONTROL: f32 = 8.0;
pub const R_CARD: f32 = 14.0;
pub const R_WINDOW: f32 = 16.0;
pub const SP_XS: f32 = 4.0;
pub const SP_S: f32 = 8.0;
pub const SP_M: f32 = 12.0;
pub const SP_L: f32 = 16.0;
pub const SP_XL: f32 = 24.0;

// ---- typography ---------------------------------------------------------------

const INTER: &str = "Inter";
const INTER_MEDIUM: &str = "Inter-Medium";
const INTER_SEMIBOLD: &str = "Inter-SemiBold";

/// Inter Medium — row labels, segment captions, button text.
pub fn medium() -> FontFamily {
    FontFamily::Name(INTER_MEDIUM.into())
}
/// Inter SemiBold — headings and inked titles.
pub fn semibold() -> FontFamily {
    FontFamily::Name(INTER_SEMIBOLD.into())
}

/// Named text styles beyond egui's built-ins.
pub fn ts_title() -> TextStyle {
    TextStyle::Name("Title".into())
}
pub fn ts_caption() -> TextStyle {
    TextStyle::Name("Caption".into())
}
pub fn ts_section() -> TextStyle {
    TextStyle::Name("Section".into())
}
pub fn ts_medium() -> TextStyle {
    TextStyle::Name("Medium".into())
}

/// Large title — one per screen.
pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(ts_title())
}
/// Card / group heading.
pub fn heading(text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(TextStyle::Heading)
}
/// Emphasised body (labels of rows, button text).
pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(ts_medium())
}
/// Secondary explanatory text.
pub fn caption(ui: &egui::Ui, text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(ts_caption()).color(tokens(ui).text2)
}
/// Tertiary text (hints, disabled).
pub fn hint(ui: &egui::Ui, text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(ts_caption()).color(tokens(ui).text3)
}
/// Uppercase section label, iOS grouped-list style.
pub fn section(ui: &egui::Ui, text: impl Into<String>) -> RichText {
    let s: String = text.into();
    RichText::new(s.to_uppercase()).text_style(ts_section()).color(tokens(ui).text3)
}

/// Register Inter (three weights) ahead of egui's bundled fallbacks so every
/// glyph Inter lacks (icons, emoji) still renders.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        INTER.into(),
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        INTER_MEDIUM.into(),
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-Medium.ttf")),
    );
    fonts.font_data.insert(
        INTER_SEMIBOLD.into(),
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
    );

    let fallbacks: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let with = |primary: &str| {
        let mut v = vec![primary.to_string()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, with(INTER));
    fonts.families.insert(medium(), with(INTER_MEDIUM));
    fonts.families.insert(semibold(), with(INTER_SEMIBOLD));
    ctx.set_fonts(fonts);
}

fn text_styles() -> BTreeMap<TextStyle, FontId> {
    use FontFamily::{Monospace, Proportional};
    [
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(14.0, medium())),
        (TextStyle::Heading, FontId::new(18.0, semibold())),
        (TextStyle::Monospace, FontId::new(13.0, Monospace)),
        (ts_title(), FontId::new(26.0, semibold())),
        (ts_caption(), FontId::new(12.5, Proportional)),
        (ts_section(), FontId::new(11.0, medium())),
        (ts_medium(), FontId::new(14.0, medium())),
    ]
    .into_iter()
    .collect()
}

// ---- style ----------------------------------------------------------------------

/// Apply the full theme (fonts must already be installed).
pub fn apply(ctx: &egui::Context, dark: bool) {
    let t = if dark { DARK } else { LIGHT };
    let mut style = egui::Style::default();
    style.text_styles = text_styles();
    style.visuals = visuals(t);

    let sp = &mut style.spacing;
    sp.item_spacing = egui::vec2(SP_S, SP_S);
    sp.button_padding = egui::vec2(12.0, 6.0);
    sp.interact_size = egui::vec2(40.0, 28.0);
    sp.slider_width = 160.0;
    sp.slider_rail_height = 4.0;
    sp.combo_width = 140.0;
    sp.icon_width = 18.0;
    sp.icon_width_inner = 10.0;
    sp.window_margin = Margin::same(SP_L);
    sp.menu_margin = Margin::same(SP_S);
    sp.tooltip_width = 320.0;
    sp.indent = SP_L;

    style.interaction.tooltip_delay = 0.35;
    style.animation_time = 0.14;

    ctx.set_style(Arc::new(style));
}

fn widget(bg: Color32, stroke: Color32, fg: Color32, expansion: f32) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0_f32, stroke),
        rounding: Rounding::same(R_CONTROL),
        fg_stroke: Stroke::new(1.0_f32, fg),
        expansion,
    }
}

fn visuals(t: Tokens) -> Visuals {
    let mut v = if t.dark { Visuals::dark() } else { Visuals::light() };

    v.panel_fill = t.bg;
    v.window_fill = t.surface;
    v.window_stroke = Stroke::new(1.0_f32, t.stroke);
    v.window_rounding = Rounding::same(R_WINDOW);
    v.window_shadow = Shadow {
        offset: egui::vec2(0.0, 12.0),
        blur: 32.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if t.dark { 120 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if t.dark { 110 } else { 36 }),
    };
    v.menu_rounding = Rounding::same(R_CARD);
    v.faint_bg_color = t.surface;
    v.extreme_bg_color = t.well;
    v.code_bg_color = t.well;
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.danger;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;

    let hover_bg = if t.dark {
        Color32::from_rgb(0x2e, 0x32, 0x3e)
    } else {
        Color32::from_rgb(0xe6, 0xe6, 0xec)
    };
    let active_bg = if t.dark {
        Color32::from_rgb(0x38, 0x3d, 0x4b)
    } else {
        Color32::from_rgb(0xd8, 0xd8, 0xe0)
    };

    // Plain labels take their color from `noninteractive.fg_stroke`.
    v.widgets.noninteractive = widget(t.surface, t.stroke, t.text, 0.0);
    v.widgets.inactive = widget(t.elevated, Color32::TRANSPARENT, t.text, 0.0);
    v.widgets.hovered = widget(hover_bg, t.stroke, t.text, 1.0);
    v.widgets.active = widget(active_bg, t.accent, t.text, 1.0);
    v.widgets.open = widget(t.elevated, t.stroke, t.text, 0.0);

    v.selection = egui::style::Selection {
        bg_fill: t.accent,
        stroke: Stroke::new(1.0_f32, Color32::WHITE),
    };
    v.text_cursor.stroke = Stroke::new(2.0_f32, t.accent);
    v
}

/// Tint a color to a translucent wash — for pills, selected rows, glows.
pub fn wash(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

/// Perceived luminance 0..1 of an sRGB color (for ink-on-swatch decisions).
pub fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}
