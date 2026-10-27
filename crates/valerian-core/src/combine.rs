// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Per-pixel combination of many frames with outlier rejection.
//!
//! This is the engine under both the master calibration frames and the final
//! stack. For every pixel position it gathers the samples from all frames
//! (NaN means "this frame has no data here" and is skipped), rejects outliers,
//! and returns the weighted mean of what is left.
//!
//! Three rejection schemes are provided, as in the usual astro stackers:
//!
//! * **Sigma clipping**: iteratively reject samples further than `low` or
//!   `high` standard deviations from the median. With few frames the outlier
//!   inflates the deviation it is judged by, so one outlier among `n` samples
//!   can never exceed about `sqrt(n - 1)` sigma: below about fifteen frames a
//!   3-sigma clip cannot reject a single bright outlier.
//! * **Winsorised sigma clipping**: the deviation is first estimated robustly
//!   by winsorising (pulling samples beyond 1.5 sigma back to that limit and
//!   iterating), which is not inflated by the outliers, so it works for small
//!   stacks.
//! * **Linear fit clipping**: sort the samples, fit a straight line to the
//!   sorted values, and reject samples that sit far from the line. Best for
//!   stacks whose level varies (sky gradients, changing transparency) once
//!   there are more than about fifteen frames.
//!
//! # References
//!
//! - Huber, P. J. (1964), Robust estimation of a location parameter, Ann. Math. Statist. 35, 73-101, doi:10.1214/aoms/1177703732.
//! - Huber, P. J. and Ronchetti, E. M. (2009), Robust Statistics, 2nd ed., Wiley, doi:10.1002/9780470434697 (winsorising, and the 1.134 factor for its bias).
//! - Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).
//! - Sigma clipping, winsorised sigma clipping and linear fit clipping are matched from the behaviour other stackers document (names and ranges), not from their code.

use rayon::prelude::*;

use crate::error::{Error, Result};
use helia_core::Frame;

/// Which samples to reject at each pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rejection {
    /// Keep everything.
    None,
    /// Drop the `low` smallest and `high` largest samples.
    MinMax {
        /// Number of smallest samples to drop.
        low: usize,
        /// Number of largest samples to drop.
        high: usize,
    },
    /// Iterative sigma clipping about the median.
    Sigma {
        /// Rejection limit below the centre, in sigma.
        low: f32,
        /// Rejection limit above the centre, in sigma.
        high: f32,
    },
    /// Winsorised sigma clipping.
    Winsorised {
        /// Rejection limit below the centre, in sigma.
        low: f32,
        /// Rejection limit above the centre, in sigma.
        high: f32,
    },
    /// Linear fit clipping.
    LinearFit {
        /// Rejection limit below the fitted line, in residual sigma.
        low: f32,
        /// Rejection limit above the fitted line, in residual sigma.
        high: f32,
    },
}

impl Rejection {
    /// A sensible default for `n` frames: winsorised below 15 frames, sigma
    /// clipping from 15 to 30 and linear fit above.
    #[must_use]
    pub fn auto(n: usize) -> Self {
        match n {
            0..=2 => Self::None,
            3..=14 => Self::Winsorised {
                low: 3.0,
                high: 3.0,
            },
            15..=29 => Self::Sigma {
                low: 3.0,
                high: 3.0,
            },
            _ => Self::LinearFit {
                low: 5.0,
                high: 2.5,
            },
        }
    }
}

/// Settings for [`combine`].
#[derive(Clone, Debug)]
pub struct Options {
    /// The rejection scheme.
    pub rejection: Rejection,
    /// Most clipping passes per pixel.
    pub max_iterations: usize,
    /// Weight of each frame in the mean; `None` is equal weights.
    pub weights: Option<Vec<f32>>,
    /// Also record, per pixel, how many samples were rejected below and above
    /// the centre and the summed weight of the samples kept ([`Maps`]).
    pub maps: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rejection: Rejection::Sigma {
                low: 3.0,
                high: 3.0,
            },
            max_iterations: 10,
            weights: None,
            maps: false,
        }
    }
}

/// Per-pixel accounts of a combine, asked for with [`Options::maps`].
#[derive(Clone, Debug, PartialEq)]
pub struct Maps {
    /// Samples rejected below the centre at each pixel.
    pub low: Vec<u16>,
    /// Samples rejected above the centre at each pixel.
    pub high: Vec<u16>,
    /// Summed weight of the samples that were kept at each pixel.
    pub weight: Vec<f32>,
}

