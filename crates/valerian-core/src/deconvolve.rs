// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sharpening a stacked image with the stars' own point spread function.
//!
//! A stack of 10 s sub-frames from a small alt-azimuth telescope has stars wider
//! and less round than the optics and the sky would give: seeing, tracking
//! error, field rotation during the exposure and the lens aberrations all blur
//! the sub-frames, and averaging them cannot undo that. Deconvolution can, to
//! the extent that the blur is known. Here it is measured from the image
//! itself:
//!
//! 1. **Stars.** The stars are detected on the luminance ([`crate::stars`]).
//!    Those fit to measure a PSF are bright (signal-to-noise above the limit),
//!    unsaturated, of normal width and isolated.
//! 2. **PSF per tile.** The image is cut into tiles. For each tile the PSF is
//!    the per-pixel median of the sub-pixel-centred, unit-sum stamps of its
//!    nearest good stars (the sub-pixel shift is a Catmull-Rom interpolation),
//!    clipped at zero, made point-symmetric and faded to zero in the wings by a
//!    flat-topped cosine window (a Gaussian taper would narrow the core and
//!    leave the PSF's elongation under-estimated). A tile with too few stars
//!    widens its search until it has enough, so the PSF follows the field's
//!    elongation and aberration from the middle to the corners.
//! 3. **Clipped stars.** A saturated star peaks at the clip level whatever its
//!    brightness, so the highest peaks of an image with clipped stars pile up
//!    at one value per colour plane. Pixels at the top of that pile are taken
//!    as clipped and given zero weight in the fit; the core they hide is
//!    first filled with the measured PSF scaled to the intact wings, then
//!    rebuilt by the iterations. Stars near the clip level are not used to
//!    measure the PSF. A clipped core fed to Richardson-Lucy as if it were
//!    data pulls the fit down to its flat top and leaves a black core inside a
//!    dark ring.
//! 4. **Richardson-Lucy.** Each colour plane of each tile (with a margin) is
//!    deconvolved by iterating the Richardson-Lucy update with the
//!    total-variation damping of Dey et al. (2006), which keeps the noise from
//!    turning into speckle. The tiles are blended with a linear ramp over their
//!    overlap so no seam shows. Around stars that stand far above the rest of
//!    the image the result is held to the lowest value the input takes nearby,
//!    so a PSF that is a little too wide in the wings cannot dig a dark ring.
//! 5. **Round stars.** In [`Mode::Round`] the PSF's elongation is first blurred
//!    away along its narrow axis ([`DeconvolveOptions::roundness`]); the
//!    iterations then sharpen with a round kernel. Deconvolving by the
//!    elongated remainder instead converges ten times more slowly along the
//!    long axis than the short one, and 20 iterations leave the stars as
//!    elongated as they were.
//!
//! RL conserves flux and cannot make a negative value, so it is run on the
//! linear stack before any stretch. It sharpens the stars and the nebula by the
//! same measure and raises the noise; the iteration count and the damping set
//! the trade, and neither the stars' ringing nor the noise is hidden by the
//! report.
//!
//! The RL update is written here (`rl_weighted`); the kernel convolution is
//! [`helia_core::edit::deconv::PsfKernel`] (the owner's own code, shared with
//! Helia). The PSF measurement, the tiling, the clipped-star handling and the
//! blending are here.
//!
//! # References
//!
//! - Richardson, W. H. (1972), Bayesian-based iterative method of image
//!   restoration, J. Opt. Soc. Am. 62, 55-59, doi:10.1364/JOSA.62.000055.
//! - Lucy, L. B. (1974), An iterative technique for the rectification of
//!   observed distributions, Astron. J. 79, 745, doi:10.1086/111605.
//! - Dey, N. et al. (2006), Richardson-Lucy algorithm with total variation
//!   regularization for 3D confocal microscope deconvolution, Microsc. Res.
//!   Tech. 69, 260-266, doi:10.1002/jemt.20294.
//! - Hanisch, R. J., White, R. L. and Gilliland, R. L. (1997), Deconvolution of
//!   Hubble Space Telescope images and spectra, in Deconvolution of Images and
//!   Spectra, 2nd ed., Academic Press, 310-356 (bad pixels taken out of the
//!   likelihood).
//! - Snyder, D. L., Hammoud, A. M. and White, R. L. (1993), Image recovery from
//!   data acquired with a charge-coupled-device camera, J. Opt. Soc. Am. A 10,
//!   1014-1023, doi:10.1364/JOSAA.10.001014 (the expectation-maximisation form
//!   of Richardson-Lucy for CCD data, from which the missing-data update
//!   follows).
//! - Catmull, E. and Rom, R. (1974), A class of local interpolating splines,
//!   in Computer Aided Geometric Design, Academic Press, 317-326.

use helia_core::Frame;
use helia_core::edit::deconv::{PsfKernel, RATIO_EPS, TV_EPS2, TV_FLOOR, protection};
use rayon::prelude::*;

use crate::cancel::CancelToken;
use crate::error::{Error, Result};
use crate::fits::Image;
use crate::stars::{DetectOptions, Star, detect, luminance};
use crate::stats;

/// FWHM of a Gaussian of unit sigma.
const FWHM_PER_SIGMA: f64 = 2.354_820_045;

/// Share of the PSF radius inside which the kernel is left untouched; the
/// noisy wings beyond it fall to zero on a cosine.
const TAPER_FLAT: f64 = 0.55;

/// Variance in pixels squared of the pre-blur along the long axis of the PSF
/// (the kernel must be sampled on the pixel grid).
const PRE_BLUR_FLOOR: f64 = 0.15;

/// Where the PSF comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PsfSource {
    /// Measured from the stars of each tile.
    Measured,
    /// A round Gaussian of this FWHM in pixels, the same everywhere.
    Gaussian(f32),
}

/// What the stars are deconvolved to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Round stars of one width everywhere. Each tile's PSF is first made
    /// rounder by a blur along its narrow axis (by
    /// [`DeconvolveOptions::roundness`]), then deconvolved by the Gaussian that
    /// leaves stars of FWHM [`DeconvolveOptions::target_fwhm`].
    #[default]
    Round,
    /// Deconvolve by the whole measured PSF: the sharpest result, which
    /// keeps whatever elongation survives the iterations.
    Full,
}

/// How clipped (saturated) pixels are found.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Saturation {
    /// From the image: the value the brightest stars pile up at, per plane.
    #[default]
    Auto,
    /// Every plane clips at this pixel value.
    Level(f32),
    /// No pixel is treated as clipped.
    Off,
}

/// Settings for [`deconvolve`].
#[derive(Clone, Debug)]
pub struct DeconvolveOptions {
    /// Where the PSF comes from.
    pub psf: PsfSource,
    /// What the stars are deconvolved to.
    pub mode: Mode,
    /// Star FWHM in pixels that [`Mode::Round`] aims for; `None` takes
    /// [`DeconvolveOptions::target_ratio`] of the narrowest PSF axis of any tile.
    pub target_fwhm: Option<f32>,
    /// The automatic target as a share of the narrowest PSF axis, 0.3 to 1.
    pub target_ratio: f32,
    /// Clipped pixels are left out of the fit and rebuilt from the star's wings.
    pub saturation: Saturation,
    /// Richardson-Lucy iterations: more is sharper and noisier.
    pub iterations: u32,
    /// Total-variation damping `lambda` of Dey et al.; 0 is plain RL.
    pub tv: f32,
    /// How much of the PSF's elongation [`Mode::Round`] blurs away before it
    /// sharpens, 0 to 1: 1 rounds the stars fully, 0 leaves all of it to the
    /// iterations (sharpest, rounder only as far as they get).
    pub roundness: f32,
    /// Strength 0 to 1 of the guard that keeps the result from falling below
    /// the input around bright stars (the dark ring); 0 turns it off.
    pub ring_guard: f32,
    /// Tiles across and down; `None` cuts the image into tiles of about 480 pixels.
    pub tiles: Option<(usize, usize)>,
    /// Half-width of the PSF kernel in pixels.
    pub psf_radius: usize,
    /// Most stars used for one tile's PSF.
    pub psf_stars: usize,
    /// Fewest stars a tile may be measured from; below this its search widens.
    pub min_psf_stars: usize,
    /// Faintest star used, as peak over pixel noise.
    pub psf_min_snr: f32,
    /// Star detection.
    pub detect: DetectOptions,
    /// Stops the run between tiles; never set by default.
    pub cancel: CancelToken,
}

