// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Background extraction: modelling and removing sky gradients.
//!
//! Light pollution, moonlight and airglow add a smooth, slowly varying signal
//! under the image; vignetting that a flat did not fully remove multiplies it
//! by a smooth factor. Both are removed by fitting a smooth surface to samples
//! of the sky and subtracting it (or dividing by it).
//!
//! The method, in the order it runs:
//!
//! 1. The frame is cut into tiles. Each tile gives one sample: the median of
//!    its pixels that are neither masked nor missing (NaN).
//! 2. A surface is fitted to the samples, either a polynomial
//!    ([`Model::Polynomial`]) or a smoothing thin-plate spline
//!    ([`Model::ThinPlate`]), a flexible surface that passes near the samples
//!    in the way a manual "dynamic background extraction" does. Samples far
//!    above the surface (nebulosity, a galaxy's halo) or far below it are
//!    rejected and the fit is repeated.
//! 3. The residual of the image against the surface marks the stars: pixels
//!    well above it, grown by a margin. Tiles the fit rejected as high are
//!    grown by one tile and treated as nebula. Steps 1 to 3 repeat with that
//!    mask, so the samples come from sky and not from the target.
//! 4. The surface is evaluated and applied by [`Fit::apply`].
//!
//! Two failure modes of this kind of fit are handled on purpose, because the
//! owner's Python pipeline found them on real data. A plane fitted to a frame
//! that a large target fills absorbs the target's outer light, so the target
//! can be excluded ([`Options::exclude_target`]) and the fit is *declined*
//! when too little clean sky is left, which [`Fit::declined`] reports instead
//! of subtracting something that is not background; it is also declined when
//! the samples scatter about the surface by more than the gradient it removes,
//! which means they are the target and not the sky. And a correction should
//! remove tilt, not level: by default the mean of the surface is put back
//! ([`Level::Preserve`]), so the correction does not move the sky's brightness.
//!
//! Coordinates in the fit run from -1 to 1 on each axis (pixel centres), so
//! the polynomial terms are `x^i y^j` in those units; [`terms`] gives their
//! order and [`Fit::coefficients`] the values.
//!
//! # References
//!
//! - Bookstein, F. L. (1989), Principal warps: thin-plate splines and the decomposition of deformations, IEEE Trans. Pattern Anal. Mach. Intell. 11, 567-585, doi:10.1109/34.24792.
//! - Duchon, J. (1977), Splines minimizing rotation-invariant semi-norms in Sobolev spaces, Lecture Notes in Mathematics 571, 85-100, doi:10.1007/BFb0086566 (the r^2 ln r kernel).
//! - Bertin, E. and Arnouts, S. (1996), `SExtractor`: software for source extraction, Astron. Astrophys. Suppl. 117, 393-404, doi:10.1051/aas:1996164 (a smooth background from a mesh of medians).
//! - Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::stats;
use helia_core::Frame;

/// The surface fitted to the samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    /// A polynomial of total degree `degree` (0 to 6). Degree 1 is a plane
    /// (tilt only), the safest choice; 2 to 4 follow a bowl or an uneven
    /// glow.
    Polynomial {
        /// The total degree.
        degree: usize,
    },
    /// A thin-plate spline through the samples with smoothing. Follows any
    /// smooth gradient; needs `smoothing` to stop it chasing sample noise.
    ThinPlate {
        /// The weight of the roughness penalty, `>= 0`, in the units of the
        /// normalised coordinates. Zero interpolates the samples exactly;
        /// larger values tend to a plane. For a hundred or so tiles, 0.001 to
        /// 0.01 follows a smooth gradient and averages out the noise; 0.05 is
        /// already too stiff for a bowl.
        smoothing: f64,
    },
}

/// How the surface is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Subtract: for light pollution and other added light.
    #[default]
    Subtract,
    /// Divide: for vignetting and other multiplicative falloff.
    Divide,
}

/// What happens to the overall level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Keep the image's mean level: subtract (or divide by) the surface minus
    /// (or over) its own mean. Removes the gradient and leaves the sky where
    /// it was.
    #[default]
    Preserve,
    /// Remove the whole surface: with [`Mode::Subtract`] the sky goes to about
    /// zero, with [`Mode::Divide`] to about one.
    Zero,
}

/// Settings for [`extract`].
#[derive(Clone, Debug)]
pub struct Options {
    /// The surface.
    pub model: Model,
    /// Tile size in pixels; 0 chooses `max(32, min(width, height) / 32)`.
    pub tile: usize,
    /// Stars: pixels this many robust sigmas above the surface are masked.
    pub star_sigma: f32,
    /// Margin, in pixels, grown around masked stars.
    pub mask_grow: usize,
    /// Samples more than this many sigmas below the fit are rejected.
    pub clip_low: f32,
    /// Samples more than this many sigmas above the fit are rejected; tighter
    /// than the low limit because nebulosity and stars only add light.
    pub clip_high: f32,
    /// Rounds of mask refinement after the first fit.
    pub rounds: usize,
    /// Also leave out the bright central target, sized from the data.
    pub exclude_target: bool,
    /// Decline the fit when the samples scatter about the surface by more
    /// than this many times the gradient it describes (plus a noise
    /// allowance); infinity never declines for this reason.
    pub max_scatter: f32,
    /// Most samples handed to a thin-plate spline (more are thinned evenly).
    pub max_samples: usize,
    /// The surface is evaluated every this many pixels and interpolated
    /// linearly between, since it is smooth.
    pub grid_step: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            model: Model::Polynomial { degree: 2 },
            tile: 0,
            star_sigma: 3.0,
            mask_grow: 3,
            clip_low: 3.0,
            clip_high: 2.0,
            rounds: 3,
            exclude_target: false,
            max_scatter: 2.0,
            max_samples: 400,
            grid_step: 8,
        }
    }
}

