//! The widget kit — the handful of controls that make the app read as a
//! product instead of a debug panel. All painted with egui primitives, all
//! theme-aware through [`super::theme`], none touching app state.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, Vec2};

use super::theme::{self, tokens, wash, Tokens, R_CARD, R_CONTROL, SP_L, SP_M, SP_S, SP_XL, SP_XS};
use colormemuch::rgb::Rgb;

pub fn rgb32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
pub fn arr32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// ---- segmented control --------------------------------------------------------

/// One segment of a [`segmented`] control.
pub struct Segment<T> {
    pub value: T,
    pub label: String,
    pub enabled: bool,
    /// Tooltip (shown for disabled segments too — explain *why*).
    pub hint: Option<String>,
    /// Optional accent for the label when selected (e.g. effect-type hues).
    pub tint: Option<Color32>,
}

impl<T> Segment<T> {
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            enabled: true,
            hint: None,
            tint: None,
        }
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn hint(mut self, s: impl Into<String>) -> Self {
        self.hint = Some(s.into());
        self
    }
    pub fn tint(mut self, c: Color32) -> Self {
        self.tint = Some(c);
        self
    }
}

/// iOS-style segmented control. Returns `true` when the selection changed.
pub fn segmented<T: PartialEq + Clone>(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    current: &mut T,
    segments: &[Segment<T>],
) -> bool {
    let t = tokens(ui);
    let id = ui.make_persistent_id(id);
    let font = FontId::new(13.0, theme::medium());
    let pad_x = 12.0;
    let h = 28.0;

    // Measure.
    let widths: Vec<f32> = segments
        .iter()
        .map(|s| {
            let g = ui.painter().layout_no_wrap(s.label.clone(), font.clone(), t.text);
            g.size().x + pad_x * 2.0
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, h), Sense::hover());
    let p = ui.painter();
    p.rect(rect, R_CONTROL + 1.0, t.well, Stroke::new(1.0_f32, t.stroke));

    let mut changed = false;
    let mut x = rect.left() + 2.0;
    for (i, s) in segments.iter().enumerate() {
        let seg = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), Vec2::new(widths[i], h - 4.0));
        x += widths[i];
        let selected = *current == s.value;
        let resp = ui.interact(seg, id.with(i), if s.enabled { Sense::click() } else { Sense::hover() });
        let sel_t = ui.ctx().animate_bool(id.with(("sel", i)), selected);
        if sel_t > 0.0 {
            let fill = t.elevated.lerp_to_gamma(
                if t.dark { Color32::from_rgb(0x3a, 0x3f, 0x4d) } else { Color32::WHITE },
                0.6,
            );
            p.rect(
                seg,
                R_CONTROL - 1.0,
                wash(fill, (sel_t * 255.0) as u8),
                Stroke::new(1.0_f32, wash(t.stroke, (sel_t * 255.0) as u8)),
            );
        } else if resp.hovered() && s.enabled {
            p.rect_filled(seg, R_CONTROL - 1.0, wash(t.text, 10));
        }
        let ink = if !s.enabled {
            t.text3
        } else if selected {
            s.tint.unwrap_or(t.text)
        } else {
            t.text2
        };
        p.text(seg.center(), Align2::CENTER_CENTER, &s.label, font.clone(), ink);
        if let Some(h) = &s.hint {
            resp.clone().on_hover_text(h.clone());
        }
        if resp.clicked() && !selected {
            *current = s.value.clone();
            changed = true;
        }
    }
    changed
}

// ---- toggle switch ------------------------------------------------------------

/// iOS toggle. Returns the response; check `.changed()`.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let t = tokens(ui);
    let size = Vec2::new(44.0, 26.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = ui.ctx().animate_bool(resp.id, *on);
    let off_fill = if t.dark { Color32::from_rgb(0x3a, 0x3d, 0x48) } else { Color32::from_rgb(0xe9, 0xe9, 0xeb) };
    let fill = off_fill.lerp_to_gamma(t.success, k);
    let p = ui.painter();
    let r = rect.height() / 2.0;
    p.rect_filled(rect, r, fill);
    let knob_r = r - 2.5;
    let cx = egui::lerp((rect.left() + r)..=(rect.right() - r), k);
    let c = egui::pos2(cx, rect.center().y);
    p.circle_filled(c + Vec2::new(0.0, 1.0), knob_r, Color32::from_black_alpha(60));
    p.circle_filled(c, knob_r, Color32::WHITE);
    resp
}

