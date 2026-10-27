// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Robust statistics on pixel data. NaN means "no data" throughout and is
//! ignored.
//!
//! # References
//!
//! - Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).
//! - Rousseeuw, P. J. and Croux, C. (1993), Alternatives to the median absolute deviation, J. Am. Stat. Assoc. 88, 1273-1283, doi:10.1080/01621459.1993.10476408 (the 1.4826 factor).
//! - Immerkaer, J. (1996), Fast noise variance estimation, Comput. Vis. Image Underst. 64, 300-302, doi:10.1006/cviu.1996.0060 (noise from local differences; here a robust variant on pixel differences).

use helia_core::Frame;

/// Standard deviation of a normal distribution from its median absolute deviation.
pub const MAD_TO_SIGMA: f32 = 1.482_602_2;

/// Median of `v`, reordering it. NaN is not expected here; see [`median_finite`].
#[must_use]
pub fn median_in_place(v: &mut [f32]) -> f32 {
    let n = v.len();
    if n == 0 {
        return f32::NAN;
    }
    let mid = n / 2;
    let (lo, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    let hi = *m;
    if n % 2 == 1 {
        hi
    } else {
        let below = lo.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        f32::midpoint(below, hi)
    }
}

/// Median of the finite values of `v`, or NaN if there are none.
#[must_use]
pub fn median_finite(v: &[f32]) -> f32 {
    let mut w: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    median_in_place(&mut w)
}

/// Median and robust sigma (scaled median absolute deviation) of the finite
/// values of `v`.
#[must_use]
pub fn median_sigma(v: &[f32]) -> (f32, f32) {
    let mut w: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    let m = median_in_place(&mut w);
    if w.is_empty() {
        return (f32::NAN, f32::NAN);
    }
    for x in &mut w {
        *x = (*x - m).abs();
    }
    (m, MAD_TO_SIGMA * median_in_place(&mut w))
}

/// The `p`-th percentile (`0..=100`) of the finite values of `v`, by nearest rank.
#[must_use]
pub fn percentile(v: &[f32], p: f32) -> f32 {
    let mut w: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if w.is_empty() {
        return f32::NAN;
    }
    let k = (((w.len() - 1) as f32) * p.clamp(0.0, 100.0) / 100.0).round() as usize;
    *w.select_nth_unstable_by(k, f32::total_cmp).1
}

/// Every `stride`-th sample of a frame, so statistics on a 60-megapixel frame
/// stay cheap. At most about `limit` samples are returned.
#[must_use]
pub fn subsample(f: &Frame, limit: usize) -> Vec<f32> {
    let stride = (f.data.len() / limit.max(1)).max(1);
    f.data.iter().step_by(stride).copied().collect()
}

/// Mean and standard deviation after iterative sigma clipping at `k` sigma
/// about the median. Returns `(mean, sigma, kept)`.
#[must_use]
pub fn clipped_mean_sigma(v: &[f32], k: f32, iterations: usize) -> (f32, f32, usize) {
    let mut w: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    let (mut mean, mut sigma) = (f32::NAN, f32::NAN);
    for _ in 0..iterations.max(1) {
        if w.is_empty() {
            break;
        }
        let n = w.len() as f64;
        let m: f64 = w.iter().map(|&x| f64::from(x)).sum::<f64>() / n;
        let var: f64 = w.iter().map(|&x| (f64::from(x) - m).powi(2)).sum::<f64>() / n;
        mean = m as f32;
        sigma = var.sqrt() as f32;
        let centre = {
            let mut c = w.clone();
            median_in_place(&mut c)
        };
        let before = w.len();
        w.retain(|&x| (x - centre).abs() <= k * sigma);
        if w.len() == before {
            break;
        }
    }
    (mean, sigma, w.len())
}

/// Background level and noise of a frame: the median and robust sigma of a
/// subsample after clipping out bright structure.
#[must_use]
pub fn sky_and_noise(f: &Frame) -> (f32, f32) {
    let s = subsample(f, 1 << 20);
    let (m, sigma) = median_sigma(&s);
    let kept: Vec<f32> = s
        .iter()
        .copied()
        .filter(|&x| x.is_finite() && (x - m).abs() <= 3.0 * sigma)
        .collect();
    let (m2, s2) = median_sigma(&kept);
    (m2, s2.max(f32::MIN_POSITIVE))
}

/// Pixel noise of a frame from the median absolute deviation of horizontal
/// pixel differences, which cancels smooth structure and is robust to stars.
/// For Bayer data pass `step = 2` so differences compare like colours.
#[must_use]
pub fn noise_from_differences(f: &Frame, step: usize) -> f32 {
    let step = step.max(1);
    let mut d = Vec::with_capacity(f.data.len() / 4);
    let rows = (f.height / 512).max(1);
    for y in (0..f.height).step_by(rows) {
        let row = f.row(y);
        for x in (0..f.width.saturating_sub(step)).step_by(3) {
            let (a, b) = (row[x], row[x + step]);
            if a.is_finite() && b.is_finite() {
                d.push(b - a);
            }
        }
    }
    let (_, s) = median_sigma(&d);
    s / std::f32::consts::SQRT_2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even_and_nan() {
        assert!((median_in_place(&mut [3.0, 1.0, 2.0]) - 2.0).abs() < 1e-6);
        assert!((median_in_place(&mut [4.0, 1.0, 3.0, 2.0]) - 2.5).abs() < 1e-6);
        assert!((median_finite(&[1.0, f32::NAN, 3.0, f32::INFINITY]) - 2.0).abs() < 1e-6);
        assert!(median_finite(&[f32::NAN]).is_nan());
    }

    #[test]
    fn mad_sigma_of_known_values() {
        let v: Vec<f32> = (-50..=50).map(|i| i as f32).collect();
        let (m, s) = median_sigma(&v);
        assert!(m.abs() < 1e-6);
        assert!((s - MAD_TO_SIGMA * 25.0).abs() < 0.1, "{s}");
    }

    #[test]
    fn percentile_endpoints() {
        let v: Vec<f32> = (0..101).map(|i| i as f32).collect();
        assert!((percentile(&v, 0.0)).abs() < 1e-6);
        assert!((percentile(&v, 100.0) - 100.0).abs() < 1e-6);
        assert!((percentile(&v, 50.0) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn clipping_ignores_an_outlier() {
        let mut v = vec![10.0_f32; 50];
        for (i, x) in v.iter_mut().enumerate() {
            *x += (i % 5) as f32 * 0.1;
        }
        v.push(1000.0);
        let (mean, _, kept) = clipped_mean_sigma(&v, 3.0, 5);
        assert_eq!(kept, 50);
        assert!((mean - 10.2).abs() < 0.05, "{mean}");
    }
}
