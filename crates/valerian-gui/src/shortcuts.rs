// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The keyboard shortcuts: one table, shown by the help overlay (F1 or ?) and
//! checked against `docs/book/src/reference/gui.md` by a test, so the two cannot drift.
//!
//! What each key does is decided in `app.rs` (`App::shortcuts`); this holds the
//! words and draws the overlay.

use eframe::egui::{self, Align2, Context, RichText, Ui, vec2};

use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// One shortcut.
pub struct Shortcut {
    /// The keys, as written in the manual.
    pub keys: &'static str,
    /// What happens.
    pub does: &'static str,
}

/// A titled group of shortcuts.
pub struct Group {
    /// The heading.
    pub title: &'static str,
    /// The rows.
    pub rows: &'static [Shortcut],
}

const fn s(keys: &'static str, does: &'static str) -> Shortcut {
    Shortcut { keys, does }
}

/// Every shortcut, by group.
pub const GROUPS: &[Group] = &[
    Group {
        title: "Files",
        rows: &[
            s("Ctrl+O", "Open a FITS image"),
            s("Ctrl+Shift+O", "Choose a folder of frames to sort"),
        ],
    },
    Group {
        title: "Steps",
        rows: &[
            s("Ctrl+Enter", "Run the open step or tool"),
            s("F5", "Run the open step or tool"),
            s("Esc", "Cancel the running step, or drop a sorting review"),
            s("Enter", "Accept the sorting while it is being reviewed"),
            s("Alt+Right", "Next step"),
            s("Alt+Left", "Previous step"),
        ],
    },
    Group {
        title: "Picture",
        rows: &[
            s("+", "Zoom in (also =)"),
            s("-", "Zoom out"),
            s("0", "Fit the picture to the window"),
            s("1", "Show the picture at 100 percent"),
            s("F2", "Night vision on or off"),
            s("N", "Night vision on or off"),
        ],
    },
    Group {
        title: "Help",
        rows: &[
            s("F1", "This list (Esc closes it)"),
            s("?", "This list"),
            s("Ctrl+F1", "The About page"),
        ],
    },
];

/// What the user did in the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Nothing.
    Nothing,
    /// Close the overlay.
    Close,
    /// Close it and show the About page.
    About,
}

fn key_chip(ui: &mut Ui, keys: &str) {
    let p = palette();
    egui::Frame::new()
        .fill(p.raised)
        .stroke(egui::Stroke::new(1.0, p.border_strong))
        .corner_radius(egui::CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).font(mono(13.0)).color(p.accent_hover));
        });
}

/// The overlay: every shortcut, grouped.
pub fn window(ctx: &Context) -> Act {
    let p = palette();
    let mut act = Act::Nothing;
    egui::Window::new("Keyboard shortcuts")
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .collapsible(false)
        .resizable(false)
        .default_width(820.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.columns(2, |cols| {
                for (c, ui) in cols.iter_mut().enumerate() {
                    for g in GROUPS.iter().skip(c * 2).take(2) {
                        widgets::overline(ui, g.title);
                        egui::Grid::new(g.title)
                            .num_columns(2)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                for r in g.rows {
                                    key_chip(ui, r.keys);
                                    ui.label(RichText::new(r.does).color(p.text));
                                    ui.end_row();
                                }
                            });
                        ui.add_space(4.0);
                    }
                }
            });
            ui.label(
                RichText::new(
                    "Drop a folder of frames on the window to sort it into lights, darks, flats and bias.",
                )
                .small()
                .color(p.muted),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, "Close", Kind::Primary, 0.0).clicked() {
                    act = Act::Close;
                }
                if widgets::button(ui, "About Valerian", Kind::Outline, 0.0).clicked() {
                    act = Act::About;
                }
            });
        });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manual_lists_every_shortcut() {
        let doc = include_str!("../../../docs/book/src/reference/gui.md");
        for g in GROUPS {
            for r in g.rows {
                assert!(
                    doc.contains(&format!("| `{}` |", r.keys)),
                    "reference/gui.md has no row for `{}`",
                    r.keys
                );
            }
        }
    }

    #[test]
    fn no_key_is_listed_twice_for_different_things() {
        let mut seen = std::collections::HashSet::new();
        for g in GROUPS {
            for r in g.rows {
                assert!(seen.insert(r.keys), "{} is listed twice", r.keys);
            }
        }
    }
}