/// A fitted background.
#[derive(Clone, Debug)]
pub struct Fit {
    /// The surface at every pixel; a constant (the median) when declined.
    pub surface: Frame,
    /// Polynomial coefficients in the order of [`terms`]; empty for a spline
    /// or when declined.
    pub coefficients: Vec<f64>,
    /// Tiles that gave a sample.
    pub tiles: usize,
    /// Samples that survived rejection and went into the fit.
    pub samples: usize,
    /// Root mean square of the surviving samples' residuals, in image units.
    pub residual_rms: f32,
    /// Fraction of the pixels excluded by the star, nebula and target masks.
    pub masked_fraction: f32,
    /// Why the fit was declined, if it was. A declined fit changes nothing.
    pub declined: Option<String>,
    /// The tile size used, in pixels.
    pub tile: usize,
}

/// The polynomial terms `(i, j)` meaning `x^i y^j` for a total degree, in the
/// order of [`Fit::coefficients`]: by total degree, then by falling power of x.
#[must_use]
pub fn terms(degree: usize) -> Vec<(u32, u32)> {
    let mut v = Vec::new();
    for total in 0..=degree as u32 {
        for j in 0..=total {
            v.push((total - j, j));
        }
    }
    v
}

/// Normalised coordinate of pixel index `i` of `n` (centre of the pixel), in
/// `(-1, 1)`.
fn norm(i: f64, n: usize) -> f64 {
    2.0 * (i + 0.5) / n as f64 - 1.0
}

/// One sample of the sky.
#[derive(Clone, Copy)]
struct Sample {
    x: f64,
    y: f64,
    z: f64,
    /// The tile it came from.
    cell: usize,
}

/// A solved surface that can be evaluated.
enum Surface {
    Poly {
        terms: Vec<(u32, u32)>,
        c: Vec<f64>,
    },
    Spline {
        pts: Vec<(f64, f64)>,
        w: Vec<f64>,
        a: [f64; 3],
    },
}

impl Surface {
    fn eval(&self, x: f64, y: f64) -> f64 {
        match self {
            Self::Poly { terms, c } => terms
                .iter()
                .zip(c)
                .map(|(&(i, j), c)| c * x.powi(i as i32) * y.powi(j as i32))
                .sum(),
            Self::Spline { pts, w, a } => {
                let mut v = a[0] + a[1] * x + a[2] * y;
                for (&(px, py), w) in pts.iter().zip(w) {
                    v += w * phi((x - px).hypot(y - py));
                }
                v
            }
        }
    }
}

/// The thin-plate kernel `r^2 ln r`.
fn phi(r: f64) -> f64 {
    if r <= 0.0 { 0.0 } else { r * r * r.ln() }
}

/// Solve the dense system `a x = b` (row-major `n` by `n`) by Gaussian
/// elimination with partial pivoting. `None` when it is singular.
fn solve(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Option<Vec<f64>> {
    for col in 0..n {
        let piv =
            (col..n).max_by(|&p, &q| a[p * n + col].abs().total_cmp(&a[q * n + col].abs()))?;
        if a[piv * n + col].abs() < 1e-12 {
            return None;
        }
        if piv != col {
            for k in 0..n {
                a.swap(piv * n + k, col * n + k);
            }
            b.swap(piv, col);
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let f = a[r * n + col] / d;
            if f != 0.0 {
                for k in col..n {
                    a[r * n + k] -= f * a[col * n + k];
                }
                b[r] -= f * b[col];
            }
        }
    }
    for r in (0..n).rev() {
        let mut s = b[r];
        for k in r + 1..n {
            s -= a[r * n + k] * b[k];
        }
        b[r] = s / a[r * n + r];
    }
    Some(b)
}

fn fit_surface(s: &[Sample], model: Model) -> Option<Surface> {
    match model {
        Model::Polynomial { degree } => {
            let t = terms(degree);
            let m = t.len();
            if s.len() < m {
                return None;
            }
            // Normal equations; the coordinates are in -1..1 so they are
            // well enough conditioned for the degrees allowed.
            let mut ata = vec![0.0; m * m];
            let mut atz = vec![0.0; m];
            let mut row = vec![0.0; m];
            for p in s {
                for (k, &(i, j)) in t.iter().enumerate() {
                    row[k] = p.x.powi(i as i32) * p.y.powi(j as i32);
                }
                for a in 0..m {
                    atz[a] += row[a] * p.z;
                    for b in 0..m {
                        ata[a * m + b] += row[a] * row[b];
                    }
                }
            }
            let c = solve(ata, atz, m)?;
            Some(Surface::Poly { terms: t, c })
        }
        Model::ThinPlate { smoothing } => {
            let n = s.len();
            if n < 4 {
                return None;
            }
            let dim = n + 3;
            let mut a = vec![0.0; dim * dim];
            let mut b = vec![0.0; dim];
            for i in 0..n {
                for j in 0..n {
                    a[i * dim + j] = phi((s[i].x - s[j].x).hypot(s[i].y - s[j].y));
                }
                a[i * dim + i] += smoothing;
                a[i * dim + n] = 1.0;
                a[i * dim + n + 1] = s[i].x;
                a[i * dim + n + 2] = s[i].y;
                a[n * dim + i] = 1.0;
                a[(n + 1) * dim + i] = s[i].x;
                a[(n + 2) * dim + i] = s[i].y;
                b[i] = s[i].z;
            }
            let x = solve(a, b, dim)?;
            Some(Surface::Spline {
                pts: s.iter().map(|p| (p.x, p.y)).collect(),
                w: x[..n].to_vec(),
                a: [x[n], x[n + 1], x[n + 2]],
            })
        }
    }
}

/// The tile grid: `nx` by `ny` cells that partition the frame.
struct Grid {
    nx: usize,
    ny: usize,
    w: usize,
    h: usize,
}

impl Grid {
    fn new(w: usize, h: usize, tile: usize) -> Self {
        Self {
            nx: (w / tile).max(1),
            ny: (h / tile).max(1),
            w,
            h,
        }
    }

    fn bounds(&self, cx: usize, cy: usize) -> (usize, usize, usize, usize) {
        (
            cx * self.w / self.nx,
            (cx + 1) * self.w / self.nx,
            cy * self.h / self.ny,
            (cy + 1) * self.h / self.ny,
        )
    }
}

/// Median of the usable pixels of every cell; `None` where too few are usable.
fn cell_samples(frame: &Frame, usable: &[bool], grid: &Grid, skip: &[bool]) -> Vec<Option<Sample>> {
    (0..grid.nx * grid.ny)
        .into_par_iter()
        .map(|cell| {
            if skip[cell] {
                return None;
            }
            let (cx, cy) = (cell % grid.nx, cell / grid.nx);
            let (x0, x1, y0, y1) = grid.bounds(cx, cy);
            let mut v = Vec::with_capacity((x1 - x0) * (y1 - y0));
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * grid.w + x;
                    if usable[i] {
                        v.push(frame.data[i]);
                    }
                }
            }
            // A quarter of the cell, at least, must be clean sky.
            if v.len() < ((x1 - x0) * (y1 - y0) / 4).max(16) {
                return None;
            }
            let z = f64::from(stats::median_in_place(&mut v));
            Some(Sample {
                x: norm((x0 + x1) as f64 / 2.0 - 0.5, grid.w),
                y: norm((y0 + y1) as f64 / 2.0 - 0.5, grid.h),
                z,
                cell,
            })
        })
        .collect()
}

