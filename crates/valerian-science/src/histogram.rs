// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Histograms per channel, of the linear data and of the same data after an
//! automatic stretch, as PixInsight's HistogramTransformation shows them.

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::{Deserialize, Serialize};
use valerian_core::fits::Image;
use valerian_core::stars::luminance;
use valerian_core::stretch::{AutoLevels, auto_mtf};

use crate::plot;
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// What one channel looks like.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ChannelStats {
    /// Channel name: `red`, `green`, `blue`, or `grey` for one plane.
    pub name: String,
    /// Smallest finite value, ADU.
    pub min: f64,
    /// Largest finite value, ADU.
    pub max: f64,
    /// Median (the sky), ADU.
    pub median: f64,
    /// 1.4826 times the median absolute deviation (the sky noise), ADU.
    pub sigma: f64,
    /// Fraction of pixels at or below zero after the stretch.
    pub clipped_black: f64,
    /// Fraction of pixels at or above one after the stretch.
    pub clipped_white: f64,
    /// Median of the stretched values, 0 to 1.
    pub stretched_median: f64,
}

/// A channel's counts.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ChannelHist {
    /// Counts of the linear values.
    pub linear: Vec<u64>,
    /// Counts of the stretched values.
    pub stretched: Vec<u64>,
    /// The statistics.
    pub stats: ChannelStats,
}

/// The histograms of an image.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Histograms {
    /// Bins in each histogram.
    pub bins: usize,
    /// Range of the linear histogram, ADU.
    pub linear_range: (f64, f64),
    /// Black point of the stretch, ADU.
    pub black: f64,
    /// White point of the stretch, ADU.
    pub white: f64,
    /// Midtones balance of the stretch.
    pub midtone: f64,
    /// One entry per channel.
    pub channels: Vec<ChannelHist>,
}

fn percentile(sorted: &[f32], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let k = ((sorted.len() - 1) as f64 * p / 100.0).round() as usize;
    f64::from(sorted[k])
}

/// The histograms of `img`, `bins` bins each. The linear range runs from the
/// 0.01th to the 99.99th percentile of all channels, so they can be compared; the
/// stretch is the automatic midtone stretch of the luminance (black point 2.8
/// robust sigmas below the sky, sky at 25 per cent), applied to every channel alike.
///
/// # Errors
/// [`Error::Empty`] when the image has no finite pixels.
pub fn histograms(img: &Image, bins: usize) -> Result<Histograms> {
    let bins = bins.clamp(16, 4096);
    let names: Vec<&str> = if img.planes.len() == 3 {
        vec!["red", "green", "blue"]
    } else {
        vec!["grey"]
    };
    let lum = luminance(&img.planes);
    let stretch =
        auto_mtf(&lum, 0.25, AutoLevels::default()).map_err(|e| Error::Empty(e.to_string()))?;
    let mut sorted: Vec<Vec<f32>> = img
        .planes
        .iter()
        .take(names.len())
        .map(|p| {
            let mut v: Vec<f32> = p.data.iter().copied().filter(|x| x.is_finite()).collect();
            v.sort_by(f32::total_cmp);
            v
        })
        .collect();
    if sorted.iter().any(Vec::is_empty) {
        return Err(Error::Empty("the image has no valid pixels".into()));
    }
    let lo = sorted
        .iter()
        .map(|v| percentile(v, 0.01))
        .fold(f64::INFINITY, f64::min);
    let mut hi = sorted
        .iter()
        .map(|v| percentile(v, 99.99))
        .fold(f64::NEG_INFINITY, f64::max);
    if hi <= lo {
        hi = lo + 1.0;
    }
    let mut channels = Vec::new();
    for (k, v) in sorted.drain(..).enumerate() {
        let mut linear = vec![0u64; bins];
        let mut stretched = vec![0u64; bins];
        let (mut black, mut white) = (0u64, 0u64);
        for &x in &v {
            let b = (((f64::from(x) - lo) / (hi - lo)) * bins as f64).floor();
            if b >= 0.0 && (b as usize) < bins {
                linear[b as usize] += 1;
            } else if b >= bins as f64 {
                linear[bins - 1] += 1;
            } else {
                linear[0] += 1;
            }
            let s = f64::from(stretch.apply(x)).clamp(0.0, 1.0);
            if s <= 0.0 {
                black += 1;
            }
            if s >= 1.0 {
                white += 1;
            }
            stretched[((s * bins as f64) as usize).min(bins - 1)] += 1;
        }
        let n = v.len() as f64;
        let median = percentile(&v, 50.0);
        let mut dev: Vec<f32> = v
            .iter()
            .map(|x| (f64::from(*x) - median).abs() as f32)
            .collect();
        dev.sort_by(f32::total_cmp);
        let smed = f64::from(stretch.apply(median as f32)).clamp(0.0, 1.0);
        channels.push(ChannelHist {
            linear,
            stretched,
            stats: ChannelStats {
                name: names[k].to_owned(),
                min: f64::from(v[0]),
                max: f64::from(v[v.len() - 1]),
                median,
                sigma: 1.4826 * percentile(&dev, 50.0),
                clipped_black: black as f64 / n,
                clipped_white: white as f64 / n,
                stretched_median: smed,
            },
        });
    }
    let midtone = match stretch.curve {
        valerian_core::stretch::Curve::Mtf { midtone } => midtone,
        _ => f64::NAN,
    };
    Ok(Histograms {
        bins,
        linear_range: (lo, hi),
        black: f64::from(stretch.levels.black),
        white: f64::from(stretch.levels.white),
        midtone,
        channels,
    })
}

