// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Star removal: a star mask from detection, filled by diffusion.
//!
//! This is the classical method. It uses no learned model.
//!
//! 1. **Detect.** Stars are found on the luminance with [`stars::detect`]
//!    (lower border, higher star limit than registration needs).
//! 2. **Mask.** Each star gets a disc. Its radius is where a Moffat profile
//!    (`beta = 3`, the wings of a real PSF are heavier than a Gaussian's) with
//!    the star's FWHM and peak falls to half the pixel noise, scaled by
//!    [`Options::mask_scale`], grown by [`Options::grow`] pixels, at least two
//!    FWHM and at most [`Options::max_radius`]. The union of the discs is the
//!    mask.
//! 3. **Fill.** Every plane is filled inside the mask by solving Laplace's
//!    equation with the unmasked pixels around each connected piece of the mask
//!    as fixed values (harmonic, or diffusion, inpainting), by successive
//!    over-relaxation. The result joins the surroundings without a seam and
//!    follows a smooth gradient through the hole. Pieces are independent and
//!    are filled in parallel.
//! 4. **Noise.** A diffusion fill is smooth, and a smooth patch in a noisy sky
//!    shows. Unless switched off, Gaussian noise of the level measured on the
//!    unmasked pixels beside each piece is added inside it.
//!
//! What it cannot do: it does not recreate texture that a star hides, so a
//! bright star on a nebula leaves a smooth patch the size of its mask;
//! diffraction spikes and very faint halos beyond the mask stay; stars below
//! the detection limit stay. Everything that is removed is returned as a
//! second image (`stars = original - starless`), so a star layer can be put
//! back, scaled, after the starless image has been processed.
//! [`Options::strength`] below 1 removes only that fraction of every star,
//! which reduces them without removing them.
//!
//! # References
//!
//! - Moffat, A. F. J. (1969), A theoretical investigation of focal stellar images in the photographic emulsion and application to photographic photometry, Astron. Astrophys. 3, 455 (bibcode 1969A&A.....3..455M; no DOI).
//! - Shen, J. and Chan, T. F. (2002), Mathematical models for local nontexture inpaintings, SIAM J. Appl. Math. 62, 1019-1043, doi:10.1137/S0036139900368844 (harmonic inpainting).
//! - Young, D. (1954), Iterative methods for solving partial difference equations of elliptic type, Trans. Am. Math. Soc. 76, 92-111, doi:10.1090/S0002-9947-1954-0059635-7 (successive over-relaxation).

use rayon::prelude::*;

use crate::CancelToken;
use crate::fits::Image;
use crate::stars::{self, DetectOptions, Detection, Star};
use crate::stats;
use crate::{Error, Frame, Result};

/// A small repeatable Gaussian generator (splitmix64 and Box-Muller), so the
/// filled noise needs no dependency and is the same on every machine.
struct Noise(u64);

impl Noise {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn normal(&mut self) -> f64 {
        let u1 = ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let u2 = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Settings for [`remove_stars`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Star detection settings. `border` and `max_stars` are overridden.
    pub detect: DetectOptions,
    /// Multiplies every mask radius.
    pub mask_scale: f32,
    /// Pixels added to every mask radius.
    pub grow: f32,
    /// Largest mask radius, in pixels.
    pub max_radius: f32,
    /// Add noise to the filled pixels.
    pub noise: bool,
    /// Fraction of each star to remove, `0..=1`.
    pub strength: f32,
    /// Seed of the noise, so a run is repeatable.
    pub seed: u64,
    /// Stops the run between steps and planes; never set by default.
    pub cancel: CancelToken,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            detect: DetectOptions {
                threshold: 4.0,
                ..DetectOptions::default()
            },
            mask_scale: 1.0,
            grow: 1.5,
            max_radius: 60.0,
            noise: true,
            strength: 1.0,
            seed: 1,
            cancel: CancelToken::default(),
        }
    }
}