/// Fit with rejection of high and low samples. Returns the surface, the
/// samples kept, and the cells rejected as high.
fn robust_fit(
    samples: &[Sample],
    opts: &Options,
    min_samples: usize,
    noise_floor: f32,
) -> Option<(Surface, Vec<Sample>, Vec<usize>)> {
    let mut keep: Vec<Sample> = samples.to_vec();
    if let Model::ThinPlate { .. } = opts.model
        && keep.len() > opts.max_samples
    {
        let stride = keep.len().div_ceil(opts.max_samples);
        keep = keep.into_iter().step_by(stride).collect();
    }
    let mut high_cells: Vec<usize> = Vec::new();
    let mut surface = fit_surface(&keep, opts.model)?;
    for _ in 0..6 {
        let res: Vec<f32> = keep
            .iter()
            .map(|p| (p.z - surface.eval(p.x, p.y)) as f32)
            .collect();
        let (centre, mad_sigma) = stats::median_sigma(&res);
        // The spread of the residuals cannot be less than the noise of a tile
        // median: a fit that follows its samples closely would otherwise judge
        // ordinary noise to be an outlier.
        let sigma = mad_sigma.max(noise_floor);
        if sigma <= 0.0 || !sigma.is_finite() {
            break;
        }
        let mut next = Vec::with_capacity(keep.len());
        let mut dropped_high = Vec::new();
        for (p, r) in keep.iter().zip(&res) {
            let d = (r - centre) / sigma;
            if d > opts.clip_high {
                dropped_high.push(p.cell);
            } else if d >= -opts.clip_low {
                next.push(*p);
            }
        }
        if next.len() == keep.len() || next.len() < min_samples {
            break;
        }
        high_cells.extend(dropped_high);
        keep = next;
        surface = fit_surface(&keep, opts.model)?;
    }
    Some((surface, keep, high_cells))
}

/// Evaluate a surface at every pixel: exactly on a coarse grid and linearly
/// between its points.
fn evaluate(surface: &Surface, w: usize, h: usize, step: usize) -> Frame {
    let axis = |n: usize| -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).step_by(step.max(1)).collect();
        if *v.last().unwrap_or(&0) != n - 1 {
            v.push(n - 1);
        }
        v
    };
    let (xs, ys) = (axis(w), axis(h));
    let grid: Vec<f64> = ys
        .par_iter()
        .flat_map_iter(|&y| {
            let yn = norm(y as f64, h);
            xs.iter().map(move |&x| surface.eval(norm(x as f64, w), yn))
        })
        .collect();
    let find = |v: &[usize], p: usize| -> (usize, f64) {
        let i = match v.binary_search(&p) {
            Ok(i) => i.min(v.len().saturating_sub(2)),
            Err(i) => i - 1,
        };
        if v.len() == 1 {
            return (0, 0.0);
        }
        (i, (p - v[i]) as f64 / (v[i + 1] - v[i]) as f64)
    };
    let fx: Vec<(usize, f64)> = (0..w).map(|x| find(&xs, x)).collect();
    let mut data = vec![0.0_f32; w * h];
    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (iy, ty) = find(&ys, y);
        let iy2 = (iy + 1).min(ys.len() - 1);
        for (x, out) in row.iter_mut().enumerate() {
            let (ix, tx) = fx[x];
            let ix2 = (ix + 1).min(xs.len() - 1);
            let g = |a: usize, b: usize| grid[b * xs.len() + a];
            let top = g(ix, iy) * (1.0 - tx) + g(ix2, iy) * tx;
            let bot = g(ix, iy2) * (1.0 - tx) + g(ix2, iy2) * tx;
            *out = (top * (1.0 - ty) + bot * ty) as f32;
        }
    });
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// Grow `mask` by `r` pixels in each direction (a square structuring element).
fn grow(mask: &mut [bool], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![false; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &mask[y * w..(y + 1) * w];
        // Distance to the nearest set pixel along the row, by two sweeps.
        let mut near = usize::MAX / 2;
        let mut dist = vec![usize::MAX / 2; w];
        for x in 0..w {
            near = if row[x] { 0 } else { near.saturating_add(1) };
            dist[x] = near;
        }
        near = usize::MAX / 2;
        for x in (0..w).rev() {
            near = if row[x] { 0 } else { near.saturating_add(1) };
            dist[x] = dist[x].min(near);
            out[x] = dist[x] <= r;
        }
    });
    let src = tmp;
    let cols: Vec<Vec<bool>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let mut near = usize::MAX / 2;
            let mut dist = vec![usize::MAX / 2; h];
            for y in 0..h {
                near = if src[y * w + x] {
                    0
                } else {
                    near.saturating_add(1)
                };
                dist[y] = near;
            }
            near = usize::MAX / 2;
            let mut out = vec![false; h];
            for y in (0..h).rev() {
                near = if src[y * w + x] {
                    0
                } else {
                    near.saturating_add(1)
                };
                dist[y] = dist[y].min(near);
                out[y] = dist[y] <= r;
            }
            out
        })
        .collect();
    for (x, col) in cols.iter().enumerate() {
        for (y, &v) in col.iter().enumerate() {
            mask[y * w + x] = v;
        }
    }
}