/// The counts as CSV: `view`, bin edges and one column per channel.
#[must_use]
pub fn to_csv(h: &Histograms) -> String {
    let mut header = vec!["view".to_owned(), "bin_lo".to_owned(), "bin_hi".to_owned()];
    header.extend(h.channels.iter().map(|c| c.stats.name.clone()));
    let head: Vec<&str> = header.iter().map(String::as_str).collect();
    let mut rows = Vec::new();
    for (view, lo, hi) in [
        ("linear", h.linear_range.0, h.linear_range.1),
        ("stretched", 0.0, 1.0),
    ] {
        for b in 0..h.bins {
            let (a, c) = (
                lo + (hi - lo) * b as f64 / h.bins as f64,
                lo + (hi - lo) * (b + 1) as f64 / h.bins as f64,
            );
            let mut r = vec![view.to_owned(), num(a, 4), num(c, 4)];
            for ch in &h.channels {
                let v = if view == "linear" {
                    &ch.linear
                } else {
                    &ch.stretched
                };
                r.push(v[b].to_string());
            }
            rows.push(r);
        }
    }
    csv_text(&head, &rows)
}

/// Pixels of the histogram sheet.
pub const SIZE: (u32, u32) = (1300, 520);

fn colour(name: &str) -> RGBColor {
    match name {
        "red" => RGBColor(200, 40, 40),
        "green" => RGBColor(20, 150, 70),
        "blue" => RGBColor(40, 80, 210),
        _ => RGBColor(60, 60, 60),
    }
}

