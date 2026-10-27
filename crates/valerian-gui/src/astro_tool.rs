// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Astrometry tool: the plate solution drawn on the open image (RA/Dec
//! grid, catalogue objects, Gaia stars), the residuals of the solution, and
//! the colour-calibration scatter.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Checkbox, ColorImage, DragValue, RichText, ScrollArea, TextureHandle, TextureOptions, Ui,
};
use valerian_core::catalogue::Catalogue;
use valerian_core::fits::Image;
use valerian_core::photocal::{self, Calibration};
use valerian_core::wcs::Wcs;
use valerian_science::annotate::{self, AnnotateOptions, Annotated, Residuals};
use valerian_science::astro_report;
use valerian_science::pcc;
use valerian_science::report::ReportDir;
use valerian_science::residual_map::{self, ResidualMap};
use valerian_science::table::signed;
use valerian_science::zeropoint::{self, Photometry};

use crate::pipeline::{Progress, hint};
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Draw the solution.
    Run,
    /// Write the picture, lists, residuals and report.
    Save,
}

/// One of the pictures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// The solution drawn on the image.
    #[default]
    Annotated,
    /// Residual vectors.
    Residuals,
    /// Mean residual per cell and the radial and tangential terms.
    ResidualMap,
    /// Colour fit.
    Colour,
    /// Zero point and colour term of each plane.
    Zeropoint,
    /// Signal-to-noise and counts against magnitude.
    Limit,
}