/// A labelled toggle row: title (+ optional caption) on the left, switch on
/// the right, full width. Returns `true` on change.
pub fn toggle_row(ui: &mut Ui, title: &str, caption: Option<&str>, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(theme::strong(title));
            if let Some(c) = caption {
                ui.label(theme::caption(ui, c));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = toggle(ui, on).changed();
        });
    });
    changed
}

// ---- containers ---------------------------------------------------------------

/// A raised card. Content gets the full width the caller allotted.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let t = tokens(ui);
    egui::Frame::none()
        .fill(t.surface)
        .stroke(Stroke::new(1.0_f32, t.stroke))
        .rounding(R_CARD)
        .inner_margin(egui::Margin::same(SP_L))
        .shadow(egui::Shadow {
            offset: egui::vec2(0.0, 2.0),
            blur: 8.0,
            spread: 0.0,
            color: Color32::from_black_alpha(if t.dark { 40 } else { 14 }),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Cap the width of a block (cards otherwise fill the available width).
pub fn narrow<R>(ui: &mut Ui, max_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = ui.available_width().min(max_w);
    ui.allocate_ui_with_layout(Vec2::new(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(w);
        add(ui)
    })
    .inner
}

/// A flat inset well (for previews / grouped controls inside a card).
pub fn well<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let t = tokens(ui);
    egui::Frame::none()
        .fill(t.well)
        .rounding(R_CONTROL + 2.0)
        .inner_margin(egui::Margin::same(SP_M))
        .show(ui, add)
        .inner
}

/// Card header: swatch/icon + title + subtitle, with a trailing accessory.
pub fn card_header(
    ui: &mut Ui,
    leading: impl FnOnce(&mut Ui),
    title: &str,
    subtitle: &str,
    trailing: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        leading(ui);
        ui.add_space(SP_XS);
        ui.vertical(|ui| {
            ui.label(theme::heading(title));
            if !subtitle.is_empty() {
                ui.label(theme::caption(ui, subtitle));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    });
}

/// Uppercase section label with an optional right-aligned accessory.
pub fn section_header(ui: &mut Ui, text: &str, right: impl FnOnce(&mut Ui)) {
    ui.add_space(SP_XS);
    ui.horizontal(|ui| {
        ui.add_space(SP_XS);
        ui.label(theme::section(ui, text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    ui.add_space(SP_XS);
}

/// A settings-style row: fixed-width label on the left, control on the right.
pub fn form_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(88.0, 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(theme::caption(ui, label));
            },
        );
        add(ui)
    })
    .inner
}

/// Hairline divider that respects the theme.
pub fn divider(ui: &mut Ui) {
    let t = tokens(ui);
    ui.add_space(SP_XS);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0_f32, t.stroke));
    ui.add_space(SP_XS);
}

// ---- pills & badges -------------------------------------------------------------

/// Tinted capsule with a dot — status indicators.
pub fn status_pill(ui: &mut Ui, hue: Color32, text: &str) -> Response {
    let t = tokens(ui);
    egui::Frame::none()
        .fill(wash(hue, 28))
        .stroke(Stroke::new(1.0_f32, wash(hue, 90)))
        .rounding(999.0)
        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, hue);
                ui.label(egui::RichText::new(text).text_style(theme::ts_caption()).color(t.text));
            });
        })
        .response
}

/// Small tinted tag (e.g. "Verified", "Experimental").
pub fn chip(ui: &mut Ui, hue: Color32, text: &str) {
    egui::Frame::none()
        .fill(wash(hue, 30))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).text_style(egui::TextStyle::Small).color(hue));
        });
}

// ---- buttons ------------------------------------------------------------------

/// Accent-filled primary action.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let t = tokens(ui);
    let btn = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(t.accent)
        .stroke(Stroke::NONE)
        .rounding(R_CONTROL)
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(btn)
}

/// Quiet text button in a semantic color (Cancel / Delete).
pub fn quiet_button(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let btn = egui::Button::new(egui::RichText::new(text).color(color))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .rounding(R_CONTROL)
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(btn)
}

