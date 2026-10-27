// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Star removal tool: settings, the result of a run and what the viewer
//! shows of it.
//!
//! A run works on the full frame in a background thread (star detection needs
//! the real pixel scale) and always removes the stars completely. The strength
//! slider is applied afterwards as `original - strength * stars`, which is
//! exact because the star layer is the difference, so moving it costs nothing
//! and needs no new run.

use std::sync::Arc;

use eframe::egui::{RichText, Slider, Ui};
use valerian_core::fits::Image;
use valerian_core::preview::downsample;
use valerian_core::starless::{Options, Removal};
use valerian_core::stats::median_finite;
use valerian_core::stretch::stretch_like;

use crate::theme::palette;
use crate::widgets::{self, Kind};

/// What the viewer shows once there is a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The image without its stars.
    Starless,
    /// The image as opened.
    Original,
    /// The stars that were taken out.
    Stars,
    /// The mask the fill was made inside.
    Mask,
}

/// A finished run, with reduced copies for the viewer.
#[derive(Clone, Debug)]
pub struct StarResult {
    /// The full-size result, shared with the save thread.
    pub removal: Arc<Removal>,
    /// The star layer, reduced.
    pub small_stars: Image,
    /// The mask, reduced (soft at the edges).
    pub small_mask: Image,
}

impl StarResult {
    /// Reduced copies of `removal` no larger than `side`.
    #[must_use]
    pub fn new(removal: Removal, side: usize) -> Self {
        let small_stars = downsample(&removal.stars, side);
        let small_mask = downsample(
            &Image::mono(removal.mask.clone(), removal.stars.header.clone()),
            side,
        );
        Self {
            removal: Arc::new(removal),
            small_stars,
            small_mask,
        }
    }
}

/// The settings the panel edits.
#[derive(Clone, Debug)]
pub struct StarTool {
    /// Removal settings. `strength` is applied after the run.
    pub opts: Options,
    /// The layer on show.
    pub view: View,
    /// The last run.
    pub result: Option<StarResult>,
}

impl Default for StarTool {
    fn default() -> Self {
        Self {
            opts: Options::default(),
            view: View::Starless,
            result: None,
        }
    }
}

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Detect and remove (a new run).
    Run,
    /// Only the view or the strength changed.
    Redraw,
    /// Write the starless image and the star layer.
    Save,
}

impl StarTool {
    /// The options a run uses: always complete removal.
    #[must_use]
    pub fn run_options(&self) -> Options {
        Options {
            strength: 1.0,
            ..self.opts.clone()
        }
    }

    /// The layer on show, as an image in `0..=1`, from the reduced original.
    ///
    /// # Errors
    /// The image has no valid pixels.
    pub fn layer(&self, original: &Image, target: f64) -> Result<Image, String> {
        let Some(r) = &self.result else {
            return stretch_like(original, original, target).map_err(|e| e.to_string());
        };
        let s = self.opts.strength;
        let combine = |sign_stars: bool| -> Image {
            let planes = original
                .planes
                .iter()
                .zip(&r.small_stars.planes)
                .map(|(o, st)| {
                    let mut f = o.clone();
                    if sign_stars {
                        // The star layer sits on the sky level so it can be shown.
                        let sky = median_finite(&o.data);
                        for (v, d) in f.data.iter_mut().zip(&st.data) {
                            *v = sky + s * d;
                        }
                    } else {
                        for (v, d) in f.data.iter_mut().zip(&st.data) {
                            *v -= s * d;
                        }
                    }
                    f
                })
                .collect();
            Image {
                header: original.header.clone(),
                planes,
            }
        };
        match self.view {
            View::Original => stretch_like(original, original, target),
            View::Starless => stretch_like(original, &combine(false), target),
            View::Stars => stretch_like(original, &combine(true), target),
            View::Mask => return Ok(r.small_mask.clone()),
        }
        .map_err(|e| e.to_string())
    }

    /// The full-size starless image at the chosen strength.
    #[must_use]
    pub fn starless_full(&self, original: &Image) -> Option<Image> {
        let r = self.result.as_ref()?;
        let s = self.opts.strength;
        let planes = original
            .planes
            .iter()
            .zip(&r.removal.stars.planes)
            .map(|(o, st)| {
                let mut f = o.clone();
                for (v, d) in f.data.iter_mut().zip(&st.data) {
                    *v -= s * d;
                }
                f
            })
            .collect();
        let mut header = r.removal.starless.header.clone();
        header.add_history(&format!("valerian starless: strength {s}"));
        Some(Image { header, planes })
    }

    /// The full-size star layer at the chosen strength.
    #[must_use]
    pub fn stars_full(&self) -> Option<Image> {
        let r = self.result.as_ref()?;
        let s = self.opts.strength;
        let mut img = r.removal.stars.clone();
        for p in &mut img.planes {
            for v in &mut p.data {
                *v *= s;
            }
        }
        Some(img)
    }

