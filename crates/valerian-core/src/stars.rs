// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Background estimation, star detection and centroiding.
//!
//! The pipeline is the usual one:
//!
//! 1. **Background.** The sky level and noise are measured by a sigma-clipped
//!    median in tiles, the tile grid is median-filtered to suppress tiles
//!    dominated by a large object, and the grid is interpolated bilinearly to
//!    every pixel.
//! 2. **Detection.** The background-subtracted frame is smoothed with a
//!    Gaussian (a matched filter for a star) and every local maximum above a
//!    threshold in units of the smoothed noise becomes a candidate.
//! 3. **Centroiding.** Each candidate is refined by an iterative,
//!    Gaussian-weighted centroid (the "windowed" centroid of `SExtractor`), which
//!    reaches a few hundredths of a pixel on a good star. The same weighted
//!    second moments, corrected for the window, give the FWHM, the ellipticity
//!    and the orientation.
//! 4. **Vetting.** Candidates narrower than a real PSF (hot pixels, cosmic
//!    rays), too close to the border, or saturated are flagged or dropped.
//!
//! Pixel `(0, 0)` is centred on the coordinate `(0, 0)`.
//!
//! # References
//!
//! - Bertin, E. and Arnouts, S. (1996), `SExtractor`: software for source extraction, Astron. Astrophys. Suppl. 117, 393-404, doi:10.1051/aas:1996164 (the background mesh and the windowed centroid, implemented from the paper; no `SExtractor` code is used).
//! - Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).

use rayon::prelude::*;

use crate::stats;
use helia_core::Frame;

/// FWHM of a Gaussian of unit sigma.
const FWHM_PER_SIGMA: f64 = 2.354_820_045;

/// A sky-background model: the level and the noise at every pixel.
#[derive(Clone, Debug)]
pub struct Background {
    /// Background level at every pixel.
    pub level: Frame,
    /// Typical noise (one sigma) of a pixel of the original frame.
    pub rms: f32,
    /// Noise per tile, on the tile grid, row-major.
    pub tile_rms: Vec<f32>,
    /// Tile size in pixels.
    pub tile: usize,
}

fn tile_for(w: usize, h: usize) -> usize {
    (w.min(h) / 16).clamp(24, 256)
}

impl Background {
    /// Estimate the background of `frame`. `step` is 2 for raw Bayer data so
    /// that statistics compare like colours (a mosaic has a pixel-to-pixel
    /// pattern); pass 1 otherwise.
    #[must_use]
    pub fn estimate(frame: &Frame, step: usize) -> Self {
        let (w, h) = (frame.width, frame.height);
        let tile = tile_for(w, h);
        let (gx, gy) = (w.div_ceil(tile), h.div_ceil(tile));
        let step = step.max(1);
        // Per-tile clipped median and robust sigma.
        let stats_of: Vec<(f32, f32)> = (0..gx * gy)
            .into_par_iter()
            .map(|t| {
                let (tx, ty) = (t % gx, t / gx);
                let (x0, x1) = (tx * tile, ((tx + 1) * tile).min(w));
                let (y0, y1) = (ty * tile, ((ty + 1) * tile).min(h));
                let mut v = Vec::with_capacity((x1 - x0) * (y1 - y0) / step / step + 1);
                for y in (y0..y1).step_by(step) {
                    for x in (x0..x1).step_by(step) {
                        let p = frame.data[y * w + x];
                        if p.is_finite() {
                            v.push(p);
                        }
                    }
                }
                if v.len() < 16 {
                    return (f32::NAN, f32::NAN);
                }
                // Clip bright structure (stars) before taking the median.
                let (mut m, mut s) = stats::median_sigma(&v);
                for _ in 0..3 {
                    v.retain(|&p| (p - m).abs() <= 3.0 * s);
                    if v.len() < 16 {
                        break;
                    }
                    let (m2, s2) = stats::median_sigma(&v);
                    m = m2;
                    s = s2;
                }
                (m, s)
            })
            .collect();
        let mut levels: Vec<f32> = stats_of.iter().map(|p| p.0).collect();
        let tile_rms: Vec<f32> = stats_of.iter().map(|p| p.1).collect();
        // Median filter the grid so a tile on a big galaxy does not make a bump.
        levels = median_filter_grid(&levels, gx, gy);
        let global_rms = stats::median_finite(&tile_rms).max(f32::MIN_POSITIVE);
        let level = interpolate_grid(&levels, gx, gy, tile, w, h);
        Self {
            level,
            rms: global_rms,
            tile_rms,
            tile,
        }
    }
}

