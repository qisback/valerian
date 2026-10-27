// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Drizzle: variable-pixel linear reconstruction of dithered frames.
//!
//! A sensor whose pixels are larger than the star images on it (undersampled)
//! loses detail that no single frame can give back. If the telescope is nudged
//! between frames (dithered) each frame samples the sky at different places, and
//! together they hold the detail. Drizzle (Fruchter and Hook, 2002) puts it
//! back: every input pixel is shrunk to a smaller square, a *drop* of side
//! `pixfrac` pixels, the drop is mapped onto a finer output grid through the
//! frame's registration, and its value is spread over the output pixels it
//! overlaps in proportion to the area of the overlap. The output is the weighted
//! mean of what landed on each pixel:
//!
//! ```text
//! I_out = sum(a * w * f) / sum(a * w)
//! ```
//!
//! with `a` the overlap area, `w` the frame weight and `f` the input value. It is
//! the surface brightness of the input frames, so the output has the units of the
//! input and a star is `scale^2` times as many pixels, each `1 / scale^2` as
//! bright in total.
//!
//! The method, in the order it runs:
//!
//! 1. **Register.** The frames are registered to a reference exactly as for
//!    stacking ([`crate::stack::register_frames`]); a raw colour mosaic is
//!    registered through a demosaiced copy.
//! 2. **Normalise and weight.** Each frame's sky is matched to the reference and
//!    its transparency scaled, and frames are weighted by noise and star width,
//!    as in [`crate::stack`].
//! 3. **Drizzle.** Every frame is dropped onto the output grid. A raw Bayer
//!    mosaic is dropped *as a mosaic*: each pixel goes only to the plane of its
//!    own colour, so nothing is interpolated and the three planes are each
//!    sampled by the real pixels, which is where a one-shot-colour camera gains
//!    most.
//! 4. **Reject.** Outliers (cosmic rays, hot pixels dragged to different places by
//!    the dither, satellite trails) are found by a second pass: the first result
//!    is read back around the place of each input pixel, and a pixel outside the
//!    range of what is there by more than `reject` sigma of the frame's noise (plus
//!    a twentieth of the local signal) is left out of the final drizzle. The range
//!    and not one value, because a star's core is sharper than the read-back.
//!
//! The finer the output grid and the smaller the drops, the sharper the result and
//! the more frames it needs to fill the grid: with fewer drops than output pixels
//! there are holes (the weight map shows them) and the noise of neighbouring
//! output pixels is correlated. A `pixfrac` of 1 with a `scale` of 1 is plain
//! shift-and-add; the usual choice is a scale of 2 and a `pixfrac` of 0.6 to 0.8
//! for a dozen or more dithers.
//!
//! # References
//!
//! - Fruchter, A. S. and Hook, R. N. (2002), Drizzle: a method for the linear reconstruction of undersampled images, Publ. Astron. Soc. Pac. 114, 144-152, doi:10.1086/338393 (implemented from the paper; none of the Space Telescope Science Institute drizzle code is used).

use std::borrow::Cow;

use rayon::prelude::*;

use crate::CancelToken;
use crate::debayer::{Method, demosaic_image};
use crate::error::{Error, Result};
use crate::fits::{Image, Value};
use crate::stack::register_frames;
use crate::stack::{FrameReport, Registered, StackOptions, Status, frame_weights_for, plane_sky};
use crate::stats;
use crate::transform::Transform;
use helia_core::{BayerPattern, Frame};

/// Settings for [`drizzle`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Output pixels per reference pixel, linear: 2 makes the output twice as wide.
    pub scale: f64,
    /// Side of a drop as a fraction of an input pixel, `0 < pixfrac <= 1`.
    pub pixfrac: f64,
    /// Reject input pixels this many sigma from the first-pass result; `None`
    /// drizzles every pixel.
    pub reject: Option<f64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale: 2.0,
            pixfrac: 0.6,
            reject: Some(4.0),
        }
    }
}