impl Default for DeconvolveOptions {
    fn default() -> Self {
        Self {
            psf: PsfSource::Measured,
            mode: Mode::Round,
            target_fwhm: None,
            target_ratio: 0.7,
            saturation: Saturation::Auto,
            iterations: 20,
            tv: 0.002,
            roundness: 0.5,
            ring_guard: 1.0,
            tiles: None,
            psf_radius: 10,
            psf_stars: 40,
            min_psf_stars: 8,
            psf_min_snr: 25.0,
            detect: DetectOptions::default(),
            cancel: CancelToken::default(),
        }
    }
}

/// The PSF used for one tile.
#[derive(Clone, Debug, PartialEq)]
pub struct TileReport {
    /// First column of the tile.
    pub x0: usize,
    /// First row of the tile.
    pub y0: usize,
    /// One past the last column.
    pub x1: usize,
    /// One past the last row.
    pub y1: usize,
    /// Stars the PSF was measured from (0 for a Gaussian).
    pub stars: usize,
    /// FWHM of the PSF in pixels, from its second moments.
    pub fwhm: f32,
    /// `1 - b/a` of the PSF's second-moment ellipse.
    pub ellipticity: f32,
}

/// The deconvolved image and what was used.
#[derive(Clone, Debug)]
pub struct Deconvolved {
    /// The sharpened image: the input header plus a history line.
    pub image: Image,
    /// One entry per tile, row-major.
    pub tiles: Vec<TileReport>,
    /// The star FWHM in pixels that [`Mode::Round`] deconvolved to.
    pub target_fwhm: Option<f32>,
}

/// A point-symmetric kernel summing to one.
#[derive(Clone)]
struct Kernel {
    radius: usize,
    full: Vec<f32>,
}

impl Kernel {
    fn gaussian(fwhm: f32, max_radius: usize) -> Self {
        let sigma = f64::from(fwhm) / FWHM_PER_SIGMA;
        let radius = ((3.5 * sigma).ceil() as usize).clamp(2, max_radius.max(2));
        let side = 2 * radius + 1;
        let mut full = vec![0.0_f32; side * side];
        let mut sum = 0.0_f64;
        for j in 0..side {
            for i in 0..side {
                let (dx, dy) = (i as f64 - radius as f64, j as f64 - radius as f64);
                let v = (-0.5 * (dx * dx + dy * dy) / (sigma * sigma)).exp();
                full[j * side + i] = v as f32;
                sum += v;
            }
        }
        for v in &mut full {
            *v = (f64::from(*v) / sum) as f32;
        }
        Self { radius, full }
    }

    /// Covariance `(xx, yy, xy)` in pixels squared from Gaussian-windowed second
    /// moments, the way the stars are measured ([`crate::stars`]): the window is
    /// set from the first estimate and its effect removed, so the wings of a
    /// kernel do not widen it.
    fn covariance(&self) -> (f64, f64, f64) {
        let side = 2 * self.radius + 1;
        let moments = |sw: f64| {
            let (mut m, mut sxx, mut syy, mut sxy) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
            for j in 0..side {
                for i in 0..side {
                    let (dx, dy) = (i as f64 - self.radius as f64, j as f64 - self.radius as f64);
                    let v = f64::from(self.full[j * side + i])
                        * if sw.is_finite() {
                            (-0.5 * (dx * dx + dy * dy) / (sw * sw)).exp()
                        } else {
                            1.0
                        };
                    m += v;
                    sxx += v * dx * dx;
                    syy += v * dy * dy;
                    sxy += v * dx * dy;
                }
            }
            (sxx / m, syy / m, sxy / m)
        };
        let (mut sxx, mut syy, mut sxy) = moments(f64::INFINITY);
        let mut window = f64::midpoint(sxx, syy).sqrt().clamp(0.8, 4.0);
        for _ in 0..4 {
            let (wxx, wyy, wxy) = moments(window);
            // A Gaussian window of sigma w narrows a Gaussian of variance s to
            // 1 / (1 / s + 1 / w^2); undo it along each principal axis.
            let un = |v: f64| {
                let inv = 1.0 / v.max(1e-9) - 1.0 / (window * window);
                if inv > 1e-9 { 1.0 / inv } else { v }
            };
            let tr = f64::midpoint(wxx, wyy);
            let df = (0.25 * (wxx - wyy) * (wxx - wyy) + wxy * wxy).sqrt();
            let (ua, ub) = (un(tr + df), un((tr - df).max(1e-9)));
            let ang = 0.5 * (2.0 * wxy).atan2(wxx - wyy);
            let (c, sn) = (ang.cos(), ang.sin());
            sxx = ua * c * c + ub * sn * sn;
            syy = ua * sn * sn + ub * c * c;
            sxy = (ua - ub) * c * sn;
            window = f64::midpoint(sxx, syy).sqrt().clamp(0.8, 6.0);
        }
        (sxx, syy, sxy)
    }

    /// FWHM and ellipticity of the covariance's ellipse.
    fn shape(&self) -> (f32, f32) {
        let (a, b) = eigen(self.covariance());
        let fwhm = (a * b).sqrt().sqrt() * FWHM_PER_SIGMA;
        (fwhm as f32, (1.0 - (b / a).sqrt()) as f32)
    }

    /// An elliptical Gaussian of this covariance, integrated over each pixel.
    fn elliptical(cov: (f64, f64, f64), radius: usize) -> Self {
        let (sxx, syy, sxy) = cov;
        let det = (sxx * syy - sxy * sxy).max(1e-12);
        let (ixx, iyy, ixy) = (syy / det, sxx / det, -sxy / det);
        let side = 2 * radius + 1;
        let mut full = vec![0.0_f32; side * side];
        let mut sum = 0.0_f64;
        for j in 0..side {
            for i in 0..side {
                let mut acc = 0.0_f64;
                for sj in 0..3 {
                    for si in 0..3 {
                        let dx = i as f64 - radius as f64 + (f64::from(si) - 1.0) / 3.0;
                        let dy = j as f64 - radius as f64 + (f64::from(sj) - 1.0) / 3.0;
                        acc += (-0.5 * (ixx * dx * dx + 2.0 * ixy * dx * dy + iyy * dy * dy)).exp();
                    }
                }
                full[j * side + i] = (acc / 9.0) as f32;
                sum += acc / 9.0;
            }
        }
        for v in &mut full {
            *v = (f64::from(*v) / sum) as f32;
        }
        Self { radius, full }
    }
}

/// The larger and smaller eigenvalue of a covariance `(xx, yy, xy)`.
fn eigen((sxx, syy, sxy): (f64, f64, f64)) -> (f64, f64) {
    let mean = f64::midpoint(sxx, syy);
    let diff = (0.25 * (sxx - syy) * (sxx - syy) + sxy * sxy).sqrt();
    ((mean + diff).max(1e-12), (mean - diff).max(1e-12))
}