/// 3x3 median of a grid; NaN cells take the median of their finite neighbours.
fn median_filter_grid(v: &[f32], gx: usize, gy: usize) -> Vec<f32> {
    let mut out = vec![0.0; v.len()];
    for y in 0..gy {
        for x in 0..gx {
            let mut n = Vec::with_capacity(9);
            for yy in y.saturating_sub(1)..(y + 2).min(gy) {
                for xx in x.saturating_sub(1)..(x + 2).min(gx) {
                    let p = v[yy * gx + xx];
                    if p.is_finite() {
                        n.push(p);
                    }
                }
            }
            out[y * gx + x] = stats::median_in_place(&mut n);
        }
    }
    // Any cell still NaN (an all-NaN neighbourhood) takes the global median.
    let g = stats::median_finite(&out);
    for p in &mut out {
        if !p.is_finite() {
            *p = g;
        }
    }
    out
}

/// Bilinear interpolation of a tile grid (values at tile centres) to pixels.
fn interpolate_grid(v: &[f32], gx: usize, gy: usize, tile: usize, w: usize, h: usize) -> Frame {
    let t = tile as f32;
    let mut data = vec![0.0_f32; w * h];
    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = ((y as f32 + 0.5) / t - 0.5).clamp(0.0, (gy - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(gy - 1);
        let ay = fy - y0 as f32;
        for (x, out) in row.iter_mut().enumerate() {
            let fx = ((x as f32 + 0.5) / t - 0.5).clamp(0.0, (gx - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(gx - 1);
            let ax = fx - x0 as f32;
            let top = v[y0 * gx + x0] * (1.0 - ax) + v[y0 * gx + x1] * ax;
            let bot = v[y1 * gx + x0] * (1.0 - ax) + v[y1 * gx + x1] * ax;
            *out = top * (1.0 - ay) + bot * ay;
        }
    });
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// A detected star.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Star {
    /// Centroid x in pixels.
    pub x: f64,
    /// Centroid y in pixels.
    pub y: f64,
    /// Background-subtracted flux in an aperture of twice the FWHM (ADU).
    pub flux: f64,
    /// Peak background-subtracted pixel value (ADU).
    pub peak: f32,
    /// Full width at half maximum in pixels, from windowed second moments.
    pub fwhm: f32,
    /// `1 - b/a` of the second-moment ellipse: 0 round, towards 1 elongated.
    pub ellipticity: f32,
    /// Orientation of the major axis, radians from +x towards +y.
    pub theta: f32,
    /// Peak over the pixel noise.
    pub snr: f32,
    /// At or above the saturation level, or flat-topped.
    pub saturated: bool,
}

/// Settings for [`detect`].
#[derive(Clone, Debug)]
pub struct DetectOptions {
    /// Detection threshold, in sigmas of the *smoothed* noise.
    pub threshold: f32,
    /// Gaussian smoothing sigma in pixels before detection (the expected
    /// star sigma works best); 0 disables it.
    pub smoothing: f32,
    /// Smallest FWHM accepted, in pixels. Narrower things are hot pixels or
    /// cosmic rays.
    pub min_fwhm: f32,
    /// Largest FWHM accepted, in pixels.
    pub max_fwhm: f32,
    /// Ignore candidates closer than this to the frame edge.
    pub border: usize,
    /// Pixel value at which a star counts as saturated; `None` for never.
    pub saturation: Option<f32>,
    /// Keep at most this many stars, the brightest.
    pub max_stars: usize,
    /// Distance between pixels of one colour: 2 for raw Bayer, else 1.
    pub step: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            threshold: 5.0,
            smoothing: 1.5,
            min_fwhm: 1.2,
            max_fwhm: 25.0,
            border: 8,
            saturation: None,
            max_stars: 1000,
            step: 1,
        }
    }
}

