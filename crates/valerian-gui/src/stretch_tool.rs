// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Stretch tool: curve choice and parameters, the live preview and the
//! full-resolution apply.
//!
//! The preview stretches a reduced copy (`valerian_core::preview`) with levels
//! measured on the full frame, so what is shown is what Apply writes. The
//! levels are cached and only measured again when the automatic-level settings
//! change; moving a curve slider costs one pass over the reduced copy.

use eframe::egui::{self, RichText, Slider, Ui};
use valerian_core::fits::Image;
use valerian_core::stretch::{
    AutoLevels, Curve, Ghs, Levels, Plan, Stretch, auto_levels, midtone_for, reference_frame,
    stretch_image,
};

use crate::theme::{mono, palette};
use crate::widgets;

/// Which curve is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveKind {
    /// MTF with the midtone solved so the sky lands on the target.
    Auto,
    /// MTF with a chosen midtone.
    Mtf,
    /// Arcsinh.
    Arcsinh,
    /// Generalised hyperbolic stretch.
    Ghs,
}

/// Everything the panel edits.
#[derive(Clone, Debug)]
pub struct StretchTool {
    /// The curve.
    pub kind: CurveKind,
    /// Sky level after an automatic stretch.
    pub target: f64,
    /// Midtone of the MTF.
    pub midtone: f64,
    /// Arcsinh strength.
    pub beta: f64,
    /// GHS parameters.
    pub ghs: Ghs,
    /// Automatic black point, robust sigmas below the sky.
    pub shadow_sigma: f32,
    /// Automatic white point percentile.
    pub white_percentile: f32,
    /// One stretch for every colour plane.
    pub linked: bool,
    /// Use `black` and `white` instead of the automatic levels.
    pub manual_levels: bool,
    /// Manual black point, in the image's units.
    pub black: f32,
    /// Manual white point, in the image's units.
    pub white: f32,
}

impl Default for StretchTool {
    fn default() -> Self {
        Self {
            kind: CurveKind::Auto,
            target: 0.10,
            midtone: 0.25,
            beta: 50.0,
            ghs: Ghs {
                stretch: 3.0,
                b: 0.0,
                sp: 0.0,
                lp: 0.0,
                hp: 1.0,
            },
            shadow_sigma: 2.8,
            white_percentile: 99.995,
            linked: true,
            manual_levels: false,
            black: 0.0,
            white: 1.0,
        }
    }
}

/// Levels measured on the full frame: one per plane, or one when linked, with
/// the sky each was measured against.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    shadow_sigma: f32,
    white_percentile: f32,
    linked: bool,
    /// `(levels, sky)` per stretch.
    pub levels: Vec<(Levels, f32)>,
}

impl StretchTool {
    fn auto(&self) -> AutoLevels {
        AutoLevels {
            shadow_sigma: self.shadow_sigma,
            white_percentile: self.white_percentile,
        }
    }

    /// The curve for the chosen kind. `sky` is the normalised sky of the
    /// plane, used by the automatic kind.
    fn curve(&self, sky: f64) -> Curve {
        match self.kind {
            CurveKind::Auto => Curve::Mtf {
                midtone: midtone_for(sky, self.target),
            },
            CurveKind::Mtf => Curve::Mtf {
                midtone: self.midtone,
            },
            CurveKind::Arcsinh => Curve::Arcsinh { beta: self.beta },
            CurveKind::Ghs => Curve::Ghs(self.ghs),
        }
    }