/// An elliptical mask over the bright central object, sized from the data so
/// that it holds `frac` of the flux above the sky (the pipeline's
/// `target_mask`): a fixed radius cannot suit both a galaxy that fills the
/// frame and a small nebula.
#[must_use]
pub fn target_mask(frame: &Frame, frac: f32) -> Vec<bool> {
    let (w, h) = (frame.width, frame.height);
    let sky = stats::median_finite(&stats::subsample(frame, 1 << 20));
    let radius = |x: usize, y: usize| -> f32 {
        let dx = (x as f32 - w as f32 / 2.0) / (w as f32 / 2.0);
        let dy = (y as f32 - h as f32 / 2.0) / (h as f32 / 2.0);
        dx.hypot(dy)
    };
    let signal = |i: usize| -> f64 {
        let v = frame.data[i];
        if v.is_finite() {
            f64::from((v - sky).max(0.0))
        } else {
            0.0
        }
    };
    // Flux above the sky in rings of 0.0125 of the half-size; the corner of
    // the frame is at 1.414, so 120 rings cover it.
    let (mut total, mut radii) = (0.0_f64, vec![0.0_f64; 120]);
    for y in 0..h {
        for x in 0..w {
            let s = signal(y * w + x);
            total += s;
            let bin = ((radius(x, y) / 0.0125) as usize).min(119);
            radii[bin] += s;
        }
    }
    let within = |cut: f32| -> f64 {
        let last = ((cut / 0.0125).round() as usize).min(120);
        radii[..last].iter().sum()
    };
    let r_cut = if total <= 0.0 {
        0.5
    } else {
        let mut cut = 0.25_f32;
        let mut chosen = 0.75_f32;
        while cut < 0.96 {
            if within(cut) >= f64::from(frac) * total {
                chosen = (cut + 0.15).min(0.95);
                break;
            }
            cut += 0.05;
        }
        chosen
    };
    (0..w * h).map(|i| radius(i % w, i / w) < r_cut).collect()
}

/// Check the options and the frame, and choose the tile size.
fn check_inputs(frame: &Frame, opts: &Options) -> Result<usize> {
    if frame.bayer.is_some() {
        return Err(Error::invalid(
            "background extraction needs a demosaiced or mono image, not a raw colour mosaic",
        ));
    }
    match opts.model {
        Model::Polynomial { degree } if degree > 6 => {
            return Err(Error::invalid("polynomial degree must be 0 to 6"));
        }
        Model::ThinPlate { smoothing } if !(smoothing.is_finite() && smoothing >= 0.0) => {
            return Err(Error::invalid("the spline smoothing must be 0 or more"));
        }
        _ => {}
    }
    let (w, h) = (frame.width, frame.height);
    let tile = if opts.tile == 0 {
        (w.min(h) / 32).max(32)
    } else {
        opts.tile
    };
    if w < 2 * tile || h < 2 * tile {
        return Err(Error::invalid(format!(
            "the image ({w} x {h}) is too small for {tile}-pixel tiles"
        )));
    }
    Ok(tile)
}

/// Tiles to leave out next round: those rejected as high, and their
/// neighbours (nebulosity is bigger than the tile that caught it).
fn nebula_skip(high: &[usize], grid: &Grid) -> Vec<bool> {
    let mut skip = vec![false; grid.nx * grid.ny];
    for &cell in high {
        let (cx, cy) = ((cell % grid.nx) as i64, (cell / grid.nx) as i64);
        for dy in -1..=1_i64 {
            for dx in -1..=1_i64 {
                let (x, y) = (cx + dx, cy + dy);
                if (0..grid.nx as i64).contains(&x) && (0..grid.ny as i64).contains(&y) {
                    skip[y as usize * grid.nx + x as usize] = true;
                }
            }
        }
    }
    skip
}

/// Pixels more than `star_sigma` robust sigmas above the surface, grown by
/// `mask_grow`; `None` when the residual has no usable spread.
fn star_mask(frame: &Frame, bg: &Frame, finite: &[bool], opts: &Options) -> Option<Vec<bool>> {
    let n = frame.data.len();
    let res: Vec<f32> = (0..n)
        .step_by((n / (1 << 20)).max(1))
        .filter(|&i| finite[i])
        .map(|i| frame.data[i] - bg.data[i])
        .collect();
    let (_, sigma) = stats::median_sigma(&res);
    if !(sigma.is_finite() && sigma > 0.0) {
        return None;
    }
    let limit = opts.star_sigma * sigma;
    let mut star: Vec<bool> = (0..n)
        .into_par_iter()
        .map(|i| finite[i] && frame.data[i] - bg.data[i] > limit)
        .collect();
    grow(&mut star, frame.width, frame.height, opts.mask_grow);
    Some(star)
}

