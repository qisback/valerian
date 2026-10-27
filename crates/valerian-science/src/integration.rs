// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Integration diagnostics: where a stack (or a drizzle) rejected samples and
//! how many frames and how much weight each pixel got, drawn as maps, and how
//! much noise the integration removed against the theoretical square root of
//! the number of frames.

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::{Deserialize, Serialize};
use valerian_core::Frame;
use valerian_core::compute::Cpu;
use valerian_core::fits::Image;
use valerian_core::stack::{StackOptions, Stacked, align_with, frame_weights_for, integrate_with};

use crate::optics::colour_map;
use crate::plot::{self, GREY};
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// What a map counts, for its title and colour bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    /// Samples rejected below the centre.
    RejectionLow,
    /// Samples rejected above the centre.
    RejectionHigh,
    /// Input pixels rejected as outliers (drizzle).
    Rejected,
    /// Frames (samples) that went into the pixel.
    Count,
    /// Summed weight of the samples kept.
    Weight,
}

impl MapKind {
    /// File stem of the map.
    #[must_use]
    pub fn stem(self) -> &'static str {
        match self {
            Self::RejectionLow => "rejection_low",
            Self::RejectionHigh => "rejection_high",
            Self::Rejected => "rejected",
            Self::Count => "pixel_count",
            Self::Weight => "weight_map",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::RejectionLow => "Samples rejected low, per pixel",
            Self::RejectionHigh => "Samples rejected high, per pixel",
            Self::Rejected => "Input pixels rejected as outliers, per output pixel",
            Self::Count => "Samples used, per pixel",
            Self::Weight => "Summed weight of the samples used, per pixel",
        }
    }
}

/// A map reduced to a displayable size, with its statistics.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MapStats {
    /// Smallest finite value.
    pub min: f64,
    /// Largest finite value.
    pub max: f64,
    /// Mean over the finite pixels.
    pub mean: f64,
    /// Median over the finite pixels.
    pub median: f64,
    /// Fraction of pixels that are exactly zero.
    pub zero_fraction: f64,
    /// Pixels measured.
    pub pixels: usize,
}

/// Statistics of a map.
#[must_use]
pub fn map_stats(f: &Frame) -> MapStats {
    let mut v: Vec<f32> = f.data.iter().copied().filter(|x| x.is_finite()).collect();
    let n = v.len();
    if n == 0 {
        return MapStats {
            min: f64::NAN,
            max: f64::NAN,
            mean: f64::NAN,
            median: f64::NAN,
            zero_fraction: f64::NAN,
            pixels: 0,
        };
    }
    let zeros = v.iter().filter(|x| **x == 0.0).count();
    let mean = v.iter().map(|x| f64::from(*x)).sum::<f64>() / n as f64;
    let (lo, hi) = v
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), x| {
            (a.min(*x), b.max(*x))
        });
    v.sort_by(f32::total_cmp);
    MapStats {
        min: f64::from(lo),
        max: f64::from(hi),
        mean,
        median: f64::from(v[n / 2]),
        zero_fraction: zeros as f64 / n as f64,
        pixels: n,
    }
}

/// How many pixels hold each integer value of a count map (up to 256 values).
#[must_use]
pub fn value_counts(f: &Frame) -> Vec<(u32, usize)> {
    let mut counts = vec![0usize; 257];
    for &v in f.data.iter().filter(|v| v.is_finite()) {
        counts[(v.round().max(0.0) as usize).min(256)] += 1;
    }
    counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(v, c)| (v as u32, *c))
        .collect()
}

/// The value counts as CSV.
#[must_use]
pub fn value_counts_csv(f: &Frame) -> String {
    let total: usize = value_counts(f).iter().map(|c| c.1).sum();
    let rows: Vec<Vec<String>> = value_counts(f)
        .iter()
        .map(|(v, c)| {
            vec![
                v.to_string(),
                c.to_string(),
                num(*c as f64 / total.max(1) as f64, 6),
            ]
        })
        .collect();
    csv_text(&["value", "pixels", "fraction"], &rows)
}

/// Longest side of the picture of a map, pixels.
pub const MAP_SIDE: usize = 1000;