    /// Measure the levels on `full`, unless `cached` already holds them.
    ///
    /// # Errors
    /// The image has no valid pixels.
    pub fn measure(&self, full: &Image, cached: Option<&Measured>) -> Result<Measured, String> {
        if let Some(m) = cached
            && m.shadow_sigma.to_bits() == self.shadow_sigma.to_bits()
            && m.white_percentile.to_bits() == self.white_percentile.to_bits()
            && m.linked == self.linked
        {
            return Ok(m.clone());
        }
        let frames = if self.linked || full.planes.len() == 1 {
            vec![reference_frame(full)]
        } else {
            full.planes.clone()
        };
        let levels = frames
            .iter()
            .map(|f| auto_levels(f, self.auto()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(Measured {
            shadow_sigma: self.shadow_sigma,
            white_percentile: self.white_percentile,
            linked: self.linked,
            levels,
        })
    }

    /// The stretch of each plane (one entry when linked).
    #[must_use]
    pub fn stretches(&self, m: &Measured) -> Vec<Stretch> {
        m.levels
            .iter()
            .map(|&(auto, sky)| {
                let levels = if self.manual_levels {
                    Levels {
                        black: self.black,
                        white: self.white,
                    }
                } else {
                    auto
                };
                Stretch {
                    levels,
                    curve: self.curve(f64::from(levels.normalise(sky))),
                }
            })
            .collect()
    }

    /// Stretch the reduced copy `small` with the levels measured on the full
    /// frame.
    ///
    /// # Errors
    /// A parameter is out of range.
    pub fn render(&self, small: &Image, m: &Measured) -> Result<Image, String> {
        let s = self.stretches(m);
        for st in &s {
            st.check().map_err(|e| e.to_string())?;
        }
        let planes = small
            .planes
            .iter()
            .enumerate()
            .map(|(i, p)| s[i.min(s.len() - 1)].apply_frame(p))
            .collect();
        Ok(Image {
            header: small.header.clone(),
            planes,
        })
    }

    /// Stretch the full frame with the same stretch the preview shows.
    ///
    /// # Errors
    /// A parameter is out of range.
    pub fn apply(&self, full: &Image, m: &Measured) -> Result<Image, String> {
        let s = self.stretches(m);
        if s.len() == 1 {
            return stretch_image(full, &Plan::Fixed(s[0]), true).map_err(|e| e.to_string());
        }
        let planes = full
            .planes
            .iter()
            .zip(&s)
            .map(|(p, st)| {
                st.check().map_err(|e| e.to_string())?;
                Ok(st.apply_frame(p))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut header = full.header.clone();
        for (i, st) in s.iter().enumerate() {
            header.add_history(&format!(
                "valerian stretch: plane {i} black {:.6}, white {:.6}, {:?}",
                st.levels.black, st.levels.white, st.curve
            ));
        }
        Ok(Image { header, planes })
    }

    /// The controls. Returns true when anything changed.
    pub fn controls(&mut self, ui: &mut Ui, measured: Option<&Measured>) -> bool {
        let p = palette();
        let mut changed = false;
        widgets::overline(ui, "Curve");
        ui.horizontal_wrapped(|ui| {
            for (k, label) in [
                (CurveKind::Auto, "Auto"),
                (CurveKind::Mtf, "MTF"),
                (CurveKind::Arcsinh, "Arcsinh"),
                (CurveKind::Ghs, "GHS"),
            ] {
                changed |= ui.selectable_value(&mut self.kind, k, label).changed();
            }
        });
        ui.label(
            RichText::new(match self.kind {
                CurveKind::Auto => "Midtone solved so the sky lands on the target.",
                CurveKind::Mtf => "Midtone transfer function; below 0.5 brightens.",
                CurveKind::Arcsinh => "Linear when faint, logarithmic when bright.",
                CurveKind::Ghs => "Generalised hyperbolic stretch: where and how hard.",
            })
            .small()
            .color(p.muted),
        );
        match self.kind {
            CurveKind::Auto => {
                changed |= slider(ui, &mut self.target, 0.02..=0.5, "Sky target");
            }
            CurveKind::Mtf => {
                changed |= slider(ui, &mut self.midtone, 0.001..=0.999, "Midtone");
            }
            CurveKind::Arcsinh => {
                changed |= ui
                    .add(
                        Slider::new(&mut self.beta, 0.5..=5000.0)
                            .logarithmic(true)
                            .text("Strength"),
                    )
                    .changed();
            }
            CurveKind::Ghs => {
                changed |= slider(ui, &mut self.ghs.stretch, 0.0..=6.0, "Stretch factor");
                changed |= slider(ui, &mut self.ghs.b, -5.0..=15.0, "Local intensity");
                changed |= slider(ui, &mut self.ghs.sp, 0.0..=1.0, "Symmetry point");
                self.ghs.lp = self.ghs.lp.min(self.ghs.sp);
                self.ghs.hp = self.ghs.hp.max(self.ghs.sp);
                let sp = self.ghs.sp;
                changed |= slider(ui, &mut self.ghs.lp, 0.0..=sp, "Shadow protection");
                changed |= slider(ui, &mut self.ghs.hp, sp..=1.0, "Highlight protection");
            }
        }
        ui.add_space(4.0);
        widgets::overline(ui, "Levels");
        let was_manual = self.manual_levels;
        changed |= ui
            .checkbox(&mut self.manual_levels, "Set black and white by hand")
            .changed();
        if self.manual_levels {
            if !was_manual && let Some(&(l, _)) = measured.and_then(|m| m.levels.first()) {
                self.black = l.black;
                self.white = l.white;
            }
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.black)
                            .speed(1.0)
                            .prefix("black "),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.white)
                            .speed(1.0)
                            .prefix("white "),
                    )
                    .changed();
            });
        } else {
            changed |= slider(
                ui,
                &mut self.shadow_sigma,
                0.0..=6.0,
                "Black below sky (sigma)",
            );
            changed |= slider(
                ui,
                &mut self.white_percentile,
                99.0..=100.0,
                "White percentile",
            );
        }
        changed |= ui
            .checkbox(&mut self.linked, "Same stretch for every colour plane")
            .changed();
        if let Some(l) = measured.and_then(|m| m.levels.first()) {
            ui.label(
                RichText::new(format!("black {:.1}, white {:.1}", l.0.black, l.0.white))
                    .font(mono(12.5))
                    .color(p.faint),
            );
        }
        if widgets::button(ui, "Reset", widgets::Kind::Outline, 0.0)
            .on_hover_text("Back to the automatic stretch")
            .clicked()
        {
            *self = Self::default();
            changed = true;
        }
        changed
    }
}