/// The result of [`detect`].
#[derive(Clone, Debug)]
pub struct Detection {
    /// Stars, brightest first.
    pub stars: Vec<Star>,
    /// Sky level (median of the background model) in ADU.
    pub sky: f32,
    /// Pixel noise in ADU.
    pub noise: f32,
}

impl Detection {
    /// Median FWHM of the stars that are not saturated, or NaN.
    #[must_use]
    pub fn median_fwhm(&self) -> f32 {
        let v: Vec<f32> = self
            .stars
            .iter()
            .filter(|s| !s.saturated)
            .map(|s| s.fwhm)
            .collect();
        stats::median_finite(&v)
    }

    /// Median ellipticity of the stars that are not saturated, or NaN.
    #[must_use]
    pub fn median_ellipticity(&self) -> f32 {
        let v: Vec<f32> = self
            .stars
            .iter()
            .filter(|s| !s.saturated)
            .map(|s| s.ellipticity)
            .collect();
        stats::median_finite(&v)
    }
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as i64;
    let mut k: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f32 = k.iter().sum();
    for v in &mut k {
        *v /= s;
    }
    k
}

/// Separable Gaussian smoothing with edge replication.
fn smooth(f: &Frame, sigma: f32) -> Frame {
    let k = gaussian_kernel(sigma);
    let r = (k.len() / 2) as i64;
    let (w, h) = (f.width, f.height);
    let mut tmp = vec![0.0_f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &f.data[y * w..(y + 1) * w];
        for (x, out) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (j, &kv) in k.iter().enumerate() {
                let xx = (x as i64 + j as i64 - r).clamp(0, w as i64 - 1) as usize;
                s += kv * src[xx];
            }
            *out = s;
        }
    });
    let mut out = vec![0.0_f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (j, &kv) in k.iter().enumerate() {
                let yy = (y as i64 + j as i64 - r).clamp(0, h as i64 - 1) as usize;
                s += kv * tmp[yy * w + x];
            }
            *o = s;
        }
    });
    Frame {
        width: w,
        height: h,
        data: out,
        bayer: None,
    }
}

/// Sums of the Gaussian-weighted window around `(cx, cy)`: weight, first and
/// second moments of the displacement.
struct Window {
    mx: f64,
    my: f64,
    cxx: f64,
    cyy: f64,
    cxy: f64,
}

fn window(res: &Frame, cx: f64, cy: f64, sigma_w: f64) -> Option<Window> {
    let (w, h) = (res.width as i64, res.height as i64);
    let reach = (4.0 * sigma_w).ceil() as i64;
    let (ix, iy) = (cx.round() as i64, cy.round() as i64);
    let (mut sv, mut sx, mut sy) = (0.0_f64, 0.0_f64, 0.0_f64);
    let (mut sxx, mut syy, mut sxy) = (0.0_f64, 0.0_f64, 0.0_f64);
    for y in (iy - reach).max(0)..=(iy + reach).min(h - 1) {
        for x in (ix - reach).max(0)..=(ix + reach).min(w - 1) {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let wgt = (-(dx * dx + dy * dy) / (2.0 * sigma_w * sigma_w)).exp();
            let v = f64::from(res.data[(y * w + x) as usize]) * wgt;
            sv += v;
            sx += v * dx;
            sy += v * dy;
            sxx += v * dx * dx;
            syy += v * dy * dy;
            sxy += v * dx * dy;
        }
    }
    if !sv.is_finite() || sv <= 0.0 {
        return None;
    }
    let (mx, my) = (sx / sv, sy / sv);
    Some(Window {
        mx,
        my,
        cxx: sxx / sv - mx * mx,
        cyy: syy / sv - my * my,
        cxy: sxy / sv - mx * my,
    })
}