/// Catmull-Rom weights for a fractional offset `t` in `[0, 1)`.
fn cubic(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// The value of `f` at the fractional position `(x, y)`, by Catmull-Rom
/// interpolation with clamped edges.
fn sample(f: &Frame, x: f64, y: f64) -> f32 {
    let (xf, yf) = (x.floor(), y.floor());
    let (wx, wy) = (cubic((x - xf) as f32), cubic((y - yf) as f32));
    let (xi, yi) = (xf as i64, yf as i64);
    let mut acc = 0.0_f32;
    for (j, wyj) in wy.iter().enumerate() {
        let yy = (yi + j as i64 - 1).clamp(0, f.height as i64 - 1) as usize;
        let mut row = 0.0_f32;
        for (i, wxi) in wx.iter().enumerate() {
            let xx = (xi + i as i64 - 1).clamp(0, f.width as i64 - 1) as usize;
            row += wxi * f.data[yy * f.width + xx];
        }
        acc += wyj * row;
    }
    acc
}

/// A unit-sum stamp of the star, centred on its centroid, with the local sky
/// (the median of the outer ring) removed. `None` when it has no positive flux.
fn stamp(f: &Frame, s: &Star, radius: usize) -> Option<Vec<f32>> {
    let side = 2 * radius + 1;
    let mut v = Vec::with_capacity(side * side);
    for j in 0..side {
        for i in 0..side {
            let (dx, dy) = (i as f64 - radius as f64, j as f64 - radius as f64);
            v.push(sample(f, s.x + dx, s.y + dy));
        }
    }
    let mut ring: Vec<f32> = (0..side * side)
        .filter(|&k| {
            let (dx, dy) = (
                (k % side) as f64 - radius as f64,
                (k / side) as f64 - radius as f64,
            );
            dx.hypot(dy) >= radius as f64 - 1.0
        })
        .map(|k| v[k])
        .collect();
    let sky = stats::median_in_place(&mut ring);
    for x in &mut v {
        *x -= sky;
    }
    let sum: f32 = v.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return None;
    }
    for x in &mut v {
        *x /= sum;
    }
    Some(v)
}

/// The median of stamps, clipped, tapered, point-symmetric and unit-sum.
fn combine_stamps(stamps: &[Vec<f32>], radius: usize) -> Kernel {
    let side = 2 * radius + 1;
    let n = side * side;
    let mut med = vec![0.0_f32; n];
    let mut col = vec![0.0_f32; stamps.len()];
    for (k, m) in med.iter_mut().enumerate() {
        for (c, s) in col.iter_mut().zip(stamps) {
            *c = s[k];
        }
        *m = stats::median_in_place(&mut col).max(0.0);
    }
    // Point symmetry: RL uses the kernel as its own transpose.
    let sym: Vec<f32> = (0..n)
        .map(|k| f32::midpoint(med[k], med[n - 1 - k]))
        .collect();
    // Flat-topped (Tukey) window: a Gaussian taper would narrow the core and
    // the kernel would under-correct the elongation it is meant to remove.
    let flat = TAPER_FLAT * radius as f64;
    let mut full = vec![0.0_f32; n];
    let mut sum = 0.0_f64;
    for (k, f) in full.iter_mut().enumerate() {
        let (dx, dy) = (
            (k % side) as f64 - radius as f64,
            (k / side) as f64 - radius as f64,
        );
        let r = dx.hypot(dy);
        let w = if r <= flat {
            1.0
        } else {
            let t = ((r - flat) / (radius as f64 + 1.0 - flat)).min(1.0);
            f64::midpoint(1.0, (std::f64::consts::PI * t).cos())
        };
        *f = (f64::from(sym[k]) * w) as f32;
        sum += f64::from(*f);
    }
    for f in &mut full {
        *f = (f64::from(*f) / sum.max(1e-30)) as f32;
    }
    Kernel { radius, full }
}

/// Stars fit to measure a PSF: bright, unsaturated, of normal width, isolated.
fn good_stars<'a>(
    stars: &'a [Star],
    median_fwhm: f32,
    clip: Option<f32>,
    opts: &DeconvolveOptions,
) -> Vec<&'a Star> {
    let iso = (opts.psf_radius + 2) as f64;
    stars
        .iter()
        .filter(|s| {
            !s.saturated
                && clip.is_none_or(|c| s.peak < NEAR_CLIP * c)
                && s.snr >= opts.psf_min_snr
                && s.fwhm >= 0.5 * median_fwhm
                && s.fwhm <= 2.0 * median_fwhm
        })
        .filter(|s| {
            !stars.iter().any(|o| {
                !std::ptr::eq(*s, o) && (o.x - s.x).hypot(o.y - s.y) < iso && o.peak > 0.05 * s.peak
            })
        })
        .collect()
}

/// Tiles across and down for an image of `w` by `h`.
fn grid(w: usize, h: usize, tiles: Option<(usize, usize)>) -> (usize, usize) {
    tiles.unwrap_or_else(|| (w.div_ceil(480).max(1), h.div_ceil(480).max(1)))
}

fn blend_weights(lo: usize, hi: usize, from: usize, to: usize, len: usize, pad: usize) -> Vec<f32> {
    // Ramp up over the overlap at an interior start and down at an interior end.
    let mut w = vec![1.0_f32; to - from];
    let ramp = |k: usize| (k as f32 + 1.0) / (2.0 * pad as f32 + 1.0);
    if lo > 0 {
        for (k, v) in w.iter_mut().take(2 * pad).enumerate() {
            *v = ramp(k);
        }
    }
    if hi < len {
        for (k, v) in w.iter_mut().rev().take(2 * pad).enumerate() {
            *v = ramp(k);
        }
    }
    w
}

/// Clip level of one plane, or `None` when the brightest stars do not pile up.
///
/// A saturated star peaks at the clip level whatever its true brightness, so
/// the highest peaks of an image with clipped stars sit in a tight cluster
/// (within 4 per cent, at least five of them), where those of an image
/// without clipping fall away smoothly: at least five of them within 4 per
/// cent, and at least three times as many as in the 4 per cent band below.
/// and flat on top. The level is the top of that cluster.
fn clip_level(p: &Frame) -> Option<f32> {
    let (w, h) = (p.width, p.height);
    let top = p
        .data
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f32::NEG_INFINITY, f32::max);
    if !top.is_finite() || top <= 0.0 || w < 7 || h < 7 {
        return None;
    }
    let mut cand: Vec<(f32, usize, usize)> = Vec::new();
    for y in 3..h - 3 {
        for x in 3..w - 3 {
            let v = p.data[y * w + x];
            if v < 0.5 * top || !v.is_finite() {
                continue;
            }
            let mut best = true;
            'win: for yy in y - 3..=y + 3 {
                for xx in x - 3..=x + 3 {
                    if p.data[yy * w + xx] > v {
                        best = false;
                        break 'win;
                    }
                }
            }
            if best {
                cand.push((v, x, y));
            }
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    // One peak per star: a flat top is many equal pixels.
    let mut peaks: Vec<(f32, usize, usize)> = Vec::new();
    for c in cand {
        if peaks.len() >= 60 {
            break;
        }
        if peaks
            .iter()
            .all(|q| c.1.abs_diff(q.1) > 6 || c.2.abs_diff(q.2) > 6)
        {
            peaks.push(c);
        }
    }
    (0..peaks.len().min(8)).find_map(|i| {
        let v = peaks[i].0;
        let band = |lo: f32, hi: f32| {
            peaks
                .iter()
                .filter(|q| q.0 >= lo * v && q.0 < hi * v)
                .count()
        };
        let n = band(CLIP_CLUSTER, 1.0 + 1e-6);
        let next = band(CLIP_CLUSTER - 0.04, CLIP_CLUSTER);
        if n < CLIP_PEAKS || n < 3 * next {
            return None;
        }
        // A clipped star is flat on top: its neighbours sit within 3 per cent.
        let mut flat: Vec<usize> = peaks
            .iter()
            .filter(|q| q.0 >= CLIP_CLUSTER * v && q.0 < v * (1.0 + 1e-6))
            .map(|&(pv, x, y)| {
                let mut c = 0;
                for yy in y - 3..=y + 3 {
                    for xx in x - 3..=x + 3 {
                        if p.data[yy * w + xx] >= 0.97 * pv {
                            c += 1;
                        }
                    }
                }
                c
            })
            .collect();
        flat.sort_unstable();
        (flat[flat.len() / 2] >= CLIP_FLAT).then_some(v)
    })
}

