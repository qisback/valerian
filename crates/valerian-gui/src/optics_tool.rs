// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Optics tool: what the stars of the open image say about the telescope.
//!
//! The stars are measured on a worker thread and every plot is drawn there;
//! the viewer then shows the aberration inspector, the FWHM and eccentricity
//! maps, the corner and centre FWHM, or the stacked star profile with its
//! Moffat fit, and the panel gives the numbers and the tilt and backfocus hint
//! in words.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, ColorImage, DragValue, RichText, ScrollArea, TextureHandle, TextureOptions, Ui,
};
use valerian_core::fits::Image;
use valerian_science::optics::{self, Field, PsfFit, Quantity, Tilt};
use valerian_science::optics_report::{self, Params};
use valerian_science::report::ReportDir;

use crate::pipeline::{Progress, hint};
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Measure the stars and draw the plots.
    Run,
    /// Write the plots, CSV, JSON and report.
    Save,
}

/// One of the plots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum View {
    /// The 3 by 3 mosaic of crops.
    #[default]
    Inspector,
    /// FWHM across the field.
    Fwhm,
    /// Eccentricity across the field.
    Ellipticity,
    /// FWHM of the centre and corners.
    Tilt,
    /// The stacked radial profile.
    Psf,
}

impl View {
    /// Every view, in the order of the chips.
    pub const ALL: [Self; 5] = [
        Self::Inspector,
        Self::Fwhm,
        Self::Ellipticity,
        Self::Tilt,
        Self::Psf,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Fwhm => "FWHM map",
            Self::Ellipticity => "Eccentricity map",
            Self::Tilt => "Corners",
            Self::Psf => "Star profile",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// A drawn plot.
#[derive(Clone)]
pub struct Plot {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

/// What a measuring run found.
#[derive(Clone)]
pub struct OpticsRun {
    /// The image measured.
    pub image: Arc<Image>,
    /// The stars.
    pub field: Arc<Field>,
    /// The tilt and backfocus numbers and hints.
    pub tilt: Tilt,
    /// The profile fit, if enough isolated stars.
    pub psf: Option<PsfFit>,
    plots: Vec<Option<Plot>>,
}

/// The Optics panel.
pub struct OpticsTool {
    /// Inspector crop, pixels.
    pub crop: usize,
    /// Inspector zoom.
    pub zoom: usize,
    /// Degree of the maps' surface.
    pub order: usize,
    /// Stars stacked in the profile.
    pub psf_stars: usize,
    /// Smallest peak-to-noise of a measured star.
    pub min_snr: f64,
    /// The plot shown.
    pub view: View,
    /// Where the report is written.
    pub report: String,
    /// The last run.
    pub result: Option<OpticsRun>,
    textures: Vec<Option<TextureHandle>>,
}

impl Default for OpticsTool {
    fn default() -> Self {
        let p = Params::default();
        Self {
            crop: p.crop,
            zoom: p.zoom,
            order: p.order,
            psf_stars: p.psf_stars,
            min_snr: 10.0,
            view: View::default(),
            report: String::new(),
            result: None,
            textures: vec![None; View::ALL.len()],
        }
    }
}

fn plot(rgb: Result<Vec<u8>, valerian_science::Error>, size: (u32, u32)) -> Option<Plot> {
    rgb.ok().map(|rgb| Plot {
        width: size.0 as usize,
        height: size.1 as usize,
        rgb,
    })
}

impl OpticsTool {
    fn params(&self) -> Params {
        Params {
            crop: self.crop,
            zoom: self.zoom,
            order: self.order,
            psf_stars: self.psf_stars,
        }
    }

    /// A tool with these settings and nothing else, to move into the worker thread.
    #[must_use]
    pub fn for_run(&self) -> Self {
        Self {
            crop: self.crop,
            zoom: self.zoom,
            order: self.order,
            psf_stars: self.psf_stars,
            min_snr: self.min_snr,
            ..Self::default()
        }
    }

    /// Measure and draw everything. Runs on the worker thread.
    ///
    /// # Errors
    /// Too few stars, or an image too small for the crops.
    pub fn run(&self, image: &Arc<Image>, progress: &Progress) -> Result<OpticsRun, String> {
        progress.say("Measuring the stars...");
        let field = optics::measure_field(image, 5.0, self.min_snr).map_err(|e| e.to_string())?;
        progress.check()?;
        progress.say("Drawing the maps and the inspector...");
        let p = self.params();
        let insp =
            optics::inspector(image, Some(&field), p.crop, p.zoom).map_err(|e| e.to_string())?;
        let tilt = optics::tilt(&field);
        let psf = optics::psf(image, &field, p.psf_stars).ok();
        let plots = vec![
            Some(Plot {
                width: insp.width as usize,
                height: insp.height as usize,
                rgb: insp.rgb,
            }),
            plot(
                optics::map_rgb(&field, Quantity::Fwhm, p.order),
                optics::MAP_SIZE,
            ),
            plot(
                optics::map_rgb(&field, Quantity::Ellipticity, p.order),
                optics::MAP_SIZE,
            ),
            plot(optics::tilt_rgb(&tilt), optics::TILT_SIZE),
            psf.as_ref()
                .and_then(|s| plot(optics::psf_rgb(s), optics::PSF_SIZE)),
        ];
        Ok(OpticsRun {
            image: Arc::clone(image),
            field: Arc::new(field),
            tilt,
            psf: psf.map(|s| s.fit),
            plots,
        })
    }

    /// Take a finished run.
    pub fn accept(&mut self, run: OpticsRun, image_path: &Path) {
        if self.report.trim().is_empty() {
            self.report = image_path
                .parent()
                .map_or_else(
                    || PathBuf::from("optics-report"),
                    |p| p.join("optics-report"),
                )
                .to_string_lossy()
                .into_owned();
        }
        self.textures = vec![None; View::ALL.len()];
        self.result = Some(run);
    }

    /// Write the report folder.
    ///
    /// # Errors
    /// The folder cannot be written or a plot cannot be drawn.
    pub fn save(&self) -> Result<PathBuf, String> {
        let r = self.result.as_ref().ok_or("Measure the stars first.")?;
        let dir = self.report.trim();
        if dir.is_empty() {
            return Err("Name a folder for the report.".into());
        }
        let mut report = ReportDir::open(Path::new(dir)).map_err(|e| e.to_string())?;
        optics_report::write_report(&mut report, &r.image, &r.field, &self.params())
            .map_err(|e| e.to_string())?;
        Ok(Path::new(dir).join("report.html"))
    }

    /// The controls. `busy` while a run is going; `have_image` when one is open.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        hint(
            ui,
            "Measures the stars and shows what they say about the optics: an aberration \
             inspector of the corners, edges and centre, FWHM and eccentricity maps across \
             the field, a tilt and backfocus hint, and the star profile with a Moffat fit.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            widgets::overline(ui, "Settings");
            ui.horizontal(|ui| {
                ui.label("Crop (px)");
                ui.add(DragValue::new(&mut self.crop).range(32..=512).speed(2));
                ui.label("Zoom");
                ui.add(DragValue::new(&mut self.zoom).range(1..=6));
            });
            ui.horizontal(|ui| {
                ui.label("Map degree");
                ui.add(DragValue::new(&mut self.order).range(1..=4));
                ui.label("Profile stars");
                ui.add(DragValue::new(&mut self.psf_stars).range(8..=200));
            });
            ui.horizontal(|ui| {
                ui.label("Smallest star S/N");
                ui.add(
                    DragValue::new(&mut self.min_snr)
                        .range(3.0..=200.0)
                        .speed(0.5),
                );
            });
            if widgets::button(
                ui,
                if busy {
                    "Measuring..."
                } else {
                    "Measure the stars"
                },
                Kind::Primary,
                0.0,
            )
            .on_hover_text("Detect the stars of the open image and draw the plots")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        let Some(r) = &self.result else {
            return ask;
        };
        widgets::overline(ui, "Plot");
        ui.horizontal_wrapped(|ui| {
            for v in View::ALL {
                if r.plots[v.index()].is_some()
                    && widgets::chip(ui, self.view == v, v.label()).clicked()
                {
                    self.view = v;
                }
            }
        });
        widgets::overline(ui, "Zones");
        ui.label(
            RichText::new(format!(
                "{} stars, median FWHM {:.2} px, eccentricity {:.3}",
                r.field.stars.len(),
                r.field.median_fwhm(),
                r.field.median_ellipticity()
            ))
            .font(mono(12.0))
            .color(p.text),
        );
        for z in &r.tilt.zones {
            ui.label(
                RichText::new(format!(
                    "{:<13} {:>3} stars  {}  {}",
                    z.name,
                    z.stars,
                    z.fwhm.map_or("   -  ".to_owned(), |v| format!("{v:.2} px")),
                    z.ellipticity
                        .map_or("  -  ".to_owned(), |v| format!("e {v:.2}"))
                ))
                .font(mono(12.0))
                .color(p.muted),
            );
        }
        if let Some(s) = &r.psf {
            ui.label(
                RichText::new(format!(
                    "Moffat FWHM {:.2} px, Gaussian {:.2} px",
                    s.fwhm_moffat, s.fwhm_gauss
                ))
                .font(mono(12.0))
                .color(p.muted),
            );
        }
        widgets::overline(ui, "Hint");
        for h in &r.tilt.hints {
            ui.label(RichText::new(h).small().color(p.text));
        }
        widgets::overline(ui, "Report");
        ui.add(
            egui::TextEdit::singleline(&mut self.report)
                .hint_text("Folder for the plots, CSV, JSON and report.html")
                .desired_width(f32::INFINITY),
        );
        if widgets::button(ui, "Save report", Kind::Outline, 0.0)
            .on_hover_text("Write the plots, their data and report.html")
            .clicked()
        {
            ask = Ask::Save;
        }
        ask
    }

    /// Draw the chosen plot into the viewer.
    pub fn show(&mut self, ui: &mut Ui) {
        let p = palette();
        let Some(r) = &self.result else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(
                    RichText::new("No stars measured yet")
                        .size(18.0)
                        .color(p.text),
                );
                ui.label(
                    RichText::new("Open an image and press Measure the stars.").color(p.muted),
                );
            });
            return;
        };
        let k = self.view.index();
        let Some(plot) = r.plots.get(k).and_then(Option::as_ref) else {
            ui.label("This plot could not be drawn for this image.");
            return;
        };
        if self.textures[k].is_none() {
            let img = ColorImage::from_rgb([plot.width, plot.height], &plot.rgb);
            self.textures[k] = Some(ui.ctx().load_texture(
                format!("optics-{k}"),
                img,
                TextureOptions::LINEAR,
            ));
        }
        if let Some(tex) = &self.textures[k] {
            ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_size();
                    let scale = (avail.x / plot.width as f32)
                        .min(avail.y / plot.height as f32)
                        .max(0.1);
                    let size = egui::vec2(plot.width as f32 * scale, plot.height as f32 * scale);
                    ui.add(egui::Image::new(tex).fit_to_exact_size(size));
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_the_command_line() {
        let t = OpticsTool::default();
        assert_eq!((t.crop, t.zoom, t.order, t.psf_stars), (128, 2, 3, 50));
        assert_eq!(View::ALL.len(), t.textures.len());
    }
}
