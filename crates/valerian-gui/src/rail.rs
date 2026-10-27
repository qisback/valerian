// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The workflow rail: the five steps of a deep-sky session, top to bottom.
//!
//! Calibrate, Register, Stack, Process and Export, numbered and joined by a
//! line, each with a status and a one-line hint, as Helia's pipeline rail has.
//! The rail only draws and reports a click; what a step holds is decided by
//! the workspace (`workspace.rs`).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Pos2, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType, pos2, vec2,
};

use crate::theme::{mono, ok_colour, palette, sans, semibold};

/// A step of the workflow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Step {
    /// Dark, flat and bias frames applied to the lights.
    #[default]
    Calibrate,
    /// Stars found and every frame aligned to a reference.
    Register,
    /// Frames combined into one picture (or drizzled).
    Stack,
    /// Stretch, gradient, stars, plate solve and colour.
    Process,
    /// The finished picture written to a file.
    Export,
}

impl Step {
    /// Every step, in workflow order.
    pub const ALL: [Self; 5] = [
        Self::Calibrate,
        Self::Register,
        Self::Stack,
        Self::Process,
        Self::Export,
    ];

    /// Position in the workflow, from 0.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// Name on the rail and in the inspector.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Calibrate => "Calibrate",
            Self::Register => "Register",
            Self::Stack => "Stack",
            Self::Process => "Process",
            Self::Export => "Export",
        }
    }

    /// What the step is for, in a sentence.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Calibrate => {
                "Takes the dark, flat and bias frames out of the lights. Optional when the frames are already calibrated."
            }
            Self::Register => {
                "Finds the stars in every frame and lines the frames up on the best one."
            }
            Self::Stack => {
                "Combines the aligned frames into one picture, rejecting satellites and hot pixels."
            }
            Self::Process => {
                "Stretch the stack, remove the stars, plate solve and calibrate the colour."
            }
            Self::Export => "Writes the finished picture as FITS or as a stretched PNG.",
        }
    }

    /// The hint under the name while the step has nothing to say.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::Calibrate => "Optional: darks, flats, bias",
            Self::Register => "Optional: stack aligns too",
            Self::Stack => "Plain or drizzle",
            Self::Process => "Stretch, stars, solve, colour",
            Self::Export => "FITS or PNG",
        }
    }
}

/// Where a step stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// Nothing asked of it yet.
    #[default]
    Idle,
    /// Ready to run.
    Ready,
    /// Running now.
    Running,
    /// Finished.
    Done,
    /// Waiting for an earlier step or an image.
    Waiting,
}

/// What the rail needs to know of one step.
#[derive(Clone, Debug)]
pub struct Row {
    /// The step.
    pub step: Step,
    /// Its state.
    pub state: State,
    /// The status line: a result, or the state's word.
    pub status: String,
    /// The hint line.
    pub hint: String,
}

/// Height of one row of the rail.
pub const ROW: f32 = 84.0;

/// Draw the rail and return the step clicked, if any. `narrow` shows the
/// numbers only.
pub fn show(ui: &mut Ui, rows: &[Row], current: Option<Step>, narrow: bool) -> Option<Step> {
    let p = palette();
    let mut clicked = None;
    let width = ui.available_width();
    for (i, row) in rows.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, row.step.label()));
        let resp = resp.on_hover_text(format!("{}: {}", row.step.label(), row.step.summary()));
        ui.output_mut(|o| {
            if resp.hovered() {
                o.cursor_icon = CursorIcon::PointingHand;
            }
        });
        if resp.clicked() {
            clicked = Some(row.step);
        }
        let painter = ui.painter();
        let selected = Some(row.step) == current;
        let card = rect.shrink2(vec2(4.0, 3.0));
        if selected || resp.hovered() {
            painter.rect(
                card,
                CornerRadius::same(10),
                if selected { p.selected_fill } else { p.raised },
                Stroke::new(
                    1.0,
                    if selected {
                        p.selected_border
                    } else {
                        p.border
                    },
                ),
                StrokeKind::Inside,
            );
        }
        let cx = card.left() + 24.0;
        let cy = card.top() + 26.0;
        // The line that joins the steps, behind the circles.
        if i > 0 {
            painter.line_segment(
                [pos2(cx, rect.top()), pos2(cx, cy - 15.0)],
                Stroke::new(1.5, p.border_strong),
            );
        }
        if i + 1 < rows.len() {
            painter.line_segment(
                [pos2(cx, cy + 15.0), pos2(cx, rect.bottom())],
                Stroke::new(1.5, p.border_strong),
            );
        }
        circle(painter, pos2(cx, cy), i + 1, row.state, selected);
        if narrow {
            continue;
        }
        let tx = cx + 26.0;
        painter.text(
            pos2(tx, cy - 9.0),
            Align2::LEFT_CENTER,
            row.step.label(),
            semibold(15.0),
            p.text,
        );
        let colour = match row.state {
            State::Done => ok_colour(),
            State::Running | State::Ready => p.accent,
            State::Idle | State::Waiting => p.faint,
        };
        painter.circle_filled(pos2(tx + 4.0, cy + 11.0), 3.5, colour);
        let status = fit(ui, &row.status, &mono(12.0), card.right() - tx - 22.0);
        ui.painter().text(
            pos2(tx + 14.0, cy + 11.0),
            Align2::LEFT_CENTER,
            status,
            mono(12.0),
            if selected { colour } else { p.muted },
        );
        let hint = ui.painter().layout(
            row.hint.clone(),
            sans(12.0),
            p.faint,
            card.right() - tx - 8.0,
        );
        ui.painter().galley(pos2(tx, cy + 24.0), hint, p.faint);
    }
    clicked
}

/// `text` cut with an ellipsis so it fits in `width` points.
fn fit(ui: &Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let measure = |s: &str| {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if measure(text) <= width {
        return text.to_owned();
    }
    let mut s: String = text.to_owned();
    while !s.is_empty() && measure(&format!("{s}...")) > width {
        s.pop();
    }
    format!("{s}...")
}

/// The numbered circle of a step.
fn circle(painter: &egui::Painter, c: Pos2, n: usize, state: State, selected: bool) {
    let p = palette();
    let (fill, ring, ink) = if selected {
        (p.accent, p.accent, p.ground)
    } else if state == State::Done {
        (p.panel, ok_colour(), ok_colour())
    } else {
        (p.panel, p.border_strong, p.muted)
    };
    painter.circle(c, 14.0, fill, Stroke::new(1.5, ring));
    if state == State::Done && !selected {
        // A tick.
        let s = Stroke::new(2.0, ink);
        painter.line_segment([c + vec2(-5.0, 0.5), c + vec2(-1.5, 4.0)], s);
        painter.line_segment([c + vec2(-1.5, 4.0), c + vec2(5.5, -4.0)], s);
    } else {
        painter.text(c, Align2::CENTER_CENTER, n.to_string(), semibold(13.0), ink);
    }
}