/// A drizzled image.
#[derive(Clone, Debug)]
pub struct Drizzled {
    /// The result, `scale` times the reference's size, one plane per colour (three
    /// for a mosaic). Output pixels no drop reached are NaN.
    pub image: Image,
    /// The weight map of the first plane: the summed overlap area times frame
    /// weight at every output pixel. Low values are thinly covered.
    pub weight: Frame,
    /// How many input pixels (drops) reached each output pixel of the first
    /// plane (saturating at 65535).
    pub count: Frame,
    /// How many input pixels the second pass rejected as outliers, counted at
    /// the output pixel their centre falls in (first plane); all zero without
    /// the second pass.
    pub rejection: Frame,
    /// One entry per input frame, as for a stack.
    pub frames: Vec<FrameReport>,
    /// Index of the reference frame.
    pub reference: usize,
    /// Fraction of all input samples rejected as outliers.
    pub rejected: f64,
    /// The scale used.
    pub scale: f64,
    /// The drop size used.
    pub pixfrac: f64,
}

impl Drizzled {
    /// Number of frames drizzled, the reference included.
    #[must_use]
    pub fn used(&self) -> usize {
        self.frames
            .iter()
            .filter(|f| matches!(f.status, Status::Stacked | Status::Reference))
            .count()
    }

    /// Fraction of the output pixels that no drop reached.
    #[must_use]
    pub fn holes(&self) -> f64 {
        let n = self.image.planes.first().map_or(0, |p| p.data.len());
        if n == 0 {
            return 0.0;
        }
        self.image.planes[0]
            .data
            .iter()
            .filter(|v| !v.is_finite())
            .count() as f64
            / n as f64
    }
}

type Pt = (f64, f64);

/// One output plane: the running sums.
struct Layer {
    w: usize,
    h: usize,
    num: Vec<f32>,
    den: Vec<f32>,
    /// Drops that reached each pixel.
    cnt: Vec<u16>,
    /// Drops rejected as outliers, at the pixel of their centre.
    rej: Vec<u16>,
}

impl Layer {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            num: vec![0.0; w * h],
            den: vec![0.0; w * h],
            cnt: vec![0; w * h],
            rej: vec![0; w * h],
        }
    }

    /// The mean at `(x, y)` by bilinear interpolation over the pixels that have
    /// data; `None` where the neighbourhood is empty.
    fn sample(&self, x: f64, y: f64) -> Option<f32> {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
        let (mut acc, mut wsum) = (0.0f32, 0.0f32);
        for (dy, wy) in [(0i64, 1.0 - fy), (1, fy)] {
            for (dx, wx) in [(0i64, 1.0 - fx), (1, fx)] {
                let (xi, yi) = (x0 as i64 + dx, y0 as i64 + dy);
                if xi < 0 || yi < 0 || xi >= self.w as i64 || yi >= self.h as i64 {
                    continue;
                }
                let i = yi as usize * self.w + xi as usize;
                if self.den[i] > 0.0 {
                    let k = wx * wy;
                    acc += k * self.num[i] / self.den[i];
                    wsum += k;
                }
            }
        }
        (wsum > 0.25).then(|| acc / wsum)
    }

    /// The lowest and highest of the mean at `(x, y)` and four points `d` away
    /// along the axes: what the first pass says this input pixel could be.
    fn range(&self, x: f64, y: f64, d: f64) -> Option<(f32, f32)> {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for (dx, dy) in [(0.0, 0.0), (d, 0.0), (-d, 0.0), (0.0, d), (0.0, -d)] {
            if let Some(v) = self.sample(x + dx, y + dy) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        (lo <= hi).then_some((lo, hi))
    }
}

/// What the second pass compares each input pixel with.
struct Blot<'a> {
    layer: &'a Layer,
    /// Rejection limit in sigmas.
    nsig: f32,
    /// Noise of the frame after normalisation, in the output's units.
    sigma: f32,
    /// Sky level of the reference, which the share of local signal is above.
    sky: f32,
    /// Half an input pixel in output pixels: how far the read-back looks around.
    reach: f64,
}