/// Stars peaking above this share of the clip level are not used for the PSF:
/// their wings are broadened by the bright core.
const NEAR_CLIP: f32 = 0.5;

/// Pixels within 3 per cent of the peak (itself included) that make a top flat.
const CLIP_FLAT: usize = 4;
/// Peaks that must pile up for the level to count.
const CLIP_PEAKS: usize = 5;
/// Peaks within this share of the top one count as piled up at the clip level.
const CLIP_CLUSTER: f32 = 0.96;
/// Pixels above this share of the clip level are taken as clipped.
const CLIP_SHARE: f32 = 0.9;

/// Weight of every pixel of a plane: 0 where the value is clipped (within one
/// pixel of a pixel at or above the clip share) or missing, else 1. `None`
/// when nothing is left out.
fn data_weights(p: &Frame, level: Option<f32>) -> Option<Vec<f32>> {
    let (w, h) = (p.width, p.height);
    let cut = level.map(|l| CLIP_SHARE * l);
    let bad: Vec<bool> = p
        .data
        .iter()
        .map(|&v| !v.is_finite() || cut.is_some_and(|c| v >= c))
        .collect();
    if !bad.iter().any(|&b| b) {
        return None;
    }
    let mut out = vec![1.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            if !p.data[k].is_finite() {
                out[k] = 0.0;
            } else if cut.is_some_and(|c| p.data[k] >= c) {
                for yy in y.saturating_sub(1)..(y + 2).min(h) {
                    for xx in x.saturating_sub(1)..(x + 2).min(w) {
                        out[yy * w + xx] = 0.0;
                    }
                }
            }
        }
    }
    Some(out)
}

/// Largest clipped blob, in pixels across, that is rebuilt as a star.
const MAX_BLOB: usize = 16;
/// A rebuilt core is held to this many times the brightest clipped pixel of its blob.
const FILL_CAP: f32 = 2.5;
/// How far beyond a clipped blob the wings are read to scale the star.
const RING: usize = 4;

/// Replace each clipped blob (zero weight, positive value) by the star it hides.
///
/// The star's profile is the measured PSF centred on the blob; its amplitude
/// is the least-squares fit to the good pixels in a ring `RING` pixels wide
/// round the blob, where the wings are intact. Each blob pixel takes the
/// larger of its clipped value and the model, so the core starts near its
/// true height and Richardson-Lucy, which gives a zero-weight pixel no data of
/// its own, has only to refine it. Blobs too large to be a star (a bright
/// nebula) are left alone.
fn fill_clipped(obs: &mut [f32], weight: &[f32], w: usize, h: usize, psf: &Kernel) {
    let side = 2 * psf.radius + 1;
    let model = Frame {
        width: side,
        height: side,
        data: psf.full.clone(),
        bayer: None,
    };
    let r = psf.radius as f64;
    let mut seen = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || weight[start] != 0.0 || obs[start] <= 0.0 {
            continue;
        }
        let mut blob = Vec::new();
        seen[start] = true;
        stack.push(start);
        while let Some(k) = stack.pop() {
            blob.push(k);
            let (x, y) = (k % w, k / w);
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    let n = yy * w + xx;
                    if !seen[n] && weight[n] == 0.0 && obs[n] > 0.0 {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
        let (mut sx, mut sy, mut sv) = (0.0_f64, 0.0_f64, 0.0_f64);
        for &k in &blob {
            let (x, y) = (k % w, k / w);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            let v = f64::from(obs[k]);
            sx += v * x as f64;
            sy += v * y as f64;
            sv += v;
        }
        if x1 - x0 >= MAX_BLOB || y1 - y0 >= MAX_BLOB || sv <= 0.0 {
            continue;
        }
        let (cx, cy) = (sx / sv, sy / sv);
        let at = |x: usize, y: usize| sample(&model, r + x as f64 - cx, r + y as f64 - cy);
        let mut ring: Vec<(usize, f64)> = Vec::new();
        for y in y0.saturating_sub(RING)..(y1 + RING + 1).min(h) {
            for x in x0.saturating_sub(RING)..(x1 + RING + 1).min(w) {
                let k = y * w + x;
                if weight[k] > 0.5 && !blob.contains(&k) {
                    ring.push((k, f64::from(at(x, y))));
                }
            }
        }
        if ring.len() < 8 {
            continue;
        }
        // The nebula or sky under the star: the lowest tenth of the ring.
        let mut vals: Vec<f32> = ring.iter().map(|&(k, _)| obs[k]).collect();
        vals.sort_by(f32::total_cmp);
        let bg = f64::from(vals[vals.len() / 10]);
        let (num, den) = ring.iter().fold((0.0_f64, 0.0_f64), |(a, b), &(k, m)| {
            (a + (f64::from(obs[k]) - bg).max(0.0) * m, b + m * m)
        });
        if den <= 0.0 || num <= 0.0 {
            continue;
        }
        let amp = num / den;
        let cap = f64::from(FILL_CAP) * blob.iter().map(|&k| f64::from(obs[k])).fold(0.0, f64::max);
        for &k in &blob {
            let m = (bg + amp * f64::from(at(k % w, k / w))).min(cap);
            if m > f64::from(obs[k]) {
                obs[k] = m as f32;
            }
        }
    }
}

/// The plane as the deconvolution sees it: offset to be non-negative, missing
/// samples zero.
fn obs_full(p: &Frame, offset: f32) -> Vec<f32> {
    p.data
        .iter()
        .map(|&v| {
            if v.is_finite() {
                (v + offset).max(0.0)
            } else {
                0.0
            }
        })
        .collect()
}

/// Divergence of the unit gradient of `u` at `(x, y)`, the total-variation
/// term of Dey et al. (2006): forward differences, clamped borders.
fn tv_divergence(u: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let at = |xx: usize, yy: usize| u[yy.min(h - 1) * w + xx.min(w - 1)];
    let unit = |xx: usize, yy: usize| {
        let c = at(xx, yy);
        let (gx, gy) = (at(xx + 1, yy) - c, at(xx, yy + 1) - c);
        let n = (gx * gx + gy * gy + TV_EPS2).sqrt();
        (gx / n, gy / n)
    };
    let (nx, ny) = unit(x, y);
    let (px, _) = unit(x.saturating_sub(1), y);
    let (_, py) = unit(x, y.saturating_sub(1));
    (nx - px) + (ny - py)
}

/// Richardson-Lucy with total-variation damping where some pixels carry no
/// information: `weight` is 0 for clipped or missing pixels, 1 elsewhere.
///
/// The update is `u * K^T(weight * f / (K u)) / K^T(weight) / (1 - tv * div)`.
/// With every weight 1 it is the plain damped update of Richardson (1972),
/// Lucy (1974) and Dey et al. (2006). A zero weight takes the pixel out of
/// the likelihood, so a clipped core neither pulls the fit down to its
/// clipped value nor leaves a hole: the estimate there is set by the star's
/// wings, which is how data with bad pixels are handled in Richardson-Lucy
/// restoration (Hanisch, White and Gilliland 1997; the missing-data form of
/// the expectation-maximisation derivation of Snyder et al. 1993).
fn rl_weighted(
    obs: &[f32],
    weight: Option<&[f32]>,
    w: usize,
    h: usize,
    kernel: &PsfKernel,
    iterations: u32,
    tv: f32,
) -> Vec<f32> {
    let norm = weight.map(|m| kernel.apply(m, w, h));
    let mut est = obs.to_vec();
    for _ in 0..iterations {
        let blur = kernel.apply(&est, w, h);
        let ratio: Vec<f32> = obs
            .par_iter()
            .zip(&blur)
            .enumerate()
            .map(|(i, (&o, &b))| weight.map_or(1.0, |m| m[i]) * o / (b + RATIO_EPS))
            .collect();
        let corr = kernel.apply(&ratio, w, h);
        let prev = est.clone();
        est.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, e) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let mut c = corr[i];
                if let Some(n) = &norm {
                    c = if n[i] > 1e-3 { c / n[i] } else { 1.0 };
                }
                let denom = if tv > 0.0 {
                    (1.0 - tv * tv_divergence(&prev, w, h, x, y)).max(TV_FLOOR)
                } else {
                    1.0
                };
                *e = (prev[i] * c / denom).max(0.0);
            }
        });
    }
    est
}