/// Measure a star near `(x0, y0)` on a background-subtracted frame.
/// Returns `None` if the measurement does not converge to a sensible star.
fn measure(
    res: &Frame,
    x0: f64,
    y0: f64,
    fwhm_guess: f64,
    noise: f32,
    opts: &DetectOptions,
) -> Option<Star> {
    let (w, h) = (res.width as i64, res.height as i64);
    let mut sigma_w = (fwhm_guess / FWHM_PER_SIGMA).clamp(1.0, 6.0);
    let (mut cx, mut cy) = (x0, y0);
    let mut cov = (0.0, 0.0, 0.0);
    for _ in 0..30 {
        let win = window(res, cx, cy, sigma_w)?;
        cx += win.mx;
        cy += win.my;
        if (cx - x0).hypot(cy - y0) > 4.0 * sigma_w + 2.0 {
            return None;
        }
        // A Gaussian of variance s^2 seen through a window of variance sw^2
        // looks like 1/(1/s^2 + 1/sw^2); invert that for each eigenvalue.
        let inv_w2 = 1.0 / (sigma_w * sigma_w);
        let fix = |obs: f64| -> f64 {
            let inv = 1.0 / obs - inv_w2;
            if inv > 1e-9 { 1.0 / inv } else { f64::NAN }
        };
        let mean = f64::midpoint(win.cxx, win.cyy);
        let diff = (0.5 * (win.cxx - win.cyy)).hypot(win.cxy);
        let (f1, f2) = (fix(mean + diff), fix(mean - diff));
        if !f1.is_finite() || !f2.is_finite() || f2 <= 0.0 {
            return None;
        }
        cov = (f1, f2, 0.5 * (2.0 * win.cxy).atan2(win.cxx - win.cyy));
        let sigma_new = f64::midpoint(f1, f2).sqrt().clamp(0.7, 8.0);
        let settled = win.mx.hypot(win.my) < 2e-4 && (sigma_new - sigma_w).abs() < 2e-3;
        sigma_w = sigma_new.max(1.0);
        if settled {
            break;
        }
    }
    let fwhm = FWHM_PER_SIGMA * f64::midpoint(cov.0, cov.1).sqrt();
    if !(f64::from(opts.min_fwhm)..=f64::from(opts.max_fwhm)).contains(&fwhm) {
        return None;
    }
    let ellip = 1.0 - (cov.1 / cov.0).sqrt();
    // Aperture flux and peak.
    let ap = fwhm.max(2.0);
    let reach = (ap + 1.0).ceil() as i64;
    let (ix, iy) = (cx.round() as i64, cy.round() as i64);
    let (mut flux, mut peak) = (0.0_f64, f32::MIN);
    for y in (iy - reach).max(0)..=(iy + reach).min(h - 1) {
        for x in (ix - reach).max(0)..=(ix + reach).min(w - 1) {
            let d = (x as f64 - cx).hypot(y as f64 - cy);
            let v = res.data[(y * w + x) as usize];
            if d <= ap {
                flux += f64::from(v);
            }
            if d <= 1.5 {
                peak = peak.max(v);
            }
        }
    }
    if !flux.is_finite() || flux <= 0.0 {
        return None;
    }
    Some(Star {
        x: cx,
        y: cy,
        flux,
        peak,
        fwhm: fwhm as f32,
        ellipticity: ellip as f32,
        theta: cov.2 as f32,
        snr: peak / noise.max(f32::MIN_POSITIVE),
        saturated: false,
    })
}

