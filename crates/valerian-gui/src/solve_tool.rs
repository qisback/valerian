// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Plate solve tool: where to look, the run, and the result.
//!
//! The position and pixel scale are read from the FITS header when it has them
//! (`RA`/`DEC` or `OBJCTRA`/`OBJCTDEC`, `FOCALLEN` and `XPIXSZ`, or an existing
//! WCS) and stay editable. The run detects stars on the full image and matches
//! them to the catalogue in a background thread.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{DragValue, RichText, Slider, TextEdit, Ui};
use valerian_core::catalogue::{self, Catalogue};
use valerian_core::fits::Header;
use valerian_core::solve::{Hint, Options, Solution, hint_from_header, parse_angle};

use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Solve the image.
    Run,
    /// Write a copy with the WCS in its header.
    Save,
    /// Choose the catalogue file with the file dialog.
    Catalogue,
}

/// The settings the panel edits.
#[derive(Clone, Debug)]
pub struct SolveTool {
    /// Field centre, right ascension: degrees or `hh:mm:ss`.
    pub ra: String,
    /// Field centre, declination: degrees or `dd:mm:ss`.
    pub dec: String,
    /// How far the true centre may be from the position, degrees.
    pub radius: f64,
    /// Pixel scale in arc seconds per pixel.
    pub scale: f64,
    /// Search the whole sky.
    pub blind: bool,
    /// A catalogue file; empty for the one beside the program.
    pub catalogue_path: String,
    /// A catalogue already in memory (tests, and after the first run).
    pub catalogue: Option<Arc<Catalogue>>,
    /// The last solution.
    pub result: Option<Solution>,
}

impl Default for SolveTool {
    fn default() -> Self {
        Self {
            ra: String::new(),
            dec: String::new(),
            radius: 1.0,
            scale: 1.0,
            blind: false,
            catalogue_path: String::new(),
            catalogue: None,
            result: None,
        }
    }
}

impl SolveTool {
    /// Fill the position and scale from a header, where it has them.
    pub fn prefill(&mut self, header: &Header, size: (usize, usize)) {
        let h = hint_from_header(header, size);
        if let Some((ra, dec)) = h.position {
            self.ra = format!("{ra:.4}");
            self.dec = format!("{dec:.4}");
            self.blind = false;
        } else {
            self.ra.clear();
            self.dec.clear();
        }
        if let Some(s) = h.scale {
            self.scale = (s * 1000.0).round() / 1000.0;
        }
        self.result = None;
    }

    /// The options for a run.
    ///
    /// # Errors
    /// The position cannot be read, or is missing without a blind search.
    pub fn options(&self) -> Result<Options, String> {
        let hint = if self.blind {
            None
        } else {
            let ra = parse_angle(&self.ra, true).ok_or(
                "Enter the right ascension (degrees or hh:mm:ss), or tick the blind search.",
            )?;
            let dec = parse_angle(&self.dec, false)
                .ok_or("Enter the declination (degrees or dd:mm:ss), or tick the blind search.")?;
            Some(Hint {
                ra,
                dec,
                radius: self.radius,
            })
        };
        if self.scale <= 0.0 {
            return Err("The pixel scale must be above zero.".into());
        }
        Ok(Options {
            hint,
            scale: (self.scale * 0.85, self.scale * 1.15),
            ..Options::default()
        })
    }

    /// The catalogue: the one in memory, else the file named, else the default.
    ///
    /// # Errors
    /// No catalogue can be read.
    pub fn load_catalogue(&self) -> Result<Arc<Catalogue>, String> {
        if let Some(c) = &self.catalogue {
            return Ok(Arc::clone(c));
        }
        let p = self.catalogue_path.trim();
        let c = if p.is_empty() {
            catalogue::open_default()
        } else {
            Catalogue::open(&PathBuf::from(p))
        };
        c.map(Arc::new).map_err(|e| e.to_string())
    }