/// The result of [`combine`].
#[derive(Clone, Debug)]
pub struct Combined {
    /// The weighted mean of the surviving samples; NaN where no frame had data.
    pub mean: Frame,
    /// Number of samples that went into the mean at each pixel.
    pub used: Vec<u16>,
    /// Samples rejected below the centre, per input frame.
    pub rejected_low: Vec<u64>,
    /// Samples rejected above the centre, per input frame.
    pub rejected_high: Vec<u64>,
    /// Samples with data, per input frame.
    pub valid: Vec<u64>,
    /// The per-pixel accounts, when asked for and the engine keeps them.
    pub maps: Option<Maps>,
}

impl Combined {
    /// Fraction of each frame's valid samples that were rejected.
    #[must_use]
    pub fn rejection_fraction(&self) -> Vec<f64> {
        (0..self.valid.len())
            .map(|i| {
                (self.rejected_low[i] + self.rejected_high[i]) as f64 / self.valid[i].max(1) as f64
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
struct Sample {
    v: f32,
    w: f32,
    frame: u32,
    keep: bool,
}

fn kept_values(s: &[Sample], out: &mut Vec<f32>) {
    out.clear();
    out.extend(s.iter().filter(|x| x.keep).map(|x| x.v));
}

fn median_of(v: &mut [f32]) -> f32 {
    crate::stats::median_in_place(v)
}

/// Standard deviation about `centre` over `v` (population form).
fn rms_about(v: &[f32], centre: f32) -> f32 {
    let s: f32 = v.iter().map(|&x| (x - centre) * (x - centre)).sum();
    (s / v.len() as f32).sqrt()
}

fn sigma_clip(s: &mut [Sample], low: f32, high: f32, max_iter: usize, scratch: &mut Vec<f32>) {
    for _ in 0..max_iter {
        kept_values(s, scratch);
        if scratch.len() < 3 {
            return;
        }
        let centre = median_of(scratch);
        let sigma = rms_about(scratch, centre);
        if sigma <= 0.0 {
            return;
        }
        let mut changed = false;
        for x in s.iter_mut().filter(|x| x.keep) {
            if x.v < centre - low * sigma || x.v > centre + high * sigma {
                x.keep = false;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Robust sigma by winsorising at 1.5 sigma until it settles (Huber's
/// estimator; 1.134 corrects the bias winsorising puts into a normal sample).
fn winsorised_sigma(v: &[f32], centre: f32, sigma0: f32) -> f32 {
    let mut sigma = sigma0;
    let mut mean = centre;
    for _ in 0..50 {
        let lo = mean - 1.5 * sigma;
        let hi = mean + 1.5 * sigma;
        let n = v.len() as f32;
        let w: Vec<f32> = v.iter().map(|&x| x.clamp(lo, hi)).collect();
        let m = w.iter().sum::<f32>() / n;
        let var = w.iter().map(|&x| (x - m) * (x - m)).sum::<f32>() / (n - 1.0).max(1.0);
        let next = 1.134 * var.sqrt();
        let done = (next - sigma).abs() <= 0.0005 * sigma.max(f32::MIN_POSITIVE);
        sigma = next;
        mean = m;
        if done || sigma <= 0.0 {
            break;
        }
    }
    sigma
}

fn winsorised_clip(s: &mut [Sample], low: f32, high: f32, max_iter: usize, scratch: &mut Vec<f32>) {
    for _ in 0..max_iter {
        kept_values(s, scratch);
        if scratch.len() < 4 {
            return;
        }
        let centre = median_of(scratch);
        let sigma0 = rms_about(scratch, centre);
        if sigma0 <= 0.0 {
            return;
        }
        let sigma = winsorised_sigma(scratch, centre, sigma0);
        if sigma <= 0.0 {
            return;
        }
        let mut changed = false;
        for x in s.iter_mut().filter(|x| x.keep) {
            if x.v < centre - low * sigma || x.v > centre + high * sigma {
                x.keep = false;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

fn linear_fit_clip(s: &mut [Sample], low: f32, high: f32, max_iter: usize) {
    let mut order: Vec<usize> = Vec::with_capacity(s.len());
    for _ in 0..max_iter {
        order.clear();
        order.extend((0..s.len()).filter(|&i| s[i].keep));
        let n = order.len();
        if n < 5 {
            return;
        }
        order.sort_by(|&a, &b| s[a].v.total_cmp(&s[b].v));
        // Least-squares line through (rank, value).
        let nf = n as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for (r, &i) in order.iter().enumerate() {
            let (x, y) = (r as f64, f64::from(s[i].v));
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let denom = nf * sxx - sx * sx;
        let slope = (nf * sxy - sx * sy) / denom;
        let icpt = (sy - slope * sx) / nf;
        let resid = |r: usize, i: usize| f64::from(s[i].v) - (icpt + slope * r as f64);
        let ss: f64 = order
            .iter()
            .enumerate()
            .map(|(r, &i)| resid(r, i).powi(2))
            .sum();
        let sigma = (ss / (nf - 2.0)).sqrt();
        if sigma <= 0.0 {
            return;
        }
        let drop: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|&(r, &i)| {
                let d = resid(r, i);
                d < -f64::from(low) * sigma || d > f64::from(high) * sigma
            })
            .map(|(_, &i)| i)
            .collect();
        if drop.is_empty() {
            return;
        }
        for i in drop {
            s[i].keep = false;
        }
    }
}

fn min_max(s: &mut [Sample], low: usize, high: usize) {
    let mut idx: Vec<usize> = (0..s.len()).collect();
    idx.sort_by(|&a, &b| s[a].v.total_cmp(&s[b].v));
    if idx.len() <= low + high + 1 {
        return;
    }
    for &i in idx.iter().take(low) {
        s[i].keep = false;
    }
    for &i in idx.iter().rev().take(high) {
        s[i].keep = false;
    }
}

/// Reject outliers in one pixel's samples; the survivors have `keep` set.
fn reject(s: &mut [Sample], rejection: Rejection, max_iter: usize, scratch: &mut Vec<f32>) {
    match rejection {
        Rejection::None => {}
        Rejection::MinMax { low, high } => min_max(s, low, high),
        Rejection::Sigma { low, high } => sigma_clip(s, low, high, max_iter, scratch),
        Rejection::Winsorised { low, high } => winsorised_clip(s, low, high, max_iter, scratch),
        Rejection::LinearFit { low, high } => linear_fit_clip(s, low, high, max_iter),
    }
}

/// Per-row results gathered before assembly.
struct Row {
    mean: Vec<f32>,
    used: Vec<u16>,
    low: Vec<u32>,
    high: Vec<u32>,
    valid: Vec<u32>,
    low_map: Vec<u16>,
    high_map: Vec<u16>,
    weight_map: Vec<f32>,
}

/// Combine `frames` pixel by pixel.
///
/// # Errors
/// [`Error::Invalid`] when there are no frames, they differ in size, or the
/// weights do not match the frames.
pub fn combine(frames: &[&Frame], opts: &Options) -> Result<Combined> {
    combine_impl(frames, None, opts)
}

/// [`combine`] where every sample also has its own weight: sample `x` of frame
/// `i` counts `weights[i] * pixel_weights[i][x]` in the mean. A mosaic uses it
/// to feather the seams ([`crate::mosaic`]). A sample whose pixel weight is not
/// positive and finite is treated as no data.
///
/// # Errors
/// As for [`combine`], and [`Error::Invalid`] when the pixel weights differ in
/// number or size from the frames.
pub fn combine_pixel_weighted(
    frames: &[&Frame],
    pixel_weights: &[&Frame],
    opts: &Options,
) -> Result<Combined> {
    if pixel_weights.len() != frames.len()
        || frames
            .iter()
            .zip(pixel_weights)
            .any(|(f, p)| f.width != p.width || f.height != p.height)
    {
        return Err(Error::invalid("pixel weights must match the frames"));
    }
    combine_impl(frames, Some(pixel_weights), opts)
}

#[allow(
    clippy::too_many_lines,
    reason = "one pass over the pixels with the per-row accounting"
)]
fn combine_impl(
    frames: &[&Frame],
    pixel_weights: Option<&[&Frame]>,
    opts: &Options,
) -> Result<Combined> {
    let first = frames
        .first()
        .ok_or_else(|| Error::invalid("nothing to combine"))?;
    let (w, h) = (first.width, first.height);
    if frames.iter().any(|f| f.width != w || f.height != h) {
        return Err(Error::invalid("frames to combine differ in size"));
    }
    let n = frames.len();
    let weights: Vec<f32> = match &opts.weights {
        Some(v) if v.len() == n && v.iter().all(|x| x.is_finite() && *x > 0.0) => v.clone(),
        Some(_) => return Err(Error::invalid("weights must be positive, one per frame")),
        None => vec![1.0; n],
    };
    let rows: Vec<Row> = (0..h)
        .into_par_iter()
        .map(|y| {
            let slices: Vec<&[f32]> = frames.iter().map(|f| f.row(y)).collect();
            let pw: Vec<&[f32]> =
                pixel_weights.map_or_else(Vec::new, |p| p.iter().map(|f| f.row(y)).collect());
            let mut row = Row {
                mean: vec![f32::NAN; w],
                used: vec![0; w],
                low: vec![0; n],
                high: vec![0; n],
                valid: vec![0; n],
                low_map: if opts.maps { vec![0; w] } else { Vec::new() },
                high_map: if opts.maps { vec![0; w] } else { Vec::new() },
                weight_map: if opts.maps { vec![0.0; w] } else { Vec::new() },
            };
            let mut buf: Vec<Sample> = Vec::with_capacity(n);
            let mut scratch: Vec<f32> = Vec::with_capacity(n);
            for x in 0..w {
                buf.clear();
                for (i, sl) in slices.iter().enumerate() {
                    let v = sl[x];
                    let px = if pw.is_empty() { 1.0 } else { pw[i][x] };
                    if v.is_finite() && px.is_finite() && px > 0.0 {
                        buf.push(Sample {
                            v,
                            w: weights[i] * px,
                            frame: i as u32,
                            keep: true,
                        });
                    }
                }
                if buf.is_empty() {
                    continue;
                }
                for s in &buf {
                    row.valid[s.frame as usize] += 1;
                }
                reject(&mut buf, opts.rejection, opts.max_iterations, &mut scratch);
                let (mut sw, mut swv, mut cnt) = (0.0_f64, 0.0_f64, 0u16);
                let centre = if buf.iter().any(|s| !s.keep) {
                    kept_values(&buf, &mut scratch);
                    median_of(&mut scratch)
                } else {
                    0.0
                };
                for s in &buf {
                    if s.keep {
                        sw += f64::from(s.w);
                        swv += f64::from(s.w) * f64::from(s.v);
                        cnt += 1;
                    }
                }
                for s in buf.iter().filter(|s| !s.keep) {
                    if s.v < centre {
                        row.low[s.frame as usize] += 1;
                        if opts.maps {
                            row.low_map[x] += 1;
                        }
                    } else {
                        row.high[s.frame as usize] += 1;
                        if opts.maps {
                            row.high_map[x] += 1;
                        }
                    }
                }
                if opts.maps {
                    row.weight_map[x] = sw as f32;
                }
                if sw > 0.0 {
                    row.mean[x] = (swv / sw) as f32;
                    row.used[x] = cnt;
                }
            }
            row
        })
        .collect();
    let mut data = Vec::with_capacity(w * h);
    let mut used = Vec::with_capacity(w * h);
    let (mut low_map, mut high_map, mut weight_map) = (Vec::new(), Vec::new(), Vec::new());
    let mut rejected_low = vec![0u64; n];
    let mut rejected_high = vec![0u64; n];
    let mut valid = vec![0u64; n];
    for r in rows {
        data.extend_from_slice(&r.mean);
        used.extend_from_slice(&r.used);
        low_map.extend_from_slice(&r.low_map);
        high_map.extend_from_slice(&r.high_map);
        weight_map.extend_from_slice(&r.weight_map);
        for i in 0..n {
            rejected_low[i] += u64::from(r.low[i]);
            rejected_high[i] += u64::from(r.high[i]);
            valid[i] += u64::from(r.valid[i]);
        }
    }
    let mut mean = Frame::new(w, h, data).map_err(|e| Error::invalid(e.to_string()))?;
    mean.bayer = first.bayer;
    Ok(Combined {
        mean,
        used,
        rejected_low,
        rejected_high,
        valid,
        maps: opts.maps.then_some(Maps {
            low: low_map,
            high: high_map,
            weight: weight_map,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn noisy(w: usize, h: usize, level: f32, sigma: f32, rng: &mut Rng) -> Frame {
        let data = (0..w * h)
            .map(|_| rng.gauss(f64::from(level), f64::from(sigma)) as f32)
            .collect();
        Frame::new(w, h, data).expect("frame")
    }

    fn std_dev(f: &Frame) -> f32 {
        let n = f.data.len() as f32;
        let m = f.data.iter().sum::<f32>() / n;
        (f.data.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / n).sqrt()
    }

    fn mean_of(f: &Frame) -> f32 {
        f.data.iter().sum::<f32>() / f.data.len() as f32
    }

    const ALL: [Rejection; 4] = [
        Rejection::MinMax { low: 1, high: 1 },
        Rejection::Sigma {
            low: 3.0,
            high: 3.0,
        },
        Rejection::Winsorised {
            low: 3.0,
            high: 3.0,
        },
        Rejection::LinearFit {
            low: 5.0,
            high: 2.5,
        },
    ];

    #[test]
    fn plain_mean_and_weights() {
        let a = Frame::new(2, 1, vec![1.0, 10.0]).expect("a");
        let b = Frame::new(2, 1, vec![3.0, 20.0]).expect("b");
        let r = combine(
            &[&a, &b],
            &Options {
                rejection: Rejection::None,
                ..Options::default()
            },
        )
        .expect("combine");
        assert_eq!(r.mean.data, vec![2.0, 15.0]);
        let r = combine(
            &[&a, &b],
            &Options {
                rejection: Rejection::None,
                weights: Some(vec![3.0, 1.0]),
                ..Options::default()
            },
        )
        .expect("combine");
        assert!((r.mean.data[0] - 1.5).abs() < 1e-6);
        assert!((r.mean.data[1] - 12.5).abs() < 1e-6);
    }

    #[test]
    fn bad_inputs_are_errors() {
        let a = Frame::zeros(2, 2);
        let b = Frame::zeros(3, 2);
        assert!(combine(&[], &Options::default()).is_err());
        assert!(combine(&[&a, &b], &Options::default()).is_err());
        let o = Options {
            weights: Some(vec![1.0]),
            ..Options::default()
        };
        assert!(combine(&[&a, &a], &o).is_err());
    }

    #[test]
    fn nan_means_no_data() {
        let a = Frame::new(2, 1, vec![f32::NAN, 4.0]).expect("a");
        let b = Frame::new(2, 1, vec![6.0, 8.0]).expect("b");
        let r = combine(
            &[&a, &b],
            &Options {
                rejection: Rejection::None,
                ..Options::default()
            },
        )
        .expect("combine");
        assert_eq!(r.mean.data, vec![6.0, 6.0]);
        assert_eq!(r.used, vec![1, 2]);
        let all_nan = Frame::new(1, 1, vec![f32::NAN]).expect("n");
        let r = combine(&[&all_nan], &Options::default()).expect("combine");
        assert!(r.mean.data[0].is_nan());
    }

    #[test]
    fn pure_noise_loses_little_to_any_scheme() {
        // A scheme that rejects a lot of clean Gaussian data is throwing signal away.
        let mut rng = Rng::new(11);
        for n in [10usize, 20, 40] {
            let frames: Vec<Frame> = (0..n)
                .map(|_| noisy(60, 60, 1000.0, 10.0, &mut rng))
                .collect();
            let refs: Vec<&Frame> = frames.iter().collect();
            for rej in &ALL[1..] {
                let r = combine(
                    &refs,
                    &Options {
                        rejection: *rej,
                        ..Options::default()
                    },
                )
                .expect("combine");
                let total: u64 = r.valid.iter().sum();
                let rejected: u64 = r.rejected_low.iter().chain(&r.rejected_high).sum();
                let frac = rejected as f64 / total as f64;
                assert!(
                    frac < 0.03,
                    "{rej:?} n={n} rejected {frac:.4} of clean data"
                );
            }
        }
    }

    #[test]
    fn one_bright_outlier_is_rejected_by_the_right_schemes() {
        // 3-sigma clipping cannot catch a single outlier among few frames; the
        // winsorised scheme can. The truth is 1000.
        let mut rng = Rng::new(5);
        for (n, rej, must_reject) in [
            (
                8usize,
                Rejection::Winsorised {
                    low: 3.0,
                    high: 3.0,
                },
                true,
            ),
            (
                8,
                Rejection::Sigma {
                    low: 3.0,
                    high: 3.0,
                },
                false,
            ),
            (
                20,
                Rejection::Sigma {
                    low: 3.0,
                    high: 3.0,
                },
                true,
            ),
            (
                20,
                Rejection::LinearFit {
                    low: 5.0,
                    high: 2.5,
                },
                true,
            ),
        ] {
            let mut frames: Vec<Frame> = (0..n)
                .map(|_| noisy(40, 40, 1000.0, 5.0, &mut rng))
                .collect();
            // A "satellite trail" of 5000 ADU across a row of the third frame.
            for x in 0..40 {
                frames[2].data[10 * 40 + x] += 5000.0;
            }
            let refs: Vec<&Frame> = frames.iter().collect();
            let r = combine(
                &refs,
                &Options {
                    rejection: rej,
                    ..Options::default()
                },
            )
            .expect("combine");
            let row: f32 = r.mean.row(10).iter().sum::<f32>() / 40.0;
            let excess = row - 1000.0;
            if must_reject {
                assert!(excess.abs() < 3.0, "{rej:?} n={n}: row excess {excess}");
            } else {
                assert!(excess > 100.0, "{rej:?} n={n} unexpectedly clean: {excess}");
            }
        }
    }

    #[test]
    fn hot_pixel_across_frames_is_rejected() {
        let mut rng = Rng::new(9);
        let mut frames: Vec<Frame> = (0..12)
            .map(|_| noisy(30, 30, 500.0, 4.0, &mut rng))
            .collect();
        // Different frames each get a different cosmic-ray hit.
        for (i, f) in frames.iter_mut().enumerate() {
            f.data[(3 + i) * 30 + i] = 60_000.0;
        }
        let refs: Vec<&Frame> = frames.iter().collect();
        for rej in &ALL[2..] {
            let r = combine(
                &refs,
                &Options {
                    rejection: *rej,
                    ..Options::default()
                },
            )
            .expect("combine");
            for i in 0..12 {
                let v = r.mean.data[(3 + i) * 30 + i];
                assert!((v - 500.0).abs() < 10.0, "{rej:?} hit {i}: {v}");
            }
        }
    }

    #[test]
    fn noise_falls_as_the_square_root_of_the_frame_count() {
        let mut rng = Rng::new(3);
        let sigma = 20.0;
        let frames: Vec<Frame> = (0..64)
            .map(|_| noisy(120, 120, 800.0, sigma, &mut rng))
            .collect();
        let single = std_dev(&frames[0]);
        for n in [4usize, 16, 64] {
            let refs: Vec<&Frame> = frames.iter().take(n).collect();
            let r = combine(
                &refs,
                &Options {
                    rejection: Rejection::Winsorised {
                        low: 3.0,
                        high: 3.0,
                    },
                    ..Options::default()
                },
            )
            .expect("combine");
            let gain = single / std_dev(&r.mean);
            let expect = (n as f32).sqrt();
            assert!(
                (gain / expect - 1.0).abs() < 0.08,
                "n={n}: noise gain {gain:.3}, expected {expect:.3}"
            );
            assert!((mean_of(&r.mean) - 800.0).abs() < 1.0);
        }
    }

    #[test]
    fn per_frame_rejection_counts_find_the_bad_frame() {
        let mut rng = Rng::new(21);
        let mut frames: Vec<Frame> = (0..10)
            .map(|_| noisy(30, 30, 100.0, 2.0, &mut rng))
            .collect();
        for v in &mut frames[4].data {
            *v += 500.0;
        }
        let refs: Vec<&Frame> = frames.iter().collect();
        let r = combine(
            &refs,
            &Options {
                rejection: Rejection::Winsorised {
                    low: 3.0,
                    high: 3.0,
                },
                ..Options::default()
            },
        )
        .expect("combine");
        let frac = r.rejection_fraction();
        assert!(frac[4] > 0.95, "{frac:?}");
        assert!(r.rejected_high[4] > 0 && r.rejected_low[4] == 0);
        for (i, f) in frac.iter().enumerate() {
            if i != 4 {
                assert!(*f < 0.05, "frame {i}: {f}");
            }
        }
    }

    #[test]
    fn auto_picks_a_scheme_by_count() {
        assert_eq!(Rejection::auto(2), Rejection::None);
        assert!(matches!(Rejection::auto(8), Rejection::Winsorised { .. }));
        assert!(matches!(Rejection::auto(20), Rejection::Sigma { .. }));
        assert!(matches!(Rejection::auto(60), Rejection::LinearFit { .. }));
    }
}
