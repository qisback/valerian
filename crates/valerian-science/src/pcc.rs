// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The photometric colour-calibration scatter: the instrumental colour of
//! every measured Gaia star against its Gaia BP-RP, with the fitted line that
//! sets the gains, as PixInsight's PhotometricColorCalibration shows it.

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::Serialize;
use valerian_core::photocal::{Calibration, Line, StarPhot};

use crate::plot;
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// Instrumental colour of a star in channel `c` against green, magnitudes.
#[must_use]
pub fn instrumental(s: &StarPhot, c: usize) -> Option<f64> {
    (s.flux[c] > 0.0 && s.flux[1] > 0.0).then(|| -2.5 * (s.flux[c] / s.flux[1]).log10())
}

/// The line's value at a BP-RP colour.
#[must_use]
pub fn line_at(l: &Line, colour: f64, reference: f64) -> f64 {
    l.zero + l.slope * (colour - reference)
}

#[derive(Serialize)]
struct LineJson {
    channel: &'static str,
    zero_mag: f64,
    slope_mag_per_bp_rp: f64,
    scatter_mag: f64,
    gain: f64,
    gain_error: f64,
}

#[derive(Serialize)]
struct Summary<'a> {
    reference_colour: f32,
    stars_measured: usize,
    stars_used: usize,
    aperture_px: f64,
    lines: Vec<LineJson>,
    offsets: [f64; 3],
    warnings: &'a [String],
}

/// The fit and the evidence as JSON.
///
/// # Errors
/// If serialisation fails.
pub fn to_json(c: &Calibration) -> Result<String> {
    let s = Summary {
        reference_colour: c.reference_colour,
        stars_measured: c.stars.len(),
        stars_used: c.used,
        aperture_px: c.aperture_px,
        lines: vec![
            LineJson {
                channel: "red against green",
                zero_mag: c.lines[0].zero,
                slope_mag_per_bp_rp: c.lines[0].slope,
                scatter_mag: c.lines[0].rms,
                gain: c.gains[0],
                gain_error: c.gain_error[0] * c.gains[0],
            },
            LineJson {
                channel: "blue against green",
                zero_mag: c.lines[1].zero,
                slope_mag_per_bp_rp: c.lines[1].slope,
                scatter_mag: c.lines[1].rms,
                gain: c.gains[2],
                gain_error: c.gain_error[1] * c.gains[2],
            },
        ],
        offsets: c.offsets,
        warnings: &c.warnings,
    };
    serde_json::to_string_pretty(&s).map_err(|e| Error::Parse(e.to_string()))
}

/// Every star measured, with its colours and whether the fit kept it.
#[must_use]
pub fn to_csv(c: &Calibration) -> String {
    let rows: Vec<Vec<String>> = c
        .stars
        .iter()
        .map(|s| {
            let r = instrumental(s, 0);
            let b = instrumental(s, 2);
            let res = |y: Option<f64>, l: &Line| {
                y.map_or(String::new(), |y| {
                    num(
                        y - line_at(l, f64::from(s.colour), f64::from(c.reference_colour)),
                        4,
                    )
                })
            };
            vec![
                num(s.x, 2),
                num(s.y, 2),
                num(f64::from(s.g), 2),
                num(f64::from(s.colour), 3),
                r.map_or(String::new(), |v| num(v, 4)),
                b.map_or(String::new(), |v| num(v, 4)),
                res(r, &c.lines[0]),
                res(b, &c.lines[1]),
                u8::from(s.used).to_string(),
            ]
        })
        .collect();
    csv_text(
        &[
            "x",
            "y",
            "g",
            "bp_rp",
            "red_green_mag",
            "blue_green_mag",
            "red_residual_mag",
            "blue_residual_mag",
            "used",
        ],
        &rows,
    )
}

/// Pixels of the scatter sheet.
pub const SIZE: (u32, u32) = (1300, 560);