/// Deconvolve every plane of `image`.
///
/// # Errors
/// [`Error::Invalid`] for unusable settings or a frame with no planes,
/// [`Error::NoMatch`] when too few stars can measure a PSF (use
/// [`PsfSource::Gaussian`]) and [`Error::Cancelled`].
#[allow(
    clippy::too_many_lines,
    reason = "measure, then deconvolve per tile and plane"
)]
pub fn deconvolve(
    image: &Image,
    opts: &DeconvolveOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Deconvolved> {
    if image.planes.is_empty() {
        return Err(Error::invalid("the image has no planes"));
    }
    if opts.iterations == 0 || opts.iterations > 200 {
        return Err(Error::invalid("iterations must be from 1 to 200"));
    }
    if !(0.0..=1.0).contains(&opts.tv) {
        return Err(Error::invalid("the TV damping must be from 0 to 1"));
    }
    if !(0.0..=1.0).contains(&opts.roundness) {
        return Err(Error::invalid("the roundness must be from 0 to 1"));
    }
    if !(0.0..=1.0).contains(&opts.ring_guard) {
        return Err(Error::invalid("the ring guard must be from 0 to 1"));
    }
    if !(0.3..=1.0).contains(&opts.target_ratio) {
        return Err(Error::invalid("the target ratio must be from 0.3 to 1"));
    }
    if let Some(t) = opts.target_fwhm
        && !(0.8..=12.0).contains(&t)
    {
        return Err(Error::invalid(
            "the target FWHM must be from 0.8 to 12 pixels",
        ));
    }
    if !(3..=24).contains(&opts.psf_radius) {
        return Err(Error::invalid("the PSF radius must be from 3 to 24 pixels"));
    }
    if let PsfSource::Gaussian(f) = opts.psf
        && !(0.8..=12.0).contains(&f)
    {
        return Err(Error::invalid("the PSF FWHM must be from 0.8 to 12 pixels"));
    }
    let (w, h) = (image.width(), image.height());
    let lum = luminance(&image.planes);
    let (gx, gy) = grid(w, h, opts.tiles);
    if gx == 0 || gy == 0 || gx > w || gy > h {
        return Err(Error::invalid("the tile grid does not fit the image"));
    }
    // PSF per tile.
    let mut kernels: Vec<Kernel> = Vec::with_capacity(gx * gy);
    let mut pre_blur: Vec<Option<Kernel>> = Vec::new();
    let mut tiles: Vec<TileReport> = Vec::with_capacity(gx * gy);
    let levels: Vec<Option<f32>> = image
        .planes
        .iter()
        .map(|p| match opts.saturation {
            Saturation::Auto => clip_level(p),
            Saturation::Level(l) => Some(l),
            Saturation::Off => None,
        })
        .collect();
    let known: Vec<f32> = levels.iter().flatten().copied().collect();
    let lum_clip = (!known.is_empty()).then(|| known.iter().sum::<f32>() / known.len() as f32);
    if lum_clip.is_some() {
        log(&format!(
            "clipped stars found: clip level {}; the clipped cores are rebuilt from the wings",
            levels
                .iter()
                .map(|l| l.map_or("none".to_owned(), |v| format!("{v:.0}")))
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    let det = detect(&lum, &opts.detect);
    let median_fwhm = det.median_fwhm();
    let good = if opts.psf == PsfSource::Measured {
        let g = good_stars(&det.stars, median_fwhm, lum_clip, opts);
        log(&format!(
            "{} stars detected, {} fit to measure a PSF (median FWHM {:.2} px)",
            det.stars.len(),
            g.len(),
            median_fwhm
        ));
        if g.len() < opts.min_psf_stars {
            return Err(Error::NoMatch(format!(
                "only {} stars are fit to measure a PSF and {} are needed; lower --psf-min-snr, \
                 or give the PSF with --psf-fwhm",
                g.len(),
                opts.min_psf_stars
            )));
        }
        g
    } else {
        Vec::new()
    };
    // Sky-subtracted luminance for the stamps is made by `stamp` itself.
    let (tw, th) = (w.div_ceil(gx), h.div_ceil(gy));
    for iy in 0..gy {
        for ix in 0..gx {
            opts.cancel.check()?;
            let (x0, x1) = (ix * tw, ((ix + 1) * tw).min(w));
            let (y0, y1) = (iy * th, ((iy + 1) * th).min(h));
            let (cx, cy) = (0.5 * (x0 + x1) as f64, 0.5 * (y0 + y1) as f64);
            let (kernel, used) = match opts.psf {
                PsfSource::Gaussian(f) => (Kernel::gaussian(f, opts.psf_radius), 0),
                PsfSource::Measured => {
                    let mut reach = 0.75 * tw.max(th) as f64;
                    let sel = loop {
                        let mut near: Vec<&&Star> = good
                            .iter()
                            .filter(|s| (s.x - cx).hypot(s.y - cy) <= reach)
                            .collect();
                        if near.len() >= opts.min_psf_stars || reach > (w + h) as f64 {
                            near.sort_by(|a, b| b.peak.total_cmp(&a.peak));
                            near.truncate(opts.psf_stars);
                            break near;
                        }
                        reach *= 1.4;
                    };
                    let stamps: Vec<Vec<f32>> = sel
                        .iter()
                        .filter_map(|s| stamp(&lum, s, opts.psf_radius))
                        .collect();
                    if stamps.is_empty() {
                        return Err(Error::NoMatch("no star gave a usable PSF stamp".into()));
                    }
                    (combine_stamps(&stamps, opts.psf_radius), stamps.len())
                }
            };
            let (fwhm, ellipticity) = kernel.shape();
            tiles.push(TileReport {
                x0,
                y0,
                x1,
                y1,
                stars: used,
                fwhm,
                ellipticity,
            });
            kernels.push(kernel);
        }
    }
    let psfs: Vec<Kernel> = kernels.clone();
    // Round mode: swap each PSF for what is left once the round target is taken out.
    let mut target_fwhm = None;
    if opts.mode == Mode::Round {
        let covs: Vec<(f64, f64, f64)> = kernels.iter().map(Kernel::covariance).collect();
        let narrowest = covs
            .iter()
            .map(|&c| eigen(c).1)
            .fold(f64::INFINITY, f64::min);
        let want = opts.target_fwhm.map_or_else(
            || f64::from(opts.target_ratio) * narrowest,
            |f| (f64::from(f) / FWHM_PER_SIGMA).powi(2),
        );
        // The target cannot be narrower than the narrowest axis, or the kernel would be negative.
        let var = want.min(0.95 * narrowest);
        if opts.target_fwhm.is_some() && var < want {
            log(&format!(
                "the target FWHM is wider than the PSF allows; using {:.2} px",
                var.sqrt() * FWHM_PER_SIGMA
            ));
        }
        // Round the PSF first by a blur along its narrow axis (exact and noise
        // free), then sharpen with a round kernel, which Richardson-Lucy
        // converges on at the same rate in every direction. Deconvolving by the
        // elongated remainder instead needs 10 times the iterations along the
        // long axis, and stops short of round at the default count.
        let frac = f64::from(opts.roundness);
        let mut round = Vec::with_capacity(covs.len());
        for &(xx, yy, xy) in &covs {
            let (a, b) = eigen((xx, yy, xy));
            let ang = 0.5 * (2.0 * xy).atan2(xx - yy);
            let (c, sn) = (ang.cos(), ang.sin());
            let (ua, ub) = ((a - b) * frac, PRE_BLUR_FLOOR);
            let pre = (ua > 0.02).then(|| {
                Kernel::elliptical(
                    (
                        ub * c * c + ua * sn * sn,
                        ub * sn * sn + ua * c * c,
                        (ub - ua) * c * sn,
                    ),
                    opts.psf_radius,
                )
            });
            let (pxx, pyy, pxy) = pre.as_ref().map_or((0.0, 0.0, 0.0), Kernel::covariance);
            let (cxx, cyy, cxy) = (xx + pxx, yy + pyy, xy + pxy);
            let (ka, kb) = eigen((cxx - var, cyy - var, cxy));
            let kang = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
            let (kc, ks) = (kang.cos(), kang.sin());
            // A kernel needs at least a pixel's worth of width on every axis.
            let floor = 0.35_f64 * 0.35;
            let (ka, kb) = (ka.max(floor), kb.max(floor));
            round.push((
                pre,
                Kernel::elliptical(
                    (
                        ka * kc * kc + kb * ks * ks,
                        ka * ks * ks + kb * kc * kc,
                        (ka - kb) * kc * ks,
                    ),
                    opts.psf_radius,
                ),
            ));
        }
        let (pres, sharps): (Vec<_>, Vec<_>) = round.into_iter().unzip();
        pre_blur = pres;
        kernels = sharps;
        target_fwhm = Some((var.sqrt() * FWHM_PER_SIGMA) as f32);
        log(&format!(
            "deconvolving to round stars of FWHM {:.2} px",
            var.sqrt() * FWHM_PER_SIGMA
        ));
    }
    log(&format!(
        "{} tile(s) of about {tw} x {th} px; PSF FWHM {:.2} to {:.2} px, ellipticity {:.3} to {:.3}",
        tiles.len(),
        tiles.iter().map(|t| t.fwhm).fold(f32::INFINITY, f32::min),
        tiles.iter().map(|t| t.fwhm).fold(0.0, f32::max),
        tiles
            .iter()
            .map(|t| t.ellipticity)
            .fold(f32::INFINITY, f32::min),
        tiles.iter().map(|t| t.ellipticity).fold(0.0, f32::max),
    ));
    // Richardson-Lucy per plane and tile, blended over the overlaps.
    let pad = opts.psf_radius + 12;
    let guard_sigma = {
        let mut f: Vec<f32> = tiles.iter().map(|t| t.fwhm).collect();
        (stats::median_in_place(&mut f) / FWHM_PER_SIGMA as f32).max(0.8)
    };
    let mut planes = Vec::with_capacity(image.planes.len());
    // A pixel clipped in one colour is left out of all of them, so the cores
    // are rebuilt together and keep one colour.
    let merged: Option<Vec<f32>> = {
        let per: Vec<Option<Vec<f32>>> = image
            .planes
            .iter()
            .zip(&levels)
            .map(|(p, l)| data_weights(p, *l))
            .collect();
        per.iter().any(Option::is_some).then(|| {
            let mut m = vec![1.0_f32; w * h];
            for v in per.iter().flatten() {
                for (a, b) in m.iter_mut().zip(v) {
                    *a = a.min(*b);
                }
            }
            m
        })
    };
    for plane in &image.planes {
        let weights = &merged;
        let mut finite: Vec<f32> = plane
            .data
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        let k = finite.len() / 1000;
        let floor = if finite.is_empty() {
            0.0
        } else {
            *finite.select_nth_unstable_by(k, f32::total_cmp).1
        };
        let offset = (-floor).max(0.0);
        let mut acc = vec![0.0_f32; w * h];
        let mut wsum = vec![0.0_f32; w * h];
        for (ti, (t, kernel)) in tiles.iter().zip(&kernels).enumerate() {
            opts.cancel.check()?;
            let (fx0, fx1) = (t.x0.saturating_sub(pad), (t.x1 + pad).min(w));
            let (fy0, fy1) = (t.y0.saturating_sub(pad), (t.y1 + pad).min(h));
            let (tw2, th2) = (fx1 - fx0, fy1 - fy0);
            let mut obs = Vec::with_capacity(tw2 * th2);
            for y in fy0..fy1 {
                for x in fx0..fx1 {
                    let v = plane.data[y * w + x];
                    obs.push(if v.is_finite() {
                        (v + offset).max(0.0)
                    } else {
                        0.0
                    });
                }
            }
            let wt_tile: Option<Vec<f32>> = weights.as_ref().map(|m| {
                let mut v = Vec::with_capacity(tw2 * th2);
                for y in fy0..fy1 {
                    v.extend_from_slice(&m[y * w + fx0..y * w + fx1]);
                }
                v
            });
            if let Some(m) = &wt_tile {
                fill_clipped(&mut obs, m, tw2, th2, &psfs[ti]);
            }
            if let Some(Some(pre)) = pre_blur.get(ti) {
                let p = PsfKernel {
                    radius: pre.radius,
                    separable: None,
                    full: pre.full.clone(),
                };
                obs = p.apply(&obs, tw2, th2);
            }
            let k = PsfKernel {
                radius: kernel.radius,
                separable: None,
                full: kernel.full.clone(),
            };
            let out = rl_weighted(
                &obs,
                wt_tile.as_deref(),
                tw2,
                th2,
                &k,
                opts.iterations,
                opts.tv,
            );
            let wx = blend_weights(t.x0, t.x1, fx0, fx1, w, pad);
            let wy = blend_weights(t.y0, t.y1, fy0, fy1, h, pad);
            for (j, y) in (fy0..fy1).enumerate() {
                for (i, x) in (fx0..fx1).enumerate() {
                    let wt = wx[i] * wy[j];
                    acc[y * w + x] += (out[j * tw2 + i] - offset) * wt;
                    wsum[y * w + x] += wt;
                }
            }
        }
        let mut data: Vec<f32> = acc
            .iter()
            .zip(&wsum)
            .zip(&plane.data)
            .map(|((&a, &ws), &orig)| {
                if orig.is_finite() && ws > 0.0 {
                    a / ws
                } else {
                    f32::NAN
                }
            })
            .collect();
        if opts.ring_guard > 0.0 {
            // Around stars that stand far above the rest of the image, hold the
            // result to the lowest value the input takes nearby: a PSF that is a
            // little too wide in the wings of a bright star otherwise digs a
            // dark ring into its halo.
            let guard = protection(&obs_full(plane, offset), w, h, guard_sigma, opts.ring_guard);
            for ((d, &wt), &lo) in data.iter_mut().zip(&guard.weight).zip(&guard.lo) {
                if d.is_finite() {
                    let floor = lo - offset;
                    if *d < floor {
                        *d += wt * (floor - *d);
                    }
                }
            }
        }
        planes.push(Frame {
            width: w,
            height: h,
            data,
            bayer: plane.bayer,
        });
    }
    let mut header = image.header.clone();
    header.add_history(&format!(
        "deconvolved by Valerian: Richardson-Lucy, {} iterations, TV {}, {}",
        opts.iterations,
        opts.tv,
        match opts.psf {
            PsfSource::Measured => format!(
                "PSF measured per tile ({} tiles), {}",
                tiles.len(),
                target_fwhm.map_or_else(
                    || "full PSF".to_owned(),
                    |f| format!("round stars of FWHM {f:.2} px")
                )
            ),
            PsfSource::Gaussian(f) => format!("Gaussian PSF of FWHM {f:.2} px"),
        }
    ));
    Ok(Deconvolved {
        image: Image { header, planes },
        tiles,
        target_fwhm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits::Header;
    use crate::testutil::{Rng, gaussian_field_elliptical};

    fn silent(_: &str) {}

    /// Stars blurred by an elliptical Gaussian, with noise.
    fn blurred(w: usize, h: usize, sx: f64, sy: f64, noise: f64, seed: u64) -> Image {
        let mut rng = Rng::new(seed);
        let stars: Vec<(f64, f64, f64, f64, f64)> = (0..70)
            .map(|_| {
                (
                    20.0 + rng.uniform() * (w as f64 - 40.0),
                    20.0 + rng.uniform() * (h as f64 - 40.0),
                    4000.0 + rng.uniform() * 20000.0,
                    sx,
                    sy,
                )
            })
            .collect();
        let mut f = gaussian_field_elliptical(w, h, 500.0, &stars);
        for v in &mut f.data {
            *v += rng.gauss(0.0, noise) as f32;
        }
        Image::mono(f, Header::new())
    }

    fn shape(img: &Image) -> (f32, f32) {
        let d = detect(&luminance(&img.planes), &DetectOptions::default());
        (d.median_fwhm(), d.median_ellipticity())
    }

    #[test]
    fn a_gaussian_kernel_reports_its_own_fwhm() {
        for f in [2.5_f32, 3.5, 5.0] {
            let k = Kernel::gaussian(f, 10);
            let (fwhm, e) = k.shape();
            assert!((fwhm - f).abs() < 0.08 * f, "{fwhm} against {f}");
            assert!(e < 0.02, "{e}");
        }
    }

    #[test]
    fn deconvolving_sharpens_and_rounds_elongated_stars_and_keeps_the_flux() {
        let img = blurred(360, 300, 1.9, 1.3, 6.0, 1);
        let (f0, e0) = shape(&img);
        let r = deconvolve(&img, &DeconvolveOptions::default(), &mut silent).expect("deconvolve");
        let (f1, e1) = shape(&r.image);
        assert!(f1 < 0.85 * f0, "FWHM {f0} -> {f1}");
        assert!(e1 < e0, "ellipticity {e0} -> {e1}");
        let before: f64 = img.planes[0].data.iter().map(|&v| f64::from(v)).sum();
        let after: f64 = r.image.planes[0].data.iter().map(|&v| f64::from(v)).sum();
        assert!(
            (after / before - 1.0).abs() < 0.01,
            "flux {before} -> {after}"
        );
        assert!(r.image.planes[0].data.iter().all(|v| v.is_finite()));
        assert!(r.tiles.iter().all(|t| t.stars > 0 && t.fwhm > 2.0));
        assert!(
            r.image
                .header
                .cards()
                .iter()
                .any(|c| c.key == "HISTORY" && c.comment.contains("deconvolved by Valerian"))
        );
    }

    #[test]
    fn the_noise_does_not_run_away() {
        let img = blurred(300, 300, 1.6, 1.6, 8.0, 2);
        let r = deconvolve(&img, &DeconvolveOptions::default(), &mut silent).expect("deconvolve");
        let sky = |i: &Image| {
            let mut v: Vec<f32> = Vec::new();
            for y in 5..25 {
                for x in 5..25 {
                    v.push(i.planes[0].data[y * 300 + x]);
                }
            }
            crate::stats::median_sigma(&v).1
        };
        assert!(
            sky(&r.image) < 3.0 * sky(&img),
            "{} {}",
            sky(&r.image),
            sky(&img)
        );
    }

    #[test]
    fn tiles_leave_no_seam_in_flat_sky() {
        let img = blurred(400, 400, 1.7, 1.4, 5.0, 3);
        let opts = DeconvolveOptions {
            tiles: Some((2, 2)),
            ..DeconvolveOptions::default()
        };
        let r = deconvolve(&img, &opts, &mut silent).expect("deconvolve");
        assert_eq!(r.tiles.len(), 4);
        // The sky just either side of the seam at x = 200 differs by noise only.
        let col = |x: usize| {
            let mut v: Vec<f32> = (5..30)
                .map(|y| r.image.planes[0].data[y * 400 + x])
                .collect();
            crate::stats::median_in_place(&mut v)
        };
        assert!(
            (col(199) - col(201)).abs() < 4.0,
            "{} {}",
            col(199),
            col(201)
        );
    }

    #[test]
    fn missing_data_stays_missing() {
        let mut img = blurred(300, 300, 1.7, 1.4, 5.0, 4);
        for y in 0..30 {
            for x in 0..300 {
                img.planes[0].data[y * 300 + x] = f32::NAN;
            }
        }
        let r = deconvolve(&img, &DeconvolveOptions::default(), &mut silent).expect("deconvolve");
        assert!(r.image.planes[0].data[5 * 300 + 5].is_nan());
        assert!(r.image.planes[0].data[150 * 300 + 150].is_finite());
    }

    #[test]
    fn a_given_gaussian_psf_works_without_stars() {
        let flat = Image::mono(
            Frame {
                width: 120,
                height: 120,
                data: vec![300.0; 120 * 120],
                bayer: None,
            },
            Header::new(),
        );
        let opts = DeconvolveOptions {
            psf: PsfSource::Gaussian(3.0),
            ..DeconvolveOptions::default()
        };
        let r = deconvolve(&flat, &opts, &mut silent).expect("deconvolve");
        assert!(
            r.image.planes[0]
                .data
                .iter()
                .all(|v| (v - 300.0).abs() < 1.0)
        );
        // A measured PSF needs stars.
        assert!(matches!(
            deconvolve(&flat, &DeconvolveOptions::default(), &mut silent),
            Err(Error::NoMatch(_))
        ));
    }

    #[test]
    fn bad_settings_are_refused() {
        let img = blurred(120, 120, 1.7, 1.4, 5.0, 5);
        for opts in [
            DeconvolveOptions {
                iterations: 0,
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                tv: 2.0,
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                psf_radius: 1,
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                psf: PsfSource::Gaussian(0.1),
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                tiles: Some((0, 3)),
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                roundness: 1.5,
                ..DeconvolveOptions::default()
            },
            DeconvolveOptions {
                ring_guard: -0.1,
                ..DeconvolveOptions::default()
            },
        ] {
            assert!(deconvolve(&img, &opts, &mut silent).is_err());
        }
    }

    #[test]
    fn the_full_mode_is_sharper_and_the_round_mode_is_rounder() {
        let img = blurred(360, 300, 1.9, 1.3, 6.0, 8);
        let go = |mode| {
            let o = DeconvolveOptions {
                mode,
                ..DeconvolveOptions::default()
            };
            shape(&deconvolve(&img, &o, &mut silent).expect("deconvolve").image)
        };
        let (full, round) = (go(Mode::Full), go(Mode::Round));
        assert!(full.0 < round.0, "full {full:?} round {round:?}");
        assert!(round.1 < full.1, "full {full:?} round {round:?}");
    }

    /// Round stars on a flat sky, with noise, clipped at `clip`.
    fn clipped_field(w: usize, h: usize, clip: f32, top: f64, seed: u64) -> Image {
        let mut rng = Rng::new(seed);
        let stars: Vec<(f64, f64, f64, f64, f64)> = (0..90)
            .map(|_| {
                (
                    25.0 + rng.uniform() * (w as f64 - 50.0),
                    25.0 + rng.uniform() * (h as f64 - 50.0),
                    48_000.0 + rng.uniform().powi(3) * top,
                    1.6,
                    1.6,
                )
            })
            .collect();
        let mut f = gaussian_field_elliptical(w, h, 500.0, &stars);
        for v in &mut f.data {
            *v = (*v + rng.gauss(0.0, 6.0) as f32).min(clip);
        }
        Image::mono(f, Header::new())
    }

    /// Darkest value of `out` within `r` pixels of any pixel of `input` at or
    /// above `level`, against the sky.
    fn deepest_dip(input: &Image, out: &Image, level: f32, r: i64, sky: f32) -> f32 {
        let (w, h) = (input.width() as i64, input.height() as i64);
        let mut deepest = f32::INFINITY;
        for y in 0..h {
            for x in 0..w {
                if input.planes[0].data[(y * w + x) as usize] >= level {
                    for yy in (y - r).max(0)..(y + r + 1).min(h) {
                        for xx in (x - r).max(0)..(x + r + 1).min(w) {
                            if (xx - x).pow(2) + (yy - y).pow(2) <= r * r {
                                deepest = deepest.min(out.planes[0].data[(yy * w + xx) as usize]);
                            }
                        }
                    }
                }
            }
        }
        deepest - sky
    }

    #[test]
    fn the_clip_level_is_found_when_stars_pile_up_and_not_otherwise() {
        let clipped = clipped_field(400, 400, 30_000.0, 1_440_000.0, 11);
        let l = clip_level(&clipped.planes[0]).expect("clipped stars");
        assert!((l - 30_000.0).abs() < 300.0, "{l}");
        let soft = clipped_field(400, 400, 1e9, 300_000.0, 11);
        assert!(clip_level(&soft.planes[0]).is_none());
    }

    #[test]
    fn clipped_cores_are_rebuilt_and_leave_no_dark_ring() {
        let img = clipped_field(400, 400, 30_000.0, 1_440_000.0, 12);
        let off = DeconvolveOptions {
            saturation: Saturation::Off,
            ring_guard: 0.0,
            ..DeconvolveOptions::default()
        };
        let old = deconvolve(&img, &off, &mut silent).expect("deconvolve");
        let new = deconvolve(&img, &DeconvolveOptions::default(), &mut silent).expect("deconvolve");
        let dip_old = deepest_dip(&img, &old.image, 29_900.0, 7, 500.0);
        let dip_new = deepest_dip(&img, &new.image, 29_900.0, 7, 500.0);
        eprintln!("dip without {dip_old} with {dip_new}");
        // Sky noise is 6; a dip of 100 is a dark ring.
        assert!(dip_old < -100.0, "without the fix {dip_old}");
        assert!(dip_new > -60.0, "with it {dip_new}");
        // The core is rebuilt above the clip, not pulled down to it.
        let peak = |i: &Image| {
            i.planes[0]
                .data
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max)
        };
        assert!(peak(&new.image) > 36_000.0, "{}", peak(&new.image));
    }

    #[test]
    fn unclipped_images_are_left_to_the_plain_update() {
        let img = blurred(240, 240, 1.7, 1.4, 5.0, 13);
        let mut lines = Vec::new();
        let _ = deconvolve(&img, &DeconvolveOptions::default(), &mut |l| {
            lines.push(l.to_owned());
        })
        .expect("deconvolve");
        assert!(lines.iter().all(|l| !l.contains("clipped")), "{lines:?}");
    }

    #[test]
    fn the_weighted_update_with_no_missing_data_is_plain_richardson_lucy() {
        use helia_core::edit::deconv::{Deconvolve, Psf, richardson_lucy};
        let img = blurred(80, 70, 1.7, 1.4, 5.0, 14);
        let obs: Vec<f32> = img.planes[0].data.iter().map(|v| v.max(0.0)).collect();
        let k = PsfKernel::new(Psf::Gaussian { sigma: 1.5 }).expect("kernel");
        let params = Deconvolve {
            psf: Psf::Gaussian { sigma: 1.5 },
            iterations: 6,
            tv: 0.002,
            protect: 0.0,
        };
        let ones = vec![1.0_f32; 80 * 70];
        // The shared code takes no gradient across the first row and column, so
        // the TV term differs there; away from the border one step is the same.
        let one = richardson_lucy(
            &obs,
            80,
            70,
            &k,
            &Deconvolve {
                iterations: 1,
                ..params
            },
            None,
        );
        let one_w = rl_weighted(&obs, Some(&ones), 80, 70, &k, 1, 0.002);
        for y in 3..67 {
            for x in 3..77 {
                let i = y * 80 + x;
                assert!(
                    (one[i] - one_w[i]).abs() <= 1e-4 * one[i].abs().max(1.0),
                    "{} {}",
                    one[i],
                    one_w[i]
                );
            }
        }
        // Without TV the two are the same everywhere.
        let p0 = Deconvolve { tv: 0.0, ..params };
        let a0 = richardson_lucy(&obs, 80, 70, &k, &p0, None);
        let b0 = rl_weighted(&obs, Some(&ones), 80, 70, &k, 6, 0.0);
        for (x, y) in a0.iter().zip(&b0) {
            assert!((x - y).abs() <= 1e-4 * x.abs().max(1.0), "{x} {y}");
        }
    }

    #[test]
    fn a_clipped_blob_is_filled_with_the_star_it_hides() {
        let (w, h) = (60, 60);
        let k = Kernel::gaussian(3.6, 10);
        let sigma = 3.6_f64 / FWHM_PER_SIGMA;
        let mut star = vec![200.0_f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f64 - 30.0, y as f64 - 30.0);
                star[y * w + x] +=
                    (80_000.0 * (-0.5 * (dx * dx + dy * dy) / (sigma * sigma)).exp()) as f32;
            }
        }
        let clip = 30_000.0_f32;
        let mut obs: Vec<f32> = star.iter().map(|v| v.min(clip)).collect();
        let mut wide = vec![1.0_f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                if obs[y * w + x] >= 0.9 * clip {
                    for yy in y - 1..=y + 1 {
                        for xx in x - 1..=x + 1 {
                            wide[yy * w + xx] = 0.0;
                        }
                    }
                }
            }
        }
        fill_clipped(&mut obs, &wide, w, h, &k);
        let centre = obs[30 * w + 30];
        assert!(centre > 1.5 * clip, "{centre}");
        assert!(centre <= 2.5 * clip + 1.0, "{centre}");
    }

    #[test]
    fn a_blob_too_big_for_a_star_is_left_alone() {
        let (w, h) = (60, 60);
        let k = Kernel::gaussian(3.0, 10);
        let mut obs = vec![1000.0_f32; w * h];
        let mut weights = vec![1.0_f32; w * h];
        for y in 10..40 {
            for x in 10..40 {
                obs[y * w + x] = 20_000.0;
                weights[y * w + x] = 0.0;
            }
        }
        let before = obs.clone();
        fill_clipped(&mut obs, &weights, w, h, &k);
        assert_eq!(obs, before);
    }

    #[test]
    fn the_round_mode_rounds_elongated_stars() {
        let img = blurred(360, 300, 1.9, 1.3, 6.0, 8);
        let (_, e0) = shape(&img);
        let o = DeconvolveOptions {
            roundness: 1.0,
            ..DeconvolveOptions::default()
        };
        let (_, e1) = shape(&deconvolve(&img, &o, &mut silent).expect("deconvolve").image);
        eprintln!("ellipticity {e0} -> {e1}");
        assert!(e0 > 0.2, "{e0}");
        assert!(e1 < 0.5 * e0, "ellipticity {e0} -> {e1}");
        assert!(e1 < 0.12, "ellipticity {e0} -> {e1}");
    }

    #[test]
    fn the_psf_kernel_keeps_the_width_of_the_stars_it_is_made_from() {
        // A Gaussian taper would narrow the kernel and under-state the elongation.
        let img = blurred(360, 300, 1.9, 1.3, 0.5, 9);
        let o = DeconvolveOptions {
            mode: Mode::Full,
            ..DeconvolveOptions::default()
        };
        let r = deconvolve(&img, &o, &mut silent).expect("deconvolve");
        let truth_fwhm = (1.9_f32 * 1.3).sqrt() * 2.354_82;
        let truth_ell = 1.0 - 1.3 / 1.9;
        for t in &r.tiles {
            eprintln!("tile {} {}", t.fwhm, t.ellipticity);
            assert!(
                (t.fwhm - truth_fwhm).abs() < 0.06 * truth_fwhm,
                "{} {truth_fwhm}",
                t.fwhm
            );
            assert!(
                (t.ellipticity - truth_ell).abs() < 0.03,
                "{} {truth_ell}",
                t.ellipticity
            );
        }
    }

    #[test]
    fn cancelling_stops_the_run() {
        let img = blurred(200, 200, 1.7, 1.4, 5.0, 6);
        let opts = DeconvolveOptions::default();
        opts.cancel.cancel();
        assert!(matches!(
            deconvolve(&img, &opts, &mut silent),
            Err(Error::Cancelled)
        ));
    }
}