    /// The controls. `can_run` is false until an image is open; `busy` while a
    /// run is going.
    pub fn controls(&mut self, ui: &mut Ui, can_run: bool, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        ui.label(
            RichText::new(
                "Finds the stars, masks them and fills the holes from their surroundings. \
                 Classical, no learned model.",
            )
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Detection and mask");
        let o = &mut self.opts;
        ui.add_enabled_ui(!busy, |ui| {
            ui.add(Slider::new(&mut o.detect.threshold, 2.5..=12.0).text("Threshold (sigma)"));
            ui.add(Slider::new(&mut o.mask_scale, 0.5..=3.0).text("Mask size"));
            ui.add(Slider::new(&mut o.grow, 0.0..=10.0).text("Grow (px)"));
            ui.add(Slider::new(&mut o.max_radius, 5.0..=150.0).text("Largest radius (px)"));
            ui.checkbox(&mut o.noise, "Add noise to the filled pixels");
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(can_run && !busy, |ui| {
            if widgets::button(
                ui,
                if busy { "Running..." } else { "Remove stars" },
                Kind::Outline,
                0.0,
            )
            .on_hover_text("Detect and remove the stars on the full image")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            ui.separator();
            widgets::overline(ui, "Result");
            ui.label(
                RichText::new(format!(
                    "{} stars found, {:.1} % of the pixels filled",
                    r.removal.found,
                    100.0 * r.removal.masked_fraction
                ))
                .color(p.muted),
            );
            ui.horizontal_wrapped(|ui| {
                for (v, label) in [
                    (View::Starless, "Starless"),
                    (View::Original, "Original"),
                    (View::Stars, "Stars"),
                    (View::Mask, "Mask"),
                ] {
                    if ui.selectable_value(&mut self.view, v, label).changed() {
                        ask = Ask::Redraw;
                    }
                }
            });
            if ui
                .add(Slider::new(&mut self.opts.strength, 0.0..=1.0).text("Strength"))
                .on_hover_text("Below 1 reduces the stars instead of removing them")
                .changed()
            {
                ask = Ask::Redraw;
            }
            ui.add_enabled_ui(!busy, |ui| {
                if widgets::button(ui, "Save starless and stars", Kind::Outline, 0.0)
                    .on_hover_text("Write the starless image and the star layer as FITS files")
                    .clicked()
                {
                    ask = Ask::Save;
                }
            });
        }
        ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::Header;
    use valerian_core::starless::remove_stars;
    use valerian_core::testutil::deep_sky;

    fn run() -> (Image, StarTool) {
        let sky = deep_sky(240, 160, 40, 8);
        let img = Image::mono(sky.frame, Header::new());
        let tool = StarTool {
            result: Some(StarResult::new(
                remove_stars(&img, &Options::default()).expect("run"),
                1600,
            )),
            ..StarTool::default()
        };
        (img, tool)
    }

    #[test]
    fn strength_one_gives_the_removed_image_and_zero_the_original() {
        let (img, mut tool) = run();
        let full = tool.starless_full(&img).expect("result");
        let direct = &tool.result.as_ref().expect("result").removal.starless;
        for (a, b) in full.plane().data.iter().zip(&direct.plane().data) {
            assert!((a - b).abs() < 1e-3 * b.abs().max(1.0));
        }
        tool.opts.strength = 0.0;
        assert_eq!(
            tool.starless_full(&img).expect("result").plane().data,
            img.plane().data
        );
    }

    #[test]
    fn half_strength_layers_add_back_to_the_original() {
        let (img, mut tool) = run();
        tool.opts.strength = 0.5;
        let (a, b) = (
            tool.starless_full(&img).expect("result"),
            tool.stars_full().expect("result"),
        );
        for i in 0..img.plane().data.len() {
            let sum = a.plane().data[i] + b.plane().data[i];
            assert!((sum - img.plane().data[i]).abs() < 1e-2 * img.plane().data[i].abs().max(1.0));
        }
    }

    #[test]
    fn every_view_is_an_image_in_range() {
        let (img, mut tool) = run();
        for v in [View::Starless, View::Original, View::Stars, View::Mask] {
            tool.view = v;
            let out = tool.layer(&img, 0.15).expect("layer");
            assert_eq!(out.width(), img.width());
            assert!(out.plane().data.iter().all(|x| (0.0..=1.0).contains(x)));
        }
    }

    #[test]
    fn without_a_result_the_layer_is_the_stretched_original() {
        let (img, _) = run();
        let tool = StarTool::default();
        assert!(tool.layer(&img, 0.15).is_ok());
        assert!(tool.starless_full(&img).is_none());
    }
}