/// Detect and measure the stars in a frame.
///
/// The frame should be a single plane in ADU; for a colour image pass the
/// luminance or the green plane (see [`luminance`]).
#[must_use]
pub fn detect(frame: &Frame, opts: &DetectOptions) -> Detection {
    let (w, h) = (frame.width, frame.height);
    let bkg = Background::estimate(frame, opts.step);
    let sky = stats::median_finite(&stats::subsample(&bkg.level, 1 << 16));
    let res = Frame {
        width: w,
        height: h,
        data: frame
            .data
            .par_iter()
            .zip(&bkg.level.data)
            .map(|(&v, &b)| v - b)
            .collect(),
        bayer: None,
    };
    let sm = if opts.smoothing > 0.0 {
        smooth(&res, opts.smoothing)
    } else {
        res.clone()
    };
    // Noise of the smoothed frame, measured on it: interpolated or stacked
    // frames have correlated noise that a white-noise formula would underrate.
    let noise_s = if opts.smoothing > 0.0 {
        Background::estimate(&sm, opts.step).rms
    } else {
        bkg.rms
    };
    let limit = opts.threshold * noise_s;
    let b = opts.border.max(2);
    // Candidates: strict 5x5 local maxima above the limit.
    let rows: Vec<Vec<(usize, usize, f32)>> = (b..h.saturating_sub(b))
        .into_par_iter()
        .map(|y| {
            let mut out = Vec::new();
            for x in b..w.saturating_sub(b) {
                let v = sm.data[y * w + x];
                // Written so that NaN (no data) fails the test.
                if v.partial_cmp(&limit) != Some(std::cmp::Ordering::Greater) {
                    continue;
                }
                let mut is_max = true;
                'scan: for yy in y - 2..=y + 2 {
                    for xx in x - 2..=x + 2 {
                        if (xx, yy) != (x, y) {
                            let o = sm.data[yy * w + xx];
                            // Ties go to the first in raster order.
                            let beaten = match o.total_cmp(&v) {
                                std::cmp::Ordering::Greater => true,
                                std::cmp::Ordering::Equal => (yy, xx) < (y, x),
                                std::cmp::Ordering::Less => false,
                            };
                            if beaten || o.is_nan() {
                                is_max = false;
                                break 'scan;
                            }
                        }
                    }
                }
                if is_max {
                    out.push((x, y, v));
                }
            }
            out
        })
        .collect();
    let mut cands: Vec<(usize, usize, f32)> = rows.into_iter().flatten().collect();
    // Brightest first, so a cap on the count keeps the best.
    cands.sort_by(|a, c| c.2.total_cmp(&a.2));
    cands.truncate(opts.max_stars * 4);
    let guess = if opts.smoothing > 0.0 {
        f64::from(opts.smoothing) * FWHM_PER_SIGMA
    } else {
        3.0
    };
    let mut stars: Vec<Star> = cands
        .par_iter()
        .filter_map(|&(x, y, _)| {
            let mut s = measure(&res, x as f64, y as f64, guess, bkg.rms, opts)?;
            if let Some(sat) = opts.saturation {
                let raw = frame.data[y * w + x];
                s.saturated = raw >= sat;
            }
            Some(s)
        })
        .collect();
    // Two candidates that converged on the same star: keep the brighter.
    stars.sort_by(|a, c| c.flux.total_cmp(&a.flux));
    let mut kept: Vec<Star> = Vec::with_capacity(stars.len());
    for s in stars {
        if kept
            .iter()
            .all(|k| (k.x - s.x).hypot(k.y - s.y) > 0.5 * f64::from(k.fwhm.min(s.fwhm)))
        {
            kept.push(s);
        }
    }
    kept.truncate(opts.max_stars);
    Detection {
        stars: kept,
        sky,
        noise: bkg.rms,
    }
}