impl View {
    const ALL: [Self; 6] = [
        Self::Annotated,
        Self::Residuals,
        Self::ResidualMap,
        Self::Colour,
        Self::Zeropoint,
        Self::Limit,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Annotated => "Annotated",
            Self::Residuals => "Residuals",
            Self::ResidualMap => "Residual map",
            Self::Colour => "Colour fit",
            Self::Zeropoint => "Zero point",
            Self::Limit => "Limiting mag",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// A drawn picture.
#[derive(Clone)]
pub struct Picture {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

/// What a run drew.
#[derive(Clone)]
pub struct AstroRun {
    /// The annotated picture's lists.
    pub annotated: Arc<Annotated>,
    /// The residuals, when stars matched.
    pub residuals: Option<Arc<Residuals>>,
    /// The residual map, when stars matched.
    pub residual_map: Option<Arc<ResidualMap>>,
    /// The colour calibration, for a colour image.
    pub calibration: Option<Arc<Calibration>>,
    /// The zero points, catalogue and limiting magnitude, for a colour image.
    pub photometry: Option<Arc<Photometry>>,
    pictures: Vec<Option<Picture>>,
}

/// The Astrometry panel.
pub struct AstroTool {
    /// Draw the RA/Dec grid.
    pub grid: bool,
    /// Draw the catalogue objects.
    pub objects: bool,
    /// Draw the Gaia stars.
    pub stars: bool,
    /// Faintest Gaia star drawn.
    pub star_mag: f32,
    /// The picture shown.
    pub view: View,
    /// Where the report is written.
    pub report: String,
    /// The last run.
    pub result: Option<AstroRun>,
    textures: Vec<Option<TextureHandle>>,
}

impl Default for AstroTool {
    fn default() -> Self {
        Self {
            grid: true,
            objects: true,
            stars: true,
            star_mag: 10.0,
            view: View::default(),
            report: String::new(),
            result: None,
            textures: vec![None; View::ALL.len()],
        }
    }
}

impl AstroTool {
    /// A tool with these settings and nothing else, to move into the worker thread.
    #[must_use]
    pub fn for_run(&self) -> Self {
        Self {
            grid: self.grid,
            objects: self.objects,
            stars: self.stars,
            star_mag: self.star_mag,
            ..Self::default()
        }
    }

    /// Draw the solution, the residuals and the colour fit. Runs on the worker thread.
    ///
    /// # Errors
    /// The picture cannot be drawn.
    pub fn run(
        &self,
        image: &Arc<Image>,
        wcs: &Wcs,
        cat: &Arc<Catalogue>,
        progress: &Progress,
    ) -> Result<AstroRun, String> {
        progress.say("Drawing the plate solution...");
        let opts = AnnotateOptions {
            grid: self.grid,
            objects: self.objects,
            stars: self.stars,
            star_mag: self.star_mag,
            ..AnnotateOptions::default()
        };
        let annotated =
            annotate::annotate(image, wcs, Some(cat), &opts).map_err(|e| e.to_string())?;
        progress.check()?;
        progress.say("Measuring the residuals...");
        let residuals = annotate::residuals(image, wcs, cat, 12.0, 3.0).ok();
        let calibration = if image.planes.len() == 3
            && image.planes.iter().all(|p| p.bayer.is_none())
            && valerian_core::stretch::Stretch::from_header(&image.header).is_none()
        {
            progress.say("Fitting the star colours...");
            photocal::calibrate(image, wcs, cat, &photocal::Options::default()).ok()
        } else {
            None
        };
        let map = residuals
            .as_ref()
            .and_then(|r| residual_map::residual_map(r, 6).ok());
        let photometry = if let Some(c) = &calibration {
            progress.say("Calibrating the star magnitudes...");
            zeropoint::calibrate_field(image, wcs, c, &zeropoint::Options::default()).ok()
        } else {
            None
        };
        let mut pictures = vec![
            Some(Picture {
                width: annotated.width as usize,
                height: annotated.height as usize,
                rgb: annotated.rgb.clone(),
            }),
            None,
            None,
            None,
            None,
            None,
        ];
        if let (Some(m), Some(r)) = (&map, &residuals)
            && let Ok(rgb) = residual_map::plot_rgb(m, r)
        {
            pictures[View::ResidualMap.index()] = Some(Picture {
                width: residual_map::SIZE.0 as usize,
                height: residual_map::SIZE.1 as usize,
                rgb,
            });
        }
        if let Some(p) = &photometry {
            if let Ok(rgb) = zeropoint::zeropoint_rgb(p) {
                pictures[View::Zeropoint.index()] = Some(Picture {
                    width: zeropoint::ZP_SIZE.0 as usize,
                    height: zeropoint::ZP_SIZE.1 as usize,
                    rgb,
                });
            }
            if let Ok(rgb) = zeropoint::limit_rgb(p) {
                pictures[View::Limit.index()] = Some(Picture {
                    width: zeropoint::LIMIT_SIZE.0 as usize,
                    height: zeropoint::LIMIT_SIZE.1 as usize,
                    rgb,
                });
            }
        }
        if let Some(r) = &residuals
            && let Ok((w, h, rgb)) = annotate::residuals_rgb(r)
        {
            pictures[View::Residuals.index()] = Some(Picture {
                width: w as usize,
                height: h as usize,
                rgb,
            });
        }
        if let Some(c) = &calibration
            && let Ok(rgb) = pcc::plot_rgb(c)
        {
            pictures[View::Colour.index()] = Some(Picture {
                width: pcc::SIZE.0 as usize,
                height: pcc::SIZE.1 as usize,
                rgb,
            });
        }
        Ok(AstroRun {
            annotated: Arc::new(annotated),
            residuals: residuals.map(Arc::new),
            residual_map: map.map(Arc::new),
            calibration: calibration.map(Arc::new),
            photometry: photometry.map(Arc::new),
            pictures,
        })
    }

    /// Take a finished run.
    pub fn accept(&mut self, run: AstroRun, image_path: &Path) {
        if self.report.trim().is_empty() {
            self.report = image_path
                .parent()
                .map_or_else(
                    || PathBuf::from("astrometry-report"),
                    |p| p.join("astrometry-report"),
                )
                .to_string_lossy()
                .into_owned();
        }
        self.textures = vec![None; View::ALL.len()];
        self.view = View::Annotated;
        self.result = Some(run);
    }

    /// Write the report folder.
    ///
    /// # Errors
    /// The folder cannot be written or a plot cannot be drawn.
    pub fn save(&self) -> Result<PathBuf, String> {
        let r = self.result.as_ref().ok_or("Draw the solution first.")?;
        let dir = self.report.trim();
        if dir.is_empty() {
            return Err("Name a folder for the report.".into());
        }
        let mut report = ReportDir::open(Path::new(dir)).map_err(|e| e.to_string())?;
        astro_report::write_astrometry(&mut report, &r.annotated, r.residuals.as_deref())
            .map_err(|e| e.to_string())?;
        if let (Some(m), Some(s)) = (&r.residual_map, &r.residuals) {
            astro_report::write_residual_map(&mut report, m, s).map_err(|e| e.to_string())?;
        }
        if let Some(c) = &r.calibration {
            astro_report::write_pcc(&mut report, c).map_err(|e| e.to_string())?;
        }
        if let Some(p) = &r.photometry {
            astro_report::write_photometry_calibration(&mut report, p)
                .map_err(|e| e.to_string())?;
        }
        Ok(Path::new(dir).join("report.html"))
    }

    /// The controls. `busy` while a run is going; `have_wcs` when the image is solved.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool, have_wcs: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        hint(
            ui,
            "Draws the plate solution on the image: an RA/Dec grid, the catalogue objects \
             (Messier, NGC and IC) and Gaia stars, the residuals of the solution against the \
             Gaia stars, the map of those residuals across the frame, and for a colour image the \
             fit that sets the colour balance, the zero point of each colour and the limiting \
             magnitude. The image must be plate solved: use the Plate solve tool first.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            widgets::overline(ui, "Layers");
            ui.add(Checkbox::new(&mut self.grid, "RA/Dec grid"));
            ui.add(Checkbox::new(&mut self.objects, "Catalogue objects"));
            ui.add(Checkbox::new(&mut self.stars, "Gaia stars"));
            ui.horizontal(|ui| {
                ui.label("Faintest star (G)");
                ui.add(
                    DragValue::new(&mut self.star_mag)
                        .range(4.0..=11.0)
                        .speed(0.1),
                );
            });
            ui.add_enabled_ui(have_wcs, |ui| {
                if widgets::button(
                    ui,
                    if busy {
                        "Drawing..."
                    } else {
                        "Draw the solution"
                    },
                    Kind::Primary,
                    0.0,
                )
                .on_hover_text("Draw the grid, objects and stars and measure the residuals")
                .clicked()
                {
                    ask = Ask::Run;
                }
            });
        });
        if !have_wcs {
            ui.label(
                RichText::new("This image has no plate solution yet.")
                    .small()
                    .color(p.accent),
            );
        }
        let Some(r) = &self.result else {
            return ask;
        };
        widgets::overline(ui, "Picture");
        ui.horizontal_wrapped(|ui| {
            for v in View::ALL {
                if r.pictures[v.index()].is_some()
                    && widgets::chip(ui, self.view == v, v.label()).clicked()
                {
                    self.view = v;
                }
            }
        });
        widgets::overline(ui, "Result");
        let lines = [
            format!(
                "{} objects, {} Gaia stars drawn",
                r.annotated.objects.len(),
                r.annotated.stars.len()
            ),
            r.residuals.as_ref().map_or_else(
                || "no residuals (too few stars matched)".to_owned(),
                |s| {
                    format!(
                        "{} stars matched, rms {:.3} px = {:.2} arcsec",
                        s.stars.len(),
                        s.rms_px,
                        s.rms_arcsec
                    )
                },
            ),
        ];
        for l in lines {
            ui.label(RichText::new(l).font(mono(12.0)).color(p.text));
        }
        if let Some(c) = &r.calibration {
            ui.label(
                RichText::new(format!(
                    "colour: {} stars used, red slope {:+.3}, blue slope {:+.3}",
                    c.used, c.lines[0].slope, c.lines[1].slope
                ))
                .font(mono(12.0))
                .color(p.muted),
            );
        }
        if let Some(m) = &r.residual_map {
            ui.label(
                RichText::new(format!(
                    "map: scale {} ppm, rotation {} arcsec",
                    signed(m.linear.scale_ppm, 0),
                    signed(m.linear.rotation_arcsec, 2)
                ))
                .font(mono(12.0))
                .color(p.muted),
            );
        }
        if let Some(ph) = &r.photometry {
            ui.label(
                RichText::new(format!(
                    "zero point (green) {:.2} +/- {:.2}, {:.0}-sigma limit {:.1}",
                    ph.channels[1].zero,
                    ph.channels[1].zero_error,
                    ph.limit.snr,
                    ph.limit.from_noise[1]
                ))
                .font(mono(12.0))
                .color(p.muted),
            );
        }
        for o in r.annotated.objects.iter().take(8) {
            ui.label(
                RichText::new(format!("{}  {}", o.object.name, o.object.common))
                    .small()
                    .color(p.muted),
            );
        }
        widgets::overline(ui, "Report");
        ui.add(
            egui::TextEdit::singleline(&mut self.report)
                .hint_text("Folder for the picture, lists, residuals and report.html")
                .desired_width(f32::INFINITY),
        );
        if widgets::button(ui, "Save report", Kind::Outline, 0.0)
            .on_hover_text("Write the picture, the lists, the residuals and report.html")
            .clicked()
        {
            ask = Ask::Save;
        }
        ask
    }