impl Options {
    fn check(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.strength) {
            return Err(Error::invalid("the strength must be between 0 and 1"));
        }
        if self.mask_scale <= 0.0 || self.grow < 0.0 || self.max_radius < 2.0 {
            return Err(Error::invalid(
                "the mask scale must be positive, the growth not negative and the largest radius at least 2",
            ));
        }
        Ok(())
    }
}

/// What [`remove_stars`] returns.
#[derive(Clone, Debug)]
pub struct Removal {
    /// The image with the stars removed (or reduced by the strength).
    pub starless: Image,
    /// What was taken out: `original - starless`.
    pub stars: Image,
    /// The mask: 1 where stars were filled, else 0.
    pub mask: Frame,
    /// Stars found.
    pub found: usize,
    /// Fraction of the pixels that are masked.
    pub masked_fraction: f32,
}

/// Mask radius of a star, in pixels.
#[must_use]
pub fn radius(star: &Star, noise: f32, opts: &Options) -> f32 {
    const BETA: f64 = 3.0;
    let fwhm = f64::from(star.fwhm.max(1.0));
    let alpha = fwhm / (2.0 * (2f64.powf(1.0 / BETA) - 1.0).sqrt());
    let ratio = (f64::from(star.peak) / (0.5 * f64::from(noise.max(1e-6)))).max(1.0);
    let r = alpha * (ratio.powf(1.0 / BETA) - 1.0).max(0.0).sqrt();
    let r = r as f32 * opts.mask_scale + opts.grow;
    r.max(2.0 * star.fwhm).min(opts.max_radius)
}

/// The mask of `stars`: 1 inside the disc of any star, else 0.
#[must_use]
pub fn star_mask(width: usize, height: usize, stars: &[Star], noise: f32, opts: &Options) -> Frame {
    let mut data = vec![0.0f32; width * height];
    for s in stars {
        let r = radius(s, noise, opts);
        let (x0, x1) = (
            (s.x - f64::from(r)).floor().max(0.0) as usize,
            ((s.x + f64::from(r)).ceil() as usize).min(width - 1),
        );
        let (y0, y1) = (
            (s.y - f64::from(r)).floor().max(0.0) as usize,
            ((s.y + f64::from(r)).ceil() as usize).min(height - 1),
        );
        let r2 = f64::from(r) * f64::from(r);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f64 - s.x, y as f64 - s.y);
                if dx * dx + dy * dy <= r2 {
                    data[y * width + x] = 1.0;
                }
            }
        }
    }
    Frame {
        width,
        height,
        data,
        bayer: None,
    }
}

/// A 4-connected piece of the mask: its pixels and bounding box.
struct Piece {
    pixels: Vec<usize>,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

fn pieces(mask: &Frame) -> Vec<Piece> {
    let (w, h) = (mask.width, mask.height);
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    for start in 0..w * h {
        if mask.data[start] < 0.5 || seen[start] {
            continue;
        }
        let mut p = Piece {
            pixels: Vec::new(),
            x0: usize::MAX,
            x1: 0,
            y0: usize::MAX,
            y1: 0,
        };
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            p.pixels.push(i);
            p.x0 = p.x0.min(x);
            p.x1 = p.x1.max(x);
            p.y0 = p.y0.min(y);
            p.y1 = p.y1.max(y);
            let mut push = |j: usize| {
                if mask.data[j] >= 0.5 && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        out.push(p);
    }
    out
}

/// Noise of the unmasked pixels around a piece: the robust scatter of each
/// pixel about the mean of its four neighbours, which a smooth sky and
/// gradient do not contribute to.
fn local_noise(f: &Frame, mask: &Frame, p: &Piece, fallback: f32) -> f32 {
    let (w, h) = (f.width, f.height);
    let m = 10;
    let (x0, x1) = (
        p.x0.saturating_sub(m).max(1),
        (p.x1 + m).min(w.saturating_sub(2)),
    );
    let (y0, y1) = (
        p.y0.saturating_sub(m).max(1),
        (p.y1 + m).min(h.saturating_sub(2)),
    );
    let mut d = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * w + x;
            let near = [i, i - 1, i + 1, i - w, i + w];
            if near.iter().any(|&j| mask.data[j] >= 0.5) {
                continue;
            }
            let v = f.data[i];
            let avg = 0.25 * (f.data[i - 1] + f.data[i + 1] + f.data[i - w] + f.data[i + w]);
            if v.is_finite() && avg.is_finite() {
                d.push(v - avg);
            }
        }
    }
    if d.len() < 30 {
        return fallback;
    }
    let (_, sigma) = stats::median_sigma(&d);
    // var(x - mean of 4 neighbours) = 1.25 sigma^2 for white noise.
    sigma / 1.25f32.sqrt()
}