/// The two scatter plots (red against green, blue against green) with the
/// fitted lines, as RGB bytes of [`SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn plot_rgb(c: &Calibration) -> Result<Vec<u8>> {
    let reference = f64::from(c.reference_colour);
    let xs: Vec<f64> = c.stars.iter().map(|s| f64::from(s.colour)).collect();
    let (x0, x1) = plot::padded_range(xs.iter().copied());
    plot::render_rgb(SIZE.0, SIZE.1, |root| {
        let (a, b) = root.split_horizontally(SIZE.0 / 2);
        for (area, ch, name, line, gain, err) in [
            (&a, 0usize, "red", &c.lines[0], c.gains[0], c.gain_error[0]),
            (&b, 2usize, "blue", &c.lines[1], c.gains[2], c.gain_error[1]),
        ] {
            let ys: Vec<f64> = c.stars.iter().filter_map(|s| instrumental(s, ch)).collect();
            let (y0, y1) = plot::padded_range(
                ys.iter()
                    .copied()
                    .chain([line_at(line, x0, reference), line_at(line, x1, reference)]),
            );
            let mut chart = ChartBuilder::on(area)
                .caption(
                    format!(
                        "{name} against green: slope {:+.3}, scatter {:.3} mag, gain {gain:.3} \u{b1} {:.3}",
                        line.slope,
                        line.rms,
                        err * gain
                    ),
                    (FAMILY, 14).into_font().style(FontStyle::Bold),
                )
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(x0..x1, y0..y1)
                .map_err(Error::plot)?;
            chart
                .configure_mesh()
                .light_line_style(WHITE)
                .x_desc("Gaia BP-RP")
                .y_desc(format!("-2.5 log10 ({name} / green) (mag)"))
                .x_label_formatter(&|v| plot::tick(*v))
                .y_label_formatter(&|v| plot::tick(*v))
                .label_style((FAMILY, 11))
                .axis_desc_style((FAMILY, 12))
                .draw()
                .map_err(Error::plot)?;
            let used: Vec<(f64, f64)> = c
                .stars
                .iter()
                .filter(|s| s.used)
                .filter_map(|s| instrumental(s, ch).map(|y| (f64::from(s.colour), y)))
                .collect();
            let rejected: Vec<(f64, f64)> = c
                .stars
                .iter()
                .filter(|s| !s.used)
                .filter_map(|s| instrumental(s, ch).map(|y| (f64::from(s.colour), y)))
                .collect();
            chart
                .draw_series(
                    rejected
                        .iter()
                        .map(|&p| Cross::new(p, 4, plot::RED.stroke_width(1))),
                )
                .map_err(Error::plot)?
                .label(format!("rejected ({})", rejected.len()))
                .legend(|(x, y)| Cross::new((x + 9, y), 4, plot::RED.stroke_width(1)));
            chart
                .draw_series(
                    used.iter()
                        .map(|&p| Circle::new(p, 3, plot::BLUE.mix(0.6).filled())),
                )
                .map_err(Error::plot)?
                .label(format!("used in the fit ({})", used.len()))
                .legend(|(x, y)| Circle::new((x + 9, y), 3, plot::BLUE.filled()));
            chart
                .draw_series(LineSeries::new(
                    [
                        (x0, line_at(line, x0, reference)),
                        (x1, line_at(line, x1, reference)),
                    ],
                    plot::GREEN.stroke_width(2),
                ))
                .map_err(Error::plot)?
                .label("fitted line")
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::GREEN.stroke_width(2))
                });
            chart
                .draw_series(LineSeries::new(
                    [(reference, y0), (reference, y1)],
                    plot::GREY.stroke_width(1),
                ))
                .map_err(Error::plot)?
                .label(format!("neutral at BP-RP {reference:.2}"))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::GREY.stroke_width(1))
                });
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        Ok(())
    })
}

/// The scatter sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn plot_png(c: &Calibration) -> Result<Vec<u8>> {
    plot::encode_png(&plot_rgb(c)?, SIZE.0, SIZE.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calibration() -> Calibration {
        let stars: Vec<StarPhot> = (0..60)
            .map(|i| {
                let colour = 0.3 + 0.03 * f64::from(i);
                // red / green falls with colour, blue / green rises.
                let fr = 10f64.powf(0.4 * (0.25 * (colour - 0.82)));
                let fb = 10f64.powf(-0.4 * (0.35 * (colour - 0.82)));
                StarPhot {
                    x: 10.0 * f64::from(i),
                    y: 5.0 * f64::from(i),
                    g: 9.0,
                    colour: colour as f32,
                    flux: [1000.0 * fr, 1000.0, 1000.0 * fb],
                    snr: [50.0; 3],
                    used: i % 10 != 3,
                }
            })
            .collect();
        Calibration {
            gains: [1.1, 1.0, 0.9],
            offsets: [0.0; 3],
            lines: [
                Line {
                    zero: 0.0,
                    slope: -0.25,
                    rms: 0.02,
                },
                Line {
                    zero: 0.0,
                    slope: 0.35,
                    rms: 0.03,
                },
            ],
            gain_error: [0.01, 0.01],
            sky_before: [0.0; 3],
            sky_after: [0.0; 3],
            stars,
            used: 54,
            aperture_px: 6.0,
            reference_colour: 0.82,
            warnings: vec!["a warning".into()],
        }
    }

    #[test]
    fn the_scatter_renders_and_the_tables_hold_every_star() {
        let c = calibration();
        let png = plot_png(&c).expect("plot");
        assert_eq!(&png[1..4], b"PNG");
        let csv = to_csv(&c);
        assert_eq!(csv.lines().count(), 61);
        let json = to_json(&c).expect("json");
        assert!(json.contains("\"slope_mag_per_bp_rp\"") && json.contains("a warning"));
    }

    #[test]
    fn the_line_residuals_of_a_perfect_star_field_are_zero() {
        let c = calibration();
        let line = c.lines[0];
        let s = &c.stars[20];
        let r = instrumental(s, 0).expect("r") - line_at(&line, f64::from(s.colour), 0.82);
        assert!(r.abs() < 1e-6, "{r}");
    }
}