/// Circled "i" — painted, so it never depends on glyph coverage.
pub fn info_button(ui: &mut Ui) -> Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let k = ui.ctx().animate_bool(resp.id, resp.hovered());
    if k > 0.0 {
        ui.painter().rect_filled(rect, R_CONTROL, wash(t.text, (k * 22.0) as u8));
    }
    let ink = if resp.hovered() { t.text } else { t.text2 };
    ui.painter().circle_stroke(rect.center(), 7.0, Stroke::new(1.3_f32, ink));
    ui.painter().text(
        rect.center() + Vec2::new(0.0, 0.5),
        Align2::CENTER_CENTER,
        "i",
        FontId::new(11.0, theme::semibold()),
        ink,
    );
    resp
}

/// Square glyph button for toolbars.
pub fn icon_button(ui: &mut Ui, glyph: &str) -> Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let k = ui.ctx().animate_bool(resp.id, resp.hovered());
    if k > 0.0 {
        ui.painter()
            .rect_filled(rect, R_CONTROL, wash(t.text, (k * 22.0) as u8));
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(16.0),
        if resp.hovered() { t.text } else { t.text2 },
    );
    resp
}

// ---- list rows ------------------------------------------------------------------

/// Sidebar row: leading painter, title, optional subtitle. Selected rows get an
/// accent wash; hovered rows a faint one.
pub fn list_row(
    ui: &mut Ui,
    selected: bool,
    leading: impl FnOnce(&mut Ui, Rect),
    title: &str,
    subtitle: &str,
) -> Response {
    let t = tokens(ui);
    let h = if subtitle.is_empty() { 34.0 } else { 44.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    let k = ui.ctx().animate_bool(resp.id.with("sel"), selected);
    let hk = ui.ctx().animate_bool(resp.id.with("hov"), resp.hovered() && !selected);
    let p = ui.painter();
    if k > 0.0 {
        p.rect_filled(rect, R_CONTROL + 2.0, wash(t.accent, (k * 46.0) as u8));
    }
    if hk > 0.0 {
        p.rect_filled(rect, R_CONTROL + 2.0, wash(t.text, (hk * 12.0) as u8));
    }
    let lead = Rect::from_center_size(
        egui::pos2(rect.left() + 10.0 + 11.0, rect.center().y),
        Vec2::splat(22.0),
    );
    leading(ui, lead);
    let text_x = lead.right() + 10.0;
    if subtitle.is_empty() {
        ui.painter().text(
            egui::pos2(text_x, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            FontId::new(14.0, theme::medium()),
            t.text,
        );
    } else {
        ui.painter().text(
            egui::pos2(text_x, rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            title,
            FontId::new(14.0, theme::medium()),
            t.text,
        );
        ui.painter().text(
            egui::pos2(text_x, rect.center().y + 8.0),
            Align2::LEFT_CENTER,
            subtitle,
            FontId::proportional(12.0),
            t.text2,
        );
    }
    resp
}

// ---- color surfaces --------------------------------------------------------------

/// Rounded color tile. Glows when `on`; reads as unlit hardware when off.
pub fn swatch(ui: &mut Ui, color: Color32, on: bool, size: f32) -> Rect {
    let t = tokens(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_swatch(ui, rect, color, on, t);
    rect
}

pub fn paint_swatch(ui: &Ui, rect: Rect, color: Color32, on: bool, t: Tokens) {
    let p = ui.painter();
    let r = (rect.width() * 0.28).min(12.0);
    if on {
        // Soft glow: a few expanding translucent rings.
        for (i, a) in [(6.0, 26u8), (3.0, 40u8)] {
            p.rect_filled(rect.expand(i), r + i, wash(color, a));
        }
        p.rect(rect, r, color, Stroke::new(1.0_f32, wash(Color32::WHITE, 60)));
    } else {
        p.rect(rect, r, t.well, Stroke::new(1.0_f32, t.stroke));
        // A faint memory of the color, like an unpowered LED.
        p.rect_filled(rect.shrink(rect.width() * 0.3), r * 0.5, wash(color, 70));
    }
}

/// Live gradient strip (custom-effect thumbnails and previews).
pub fn gradient_bar(ui: &mut Ui, colors: &[Rgb], size: Vec2, rounding: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_gradient(ui, rect, colors, rounding);
    rect
}

/// Cells are painted edge to edge; only the outermost two carry the corner
/// radii, so the strip reads as one rounded gradient without any clipping.
pub fn paint_gradient(ui: &Ui, rect: Rect, colors: &[Rgb], rounding: f32) {
    let p = ui.painter();
    if colors.is_empty() {
        p.rect_filled(rect, rounding, Color32::from_gray(40));
        return;
    }
    let n = colors.len();
    let r = rounding.min(rect.height() / 2.0);
    let cw = rect.width() / n as f32;
    for (i, c) in colors.iter().enumerate() {
        let x0 = rect.left() + i as f32 * cw;
        // Overlap interior cells by a hair to hide seams; keep the ends exact.
        let x1 = if i + 1 == n { rect.right() } else { x0 + cw + 0.6 };
        let cell = Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
        let rounding = egui::Rounding {
            nw: if i == 0 { r } else { 0.0 },
            sw: if i == 0 { r } else { 0.0 },
            ne: if i + 1 == n { r } else { 0.0 },
            se: if i + 1 == n { r } else { 0.0 },
        };
        p.rect_filled(cell, rounding, rgb32(*c));
    }
}

/// Color picker button styled as a small swatch + hex readout.
pub fn color_field(ui: &mut Ui, color: &mut [u8; 3]) -> bool {
    let t = tokens(ui);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SP_S;
        if ui.color_edit_button_srgb(color).changed() {
            changed = true;
        }
        ui.label(
            egui::RichText::new(format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]))
                .monospace()
                .color(t.text2),
        );
    });
    changed
}