    /// The controls. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        ui.label(
            RichText::new(
                "Matches the stars to the Gaia catalogue and writes a TAN world coordinate \
                 system, with SIP distortion when it clearly helps.",
            )
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Where to look");
        ui.add_enabled_ui(!busy, |ui| {
            ui.add_enabled_ui(!self.blind, |ui| {
                ui.horizontal(|ui| {
                    ui.label("RA");
                    ui.add(TextEdit::singleline(&mut self.ra).desired_width(110.0));
                    ui.label("Dec");
                    ui.add(TextEdit::singleline(&mut self.dec).desired_width(110.0));
                });
                ui.add(Slider::new(&mut self.radius, 0.1..=5.0).text("Search radius (deg)"));
            });
            ui.checkbox(&mut self.blind, "Blind: search the whole sky");
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut self.scale)
                        .speed(0.01)
                        .range(0.1..=60.0)
                        .max_decimals(3),
                );
                ui.label("arcsec per pixel (searched 15 % either way)");
            });
            widgets::overline(ui, "Catalogue");
            ui.add(
                TextEdit::singleline(&mut self.catalogue_path)
                    .hint_text("Default: gaia-dr3-g11.vcat beside the program")
                    .desired_width(f32::INFINITY),
            );
            if widgets::button(ui, "Choose catalogue...", Kind::Outline, 0.0)
                .on_hover_text("Choose a .vcat star catalogue with the file dialog")
                .clicked()
            {
                ask = Ask::Catalogue;
            }
        });
        ui.add_enabled_ui(!busy, |ui| {
            if widgets::button(
                ui,
                if busy { "Solving..." } else { "Solve" },
                Kind::Outline,
                0.0,
            )
            .on_hover_text("Detect the stars and match them to the catalogue")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            ui.separator();
            widgets::overline(ui, "Result");
            let w = &r.wcs;
            let lines = [
                format!("centre RA {:.5}, Dec {:+.5}", r.centre.0, r.centre.1),
                format!(
                    "{:.3} arcsec/px, field {:.2} x {:.2} deg",
                    w.scale_arcsec(),
                    r.field.0,
                    r.field.1
                ),
                format!(
                    "rotation {:.2} deg east of north{}",
                    w.rotation(),
                    if w.mirrored() { ", mirrored" } else { "" }
                ),
                format!(
                    "{} of {} stars matched, rms {:.2} arcsec",
                    r.matched, r.detected, r.rms_arcsec
                ),
                match &w.sip {
                    Some(s) => format!("TAN-SIP, order {}", s.order),
                    None => "TAN, no distortion terms".to_owned(),
                },
            ];
            for l in lines {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
            ui.label(
                RichText::new("The matched stars are circled in the viewer.")
                    .small()
                    .color(p.muted),
            );
            ui.add_enabled_ui(!busy, |ui| {
                if widgets::button(ui, "Save with WCS", Kind::Outline, 0.0)
                    .on_hover_text("Write a copy of the image with the solution in its header")
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
    use valerian_core::fits::Value;

    #[test]
    fn a_header_fills_the_position_and_scale() {
        let mut h = Header::new();
        h.set("RA", Value::Float(313.02084), "");
        h.set("DEC", Value::Float(44.4975), "");
        h.set("FOCALLEN", Value::Float(250.0), "");
        h.set("XPIXSZ", Value::Float(2.9), "");
        let mut t = SolveTool::default();
        t.prefill(&h, (1080, 1920));
        assert_eq!((t.ra.as_str(), t.dec.as_str()), ("313.0208", "44.4975"));
        assert!((t.scale - 2.393).abs() < 0.001, "{}", t.scale);
        let o = t.options().expect("options");
        assert!(o.hint.is_some() && (o.scale.0 - 2.034).abs() < 0.01);
        // A header with no position clears the boxes.
        t.prefill(&Header::new(), (10, 10));
        assert_eq!(t.ra, "");
    }

    #[test]
    fn a_missing_position_needs_the_blind_search_and_bad_text_is_refused() {
        let mut t = SolveTool::default();
        assert!(t.options().is_err());
        t.blind = true;
        assert!(t.options().expect("blind").hint.is_none());
        t.blind = false;
        t.ra = "12:30:00".into();
        t.dec = "-45:00:00".into();
        let o = t.options().expect("sexagesimal");
        let h = o.hint.expect("hint");
        assert!((h.ra - 187.5).abs() < 1e-9 && (h.dec + 45.0).abs() < 1e-9);
        t.ra = "banana".into();
        assert!(t.options().is_err());
        t.ra = "10".into();
        t.scale = 0.0;
        assert!(t.options().is_err());
    }

    #[test]
    fn a_missing_catalogue_says_where_it_looked() {
        let t = SolveTool {
            catalogue_path: "/nonexistent/none.vcat".into(),
            ..SolveTool::default()
        };
        assert!(
            t.load_catalogue()
                .expect_err("missing")
                .contains("none.vcat")
        );
    }
}