fn slider<N: egui::emath::Numeric>(
    ui: &mut Ui,
    v: &mut N,
    range: std::ops::RangeInclusive<N>,
    label: &str,
) -> bool {
    ui.add(Slider::new(v, range).text(label)).changed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::Frame;
    use valerian_core::fits::Header;
    use valerian_core::preview::downsample;
    use valerian_core::stats::median_finite;
    use valerian_core::testutil::Rng;

    fn sky(w: usize, h: usize) -> Image {
        let mut rng = Rng::new(5);
        let data = (0..w * h)
            .map(|i| 200.0 + if i % 97 == 0 { 3000.0 } else { 0.0 } + rng.gauss(0.0, 6.0) as f32)
            .collect();
        Image::mono(Frame::new(w, h, data).expect("ok"), Header::new())
    }

    #[test]
    fn auto_matches_the_command_line_stretch() {
        let img = sky(120, 80);
        let tool = StretchTool::default();
        let m = tool.measure(&img, None).expect("ok");
        let ours = tool.apply(&img, &m).expect("ok");
        let cli = stretch_image(
            &img,
            &Plan::AutoMtf {
                target: 0.10,
                auto: AutoLevels {
                    shadow_sigma: 2.8,
                    white_percentile: 99.995,
                },
            },
            true,
        )
        .expect("ok");
        for (a, b) in ours.plane().data.iter().zip(&cli.plane().data) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn preview_uses_the_full_frame_levels() {
        let img = sky(160, 96);
        let small = downsample(&img, 40);
        let tool = StretchTool::default();
        let m = tool.measure(&img, None).expect("ok");
        let prev = tool.render(&small, &m).expect("ok");
        let med = median_finite(&prev.plane().data);
        assert!((med - 0.10).abs() < 0.03, "sky at {med}");
    }

    #[test]
    fn levels_are_only_measured_again_when_their_settings_change() {
        let img = sky(64, 64);
        let mut tool = StretchTool::default();
        let m = tool.measure(&img, None).expect("ok");
        tool.kind = CurveKind::Ghs;
        tool.ghs.stretch = 4.0;
        assert_eq!(tool.measure(&img, Some(&m)).expect("ok"), m);
        tool.shadow_sigma = 1.0;
        assert_ne!(tool.measure(&img, Some(&m)).expect("ok"), m);
    }

    #[test]
    fn every_curve_renders_and_stays_in_range() {
        let img = sky(64, 64);
        let mut tool = StretchTool::default();
        let m = tool.measure(&img, None).expect("ok");
        for kind in [
            CurveKind::Auto,
            CurveKind::Mtf,
            CurveKind::Arcsinh,
            CurveKind::Ghs,
        ] {
            tool.kind = kind;
            let out = tool.render(&img, &m).expect("ok");
            assert!(out.plane().data.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn a_bad_parameter_is_an_error_not_a_panic() {
        let img = sky(32, 32);
        let mut tool = StretchTool {
            kind: CurveKind::Mtf,
            midtone: 1.5,
            ..StretchTool::default()
        };
        let m = tool.measure(&img, None).expect("ok");
        assert!(tool.render(&img, &m).is_err());
        tool.midtone = 0.3;
        assert!(tool.render(&img, &m).is_ok());
    }
}
