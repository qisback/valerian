// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The few widgets the shell needs.

use eframe::egui::{
    self, Color32, CursorIcon, Painter, Pos2, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    WidgetType, vec2,
};

use crate::theme::{HIT, RADIUS, palette, semibold};

/// Button style.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Outlined.
    Outline,
    /// Filled with the accent: the one action a panel is for.
    Primary,
}

/// A 44 px button. `min_width` of 0 sizes to the label.
pub fn button(ui: &mut Ui, label: &str, kind: Kind, min_width: f32) -> Response {
    let p = palette();
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), semibold(14.0), Color32::PLACEHOLDER);
    let size = vec2((galley.size().x + 36.0).max(min_width), HIT);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    ui.output_mut(|o| {
        if resp.hovered() {
            o.cursor_icon = CursorIcon::PointingHand;
        }
    });
    let hot = resp.hovered();
    let (fill, stroke, text) = match kind {
        Kind::Outline => (
            if hot { p.raised } else { Color32::TRANSPARENT },
            Stroke::new(1.0, if hot { p.accent_hover } else { p.border_strong }),
            p.text,
        ),
        Kind::Primary => (
            if hot { p.accent_hover } else { p.accent },
            Stroke::new(1.0, p.accent_hover),
            p.ground,
        ),
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(RADIUS),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    let g = painter.layout_no_wrap(label.to_owned(), semibold(14.0), text);
    painter.galley(rect.center() - g.size() / 2.0, g, text);
    if resp.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(RADIUS + 2),
            Stroke::new(2.0, p.accent_hover),
            StrokeKind::Outside,
        );
    }
    resp
}

/// Overline: small uppercase caption.
pub fn overline(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(semibold(11.0))
            .color(palette().faint),
    );
}

/// A crescent moon of radius `r` at `c`, cut out of `colour` against `ground`.
pub fn paint_moon(painter: &Painter, c: Pos2, r: f32, colour: Color32, ground: Color32) {
    painter.circle_filled(c, r, colour);
    painter.circle_filled(c + vec2(r * 0.45, -r * 0.25), r * 0.85, ground);
}

/// A labelled path box with a button that opens the dialog for `target`.
/// Returns the target when the button was pressed.
pub fn folder_row(
    ui: &mut Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    target: crate::dialogs::Target,
) -> Option<crate::dialogs::Target> {
    overline(ui, label);
    let mut asked = None;
    let row = vec2(ui.available_width(), HIT);
    ui.allocate_ui_with_layout(
        row,
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let word = if target.saves() {
                "Save as..."
            } else {
                "Choose..."
            };
            if button(ui, word, Kind::Outline, 0.0)
                .on_hover_text("Choose with the file dialog")
                .clicked()
            {
                asked = Some(target);
            }
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
        },
    );
    asked
}

/// A pill that switches a mode on or off; `on` is the chosen one.
pub fn chip(ui: &mut Ui, on: bool, label: &str) -> Response {
    pill(ui, on, label, 32.0, 13.0, 28.0)
}

/// A [`chip`] as tall as a button, for the top bar.
pub fn chip_tall(ui: &mut Ui, on: bool, label: &str) -> Response {
    pill(ui, on, label, HIT, 14.0, 36.0)
}

fn pill(ui: &mut Ui, on: bool, label: &str, height: f32, font: f32, pad: f32) -> Response {
    let p = palette();
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), semibold(font), Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + pad, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, label));
    ui.output_mut(|o| {
        if resp.hovered() {
            o.cursor_icon = CursorIcon::PointingHand;
        }
    });
    let hot = resp.hovered();
    let (fill, stroke, text) = if on {
        (p.selected_fill, p.selected_border, p.accent_hover)
    } else {
        (
            if hot { p.raised } else { p.panel },
            if hot { p.accent_hover } else { p.border_strong },
            p.text,
        )
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(RADIUS),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let g = painter.layout_no_wrap(label.to_owned(), semibold(font), text);
    painter.galley(rect.center() - g.size() / 2.0, g, text);
    if resp.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            egui::CornerRadius::same(RADIUS + 2),
            Stroke::new(2.0, p.accent_hover),
            StrokeKind::Outside,
        );
    }
    resp
}