/// Everything one frame, one output plane needs.
struct Job<'a> {
    src: &'a Frame,
    /// For a mosaic, the pattern and the colour this plane takes.
    only: Option<(BayerPattern, usize)>,
    /// Input pixel coordinates to output grid coordinates.
    map: Transform,
    /// And back.
    inverse: Transform,
    /// Half the drop side, input pixels.
    half: f64,
    weight: f32,
    /// Sky of this frame and plane, transparency scale, and the reference's sky.
    sky: f32,
    scale: f32,
    ref_sky: f32,
    blot: Option<Blot<'a>>,
}

type Poly = [Pt; 12];

fn coord(p: Pt, axis: usize) -> f64 {
    if axis == 0 { p.0 } else { p.1 }
}

/// Clip `inp[..n]` to the half plane `sign * (coord - bound) >= 0`.
fn clip_edge(inp: &Poly, n: usize, out: &mut Poly, axis: usize, bound: f64, sign: f64) -> usize {
    let mut m = 0;
    for k in 0..n {
        let (a, b) = (inp[k], inp[(k + 1) % n]);
        let (da, db) = (
            sign * (coord(a, axis) - bound),
            sign * (coord(b, axis) - bound),
        );
        if da >= 0.0 {
            out[m] = a;
            m += 1;
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            out[m] = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
            m += 1;
        }
    }
    m
}

fn area(p: &Poly, n: usize) -> f64 {
    let mut s = 0.0;
    for k in 0..n {
        let (a, b) = (p[k], p[(k + 1) % n]);
        s += a.0 * b.1 - b.0 * a.1;
    }
    0.5 * s.abs()
}

/// Area of the convex quadrilateral `quad` inside the box `[x0, x1] x [y0, y1]`.
fn overlap(quad: &[Pt; 4], x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    let mut a: Poly = [(0.0, 0.0); 12];
    let mut b: Poly = [(0.0, 0.0); 12];
    a[..4].copy_from_slice(quad);
    let mut n = clip_edge(&a, 4, &mut b, 0, x0, 1.0);
    if n < 3 {
        return 0.0;
    }
    n = clip_edge(&b, n, &mut a, 0, x1, -1.0);
    if n < 3 {
        return 0.0;
    }
    n = clip_edge(&a, n, &mut b, 1, y0, 1.0);
    if n < 3 {
        return 0.0;
    }
    n = clip_edge(&b, n, &mut a, 1, y1, -1.0);
    if n < 3 {
        return 0.0;
    }
    area(&a, n)
}

/// The rows of one output band: the running sums and counts.
struct Band<'a> {
    num: &'a mut [f32],
    den: &'a mut [f32],
    cnt: &'a mut [u16],
    rej: &'a mut [u16],
}