/// What the rounds of fitting arrived at.
struct Rounds {
    best: Option<(Surface, Vec<Sample>, Frame)>,
    tiles: usize,
    masked: f32,
}

/// Fit, mask, and fit again.
fn fit_rounds(
    frame: &Frame,
    opts: &Options,
    grid: &Grid,
    min_samples: usize,
    noise_floor: f32,
) -> Rounds {
    let (w, h) = (frame.width, frame.height);
    let n = w * h;
    let finite: Vec<bool> = frame.data.iter().map(|v| v.is_finite()).collect();
    let target = opts.exclude_target.then(|| target_mask(frame, 0.45));
    let allowed = |i: usize| finite[i] && target.as_ref().is_none_or(|t| !t[i]);
    let mut usable: Vec<bool> = (0..n).map(allowed).collect();
    let mut skip = vec![false; grid.nx * grid.ny];
    let mut best = None;
    let mut tiles = 0;
    for round in 0..=opts.rounds {
        let samples: Vec<Sample> = cell_samples(frame, &usable, grid, &skip)
            .into_iter()
            .flatten()
            .collect();
        tiles = samples.len();
        if samples.len() < min_samples {
            break;
        }
        let Some((surface, kept, high)) = robust_fit(&samples, opts, min_samples, noise_floor)
        else {
            break;
        };
        let bg = evaluate(&surface, w, h, opts.grid_step);
        let star = (round < opts.rounds)
            .then(|| star_mask(frame, &bg, &finite, opts))
            .flatten();
        best = Some((surface, kept, bg));
        let Some(star) = star else {
            break;
        };
        usable = (0..n).map(|i| allowed(i) && !star[i]).collect();
        skip = nebula_skip(&high, grid);
    }
    let masked = usable.iter().filter(|&&u| !u).count() as f32 / n as f32;
    Rounds {
        best,
        tiles,
        masked,
    }
}

/// Find the background of one plane.
///
/// # Errors
/// [`Error::Invalid`] for a raw colour mosaic (demosaic first), a frame too
/// small for the tile size, or a model parameter out of range.
pub fn extract(frame: &Frame, opts: &Options) -> Result<Fit> {
    let tile = check_inputs(frame, opts)?;
    let (w, h) = (frame.width, frame.height);
    let grid = Grid::new(w, h, tile);
    // Noise of the median of a quarter-tile of pixels, the least we accept.
    let pixel_noise = stats::noise_from_differences(frame, 1);
    let noise_floor = 1.2533 * pixel_noise / ((tile * tile / 4).max(16) as f32).sqrt();
    let terms_needed = match opts.model {
        Model::Polynomial { degree } => terms(degree).len(),
        Model::ThinPlate { .. } => 6,
    };
    let min_samples = (2 * terms_needed).max(12);
    let rounds = fit_rounds(frame, opts, &grid, min_samples, noise_floor);
    let (tiles_used, masked) = (rounds.tiles, rounds.masked);
    let decline = |why: String| -> Fit {
        Fit {
            surface: Frame {
                width: w,
                height: h,
                data: vec![stats::median_finite(&stats::subsample(frame, 1 << 20)); w * h],
                bayer: None,
            },
            coefficients: Vec::new(),
            tiles: tiles_used,
            samples: 0,
            residual_rms: 0.0,
            masked_fraction: masked,
            declined: Some(why),
            tile,
        }
    };
    let Some((surface, kept, bg)) = rounds.best else {
        return Ok(decline(format!(
            "only {tiles_used} clean tiles; the fit needs at least {min_samples}, so there is no \
             clear sky to measure a background from"
        )));
    };
    if masked > 0.55 {
        return Ok(decline(format!(
            "{:.0}% of the image is star, nebula or target, so there is no clear sky to measure \
             a background from",
            masked * 100.0
        )));
    }
    let rms = (kept
        .iter()
        .map(|p| (p.z - surface.eval(p.x, p.y)).powi(2))
        .sum::<f64>()
        / kept.len() as f64)
        .sqrt() as f32;
    // Scatter about the surface larger than the gradient it describes means
    // the samples are structure (a target the mask did not cover), not sky.
    let (lo, hi) = bg.min_max();
    if rms > opts.max_scatter * ((hi - lo) + 5.0 * noise_floor) {
        return Ok(decline(format!(
            "the tile samples scatter by {rms:.1} about the surface, more than the {:.1} \
             gradient it describes, so what was measured is structure and not sky",
            hi - lo
        )));
    }
    let coefficients = match &surface {
        Surface::Poly { c, .. } => c.clone(),
        Surface::Spline { .. } => Vec::new(),
    };
    Ok(Fit {
        surface: bg,
        coefficients,
        tiles: tiles_used,
        samples: kept.len(),
        residual_rms: rms,
        masked_fraction: masked,
        declined: None,
        tile,
    })
}