/// Mean of the colour planes, for detection on a colour image.
#[must_use]
pub fn luminance(planes: &[Frame]) -> Frame {
    let mut out = planes[0].clone();
    out.bayer = None;
    if planes.len() > 1 {
        let n = planes.len() as f32;
        out.data.par_iter_mut().enumerate().for_each(|(i, v)| {
            *v = planes.iter().map(|p| p.data[i]).sum::<f32>() / n;
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Rng, gaussian_field, gaussian_field_elliptical};

    fn noisy_field(
        w: usize,
        h: usize,
        level: f32,
        sigma_noise: f64,
        stars: &[(f64, f64, f64, f64)],
        seed: u64,
    ) -> Frame {
        let mut f = gaussian_field(w, h, level, stars);
        let mut rng = Rng::new(seed);
        for v in &mut f.data {
            *v += rng.gauss(0.0, sigma_noise) as f32;
        }
        f
    }

    fn random_stars(n: usize, w: usize, h: usize, seed: u64) -> Vec<(f64, f64, f64, f64)> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| {
                (
                    20.0 + rng.uniform() * (w as f64 - 40.0),
                    20.0 + rng.uniform() * (h as f64 - 40.0),
                    3000.0 + rng.uniform() * 40_000.0,
                    1.4 + rng.uniform() * 0.9,
                )
            })
            .collect()
    }

    #[test]
    fn background_follows_a_gradient_and_ignores_stars() {
        let (w, h) = (400, 300);
        let mut f = gaussian_field(w, h, 0.0, &random_stars(30, w, h, 3));
        let mut rng = Rng::new(4);
        for (i, v) in f.data.iter_mut().enumerate() {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            *v += 500.0 + 0.2 * x + 0.1 * y + rng.gauss(0.0, 5.0) as f32;
        }
        let bkg = Background::estimate(&f, 1);
        assert!((bkg.rms - 5.0).abs() < 0.8, "rms {}", bkg.rms);
        let mut err = 0.0_f32;
        for y in (0..h).step_by(7) {
            for x in (0..w).step_by(7) {
                let truth = 500.0 + 0.2 * x as f32 + 0.1 * y as f32;
                err = err.max((bkg.level.data[y * w + x] - truth).abs());
            }
        }
        assert!(err < 12.0, "background off by {err} ADU at worst");
    }

    #[test]
    fn finds_the_stars_and_centroids_them_precisely() {
        let (w, h) = (500, 400);
        let truth = random_stars(60, w, h, 12);
        let f = noisy_field(w, h, 300.0, 6.0, &truth, 13);
        let det = detect(&f, &DetectOptions::default());
        let mut matched = 0;
        let mut sq = 0.0;
        let mut worst = 0.0_f64;
        let mut bright = 0;
        for &(tx, ty, flux, sigma) in &truth {
            // Bright stars must be found, with sub-pixel centroids.
            let peak = flux / (2.0 * std::f64::consts::PI * sigma * sigma);
            let blended = truth
                .iter()
                .any(|o| (o.0 - tx, o.1 - ty) != (0.0, 0.0) && (o.0 - tx).hypot(o.1 - ty) < 12.0);
            if peak < 200.0 || blended {
                continue;
            }
            bright += 1;
            let near = det
                .stars
                .iter()
                .min_by(|a, b| {
                    (a.x - tx)
                        .hypot(a.y - ty)
                        .total_cmp(&(b.x - tx).hypot(b.y - ty))
                })
                .expect("some star");
            let d = (near.x - tx).hypot(near.y - ty);
            if d < 1.0 {
                matched += 1;
                sq += d * d;
                worst = worst.max(d);
            }
        }
        assert!(bright >= 20, "test field too faint: {bright}");
        assert!(matched >= bright - 2, "found {matched} of {bright}");
        let rms = (sq / f64::from(matched)).sqrt();
        assert!(rms < 0.03, "centroid rms {rms:.4} px (worst {worst:.4})");
        // Hardly anything beyond the real stars.
        assert!(
            det.stars.len() <= truth.len() + 3,
            "{} detections",
            det.stars.len()
        );
        assert!((det.sky - 300.0).abs() < 3.0);
        assert!((det.noise - 6.0).abs() < 1.0);
    }

    #[test]
    fn fwhm_and_ellipticity_are_recovered() {
        let w = 200;
        let mut stars = Vec::new();
        for (i, sigma) in [1.3, 1.7, 2.1, 2.6].iter().enumerate() {
            stars.push((40.0 + 40.0 * i as f64 + 0.37, 60.0 + 0.61, 60_000.0, *sigma));
        }
        let f = noisy_field(w, 120, 200.0, 4.0, &stars, 5);
        let det = detect(&f, &DetectOptions::default());
        assert_eq!(det.stars.len(), 4);
        let mut sorted = det.stars.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        for (s, &(_, _, _, sigma)) in sorted.iter().zip(&stars) {
            let want = 2.354_82 * sigma;
            assert!(
                (f64::from(s.fwhm) / want - 1.0).abs() < 0.05,
                "FWHM {} vs {want}",
                s.fwhm
            );
            assert!(
                s.ellipticity < 0.05,
                "round star has ellipticity {}",
                s.ellipticity
            );
        }
    }

    #[test]
    fn elongated_stars_report_their_ellipticity_and_angle() {
        // Gaussian star with sigma 3 along x and 2 along y.
        let mut f = gaussian_field_elliptical(160, 120, 100.0, &[(80.3, 60.6, 60_000.0, 3.0, 2.0)]);
        let mut rng = Rng::new(8);
        for v in &mut f.data {
            *v += rng.gauss(0.0, 3.0) as f32;
        }
        let det = detect(
            &f,
            &DetectOptions {
                smoothing: 2.5,
                ..DetectOptions::default()
            },
        );
        assert_eq!(det.stars.len(), 1, "{:?}", det.stars);
        let s = det.stars[0];
        let want = 1.0 - 2.0 / 3.0;
        assert!(
            (f64::from(s.ellipticity) - want).abs() < 0.03,
            "{} vs {want}",
            s.ellipticity
        );
        assert!(s.theta.abs() < 0.05, "angle {}", s.theta);
        let fwhm = 2.354_82 * f64::midpoint(9.0, 4.0).sqrt();
        assert!(
            (f64::from(s.fwhm) / fwhm - 1.0).abs() < 0.05,
            "{} vs {fwhm}",
            s.fwhm
        );
        assert!((s.x - 80.3).abs() < 0.03 && (s.y - 60.6).abs() < 0.03);
    }

    #[test]
    fn hot_pixels_and_cosmic_rays_are_not_stars() {
        let mut f = noisy_field(200, 150, 300.0, 5.0, &[(100.3, 75.6, 30_000.0, 1.8)], 21);
        for &(x, y) in &[(30usize, 30usize), (60, 100), (150, 40)] {
            f.data[y * 200 + x] += 5000.0;
        }
        let det = detect(&f, &DetectOptions::default());
        assert_eq!(det.stars.len(), 1, "{:?}", det.stars);
        assert!((det.stars[0].x - 100.3).abs() < 0.05);
    }

    #[test]
    fn saturation_is_flagged() {
        let f = gaussian_field(
            100,
            100,
            100.0,
            &[(50.0, 50.0, 2_000_000.0, 2.0), (20.0, 20.0, 8000.0, 2.0)],
        );
        let mut capped = f.clone();
        capped.data.iter_mut().for_each(|v| *v = v.min(60_000.0));
        let det = detect(
            &capped,
            &DetectOptions {
                saturation: Some(59_000.0),
                ..DetectOptions::default()
            },
        );
        let sat = det.stars.iter().filter(|s| s.saturated).count();
        assert_eq!(sat, 1);
        assert!(det.median_fwhm().is_finite());
    }

    #[test]
    fn detection_on_an_empty_field_finds_nothing() {
        let f = noisy_field(300, 200, 500.0, 8.0, &[], 77);
        let det = detect(&f, &DetectOptions::default());
        assert!(det.stars.is_empty(), "{} false detections", det.stars.len());
    }
}