/// Drop `job.src` onto the rows `y0..y0 + rows` of an output of width `ow`;
/// `num` and `den` are those rows. Returns the input samples rejected and
/// counted whose centre falls in those rows.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the input pixels of a band"
)]
fn drop_band(
    job: &Job<'_>,
    (ow, oh): (usize, usize),
    y0: usize,
    rows: usize,
    band: &mut Band<'_>,
) -> (u64, u64) {
    let Band { num, den, cnt, rej } = band;
    let (lo, hi) = (y0 as f64 - 0.5, (y0 + rows) as f64 - 0.5);
    // The input pixels that can reach this band.
    let corners = [
        (-0.5, lo - 1.0),
        (ow as f64 - 0.5, lo - 1.0),
        (-0.5, hi + 1.0),
        (ow as f64 - 0.5, hi + 1.0),
    ];
    let (mut bx0, mut bx1, mut by0, mut by1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for c in corners {
        let p = job.inverse.apply(c);
        bx0 = bx0.min(p.0);
        bx1 = bx1.max(p.0);
        by0 = by0.min(p.1);
        by1 = by1.max(p.1);
    }
    let reach = 2.0 + job.half;
    let (sw, sh) = (job.src.width as i64, job.src.height as i64);
    let xs = ((bx0 - reach).floor().max(0.0) as i64)
        ..=((bx1 + reach).ceil().min(sw as f64 - 1.0) as i64);
    let ys = ((by0 - reach).floor().max(0.0) as i64)
        ..=((by1 + reach).ceil().min(sh as f64 - 1.0) as i64);
    let (mut rejected, mut counted) = (0u64, 0u64);
    for y in ys {
        for x in xs.clone() {
            if let Some((pat, c)) = job.only
                && pat.channel_at(x as usize, y as usize) != c
            {
                continue;
            }
            let v = job.src.data[(y * sw + x) as usize];
            if !v.is_finite() {
                continue;
            }
            let (fx, fy) = (x as f64, y as f64);
            let quad = [
                job.map.apply((fx - job.half, fy - job.half)),
                job.map.apply((fx + job.half, fy - job.half)),
                job.map.apply((fx + job.half, fy + job.half)),
                job.map.apply((fx - job.half, fy + job.half)),
            ];
            let (mut qx0, mut qx1, mut qy0, mut qy1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            for q in &quad {
                qx0 = qx0.min(q.0);
                qx1 = qx1.max(q.0);
                qy0 = qy0.min(q.1);
                qy1 = qy1.max(q.1);
            }
            if qy1 <= lo || qy0 >= hi || qx1 <= -0.5 || qx0 >= ow as f64 - 0.5 {
                continue;
            }
            let value = (v - job.sky) * job.scale + job.ref_sky;
            let centre = job.map.apply((fx, fy));
            let owned = centre.1 >= lo && centre.1 < hi;
            if let Some(b) = &job.blot {
                if owned {
                    counted += 1;
                }
                // The read-back is smoother than the pixel, so a pixel is judged
                // against the range around it, not the one value.
                if let Some((lo_read, hi_read)) = b.layer.range(centre.0, centre.1, b.reach) {
                    let tol = b.nsig * b.sigma + 0.05 * (hi_read - b.sky).max(0.0);
                    if value > hi_read + tol || value < lo_read - tol {
                        if owned {
                            rejected += 1;
                            let (ci, cj) = ((centre.0 + 0.5).floor(), (centre.1 + 0.5).floor());
                            if ci >= 0.0 && ci < ow as f64 && cj >= y0 as f64 {
                                let k = (cj as usize - y0) * ow + ci as usize;
                                if k < rej.len() {
                                    rej[k] = rej[k].saturating_add(1);
                                }
                            }
                        }
                        continue;
                    }
                }
            } else if owned {
                counted += 1;
            }
            let (i0, i1) = (
                (qx0 + 0.5).floor().max(0.0) as usize,
                ((qx1 + 0.5).floor() as i64).clamp(0, ow as i64 - 1) as usize,
            );
            let (j0, j1) = (
                ((qy0 + 0.5).floor().max(y0 as f64)) as usize,
                ((qy1 + 0.5).floor() as i64).clamp(0, (y0 + rows).min(oh) as i64 - 1) as usize,
            );
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let a = overlap(
                        &quad,
                        i as f64 - 0.5,
                        i as f64 + 0.5,
                        j as f64 - 0.5,
                        j as f64 + 0.5,
                    ) as f32;
                    if a > 0.0 {
                        let k = (j - y0) * ow + i;
                        let aw = a * job.weight;
                        num[k] += aw * value;
                        den[k] += aw;
                        cnt[k] = cnt[k].saturating_add(1);
                    }
                }
            }
        }
    }
    (rejected, counted)
}

/// Rows of the output one task owns.
const BAND: usize = 32;

