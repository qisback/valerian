// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Drizzle tool: a folder of dithered frames, the drop settings, and the
//! result.
//!
//! Unlike the other tools it does not work on the open image but on a whole
//! folder of calibrated frames (raw colour mosaics are fine), so it is usable
//! with nothing open. A run registers and drizzles the folder in a background
//! thread and shows the result, reduced, in the viewer; the result can be saved
//! or opened as the working image for stretching and the other tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{RichText, Slider, TextEdit, Ui};
use valerian_core::drizzle::{Drizzled, Options, drizzle};
use valerian_core::fits::{self, Image};
use valerian_core::preview::downsample;
use valerian_core::stack::StackOptions;

use crate::pipeline::{Progress, failure};
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Drizzle the folder.
    Run,
    /// Write the result.
    Save,
    /// Make the result the open image.
    Open,
    /// Choose the folder with the file dialog.
    Browse,
}

/// A finished run: the result and a reduced copy for the viewer.
#[derive(Clone, Debug)]
pub struct DrizzleRun {
    /// The full result, shared with the save thread.
    pub drizzled: Arc<Drizzled>,
    /// The reduced copy shown in the viewer.
    pub small: Image,
    /// Frames found in the folder.
    pub found: usize,
}

impl DrizzleRun {
    /// Reduce `drizzled` to no more than `side` pixels for the viewer.
    #[must_use]
    pub fn new(drizzled: Drizzled, found: usize, side: usize) -> Self {
        let small = downsample(&drizzled.image, side);
        Self {
            drizzled: Arc::new(drizzled),
            small,
            found,
        }
    }
}

/// The settings the panel edits, and the last result.
#[derive(Clone, Debug)]
pub struct DrizzleTool {
    /// The folder of frames.
    pub folder: String,
    /// Output pixels per input pixel.
    pub scale: f64,
    /// Drop size as a fraction of an input pixel.
    pub pixfrac: f64,
    /// Reject outliers in a second pass.
    pub reject: bool,
    /// The rejection limit, sigmas.
    pub sigma: f64,
    /// The last run.
    pub result: Option<DrizzleRun>,
}

impl Default for DrizzleTool {
    fn default() -> Self {
        let o = Options::default();
        Self {
            folder: String::new(),
            scale: o.scale,
            pixfrac: o.pixfrac,
            reject: true,
            sigma: o.reject.unwrap_or(4.0),
            result: None,
        }
    }
}

/// The FITS files of `dir`, sorted.
///
/// # Errors
/// The folder cannot be read or has no FITS files.
pub fn frame_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Cannot read the folder {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts")
                })
        })
        .collect();
    if files.is_empty() {
        return Err(format!("{} holds no FITS files.", dir.display()));
    }
    files.sort();
    Ok(files)
}

impl DrizzleTool {
    /// The drizzle settings for a run.
    #[must_use]
    pub fn options(&self) -> Options {
        Options {
            scale: self.scale,
            pixfrac: self.pixfrac,
            reject: self.reject.then_some(self.sigma),
        }
    }

    /// Read the folder and drizzle it. Runs on the worker thread.
    ///
    /// # Errors
    /// The folder, a file or the drizzle fails, with the reason.
    pub fn run(
        folder: &str,
        opts: &Options,
        side: usize,
        progress: &Progress,
    ) -> Result<DrizzleRun, String> {
        let files = frame_files(Path::new(folder.trim()))?;
        progress.set_total(files.len());
        progress.say(&format!("Reading {} frames...", files.len()));
        let frames: Vec<Image> = files
            .iter()
            .map(|p| {
                progress.check()?;
                let img = fits::read(p).map_err(|e| format!("Cannot read {}: {e}", p.display()))?;
                progress.tick();
                Ok(img)
            })
            .collect::<Result<_, String>>()?;
        let stack = StackOptions {
            cancel: progress.token(),
            ..StackOptions::default()
        };
        let d = drizzle(&frames, &stack, opts, &mut |line| progress.say(line))
            .map_err(|e| failure(&e))?;
        Ok(DrizzleRun::new(d, frames.len(), side))
    }

    /// The controls. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        ui.label(
            RichText::new(
                "Combines dithered frames onto a finer grid, which keeps detail a plain \
                 stack loses when the stars are only a pixel or two wide. Needs calibrated \
                 frames with a dither of a pixel or more between them, a dozen or more \
                 for a 2x grid; raw colour mosaics are dropped without interpolation.",
            )
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Frames");
        ui.add_enabled_ui(!busy, |ui| {
            ui.add(
                TextEdit::singleline(&mut self.folder)
                    .hint_text("Folder of calibrated FITS frames")
                    .desired_width(f32::INFINITY),
            );
            if widgets::button(ui, "Choose folder...", Kind::Outline, 0.0)
                .on_hover_text("Choose the folder of frames with the file dialog")
                .clicked()
            {
                ask = Ask::Browse;
            }
            widgets::overline(ui, "Drops");
            ui.add(
                Slider::new(&mut self.scale, 1.0..=4.0)
                    .step_by(0.5)
                    .text("Scale"),
            )
            .on_hover_text("Output pixels per input pixel, each way");
            ui.add(
                Slider::new(&mut self.pixfrac, 0.2..=1.0)
                    .text("Pixfrac")
                    .fixed_decimals(2),
            )
            .on_hover_text(
                "Drop size as a fraction of a pixel: smaller is sharper but needs more frames",
            );
            ui.checkbox(
                &mut self.reject,
                "Reject outliers (cosmic rays, hot pixels)",
            );
            ui.add_enabled(
                self.reject,
                Slider::new(&mut self.sigma, 2.0..=8.0).text("Rejection (sigma)"),
            );
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(!busy, |ui| {
            if widgets::button(
                ui,
                if busy {
                    "Drizzling..."
                } else {
                    "Drizzle frames"
                },
                Kind::Outline,
                0.0,
            )
            .on_hover_text("Register the frames in the folder and drizzle them")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            ui.separator();
            widgets::overline(ui, "Result");
            let d = &r.drizzled;
            let out = &d.image.planes[0];
            let lines = [
                format!("{} of {} frames drizzled", d.used(), r.found),
                format!(
                    "{} x {} px, {} plane{}",
                    out.width,
                    out.height,
                    d.image.planes.len(),
                    if d.image.planes.len() == 1 { "" } else { "s" }
                ),
                format!("scale {}, pixfrac {:.2}", d.scale, d.pixfrac),
                format!(
                    "{:.2} % rejected, {:.3} % no data",
                    100.0 * d.rejected,
                    100.0 * d.holes()
                ),
            ];
            for l in lines {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
            if d.holes() > 0.01 {
                ui.label(
                    RichText::new(
                        "Gaps in the output: use a larger pixfrac or a smaller scale, or more frames.",
                    )
                    .small()
                    .color(p.muted),
                );
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    if widgets::button(ui, "Save drizzled", Kind::Outline, 0.0)
                        .on_hover_text("Write the drizzled image to a FITS file")
                        .clicked()
                    {
                        ask = Ask::Save;
                    }
                    if widgets::button(ui, "Open it", Kind::Outline, 0.0)
                        .on_hover_text("Make the drizzled image the open image, to stretch it")
                        .clicked()
                    {
                        ask = Ask::Open;
                    }
                });
            });
        }
        ask
    }
}