    /// Draw the chosen picture into the viewer.
    pub fn show(&mut self, ui: &mut Ui) {
        let p = palette();
        let Some(r) = &self.result else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(RichText::new("Nothing drawn yet").size(18.0).color(p.text));
                ui.label(
                    RichText::new("Plate solve the image, then press Draw the solution.")
                        .color(p.muted),
                );
            });
            return;
        };
        let k = self.view.index();
        let Some(pic) = r.pictures.get(k).and_then(Option::as_ref) else {
            ui.label("This picture could not be drawn for this image.");
            return;
        };
        if self.textures[k].is_none() {
            let img = ColorImage::from_rgb([pic.width, pic.height], &pic.rgb);
            self.textures[k] = Some(ui.ctx().load_texture(
                format!("astro-{k}"),
                img,
                TextureOptions::LINEAR,
            ));
        }
        if let Some(tex) = &self.textures[k] {
            ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_size();
                    let scale = (avail.x / pic.width as f32)
                        .min(avail.y / pic.height as f32)
                        .max(0.1);
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(
                        pic.width as f32 * scale,
                        pic.height as f32 * scale,
                    )));
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_draw_every_layer() {
        let t = AstroTool::default();
        assert!(t.grid && t.objects && t.stars);
        assert_eq!(t.textures.len(), View::ALL.len());
        assert!((t.star_mag - 10.0).abs() < 1e-6);
    }
}