/// Drop one frame's plane onto `layer`; returns the samples (rejected, counted).
fn drop_frame(job: &Job<'_>, layer: &mut Layer) -> (u64, u64) {
    let (ow, oh) = (layer.w, layer.h);
    layer
        .num
        .par_chunks_mut(ow * BAND)
        .zip(layer.den.par_chunks_mut(ow * BAND))
        .zip(layer.cnt.par_chunks_mut(ow * BAND))
        .zip(layer.rej.par_chunks_mut(ow * BAND))
        .enumerate()
        .map(|(b, (((num, den), cnt), rej))| {
            let rows = num.len() / ow;
            drop_band(
                job,
                (ow, oh),
                b * BAND,
                rows,
                &mut Band { num, den, cnt, rej },
            )
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

fn validate(opts: &Options) -> Result<()> {
    if !(opts.scale.is_finite() && (0.25..=8.0).contains(&opts.scale)) {
        return Err(Error::invalid("the drizzle scale must be 0.25 to 8"));
    }
    if !(opts.pixfrac > 0.0 && opts.pixfrac <= 1.0) {
        return Err(Error::invalid(
            "pixfrac must be above 0 and at most 1 (the drop is that fraction of a pixel)",
        ));
    }
    if let Some(r) = opts.reject
        && !(r > 0.0 && r.is_finite())
    {
        return Err(Error::invalid("the rejection limit must be above zero"));
    }
    Ok(())
}

/// The mosaic pattern all frames share, or `None` when none is a mosaic.
fn mosaic_of(frames: &[Image]) -> Result<Option<BayerPattern>> {
    let pat = |f: &Image| (f.planes.len() == 1).then(|| f.planes[0].bayer).flatten();
    let first = pat(&frames[0]);
    for (i, f) in frames.iter().enumerate() {
        if f.planes.iter().any(|p| p.bayer.is_some()) && pat(f).is_none() {
            return Err(Error::invalid(format!(
                "frame {i} has a Bayer pattern on more than one plane"
            )));
        }
        if pat(f) != first {
            return Err(Error::invalid(format!(
                "frame {i} does not have the same Bayer pattern as the first"
            )));
        }
    }
    Ok(first)
}

/// What every job of one drizzle shares.
struct Plan<'a> {
    frames: &'a [Image],
    regs: &'a [Option<crate::register::Registration>],
    /// Transparency scale of each frame.
    flux_scales: Vec<f64>,
    reference: usize,
    weights: &'a [f32],
    keep: &'a [usize],
    /// Sky and noise of each output plane, per frame (empty for frames left out).
    measured: &'a [(Vec<f32>, Vec<f32>)],
    to_grid: Transform,
    pattern: Option<BayerPattern>,
    pixfrac: f64,
    normalise: bool,
    cancel: &'a CancelToken,
}

fn make_job<'a>(
    c: &'a Plan<'a>,
    i: usize,
    p: usize,
    blot: Option<(&'a Layer, f32)>,
) -> Result<Job<'a>> {
    let t = c.regs[i]
        .as_ref()
        .map_or_else(Transform::identity, |r| r.transform);
    let map = c.to_grid.after(&t);
    let inverse = map
        .inverse()
        .ok_or_else(|| Error::invalid("a registration cannot be inverted"))?;
    let scale = if c.normalise {
        c.flux_scales[i] as f32
    } else {
        1.0
    };
    let (sky, ref_sky) = if c.normalise {
        (c.measured[i].0[p], c.measured[c.reference].0[p])
    } else {
        (0.0, 0.0)
    };
    Ok(Job {
        src: &c.frames[i].planes[if c.pattern.is_some() { 0 } else { p }],
        only: c.pattern.map(|pat| (pat, p)),
        map,
        inverse,
        half: c.pixfrac / 2.0,
        weight: c.weights[c.keep.iter().position(|&k| k == i).unwrap_or(0)],
        sky,
        scale,
        ref_sky,
        blot: blot.map(|(layer, nsig)| Blot {
            layer,
            nsig,
            sigma: c.measured[i].1[p] * scale,
            sky: ref_sky,
            reach: 0.5 * c.to_grid.m[0][0],
        }),
    })
}

/// The reference header with the drizzle keywords.
fn drizzle_header(
    frames: &[Image],
    keep: &[usize],
    reference: usize,
    opts: &Options,
) -> crate::fits::Header {
    let mut header = frames[reference].header.clone();
    for k in [
        "CBLACK", "CWHITE", "PEDESTAL", "HISTORY", "IMAGETYP", "BAYERPAT",
    ] {
        header.remove(k);
    }
    crate::wcs::remove_wcs(&mut header);
    let total: f64 = keep
        .iter()
        .filter_map(|&i| frames[i].header.exposure())
        .sum();
    header.set("IMAGETYP", Value::Str("Master Light".into()), "");
    header.set(
        "NCOMBINE",
        Value::Int(keep.len() as i64),
        "frames in this stack",
    );
    if total > 0.0 {
        header.set("EXPTOTAL", Value::Float(total), "[s] total integration");
    }
    header.set(
        "STACKREF",
        Value::Int(reference as i64),
        "index of the registration reference",
    );
    header.set(
        "STACKALG",
        Value::Str("drizzle".into()),
        "rejection and weighting",
    );
    header.set("DRIZSCL", Value::Float(opts.scale), "drizzle output scale");
    header.set(
        "DRIZPIXF",
        Value::Float(opts.pixfrac),
        "drizzle drop size (pixfrac)",
    );
    if let Some(r) = opts.reject {
        header.set("DRIZREJ", Value::Float(r), "drizzle rejection limit, sigma");
    }
    header.add_history(&format!(
        "drizzled by Valerian: {} of {} frames, reference {reference}, scale {}, pixfrac {}",
        keep.len(),
        frames.len(),
        opts.scale,
        opts.pixfrac
    ));
    header
}

/// Sky and noise of each output plane for each frame that is in (empty for the
/// others). The sky comes from the colour copy, the noise from the frame itself.
fn measure_frames(
    frames: &[Image],
    colour: &[Image],
    keep: &[usize],
    nplanes: usize,
    mosaic: bool,
) -> Vec<(Vec<f32>, Vec<f32>)> {
    (0..frames.len())
        .into_par_iter()
        .map(|i| {
            if !keep.contains(&i) {
                return (Vec::new(), Vec::new());
            }
            let sky = colour[i].planes.iter().map(plane_sky).collect();
            let noise = (0..nplanes)
                .map(|p| {
                    if mosaic {
                        stats::noise_from_differences(&frames[i].planes[0], 2)
                    } else {
                        stats::sky_and_noise(&frames[i].planes[p]).1
                    }
                })
                .collect();
            (sky, noise)
        })
        .collect()
}

/// The first drizzle and, when asked, the second with outliers left out.
/// Returns the output layers and the fraction of input samples rejected.
fn accumulate(
    ctx: &Plan<'_>,
    (ow, oh): (usize, usize),
    nplanes: usize,
    opts: &Options,
    log: &mut dyn FnMut(&str),
    reports: &mut [FrameReport],
) -> Result<(Vec<Layer>, f64)> {
    let (keep, nframes) = (ctx.keep, ctx.frames.len());
    let mut layers: Vec<Layer> = (0..nplanes).map(|_| Layer::new(ow, oh)).collect();
    for (p, layer) in layers.iter_mut().enumerate() {
        for &i in keep {
            ctx.cancel.check()?;
            drop_frame(&make_job(ctx, i, p, None)?, layer);
        }
    }
    let mut rejected = 0.0;
    if let Some(nsig) = opts.reject {
        let first = layers;
        log(&format!(
            "second pass: rejecting input pixels more than {nsig} sigma from the first result"
        ));
        let mut second: Vec<Layer> = (0..nplanes).map(|_| Layer::new(ow, oh)).collect();
        let (mut bad, mut all) = (0u64, 0u64);
        let mut per_frame = vec![(0u64, 0u64); nframes];
        for (p, layer) in second.iter_mut().enumerate() {
            for &i in keep {
                ctx.cancel.check()?;
                let (r, n) =
                    drop_frame(&make_job(ctx, i, p, Some((&first[p], nsig as f32)))?, layer);
                per_frame[i].0 += r;
                per_frame[i].1 += n;
                bad += r;
                all += n;
            }
        }
        for &i in keep {
            reports[i].rejected = per_frame[i].0 as f64 / per_frame[i].1.max(1) as f64;
        }
        rejected = bad as f64 / all.max(1) as f64;
        layers = second;
    }
    Ok((layers, rejected))
}

/// The mean of each layer, NaN where no drop landed.
fn finish(layers: Vec<Layer>) -> Vec<Frame> {
    layers
        .into_iter()
        .map(|l| Frame {
            width: l.w,
            height: l.h,
            data: l
                .num
                .par_iter()
                .zip(l.den.par_iter())
                .map(|(&n, &d)| if d > 0.0 { n / d } else { f32::NAN })
                .collect(),
            bayer: None,
        })
        .collect()
}

/// Drizzle `frames`: register them, then drop each onto a finer grid.
///
/// Frames may be mono, colour planes, or raw Bayer mosaics (not mixed); a mosaic
/// gives a three-plane result and is not interpolated.
///
/// # Errors
/// [`Error::Invalid`] for settings out of range and frames that do not match,
/// and [`Error::NoMatch`] when the frames cannot be registered.
pub fn drizzle(
    frames: &[Image],
    stack: &StackOptions,
    opts: &Options,
    log: &mut dyn FnMut(&str),
) -> Result<Drizzled> {
    validate(opts)?;
    if frames.is_empty() {
        return Err(Error::invalid("no frames to drizzle"));
    }
    let pattern = mosaic_of(frames)?;
    // Registration works on colour planes; a mosaic goes through a demosaiced copy.
    let colour: Cow<'_, [Image]> = if pattern.is_some() {
        log("registering through demosaiced copies of the mosaics");
        Cow::Owned(
            frames
                .par_iter()
                .map(|f| demosaic_image(f, Method::default()))
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        Cow::Borrowed(frames)
    };
    let Registered {
        regs,
        keep,
        mut reports,
        reference,
    } = register_frames(&colour, stack, log)?;
    let weights = frame_weights_for(&keep, &reports, stack);
    for (&i, &w) in keep.iter().zip(&weights) {
        reports[i].weight = f64::from(w);
    }
    let (rw, rh) = (frames[reference].width(), frames[reference].height());
    let (ow, oh) = (
        (rw as f64 * opts.scale).ceil() as usize,
        (rh as f64 * opts.scale).ceil() as usize,
    );
    let nplanes = if pattern.is_some() {
        3
    } else {
        frames[reference].planes.len()
    };
    let s = opts.scale;
    let to_grid = Transform {
        m: [
            [s, 0.0, 0.5 * s - 0.5],
            [0.0, s, 0.5 * s - 0.5],
            [0.0, 0.0, 1.0],
        ],
    };
    let measured = measure_frames(frames, &colour, &keep, nplanes, pattern.is_some());
    let normalise = stack.normalisation != crate::stack::Normalisation::None;
    let ctx = Plan {
        frames,
        regs: &regs,
        flux_scales: reports.iter().map(|r| r.flux_scale).collect(),
        reference,
        weights: &weights,
        keep: &keep,
        measured: &measured,
        to_grid,
        pattern,
        pixfrac: opts.pixfrac,
        normalise,
        cancel: &stack.cancel,
    };
    log(&format!(
        "drizzling {} frames onto {ow} x {oh} (scale {s}, pixfrac {}){}",
        keep.len(),
        opts.pixfrac,
        match pattern {
            Some(p) => format!(", raw {p:?} mosaics dropped without interpolation"),
            None => String::new(),
        }
    ));
    let (layers, rejected) = accumulate(&ctx, (ow, oh), nplanes, opts, log, &mut reports)?;
    let weight = Frame {
        width: ow,
        height: oh,
        data: layers[0].den.clone(),
        bayer: None,
    };
    let as_frame = |v: &[u16]| Frame {
        width: ow,
        height: oh,
        data: v.iter().map(|&x| f32::from(x)).collect(),
        bayer: None,
    };
    let (count, rejection) = (as_frame(&layers[0].cnt), as_frame(&layers[0].rej));
    let planes = finish(layers);
    let header = drizzle_header(frames, &keep, reference, opts);
    Ok(Drizzled {
        image: Image { header, planes },
        weight,
        count,
        rejection,
        frames: reports,
        reference,
        rejected,
        scale: opts.scale,
        pixfrac: opts.pixfrac,
    })
}

#[cfg(test)]
mod tests;