// ---- states ---------------------------------------------------------------------

/// Centered empty / blocked state inside a card.
pub fn empty_state(ui: &mut Ui, glyph: &str, title: &str, body: &str, action: impl FnOnce(&mut Ui)) {
    card(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(SP_L);
            let t = tokens(ui);
            let (r, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 28.0, wash(t.accent, 30));
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(26.0),
                t.accent,
            );
            ui.add_space(SP_M);
            ui.label(theme::heading(title));
            ui.add_space(SP_XS);
            ui.set_max_width(380.0);
            ui.label(egui::RichText::new(body).color(t.text2));
            ui.add_space(SP_M);
            action(ui);
            ui.add_space(SP_S);
        });
    });
}

/// Inline banner (errors, warnings) — a tinted card with a leading bar.
pub fn banner(ui: &mut Ui, hue: Color32, title: &str, body: &str) {
    egui::Frame::none()
        .fill(wash(hue, 22))
        .stroke(Stroke::new(1.0_f32, wash(hue, 80)))
        .rounding(R_CONTROL + 2.0)
        .inner_margin(egui::Margin::symmetric(SP_M, SP_S))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(3.0, 30.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, hue);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(title).text_style(theme::ts_medium()).color(hue));
                    if !body.is_empty() {
                        ui.label(theme::caption(ui, body));
                    }
                });
            });
        });
}

/// Transient bottom-center notification.
pub fn toast(ctx: &egui::Context, hue: Color32, text: &str, age_secs: f32, lifetime: f32) {
    let alpha = ((lifetime - age_secs) / 0.5).clamp(0.0, 1.0) * (age_secs / 0.18).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    egui::Area::new(egui::Id::new("toast"))
        .anchor(Align2::CENTER_BOTTOM, [0.0, -SP_XL])
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            let t = tokens(ui);
            let a = (alpha * 255.0) as u8;
            egui::Frame::none()
                .fill(wash(t.elevated, a))
                .stroke(Stroke::new(1.0_f32, wash(hue, (alpha * 120.0) as u8)))
                .rounding(999.0)
                .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                .shadow(egui::Shadow {
                    offset: egui::vec2(0.0, 6.0),
                    blur: 18.0,
                    spread: 0.0,
                    color: Color32::from_black_alpha((alpha * 90.0) as u8),
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, wash(hue, a));
                        ui.label(egui::RichText::new(text).color(wash(t.text, a)));
                    });
                });
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(40));
}