/// The two histograms side by side (linear, stretched) on a logarithmic count
/// axis, as RGB bytes of [`SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn plot_rgb(h: &Histograms) -> Result<Vec<u8>> {
    let top = h
        .channels
        .iter()
        .flat_map(|c| c.linear.iter().chain(&c.stretched))
        .copied()
        .max()
        .unwrap_or(1) as f64;
    let ymax = (top + 1.0).log10() * 1.05;
    plot::render_rgb(SIZE.0, SIZE.1, |root| {
        let (a, b) = root.split_horizontally(SIZE.0 / 2);
        for (area, stretched) in [(&a, false), (&b, true)] {
            let (x0, x1) = if stretched {
                (0.0, 1.0)
            } else {
                h.linear_range
            };
            let mut chart = ChartBuilder::on(area)
                .caption(
                    if stretched {
                        "Stretched (automatic midtone stretch, all channels alike)"
                    } else {
                        "Linear"
                    },
                    (FAMILY, 15).into_font().style(FontStyle::Bold),
                )
                .margin(10)
                .x_label_area_size(34)
                .y_label_area_size(56)
                .build_cartesian_2d(x0..x1, 0.0..ymax)
                .map_err(Error::plot)?;
            chart
                .configure_mesh()
                .light_line_style(WHITE)
                .x_desc(if stretched {
                    "stretched value"
                } else {
                    "value (ADU)"
                })
                .y_desc("log10 (pixels + 1)")
                .label_style((FAMILY, 11))
                .axis_desc_style((FAMILY, 12))
                .x_label_formatter(&|v| plot::tick(*v))
                .draw()
                .map_err(Error::plot)?;
            for ch in &h.channels {
                let counts = if stretched { &ch.stretched } else { &ch.linear };
                let c = colour(&ch.stats.name);
                let step = (x1 - x0) / h.bins as f64;
                chart
                    .draw_series(LineSeries::new(
                        counts.iter().enumerate().map(|(i, n)| {
                            (x0 + step * (i as f64 + 0.5), (*n as f64 + 1.0).log10())
                        }),
                        c.stroke_width(2),
                    ))
                    .map_err(Error::plot)?
                    .label(format!(
                        "{} (median {})",
                        ch.stats.name,
                        plot::tick(if stretched {
                            ch.stats.stretched_median
                        } else {
                            ch.stats.median
                        })
                    ))
                    .legend(move |(x, y)| {
                        PathElement::new([(x, y), (x + 18, y)], c.stroke_width(2))
                    });
            }
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

/// The histogram sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn plot_png(h: &Histograms) -> Result<Vec<u8>> {
    plot::encode_png(&plot_rgb(h)?, SIZE.0, SIZE.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::Frame;
    use valerian_core::fits::Header;
    use valerian_core::testutil::Rng;

    fn colour_image() -> Image {
        let mut rng = Rng::new(4);
        let mut plane = |level: f64| Frame {
            width: 200,
            height: 150,
            data: (0..30_000)
                .map(|i| {
                    let star = if i % 997 == 0 { 20000.0 } else { 0.0 };
                    (rng.gauss(level, 10.0) + star) as f32
                })
                .collect(),
            bayer: None,
        };
        Image {
            header: Header::new(),
            planes: vec![plane(500.0), plane(505.0), plane(502.0)],
        }
    }

    #[test]
    fn three_channels_count_every_pixel_in_both_views() {
        let h = histograms(&colour_image(), 128).expect("histograms");
        assert_eq!(h.channels.len(), 3);
        for c in &h.channels {
            assert_eq!(c.linear.iter().sum::<u64>(), 30_000);
            assert_eq!(c.stretched.iter().sum::<u64>(), 30_000);
        }
        let g = &h.channels[1].stats;
        assert!(
            (g.median - 505.0).abs() < 1.0 && (g.sigma - 10.0).abs() < 1.0,
            "{g:?}"
        );
        // The automatic stretch puts the sky near a quarter.
        assert!(
            (g.stretched_median - 0.25).abs() < 0.08,
            "{}",
            g.stretched_median
        );
        assert_eq!(to_csv(&h).lines().count(), 1 + 2 * 128);
        let png = plot_png(&h).expect("plot");
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn one_plane_is_grey() {
        let mut img = colour_image();
        img.planes.truncate(1);
        let h = histograms(&img, 64).expect("histograms");
        assert_eq!(h.channels.len(), 1);
        assert_eq!(h.channels[0].stats.name, "grey");
    }
}
