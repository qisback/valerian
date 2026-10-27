// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Colour calibration tool: settings, the result of a run and what the
//! viewer shows of it.
//!
//! The run measures the Gaia stars of the plate solution on the full frame in a
//! background thread. The solution is the image's own WCS or the one the Plate
//! solve tool found. The preview applies the gains and offsets to the reduced
//! copy, so the original and the calibrated picture can be compared at once.

use eframe::egui::{RichText, Slider, Ui};
use valerian_core::photocal::{Calibration, Options};

use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Measure the stars and fit the balance.
    Run,
    /// The view changed.
    Redraw,
    /// Write the calibrated image.
    Save,
}

/// What the viewer shows once there is a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The image with the balance applied.
    Calibrated,
    /// The image as opened.
    Original,
}

/// The settings the panel edits, and the last result.
#[derive(Clone, Debug)]
pub struct ColourTool {
    /// Options for the run.
    pub opts: Options,
    /// What the viewer shows.
    pub view: View,
    /// The last calibration.
    pub result: Option<Calibration>,
}

impl Default for ColourTool {
    fn default() -> Self {
        Self {
            opts: Options::default(),
            view: View::Calibrated,
            result: None,
        }
    }
}

impl ColourTool {
    /// The controls. `can_run` when the image has three planes; `busy` while a
    /// run is going.
    pub fn controls(&mut self, ui: &mut Ui, can_run: bool, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        ui.label(
            RichText::new(
                "Sets the white balance from the colours Gaia measured for the stars in the \
                 picture, and makes the sky grey. Needs a linear colour image and a plate \
                 solution (run Plate solve first if the file has none).",
            )
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Stars");
        let o = &mut self.opts;
        ui.add_enabled_ui(!busy, |ui| {
            ui.add(
                Slider::new(&mut o.reference_colour, 0.3..=1.5)
                    .text("Neutral at BP-RP")
                    .fixed_decimals(2),
            )
            .on_hover_text("0.82 is a Sun-like star; a bluer value gives a warmer picture");
            ui.add(Slider::new(&mut o.min_mag, 4.0..=10.0).text("Brightest (G)"));
            ui.add(Slider::new(&mut o.max_mag, 8.0..=11.0).text("Faintest (G)"));
            ui.add(Slider::new(&mut o.aperture, 1.0..=4.0).text("Aperture (FWHM)"));
            ui.checkbox(&mut o.neutralise_background, "Make the background neutral");
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(can_run && !busy, |ui| {
            if widgets::button(
                ui,
                if busy {
                    "Measuring..."
                } else {
                    "Calibrate colour"
                },
                Kind::Outline,
                0.0,
            )
            .on_hover_text("Measure the Gaia stars and fit the white balance")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if !can_run {
            ui.label(
                RichText::new("This image is not a three-plane colour image.")
                    .small()
                    .color(p.muted),
            );
        }
        if let Some(r) = &self.result {
            ui.separator();
            widgets::overline(ui, "Result");
            let lines = [
                format!("{} of {} stars used", r.used, r.stars.len()),
                format!("gains R {:.4}  G 1  B {:.4}", r.gains[0], r.gains[2]),
                format!(
                    "red {:+.2}, blue {:+.2} mag per BP-RP",
                    r.lines[0].slope, r.lines[1].slope
                ),
                format!(
                    "scatter {:.3} and {:.3} mag",
                    r.lines[0].rms, r.lines[1].rms
                ),
                format!(
                    "sky {:.0} {:.0} {:.0}, after {:.0}",
                    r.sky_before[0], r.sky_before[1], r.sky_before[2], r.sky_after[0]
                ),
            ];
            for l in lines {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
            for w in &r.warnings {
                ui.label(RichText::new(w).small().color(p.muted));
            }
            ui.horizontal_wrapped(|ui| {
                for (v, label) in [
                    (View::Calibrated, "Calibrated"),
                    (View::Original, "Original"),
                ] {
                    if ui.selectable_value(&mut self.view, v, label).changed() {
                        ask = Ask::Redraw;
                    }
                }
            });
            ui.label(
                RichText::new("The stars used are circled in the viewer.")
                    .small()
                    .color(p.muted),
            );
            ui.add_enabled_ui(!busy, |ui| {
                if widgets::button(ui, "Save calibrated", Kind::Outline, 0.0)
                    .on_hover_text("Write the calibrated image to a new FITS file")
                    .clicked()
                {
                    ask = Ask::Save;
                }
            });
        }
        ask
    }
}