/// Solve Laplace's equation inside a piece with the surrounding unmasked
/// pixels fixed. Returns the new value of each pixel of the piece.
fn fill_piece(f: &Frame, mask: &Frame, p: &Piece, tol: f32) -> Vec<f32> {
    let (w, h) = (f.width, f.height);
    let (bw, bh) = (p.x1 - p.x0 + 1, p.y1 - p.y0 + 1);
    let n = bw.max(bh);
    // Working copy of the bounding box.
    let mut g = vec![0.0f32; bw * bh];
    let mut free = vec![false; bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let i = (p.y0 + y) * w + p.x0 + x;
            g[y * bw + x] = f.data[i];
            free[y * bw + x] = mask.data[i] >= 0.5;
        }
    }
    // Boundary values: the unmasked pixels touching the piece, read from the
    // frame (they may lie one pixel outside the box).
    let at = |x: isize, y: isize, g: &[f32], free: &[bool]| -> Option<f32> {
        let (ax, ay) = (p.x0 as isize + x, p.y0 as isize + y);
        if ax < 0 || ay < 0 || ax >= w as isize || ay >= h as isize {
            return None;
        }
        if (0..bw as isize).contains(&x) && (0..bh as isize).contains(&y) {
            let k = y as usize * bw + x as usize;
            return if free[k] {
                Some(g[k])
            } else {
                Some(f.data[ay as usize * w + ax as usize])
            };
        }
        let v = f.data[ay as usize * w + ax as usize];
        // A masked pixel of another piece cannot touch this one (4-connected).
        Some(v)
    };
    // Start from the mean of the boundary.
    let (mut sum, mut cnt) = (0.0f64, 0u32);
    for y in 0..bh as isize {
        for x in 0..bw as isize {
            let k = y as usize * bw + x as usize;
            if !free[k] {
                continue;
            }
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                let inside_free = (0..bw as isize).contains(&nx)
                    && (0..bh as isize).contains(&ny)
                    && free[ny as usize * bw + nx as usize];
                if !inside_free
                    && let Some(v) = at(nx, ny, &g, &free)
                    && v.is_finite()
                {
                    sum += f64::from(v);
                    cnt += 1;
                }
            }
        }
    }
    if cnt == 0 {
        // The mask covers the whole frame: nothing to interpolate from.
        return p.pixels.iter().map(|&i| f.data[i]).collect();
    }
    let start = (sum / f64::from(cnt)) as f32;
    for (k, &fr) in free.iter().enumerate() {
        if fr {
            g[k] = start;
        }
    }
    // Successive over-relaxation, red-black ordering is not needed at these sizes.
    let omega = 2.0 / (1.0 + (std::f32::consts::PI / (n as f32 + 1.0)).sin());
    let max_iter = (6 * n).clamp(60, 4000);
    for _ in 0..max_iter {
        let mut worst = 0.0f32;
        for y in 0..bh as isize {
            for x in 0..bw as isize {
                let k = y as usize * bw + x as usize;
                if !free[k] {
                    continue;
                }
                let (mut s, mut c) = (0.0f32, 0u32);
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    if let Some(v) = at(x + dx, y + dy, &g, &free)
                        && v.is_finite()
                    {
                        s += v;
                        c += 1;
                    }
                }
                if c == 0 {
                    continue;
                }
                let target = s / c as f32;
                let new = g[k] + omega * (target - g[k]);
                worst = worst.max((new - g[k]).abs());
                g[k] = new;
            }
        }
        if worst < tol {
            break;
        }
    }
    p.pixels
        .iter()
        .map(|&i| g[(i / w - p.y0) * bw + (i % w - p.x0)])
        .collect()
}

