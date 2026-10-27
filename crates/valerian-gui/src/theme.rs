// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Design tokens, embedded fonts and the egui style.
//!
//! Valerian shares Helia's structure and type; the colours are a night sky
//! instead of a sun: deep blue-black grounds and a pale starlight accent. Every
//! colour lives in [`Palette`]; a light palette can be added beside [`DARK`]
//! without touching the widgets. A second palette, [`NIGHT`], is red on black
//! for use at the telescope: it keeps to long wavelengths so dark-adapted eyes
//! stay dark-adapted (see [`set_night`]).

use std::cell::Cell;
use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Theme, ThemePreference, Vec2,
};

/// Colour tokens.
pub struct Palette {
    /// Window ground.
    pub ground: Color32,
    /// Side panels.
    pub panel: Color32,
    /// Raised controls.
    pub raised: Color32,
    /// Hairline borders.
    pub border: Color32,
    /// Hovered or emphasised borders.
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// Tertiary text and overlines.
    pub faint: Color32,
    /// Starlight.
    pub accent: Color32,
    /// Hovered accent.
    pub accent_hover: Color32,
    /// Fill of selected rows and chips.
    pub selected_fill: Color32,
    /// Border of selected rows and chips.
    pub selected_border: Color32,
}

/// The dark (night sky) palette.
pub const DARK: Palette = Palette {
    ground: Color32::from_rgb(0x05, 0x07, 0x0E),
    panel: Color32::from_rgb(0x0A, 0x0F, 0x1B),
    raised: Color32::from_rgb(0x12, 0x19, 0x29),
    border: Color32::from_rgb(0x1D, 0x27, 0x3D),
    border_strong: Color32::from_rgb(0x2B, 0x39, 0x57),
    text: Color32::from_rgb(0xE6, 0xEA, 0xF4),
    muted: Color32::from_rgb(0xA5, 0xB0, 0xC8),
    faint: Color32::from_rgb(0x8A, 0x96, 0xB0),
    accent: Color32::from_rgb(0xA9, 0xBC, 0xFF),
    accent_hover: Color32::from_rgb(0xCB, 0xD7, 0xFF),
    selected_fill: Color32::from_rgb(0x14, 0x1E, 0x38),
    selected_border: Color32::from_rgb(0x2E, 0x45, 0x80),
};

/// The night-vision palette: red on black, nothing above about 650 nm.
pub const NIGHT: Palette = Palette {
    ground: Color32::from_rgb(0x06, 0x00, 0x00),
    panel: Color32::from_rgb(0x0E, 0x02, 0x02),
    raised: Color32::from_rgb(0x1A, 0x05, 0x05),
    border: Color32::from_rgb(0x34, 0x0B, 0x0B),
    border_strong: Color32::from_rgb(0x56, 0x14, 0x12),
    text: Color32::from_rgb(0xEE, 0x5A, 0x4C),
    muted: Color32::from_rgb(0xD0, 0x4A, 0x3E),
    faint: Color32::from_rgb(0xB8, 0x3E, 0x34),
    accent: Color32::from_rgb(0xFF, 0x4A, 0x3A),
    accent_hover: Color32::from_rgb(0xFF, 0x7A, 0x66),
    selected_fill: Color32::from_rgb(0x2A, 0x08, 0x08),
    selected_border: Color32::from_rgb(0x8A, 0x22, 0x1C),
};

thread_local! {
    // The window is drawn from one thread, so the choice is per thread; that
    // also keeps the tests, which run on a thread each, from affecting one another.
    static NIGHT_ON: Cell<bool> = const { Cell::new(false) };
}

/// Whether the night-vision palette is in use.
#[must_use]
pub fn is_night() -> bool {
    NIGHT_ON.with(Cell::get)
}

/// Switch between the night sky palette and the night-vision palette, and
/// install the matching egui visuals.
pub fn set_night(ctx: &egui::Context, on: bool) {
    NIGHT_ON.with(|n| n.set(on));
    ctx.set_visuals_of(Theme::Dark, visuals(palette()));
}

/// Palette in use.
#[must_use]
pub fn palette() -> &'static Palette {
    if is_night() { &NIGHT } else { &DARK }
}

/// A colour that says "something went wrong" in the palette in use: salmon on
/// the night sky, a brighter red at night (no hues the night palette lacks).
#[must_use]
pub fn error_colour() -> Color32 {
    if is_night() {
        Color32::from_rgb(0xFF, 0x8A, 0x70)
    } else {
        Color32::from_rgb(0xFF, 0x9A, 0x8A)
    }
}

/// A colour for "done": green on the night sky, the accent at night.
#[must_use]
pub fn ok_colour() -> Color32 {
    if is_night() {
        NIGHT.accent_hover
    } else {
        Color32::from_rgb(0x6F, 0xD4, 0x9A)
    }
}

/// Corner radius of controls.
pub const RADIUS: u8 = 8;
/// Corner radius of cards and the viewer.
pub const RADIUS_LARGE: u8 = 10;
/// Minimum hit size of primary controls.
pub const HIT: f32 = 44.0;

const SEMIBOLD: &str = "plex-sans-semibold";

/// Semibold UI font.
#[must_use]
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Regular UI font.
#[must_use]
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Monospace font for numbers and metadata.
#[must_use]
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    for (name, bytes) in [
        (
            "plex-sans",
            &include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")[..],
        ),
        (
            SEMIBOLD,
            &include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")[..],
        ),
        (
            "plex-mono",
            &include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")[..],
        ),
    ] {
        defs.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    defs.families
        .insert(FontFamily::Proportional, vec!["plex-sans".into()]);
    defs.families
        .insert(FontFamily::Monospace, vec!["plex-mono".into()]);
    defs.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        vec![SEMIBOLD.into(), "plex-sans".into()],
    );
    defs
}

/// Install fonts, visuals and spacing.
pub fn apply(ctx: &egui::Context) {
    let p = palette();
    ctx.set_fonts(fonts());
    ctx.set_theme(ThemePreference::Dark);
    ctx.set_visuals_of(Theme::Dark, visuals(p));
    ctx.style_mut_of(Theme::Dark, |s| {
        s.text_styles = [
            (TextStyle::Small, sans(12.0)),
            (TextStyle::Body, sans(14.0)),
            (TextStyle::Button, sans(14.0)),
            (TextStyle::Heading, semibold(22.0)),
            (TextStyle::Monospace, mono(13.0)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 8.0);
        s.spacing.interact_size = Vec2::new(40.0, 32.0);
        s.spacing.window_margin = Margin::same(16);
        s.interaction.tooltip_delay = 0.35;
    });
}

fn visuals(p: &Palette) -> egui::Visuals {
    let r = CornerRadius::same(RADIUS);
    let mut v = egui::Visuals::dark();
    v.override_text_color = None;
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border_strong);
    v.window_corner_radius = CornerRadius::same(RADIUS_LARGE);
    v.menu_corner_radius = r;
    v.extreme_bg_color = p.ground;
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.hyperlink_color = p.accent;
    v.weak_text_color = Some(p.faint);
    v.selection.bg_fill = p.selected_border;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.raised;
    w.hovered.weak_bg_fill = p.raised;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.selected_fill;
    w.active.weak_bg_fill = p.selected_fill;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.accent_hover);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open = w.inactive;
    v
}