/// A map as RGB bytes: the picture (reduced by block means to `MAP_SIDE`), a title, a
/// colour bar from 0 (or the smallest value) to the largest, and the statistics.
/// Pixels without data (NaN) are drawn black.
///
/// # Errors
/// If drawing fails.
pub fn map_rgb(f: &Frame, kind: MapKind) -> Result<(u32, u32, Vec<u8>)> {
    let factor = f.width.max(f.height).div_ceil(MAP_SIDE).max(1);
    let (mw, mh) = (f.width / factor, f.height / factor);
    if mw == 0 || mh == 0 {
        return Err(Error::Empty("the map is empty".into()));
    }
    let stats = map_stats(f);
    // Reduce by block means.
    let mut small = vec![f32::NAN; mw * mh];
    for y in 0..mh {
        for x in 0..mw {
            let (mut s, mut n) = (0.0f64, 0u32);
            for yy in y * factor..(y + 1) * factor {
                for &v in &f.data[yy * f.width + x * factor..yy * f.width + (x + 1) * factor] {
                    if v.is_finite() {
                        s += f64::from(v);
                        n += 1;
                    }
                }
            }
            if n > 0 {
                small[y * mw + x] = (s / f64::from(n)) as f32;
            }
        }
    }
    // Counts and weights start at zero, as do rejections.
    let lo = 0.0_f64;
    let hi = stats.max.max(lo + 1e-9);
    let (top, bottom, side) = (44usize, 34usize, 120usize);
    let (cw, ch) = (mw + side, mh + top + bottom);
    let mut buf = vec![255u8; cw * ch * 3];
    for y in 0..mh {
        for x in 0..mw {
            let v = f64::from(small[y * mw + x]);
            let rgb = if v.is_finite() {
                let c = colour_map((v - lo) / (hi - lo));
                [c.0, c.1, c.2]
            } else {
                [0, 0, 0]
            };
            let o = ((y + top) * cw + x) * 3;
            buf[o..o + 3].copy_from_slice(&rgb);
        }
    }
    let title = kind.title().to_owned();
    let foot = format!(
        "min {}  median {}  mean {}  max {}   zero in {:.1} per cent of the pixels",
        plot::tick(stats.min),
        plot::tick(stats.median),
        plot::tick(stats.mean),
        plot::tick(stats.max),
        100.0 * stats.zero_fraction
    );
    let rgb = plot::render_rgb_over(cw as u32, ch as u32, buf, |a| {
        a.draw(&Text::new(
            title.clone(),
            (8, 12),
            (FAMILY, 17).into_font().style(FontStyle::Bold),
        ))
        .map_err(Error::plot)?;
        let bar_h = mh.saturating_sub(20);
        for k in 0..bar_h {
            let t = 1.0 - k as f64 / bar_h.max(1) as f64;
            let y = (top + 10 + k) as i32;
            a.draw(&Rectangle::new(
                [((mw + 16) as i32, y), ((mw + 36) as i32, y + 1)],
                colour_map(t).filled(),
            ))
            .map_err(Error::plot)?;
        }
        for (frac, v) in [(0.0, hi), (0.5, f64::midpoint(lo, hi)), (1.0, lo)] {
            a.draw(&Text::new(
                plot::tick(v),
                (
                    (mw + 42) as i32,
                    (top + 10) as i32 + (frac * bar_h as f64) as i32 - 6,
                ),
                (FAMILY, 12).into_font(),
            ))
            .map_err(Error::plot)?;
        }
        a.draw(&Text::new(
            foot.clone(),
            (8, (top + mh + 10) as i32),
            (FAMILY, 12).into_font(),
        ))
        .map_err(Error::plot)?;
        Ok(())
    })?;
    Ok((cw as u32, ch as u32, rgb))
}

/// A map as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn map_png(f: &Frame, kind: MapKind) -> Result<Vec<u8>> {
    let (w, h, rgb) = map_rgb(f, kind)?;
    plot::encode_png(&rgb, w, h)
}

// ---------------------------------------------------------------- noise

/// One point of the noise-against-frames curve.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct NoisePoint {
    /// Frames in each of the two half-stacks that were differenced.
    pub frames: usize,
    /// Measured noise of one such stack, ADU.
    pub noise: f64,
    /// The single-frame noise over the square root of `frames`.
    pub expected: f64,
}

/// Noise against the number of frames, from split halves.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NoiseCurve {
    /// Frames available.
    pub frames: usize,
    /// Noise of one frame, ADU (mean of up to 8 disjoint pairs).
    pub sigma1: f64,
    /// The curve.
    pub points: Vec<NoisePoint>,
}

