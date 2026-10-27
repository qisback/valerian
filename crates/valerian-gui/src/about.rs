// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The About page: dedication first, then the version and build record,
//! copyright, licence, additional terms, links and third-party licences. It
//! replaces the workspace while open (Ctrl+F1, the moon in the top bar, or Esc to
//! close). Nothing here touches the network; a link is only opened, in the
//! system browser, when it is clicked, and the GPU is only opened when asked.

use std::path::PathBuf;

use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, Ui, vec2};
use valerian_core::about;

use crate::app::App;
use crate::theme::{RADIUS_LARGE, mono, palette, semibold};
use crate::widgets::{self, Kind};

/// The version the GUI shows. Under `cargo test` it is a fixed string, so a
/// release bump never changes a snapshot.
#[must_use]
pub(crate) fn ui_version() -> &'static str {
    if cfg!(test) {
        "0.3.0"
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

/// Where the third-party licence list ships: next to the executable.
const THIRD_PARTY_FILE: &str = "THIRD-PARTY-LICENSES.txt";

fn third_party_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(THIRD_PARTY_FILE);
    p.is_file().then_some(p)
}

impl App {
    /// The text the "Copy version info" button puts on the clipboard.
    pub(crate) fn version_report(&self) -> String {
        about::version_block(
            "valerian-gui",
            ui_version(),
            valerian_gpu::WGPU_VERSION,
            self.gpu_text.as_deref(),
        )
    }

    /// Draw the page in `ui` (the centre panel).
    pub(crate) fn about_page(&mut self, ui: &mut Ui) {
        let p = palette();
        let report = self.version_report();
        let mut close = false;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(680.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("About Valerian")
                            .font(semibold(22.0))
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Close", Kind::Outline, 0.0)
                            .on_hover_text("Back to the workspace (Esc)")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(6.0);

                // The dedication comes first.
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(RADIUS_LARGE)
                    .inner_margin(Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
                        widgets::paint_moon(ui.painter(), rect.center(), 15.0, p.accent, p.panel);
                        ui.add_space(6.0);
                        widgets::overline(ui, "Dedication");
                        ui.add(
                            egui::Label::new(
                                RichText::new(about::DEDICATION_FULL)
                                    .font(semibold(17.0))
                                    .color(p.text),
                            )
                            .wrap(),
                        );
                    });

                ui.add_space(6.0);
                widgets::overline(ui, "Version");
                ui.label(
                    RichText::new(format!("Valerian {}", ui_version()))
                        .font(semibold(18.0))
                        .color(p.text),
                );
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(RADIUS_LARGE)
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(&report).font(mono(12.5)).color(p.muted),
                            )
                            .selectable(true),
                        );
                    });
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Copy version info", Kind::Outline, 0.0)
                        .on_hover_text("Copy the lines above for a bug report")
                        .clicked()
                    {
                        ui.ctx().copy_text(report.clone());
                    }
                    if self.gpu_text.is_none()
                        && widgets::button(ui, "Detect GPU", Kind::Outline, 0.0)
                            .on_hover_text("Open the GPU adapter and add it to the lines above")
                            .clicked()
                    {
                        self.gpu_text = Some(valerian_gpu::describe(
                            valerian_gpu::AdapterRequest::Hardware,
                        ));
                    }
                });

                ui.add_space(6.0);
                widgets::overline(ui, "Copyright and licence");
                ui.label(
                    RichText::new(about::COPYRIGHT)
                        .font(mono(13.0))
                        .color(p.text),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("More from {}:", about::AUTHOR)).color(p.muted));
                    ui.hyperlink_to("12hobbies.studio", about::HOBBIES_URL);
                });
                ui.label(
                    RichText::new(format!("Licence: {}", about::LICENCE_ID))
                        .font(mono(13.0))
                        .color(p.muted),
                );
                ui.label(RichText::new(about::LICENCE_NOTICE).color(p.text));
                ui.label(RichText::new(about::WARRANTY_NOTICE).color(p.muted));
                ui.label(RichText::new(about::ADDITIONAL_TERMS_NOTICE).color(p.muted));
                ui.hyperlink_to("Additional terms", about::ADDITIONAL_TERMS_URL);

                ui.add_space(6.0);
                widgets::overline(ui, "Links");
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Source code", about::SOURCE_URL);
                    ui.hyperlink_to("Licence text", about::LICENCE_URL);
                    ui.hyperlink_to("Releases", format!("{}/releases", about::SOURCE_URL));
                });

                ui.add_space(6.0);
                widgets::overline(ui, "Third-party licences");
                self.third_party(ui);
            });
        if close {
            self.show_about = false;
        }
    }

    fn third_party(&mut self, ui: &mut Ui) {
        let p = palette();
        if let Some(path) = third_party_path() {
            let label = if self.show_third_party {
                "Hide third-party licences"
            } else {
                "View third-party licences"
            };
            if widgets::button(ui, label, Kind::Outline, 0.0).clicked() {
                self.show_third_party = !self.show_third_party;
                self.third_party_text = None;
            }
            if self.show_third_party {
                let text = self.third_party_text.get_or_insert_with(|| {
                    std::fs::read_to_string(&path)
                        .unwrap_or_else(|e| format!("Cannot read {}: {e}", path.display()))
                });
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text.as_str()).font(mono(11.5)).color(p.muted),
                        )
                        .selectable(true),
                    );
                });
            }
        } else {
            ui.label(
                RichText::new(format!(
                    "The list of third-party licences ships as {THIRD_PARTY_FILE} next to the \
                     program in release archives. This build has none beside it."
                ))
                .color(p.muted),
            );
            ui.hyperlink_to("Releases", format!("{}/releases", about::SOURCE_URL));
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn page() -> Harness<'static, App> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(760.0, 900.0))
            .build_eframe(|cc| App::new(cc, None));
        h.state_mut().show_about = true;
        h
    }

    #[test]
    fn the_dedication_is_first_and_the_notices_are_all_there() {
        let mut h = page();
        h.run();
        h.get_by_label_contains("Valerian is dedicated to my son Xaviar.");
        h.get_by_label_contains("His middle name is Valerian, because he was a very sleepy child");
        h.get_by_label_contains("Copyright (C) 2026 Laurence Horrocks-Barlow");
        h.get_by_label_contains("Valerian is free software: you can redistribute it");
        h.get_by_label_contains("WITHOUT ANY WARRANTY");
        h.get_by_label_contains("Additional terms under section 7");
        h.get_by_label("12hobbies.studio");
        h.get_by_label_contains("More from Laurence Horrocks-Barlow");
        h.get_by_label_contains("Valerian 0.3.0");
        h.get_by_label("Close");
        // Nothing opens the GPU until asked.
        h.get_by_label("Detect GPU");
    }

    #[test]
    fn the_dedication_is_above_the_version() {
        let mut h = page();
        h.run();
        let dedication = h
            .get_by_label_contains("Valerian is dedicated to my son Xaviar.")
            .rect();
        let version = h.get_by_label_contains("Valerian 0.3.0").rect();
        assert!(dedication.min.y < version.min.y);
    }

    #[test]
    fn closing_returns_to_the_workspace() {
        let mut h = page();
        h.run();
        h.get_by_label("Close").click();
        h.run();
        assert!(!h.state().show_about);
    }
}
