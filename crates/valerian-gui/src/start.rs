// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The start screen: what the viewer shows while no image is open.
//!
//! Besides the hint to open or drop something it lists the sessions (sets of frame
//! folders) and the images worked on lately, from the settings file (`prefs.rs`).
//! Drawing returns what the user asked for; the workspace carries it out.

use std::path::PathBuf;

use eframe::egui::{self, Frame, Layout, Margin, RichText, Ui, vec2};

use crate::prefs::{FolderSet, Prefs};
use crate::theme::{error_colour, mono, palette, semibold};
use crate::widgets::{self, Kind};

/// What the user did on the start screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Nothing.
    Nothing,
    /// Restore the folders of a session into the step panels.
    Session(FolderSet),
    /// Open a recent image.
    Image(PathBuf),
    /// Forget one session (by its lights folder).
    ForgetSession(String),
    /// Forget one recent image.
    ForgetImage(PathBuf),
    /// Forget every session and image.
    ClearAll,
}

/// The lines that tell the user how to start, and the lists of recent work.
pub fn show(ui: &mut Ui, prefs: &Prefs, export: bool) -> Pick {
    let p = palette();
    let mut pick = Pick::Nothing;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(if prefs.sessions.is_empty() && prefs.recent.is_empty() {
                    ui.available_height() * 0.35
                } else {
                    28.0
                });
                let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), egui::Sense::hover());
                widgets::paint_moon(ui.painter(), rect.center(), 20.0, p.accent, p.ground);
                ui.label(
                    RichText::new("No image open")
                        .font(semibold(18.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(if export {
                        "Open or stack an image to export it."
                    } else {
                        "Press Open (Ctrl+O) to choose a FITS file, or drop one here. \
                         Drop a folder of frames to sort it into lights, darks, flats and bias. \
                         Calibrate, Register and Stack on the left turn frames into one. \
                         Press F1 or ? for the keyboard shortcuts."
                    })
                    .color(p.muted),
                );
            });
            if export || (prefs.sessions.is_empty() && prefs.recent.is_empty()) {
                return;
            }
            ui.add_space(20.0);
            let width = ui.available_width().min(760.0);
            ui.vertical_centered(|ui| {
                ui.allocate_ui(vec2(width, 0.0), |ui| {
                    ui.set_width(width);
                    ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                        if !prefs.sessions.is_empty() {
                            widgets::overline(ui, "Recent sessions");
                            for s in &prefs.sessions {
                                let r = session_row(ui, s);
                                if r != Pick::Nothing {
                                    pick = r;
                                }
                                ui.add_space(6.0);
                            }
                        }
                        if !prefs.recent.is_empty() {
                            ui.add_space(8.0);
                            widgets::overline(ui, "Recent images");
                            for f in &prefs.recent {
                                let r = image_row(ui, f);
                                if r != Pick::Nothing {
                                    pick = r;
                                }
                                ui.add_space(4.0);
                            }
                        }
                        ui.add_space(8.0);
                        if widgets::chip(ui, false, "Clear recent sessions and images")
                            .on_hover_text("Forget the lists above (the folders and files stay)")
                            .clicked()
                        {
                            pick = Pick::ClearAll;
                        }
                    });
                });
            });
        });
    pick
}

fn session_row(ui: &mut Ui, s: &FolderSet) -> Pick {
    let p = palette();
    let mut pick = Pick::Nothing;
    let missing = s.missing();
    let lights_gone = missing.contains(&"lights");
    Frame::new()
        .fill(p.panel)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(s.title()).font(semibold(15.0)).color(p.text));
                    let kinds: Vec<&str> = s.named().into_iter().map(|(k, _)| k).collect();
                    ui.label(
                        RichText::new(kinds.join(", "))
                            .font(mono(12.0))
                            .color(p.muted),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(&s.lights).font(mono(11.5)).color(p.faint))
                            .truncate(),
                    );
                    if !missing.is_empty() {
                        ui.label(
                            RichText::new(format!("Not found: {}", missing.join(", ")))
                                .font(mono(12.0))
                                .color(error_colour()),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::chip(ui, false, "Forget")
                        .on_hover_text("Remove this session from the list")
                        .clicked()
                    {
                        pick = Pick::ForgetSession(s.lights.clone());
                    }
                    ui.add_enabled_ui(!lights_gone, |ui| {
                        if widgets::button(ui, "Open session", Kind::Primary, 0.0)
                            .on_hover_text(if lights_gone {
                                "The lights folder is gone; forget this session"
                            } else {
                                "Fill the Calibrate, Register and Stack folders from this session"
                            })
                            .clicked()
                        {
                            pick = Pick::Session(s.clone());
                        }
                    });
                });
            });
        });
    pick
}

fn image_row(ui: &mut Ui, f: &std::path::Path) -> Pick {
    let p = palette();
    let mut pick = Pick::Nothing;
    let gone = !f.exists();
    ui.horizontal(|ui| {
        let name = f.file_name().map_or_else(
            || f.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        ui.add_enabled_ui(!gone, |ui| {
            if widgets::button(ui, &name, Kind::Outline, 0.0)
                .on_hover_text(f.display().to_string())
                .clicked()
            {
                pick = Pick::Image(f.to_owned());
            }
        });
        if gone {
            ui.label(
                RichText::new("Not found")
                    .font(mono(12.0))
                    .color(error_colour()),
            );
        } else if let Some(d) = f.parent() {
            ui.add(
                egui::Label::new(
                    RichText::new(d.display().to_string())
                        .font(mono(11.5))
                        .color(p.faint),
                )
                .truncate(),
            );
        }
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::chip(ui, false, "Forget")
                .on_hover_text("Remove this image from the list")
                .clicked()
            {
                pick = Pick::ForgetImage(f.to_owned());
            }
        });
    });
    pick
}