impl NoiseCurve {
    /// Noise reduction at the largest point against one frame: `sigma1 / noise`.
    #[must_use]
    pub fn reduction(&self) -> f64 {
        self.points
            .last()
            .map_or(f64::NAN, |p| self.sigma1 / p.noise)
    }

    /// The theoretical reduction at the largest point: the square root of its frames.
    #[must_use]
    pub fn theoretical(&self) -> f64 {
        self.points
            .last()
            .map_or(f64::NAN, |p| (p.frames as f64).sqrt())
    }
}

/// Standard deviation of `v` clipped at 3 sigma, corrected for the clipping.
fn clipped_std(v: &mut Vec<f32>) -> f64 {
    if v.len() < 1000 {
        return f64::NAN;
    }
    let mut mid = {
        let mut s = v.clone();
        s.sort_by(f32::total_cmp);
        f64::from(s[s.len() / 2])
    };
    let mut sigma = {
        let mut d: Vec<f32> = v
            .iter()
            .map(|x| (f64::from(*x) - mid).abs() as f32)
            .collect();
        d.sort_by(f32::total_cmp);
        1.4826 * f64::from(d[d.len() / 2])
    };
    for _ in 0..6 {
        v.retain(|x| (f64::from(*x) - mid).abs() <= 3.0 * sigma);
        if v.len() < 100 {
            break;
        }
        let n = v.len() as f64;
        mid = v.iter().map(|x| f64::from(*x)).sum::<f64>() / n;
        sigma = (v.iter().map(|x| (f64::from(*x) - mid).powi(2)).sum::<f64>() / n).sqrt();
    }
    sigma / 0.9866
}

/// The noise of the difference of the weighted means of two sets of frames over
/// the pixels where every frame of both has data, over the square root of two.
fn half_noise(a: &[(&Frame, f32)], b: &[(&Frame, f32)], common: &[usize]) -> f64 {
    let mean = |set: &[(&Frame, f32)], i: usize| {
        let (mut s, mut w) = (0.0f64, 0.0f64);
        for (f, wt) in set {
            s += f64::from(f.data[i]) * f64::from(*wt);
            w += f64::from(*wt);
        }
        s / w
    };
    let mut d: Vec<f32> = common
        .iter()
        .map(|&i| (mean(a, i) - mean(b, i)) as f32)
        .collect();
    clipped_std(&mut d) / std::f64::consts::SQRT_2
}

/// Noise against the number of frames. The frames must be one plane each, on one
/// grid (registered, `NaN` outside their footprint), with one weight each. For
/// each power of two `m` up to half the frames, the first `2 m` frames are split
/// alternately into two weighted means, and the noise of one is the clipped
/// standard deviation of their difference over the square root of two. That
/// needs no model of the noise and is not changed by the correlation that
/// resampling puts between neighbouring pixels.
///
/// `None` with fewer than 4 frames or fewer than 1000 pixels in common.
#[must_use]
pub fn noise_curve(frames: &[&Frame], weights: &[f32]) -> Option<NoiseCurve> {
    let n = frames.len();
    if n < 4 || weights.len() != n {
        return None;
    }
    let len = frames[0].data.len();
    let common: Vec<usize> = (0..len)
        .step_by((len / 400_000).max(1))
        .filter(|&i| frames.iter().all(|f| f.data[i].is_finite()))
        .collect();
    if common.len() < 1000 {
        return None;
    }
    let item = |i: usize| (frames[i], weights[i]);
    // One frame: disjoint pairs.
    let pairs = (n / 2).min(8);
    let mut s1 = Vec::new();
    for p in 0..pairs {
        s1.push(half_noise(&[item(2 * p)], &[item(2 * p + 1)], &common));
    }
    let sigma1 = s1.iter().sum::<f64>() / s1.len() as f64;
    let mut points = Vec::new();
    let mut m = 1;
    while 2 * m <= n {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for k in 0..2 * m {
            if k % 2 == 0 {
                a.push(item(k));
            } else {
                b.push(item(k));
            }
        }
        points.push(NoisePoint {
            frames: m,
            noise: half_noise(&a, &b, &common),
            expected: sigma1 / (m as f64).sqrt(),
        });
        m *= 2;
    }
    // The largest split too, when the count is not a power of two.
    let half = n / 2;
    if points.last().is_some_and(|p| p.frames < half) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for k in 0..2 * half {
            if k % 2 == 0 {
                a.push(item(k));
            } else {
                b.push(item(k));
            }
        }
        points.push(NoisePoint {
            frames: half,
            noise: half_noise(&a, &b, &common),
            expected: sigma1 / (half as f64).sqrt(),
        });
    }
    Some(NoiseCurve {
        frames: n,
        sigma1,
        points,
    })
}