/// Fill every piece of `mask` in one plane. `noise` is the plane's global
/// noise (the fallback and the convergence scale).
fn fill_plane(
    f: &Frame,
    mask: &Frame,
    pcs: &[Piece],
    noise: f32,
    opts: &Options,
    plane: u64,
) -> Frame {
    let tol = (0.02 * noise).max(1e-6);
    let patches: Vec<(usize, Vec<f32>)> = pcs
        .par_iter()
        .enumerate()
        .map(|(k, p)| {
            let mut v = fill_piece(f, mask, p, tol);
            if opts.noise {
                let s = local_noise(f, mask, p, noise);
                let mut rng =
                    Noise::new(opts.seed ^ (plane << 48) ^ (k as u64).wrapping_mul(0x9E37_79B9));
                for x in &mut v {
                    *x += s * rng.normal() as f32;
                }
            }
            (k, v)
        })
        .collect();
    let mut out = f.clone();
    for (k, v) in patches {
        for (&i, &x) in pcs[k].pixels.iter().zip(&v) {
            out.data[i] = x;
        }
    }
    out
}

/// Remove the stars from `image`.
///
/// Stars are detected on the luminance, so every plane gets the same mask.
/// Raw (undemosaiced) data is refused: demosaic it first.
///
/// # Errors
/// [`Error::Invalid`] for bad options, an empty image or raw Bayer data.
pub fn remove_stars(image: &Image, opts: &Options) -> Result<Removal> {
    opts.check()?;
    let (w, h) = (image.width(), image.height());
    if w < 8 || h < 8 {
        return Err(Error::invalid("the image is too small for star removal"));
    }
    if image.planes.iter().any(|p| p.bayer.is_some()) {
        return Err(Error::invalid(
            "star removal needs a demosaiced image, this one is a raw colour mosaic",
        ));
    }
    let luma = stars::luminance(&image.planes);
    opts.cancel.check()?;
    let det = detect(&luma, &opts.detect);
    opts.cancel.check()?;
    let mask = star_mask(w, h, &det.stars, det.noise, opts);
    let pcs = pieces(&mask);
    let masked = mask.data.iter().filter(|&&v| v >= 0.5).count();
    let mut starless = Vec::with_capacity(image.planes.len());
    let mut taken = Vec::with_capacity(image.planes.len());
    for (pi, plane) in image.planes.iter().enumerate() {
        opts.cancel.check()?;
        let sigma = stats::noise_from_differences(plane, 1);
        let filled = fill_plane(plane, &mask, &pcs, sigma, opts, pi as u64);
        let mut out = plane.clone();
        let mut diff = plane.clone();
        for i in 0..plane.data.len() {
            let v = plane.data[i] - opts.strength * (plane.data[i] - filled.data[i]);
            out.data[i] = v;
            diff.data[i] = plane.data[i] - v;
        }
        starless.push(out);
        taken.push(diff);
    }
    let mut header = image.header.clone();
    header.add_history(&format!(
        "valerian starless: {} stars, {:.2} % of pixels filled, strength {}, noise {}",
        det.stars.len(),
        100.0 * masked as f64 / (w * h) as f64,
        opts.strength,
        if opts.noise { "added" } else { "not added" }
    ));
    Ok(Removal {
        starless: Image {
            header: header.clone(),
            planes: starless,
        },
        stars: Image {
            header,
            planes: taken,
        },
        mask,
        found: det.stars.len(),
        masked_fraction: masked as f32 / (w * h) as f32,
    })
}

fn detect(luma: &Frame, base: &DetectOptions) -> Detection {
    let opts = DetectOptions {
        border: 2,
        max_stars: 200_000,
        ..base.clone()
    };
    stars::detect(luma, &opts)
}

#[cfg(test)]
mod tests;