impl Fit {
    /// The mean of the surface over the pixels of `frame` that have data.
    fn mean_over(&self, frame: &Frame) -> f64 {
        let (mut s, mut n) = (0.0_f64, 0usize);
        for (b, v) in self.surface.data.iter().zip(&frame.data) {
            if v.is_finite() {
                s += f64::from(*b);
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { s / n as f64 }
    }

    /// Peak-to-peak size of the gradient the surface describes, in image
    /// units.
    #[must_use]
    pub fn gradient_range(&self) -> f32 {
        let (lo, hi) = self.surface.min_max();
        hi - lo
    }

    /// `frame` with the background removed. A declined fit returns a copy of
    /// the frame. Pixels without data stay NaN.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the frame is not the size the fit was made on.
    pub fn apply(&self, frame: &Frame, mode: Mode, level: Level) -> Result<Frame> {
        if frame.width != self.surface.width || frame.height != self.surface.height {
            return Err(Error::invalid(
                "the background was fitted to a different size",
            ));
        }
        let mut out = frame.clone();
        if self.declined.is_some() {
            return Ok(out);
        }
        let mean = self.mean_over(frame);
        match mode {
            Mode::Subtract => {
                let offset = if level == Level::Preserve { mean } else { 0.0 };
                out.data
                    .par_iter_mut()
                    .zip(&self.surface.data)
                    .for_each(|(v, b)| *v -= (f64::from(*b) - offset) as f32);
            }
            Mode::Divide => {
                let scale = if level == Level::Preserve { mean } else { 1.0 };
                out.data
                    .par_iter_mut()
                    .zip(&self.surface.data)
                    .for_each(|(v, b)| {
                        *v = if *b > 0.0 {
                            (f64::from(*v) / (f64::from(*b) / scale)) as f32
                        } else {
                            f32::NAN
                        };
                    });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    const W: usize = 400;
    const H: usize = 300;

    /// Sky plus noise, a gradient `grad(xn, yn)`, some stars and a nebula.
    fn scene(grad: impl Fn(f64, f64) -> f64, seed: u64, with_target: bool) -> Frame {
        let mut rng = Rng::new(seed);
        let mut data = vec![0.0_f32; W * H];
        for y in 0..H {
            for x in 0..W {
                let g = grad(norm(x as f64, W), norm(y as f64, H));
                data[y * W + x] = (g + 10.0 * rng.normal()) as f32;
            }
        }
        // Stars: compact Gaussians, bright.
        for _ in 0..60 {
            let (cx, cy) = (rng.uniform() * W as f64, rng.uniform() * H as f64);
            let amp = 2000.0 + 20_000.0 * rng.uniform();
            for y in (cy as usize).saturating_sub(10)..(cy as usize + 10).min(H) {
                for x in (cx as usize).saturating_sub(10)..(cx as usize + 10).min(W) {
                    let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                    data[y * W + x] += (amp * (-r2 / (2.0 * 1.8 * 1.8)).exp()) as f32;
                }
            }
        }
        if with_target {
            // A faint extended nebula that is not background: 90 ADU peak.
            for y in 0..H {
                for x in 0..W {
                    let r2 = (x as f64 - 130.0).powi(2) + (y as f64 - 100.0).powi(2);
                    data[y * W + x] += (90.0 * (-r2 / (2.0 * 35.0 * 35.0)).exp()) as f32;
                }
            }
        }
        Frame::new(W, H, data).expect("frame")
    }

    fn plane(a: f64, b: f64, c: f64) -> impl Fn(f64, f64) -> f64 {
        move |x, y| c + a * x + b * y
    }

    fn poly(degree: usize) -> Options {
        Options {
            model: Model::Polynomial { degree },
            ..Options::default()
        }
    }

    #[test]
    fn terms_are_ordered_by_degree() {
        assert_eq!(terms(0), vec![(0, 0)]);
        assert_eq!(terms(1), vec![(0, 0), (1, 0), (0, 1)]);
        assert_eq!(terms(2).len(), 6);
        assert_eq!(terms(2)[3], (2, 0));
        assert_eq!(terms(4).len(), 15);
    }

    #[test]
    fn recovers_the_coefficients_of_a_plane_through_stars_and_nebula() {
        let (a, b, c) = (120.0, -80.0, 1000.0);
        let f = scene(plane(a, b, c), 1, true);
        let fit = extract(&f, &poly(1)).expect("fit");
        assert!(fit.declined.is_none(), "{:?}", fit.declined);
        let k = &fit.coefficients;
        assert_eq!(k.len(), 3);
        // Terms are 1, x, y. The nebula adds a little light at the left, so
        // allow a couple of ADU on the constant.
        assert!((k[0] - c).abs() < 3.0, "constant {}", k[0]);
        assert!((k[1] - a).abs() < 3.0, "x slope {}", k[1]);
        assert!((k[2] - b).abs() < 3.0, "y slope {}", k[2]);
        assert!(fit.masked_fraction > 0.0 && fit.masked_fraction < 0.5);
        // The surface is the fitted plane at every pixel.
        let mid = fit.surface.get(W / 2, H / 2);
        let want = c + a * norm((W / 2) as f64, W) + b * norm((H / 2) as f64, H);
        assert!((f64::from(mid) - want).abs() < 3.0, "{mid} vs {want}");
    }

    #[test]
    fn masking_stars_and_nebula_beats_a_plain_fit() {
        let (a, b, c) = (60.0, 40.0, 800.0);
        let f = scene(plane(a, b, c), 2, true);
        let masked = extract(&f, &poly(1)).expect("masked");
        // No clipping and no mask rounds: every tile counts, stars and all.
        let plain = extract(
            &f,
            &Options {
                rounds: 0,
                clip_low: 1e9,
                clip_high: 1e9,
                max_scatter: f32::INFINITY,
                ..poly(1)
            },
        )
        .expect("plain");
        let err = |fit: &Fit| {
            let want = |x: usize, y: usize| c + a * norm(x as f64, W) + b * norm(y as f64, H);
            let mut s = 0.0_f64;
            for y in (0..H).step_by(7) {
                for x in (0..W).step_by(7) {
                    s += (f64::from(fit.surface.get(x, y)) - want(x, y)).powi(2);
                }
            }
            (s / ((H / 7 + 1) * (W / 7 + 1)) as f64).sqrt()
        };
        let (em, ep) = (err(&masked), err(&plain));
        assert!(em < 2.5, "masked fit off by {em} ADU rms");
        assert!(
            ep > 2.0 * em,
            "the plain fit ({ep}) should be clearly worse than {em}"
        );
    }

    #[test]
    fn recovers_a_quadratic_with_a_cross_term() {
        // 1500 + 90 x - 60 y + 200 x^2 + 150 x y - 120 y^2
        let g = |x: f64, y: f64| {
            1500.0 + 90.0 * x - 60.0 * y + 200.0 * x * x + 150.0 * x * y - 120.0 * y * y
        };
        let f = scene(g, 3, false);
        let fit = extract(&f, &poly(2)).expect("fit");
        let want = [1500.0, 90.0, -60.0, 200.0, 150.0, -120.0];
        assert_eq!(
            terms(2),
            vec![(0, 0), (1, 0), (0, 1), (2, 0), (1, 1), (0, 2)]
        );
        for (k, (got, want)) in fit.coefficients.iter().zip(want).enumerate() {
            assert!((got - want).abs() < 4.0, "term {k}: {got} vs {want}");
        }
    }

    #[test]
    fn a_thin_plate_spline_follows_a_smooth_non_polynomial_gradient() {
        // A bowl plus a tilt: not a low-degree polynomial.
        let g =
            |x: f64, y: f64| 900.0 + 80.0 * (2.2 * x).cos() * (1.7 * y).cos() + 50.0 * x - 30.0 * y;
        let f = scene(g, 4, false);
        let spline = extract(
            &f,
            &Options {
                model: Model::ThinPlate { smoothing: 0.005 },
                ..Options::default()
            },
        )
        .expect("spline");
        let plane = extract(&f, &poly(1)).expect("plane");
        let rms = |fit: &Fit| {
            let mut s = 0.0_f64;
            let mut n = 0.0;
            for y in (4..H - 4).step_by(5) {
                for x in (4..W - 4).step_by(5) {
                    let want = g(norm(x as f64, W), norm(y as f64, H));
                    s += (f64::from(fit.surface.get(x, y)) - want).powi(2);
                    n += 1.0;
                }
            }
            (s / n).sqrt()
        };
        let (rs, rp) = (rms(&spline), rms(&plane));
        assert!(rs < 3.0, "spline rms error {rs}");
        assert!(
            rp > 4.0 * rs,
            "plane {rp} should be far worse than the spline {rs}"
        );
        assert_eq!(spline.coefficients.len(), 0);
    }

    #[test]
    fn correcting_removes_the_gradient_and_keeps_the_level() {
        let g = plane(150.0, -100.0, 1200.0);
        let f = scene(g, 5, true);
        let fit = extract(&f, &poly(1)).expect("fit");
        let out = fit
            .apply(&f, Mode::Subtract, Level::Preserve)
            .expect("apply");
        // The correction has zero mean: the level is where it was.
        let shift: f64 = out
            .data
            .iter()
            .zip(&f.data)
            .map(|(o, i)| f64::from(*o - *i))
            .sum::<f64>()
            / out.data.len() as f64;
        assert!(shift.abs() < 0.05, "mean shift {shift}");
        // Left, right, top and bottom strips now have the same sky.
        let strip = |x0: usize, x1: usize, y0: usize, y1: usize| {
            let mut v = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    v.push(out.get(x, y));
                }
            }
            f64::from(stats::median_finite(&v))
        };
        let (l, r) = (strip(0, 40, 130, 170), strip(W - 40, W, 130, 170));
        let (t, b) = (strip(220, 260, 0, 40), strip(220, 260, H - 40, H));
        assert!((l - r).abs() < 3.0, "left {l}, right {r}");
        assert!((t - b).abs() < 3.0, "top {t}, bottom {b}");
        // Before the correction they differed by 2 * 150.
        let before = |x0: usize, x1: usize| {
            let mut v = Vec::new();
            for y in 130..170 {
                for x in x0..x1 {
                    v.push(f.get(x, y));
                }
            }
            f64::from(stats::median_finite(&v))
        };
        assert!((before(W - 40, W) - before(0, 40)) > 250.0);
        // Zero takes the whole surface off, so the sky is near zero everywhere.
        let zero = fit.apply(&f, Mode::Subtract, Level::Zero).expect("apply");
        let sky = |x0: usize, y0: usize| {
            let mut v = Vec::new();
            for y in y0..y0 + 30 {
                for x in x0..x0 + 30 {
                    v.push(zero.get(x, y));
                }
            }
            f64::from(stats::median_finite(&v))
        };
        for (x, y) in [(5, 135), (W - 35, 135), (200, 5), (200, H - 35)] {
            assert!(sky(x, y).abs() < 4.0, "sky at {x},{y}: {}", sky(x, y));
        }
    }

    #[test]
    fn divide_flattens_vignetting_and_keeps_the_mean() {
        let g = |x: f64, y: f64| 2000.0 * (1.0 - 0.35 * (x * x + y * y) / 2.0);
        let f = scene(g, 6, false);
        let fit = extract(&f, &poly(2)).expect("fit");
        let out = fit.apply(&f, Mode::Divide, Level::Preserve).expect("apply");
        let patch = |fr: &Frame, cx: usize, cy: usize| {
            let mut v = Vec::new();
            for y in cy - 15..cy + 15 {
                for x in cx - 15..cx + 15 {
                    v.push(fr.get(x, y));
                }
            }
            f64::from(stats::median_finite(&v))
        };
        let centre = patch(&out, W / 2, H / 2);
        let corner = patch(&out, 30, 30);
        assert!(
            (centre - corner).abs() / centre < 0.01,
            "centre {centre}, corner {corner}"
        );
        let before = patch(&f, W / 2, H / 2) / patch(&f, 30, 30);
        assert!(
            before > 1.2,
            "the scene should be vignetted, ratio {before}"
        );
        let mean = |fr: &Frame| f64::from(stats::median_finite(&stats::subsample(fr, 1 << 20)));
        assert!((mean(&out) - mean(&f)).abs() / mean(&f) < 0.02);
    }

    #[test]
    fn missing_data_is_skipped_and_stays_missing() {
        let mut f = scene(plane(100.0, 50.0, 1000.0), 7, false);
        // A stack's uncovered edge: 50 columns of NaN down the right side.
        for y in 0..H {
            for x in W - 50..W {
                f.data[y * W + x] = f32::NAN;
            }
        }
        let fit = extract(&f, &poly(1)).expect("fit");
        assert!(
            (fit.coefficients[1] - 100.0).abs() < 6.0,
            "{:?}",
            fit.coefficients
        );
        let out = fit
            .apply(&f, Mode::Subtract, Level::Preserve)
            .expect("apply");
        assert!(out.get(W - 1, 10).is_nan());
        assert!(out.get(10, 10).is_finite());
    }

    #[test]
    fn a_target_that_fills_the_frame_is_declined_not_subtracted() {
        // Emission everywhere: a broad Gaussian much larger than the frame.
        let mut f = scene(plane(0.0, 0.0, 100.0), 8, false);
        for y in 0..H {
            for x in 0..W {
                let r2 = (x as f64 - 200.0).powi(2) + (y as f64 - 150.0).powi(2);
                f.data[y * W + x] += (900.0 * (-r2 / (2.0 * 220.0 * 220.0)).exp()) as f32;
            }
        }
        let fit = extract(
            &f,
            &Options {
                exclude_target: true,
                ..poly(1)
            },
        )
        .expect("fit");
        let why = fit.declined.clone().expect("should be declined");
        assert!(why.contains("clear sky"), "{why}");
        let out = fit.apply(&f, Mode::Subtract, Level::Zero).expect("apply");
        assert_eq!(
            out.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            f.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "a declined fit must change nothing"
        );
    }

    #[test]
    fn a_dome_the_masks_miss_is_declined_for_scatter() {
        // A broad bright dome, no target exclusion: the plane would remove a
        // little tilt and leave a scatter far larger than that.
        let mut f = scene(plane(0.0, 0.0, 300.0), 11, false);
        for y in 0..H {
            for x in 0..W {
                let r2 = (x as f64 - 200.0).powi(2) + (y as f64 - 150.0).powi(2);
                f.data[y * W + x] += (2500.0 * (-r2 / (2.0 * 110.0 * 110.0)).exp()) as f32;
            }
        }
        let fit = extract(&f, &poly(1)).expect("fit");
        let why = fit.declined.expect("declined");
        assert!(why.contains("structure"), "{why}");
    }

    #[test]
    fn excluding_the_target_stops_a_fit_eating_a_halo() {
        // The pipeline's lesson: a bright centre with a wide halo and a flat
        // sky. Fitting tile medians as they are, without rejection, the halo
        // pulls the sky level up; excluding the target, sized from the data,
        // leaves the true sky.
        let mut f = scene(plane(0.0, 0.0, 500.0), 9, false);
        for y in 0..H {
            for x in 0..W {
                let r2 = (x as f64 - 200.0).powi(2) + (y as f64 - 150.0).powi(2);
                f.data[y * W + x] += (400.0 * (-r2 / (2.0 * 45.0 * 45.0)).exp()) as f32;
            }
        }
        let bare = Options {
            rounds: 0,
            clip_low: 1e9,
            clip_high: 1e9,
            max_scatter: f32::INFINITY,
            ..poly(0)
        };
        let all = extract(&f, &bare).expect("fit");
        let without = extract(
            &f,
            &Options {
                exclude_target: true,
                ..bare.clone()
            },
        )
        .expect("fit");
        assert!(without.declined.is_none(), "{:?}", without.declined);
        let (e_all, e_without) = (
            (all.coefficients[0] - 500.0).abs(),
            (without.coefficients[0] - 500.0).abs(),
        );
        assert!(
            e_without < 4.0,
            "excluding the target: off by {e_without} (without exclusion {e_all})"
        );
        assert!(e_all > 3.0 * e_without, "{e_all} vs {e_without}");
        assert!(without.masked_fraction > 0.1);
        // The default rejection finds the sky by itself, target or not.
        let robust = extract(&f, &poly(0)).expect("fit");
        assert!(
            (robust.coefficients[0] - 500.0).abs() < 2.0,
            "{:?}",
            robust.coefficients
        );
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let f = scene(plane(0.0, 0.0, 100.0), 10, false);
        let mut mosaic = f.clone();
        mosaic.bayer = Some(crate::BayerPattern::Rggb);
        assert!(extract(&mosaic, &Options::default()).is_err());
        assert!(extract(&f, &poly(7)).is_err());
        assert!(
            extract(
                &f,
                &Options {
                    model: Model::ThinPlate { smoothing: -1.0 },
                    ..Options::default()
                }
            )
            .is_err()
        );
        let small = Frame::zeros(40, 40);
        assert!(extract(&small, &Options::default()).is_err());
        let fit = extract(&f, &poly(1)).expect("fit");
        assert!(fit.apply(&small, Mode::Subtract, Level::Preserve).is_err());
    }

    #[test]
    fn solver_and_growth_helpers() {
        // 2x + y = 5, x - y = 1  ->  x = 2, y = 1
        let x = solve(vec![2.0, 1.0, 1.0, -1.0], vec![5.0, 1.0], 2).expect("solved");
        assert!((x[0] - 2.0).abs() < 1e-12 && (x[1] - 1.0).abs() < 1e-12);
        assert!(solve(vec![1.0, 2.0, 2.0, 4.0], vec![1.0, 2.0], 2).is_none());
        let mut m = vec![false; 9 * 7];
        m[3 * 9 + 4] = true;
        grow(&mut m, 9, 7, 2);
        let set: usize = m.iter().filter(|&&v| v).count();
        assert_eq!(set, 25, "a 5 x 5 square");
        assert!(m[3 * 9 + 4] && m[9 + 2] && !m[0]);
    }
}