/// Register the frames and stack them on the CPU with the per-pixel maps on, and
/// measure the noise curve from the registered frames with the stack's weights.
///
/// # Errors
/// As for [`valerian_core::stack::stack`].
pub fn stack_with_maps(
    frames: Vec<Image>,
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> valerian_core::Result<(Stacked, Option<NoiseCurve>)> {
    let opts = StackOptions {
        maps: true,
        ..opts.clone()
    };
    let aligned = align_with(frames, &opts, &Cpu, log)?;
    let weights = frame_weights_for(&aligned.keep, &aligned.reports, &opts);
    let mid = aligned.frames[aligned.keep[0]].planes.len() / 2;
    let refs: Vec<&Frame> = aligned
        .keep
        .iter()
        .map(|&i| &aligned.frames[i].planes[mid])
        .collect();
    let curve = noise_curve(&refs, &weights);
    let stacked = integrate_with(aligned, &opts, &Cpu, log)?;
    Ok((stacked, curve))
}

/// The curve as CSV.
#[must_use]
pub fn noise_csv(c: &NoiseCurve) -> String {
    let rows: Vec<Vec<String>> = c
        .points
        .iter()
        .map(|p| {
            vec![
                p.frames.to_string(),
                num(p.noise, 3),
                num(p.expected, 3),
                num(p.noise / p.expected, 4),
                num(c.sigma1 / p.noise, 3),
                num((p.frames as f64).sqrt(), 3),
            ]
        })
        .collect();
    csv_text(
        &[
            "frames_per_half",
            "noise_adu",
            "sqrt_n_expectation_adu",
            "measured_over_expected",
            "reduction",
            "sqrt_n",
        ],
        &rows,
    )
}

/// Pixels of the noise plot.
pub const NOISE_SIZE: (u32, u32) = (900, 620);

/// The noise against frames, logarithmic on both axes, as RGB bytes of [`NOISE_SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn noise_rgb(c: &NoiseCurve) -> Result<Vec<u8>> {
    let mmax = c.points.last().map_or(2, |p| p.frames) as f64;
    let ymax = c.sigma1 * 1.25;
    let ymin = c
        .points
        .iter()
        .map(|p| p.noise.min(p.expected))
        .fold(c.sigma1, f64::min)
        * 0.8;
    plot::render_rgb(NOISE_SIZE.0, NOISE_SIZE.1, |a| {
        let mut chart = ChartBuilder::on(a)
            .caption(
                "Noise against the number of frames stacked",
                (FAMILY, 16).into_font().style(FontStyle::Bold),
            )
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(60)
            .build_cartesian_2d((0.8f64..mmax * 1.3).log_scale(), (ymin..ymax).log_scale())
            .map_err(Error::plot)?;
        chart
            .configure_mesh()
            .light_line_style(WHITE)
            .x_desc("frames in the stack (each half of the split)")
            .y_desc("noise (ADU)")
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .x_label_formatter(&|v| format!("{v:.0}"))
            .y_label_formatter(&|v| plot::tick(*v))
            .draw()
            .map_err(Error::plot)?;
        chart
            .draw_series(LineSeries::new(
                (0..=60).map(|k| {
                    let m = 1.0 * (mmax / 1.0).powf(f64::from(k) / 60.0);
                    (m, c.sigma1 / m.sqrt())
                }),
                plot::ORANGE.stroke_width(2),
            ))
            .map_err(Error::plot)?
            .label(format!(
                "one frame ({:.1} ADU) over the square root of N",
                c.sigma1
            ))
            .legend(|(x, y)| PathElement::new([(x, y), (x + 18, y)], plot::ORANGE.stroke_width(2)));
        chart
            .draw_series(LineSeries::new(
                c.points.iter().map(|p| (p.frames as f64, p.noise)),
                plot::BLUE.stroke_width(2),
            ))
            .map_err(Error::plot)?
            .label("measured, difference of two half-stacks")
            .legend(|(x, y)| PathElement::new([(x, y), (x + 18, y)], plot::BLUE.stroke_width(2)));
        chart
            .draw_series(
                c.points
                    .iter()
                    .map(|p| Circle::new((p.frames as f64, p.noise), 5, plot::BLUE.filled())),
            )
            .map_err(Error::plot)?;
        chart
            .configure_series_labels()
            .background_style(WHITE.mix(0.85))
            .border_style(GREY)
            .label_font((FAMILY, 12))
            .position(SeriesLabelPosition::UpperRight)
            .draw()
            .map_err(Error::plot)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::testutil::Rng;

    fn frames(n: usize, sigma: f64, seed: u64) -> Vec<Frame> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| Frame {
                width: 300,
                height: 200,
                data: (0..60_000)
                    .map(|_| rng.gauss(500.0, sigma) as f32)
                    .collect(),
                bayer: None,
            })
            .collect()
    }

    #[test]
    fn independent_frames_follow_the_square_root_law() {
        let fr = frames(16, 20.0, 1);
        let refs: Vec<&Frame> = fr.iter().collect();
        let c = noise_curve(&refs, &[1.0; 16]).expect("curve");
        assert!((c.sigma1 - 20.0).abs() < 0.8, "{}", c.sigma1);
        let last = c.points.last().expect("point");
        assert_eq!(last.frames, 8);
        assert!(
            (last.noise / last.expected - 1.0).abs() < 0.06,
            "{} vs {}",
            last.noise,
            last.expected
        );
        assert!((c.reduction() / c.theoretical() - 1.0).abs() < 0.06);
    }

    #[test]
    fn shared_noise_cancels_in_the_difference() {
        // A common pattern in every frame cancels in the difference, but noise
        // shared between frames (a fixed fraction) does not average down: here
        // half of each frame's variance is shared, so halves differ by less.
        let mut rng = Rng::new(2);
        let shared: Vec<f32> = (0..60_000).map(|_| rng.gauss(0.0, 14.0) as f32).collect();
        let fr: Vec<Frame> = (0..8)
            .map(|_| Frame {
                width: 300,
                height: 200,
                data: shared
                    .iter()
                    .map(|s| 500.0 + s + rng.gauss(0.0, 14.0) as f32)
                    .collect(),
                bayer: None,
            })
            .collect();
        let refs: Vec<&Frame> = fr.iter().collect();
        let c = noise_curve(&refs, &[1.0; 8]).expect("curve");
        // The difference sees only the independent part, so the curve follows
        // sigma_independent / sqrt(m) from a single-frame value of the same part.
        let last = c.points.last().expect("point");
        assert!((last.noise / last.expected - 1.0).abs() < 0.08);
        assert!(c.sigma1 < 16.0, "{}", c.sigma1);
    }

    #[test]
    fn too_few_frames_give_none() {
        let fr = frames(3, 5.0, 3);
        let refs: Vec<&Frame> = fr.iter().collect();
        assert!(noise_curve(&refs, &[1.0; 3]).is_none());
    }

    #[test]
    fn map_statistics_and_pictures() {
        let f = Frame {
            width: 400,
            height: 300,
            data: (0..120_000).map(|i| (i % 7) as f32).collect(),
            bayer: None,
        };
        let s = map_stats(&f);
        assert_eq!((s.min, s.max), (0.0, 6.0));
        assert!((s.zero_fraction - 1.0 / 7.0).abs() < 0.01);
        assert_eq!(value_counts(&f).len(), 7);
        assert_eq!(value_counts_csv(&f).lines().count(), 8);
        for k in [MapKind::RejectionLow, MapKind::Count, MapKind::Weight] {
            let png = map_png(&f, k).expect("map");
            assert_eq!(&png[1..4], b"PNG");
        }
        let curve = NoiseCurve {
            frames: 8,
            sigma1: 10.0,
            points: vec![
                NoisePoint {
                    frames: 1,
                    noise: 10.0,
                    expected: 10.0,
                },
                NoisePoint {
                    frames: 4,
                    noise: 5.2,
                    expected: 5.0,
                },
            ],
        };
        assert_eq!(
            noise_rgb(&curve).expect("plot").len(),
            (NOISE_SIZE.0 * NOISE_SIZE.1 * 3) as usize
        );
        assert!(noise_csv(&curve).contains("0.9615") || noise_csv(&curve).contains("1.04"));
    }
}
