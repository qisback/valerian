// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Mosaics: panes registered onto one larger canvas.
//!
//! A mosaic session (the Seestar's "mosaic mode" folders, or any run where the
//! mount was moved between groups of frames) points the camera at several
//! overlapping parts of a target. `valerian stack` normally keeps the grid of
//! one reference frame, so only that pane survives. With
//! [`MosaicOptions`] set the stack instead:
//!
//! 1. **Registers jointly.** Frames are first matched to the reference, as for
//!    any stack. Those that fail (they see a different part of the sky) are
//!    matched to *anchors*: one placed frame per pane found so far. A frame
//!    that matches an anchor gets the anchor's transform to the reference
//!    composed with its own, and its pane becomes a new anchor, so the next
//!    round reaches panes that only overlap their neighbour, not the reference
//!    (a chain of pairwise registrations, as in Brown and Lowe's panorama
//!    pipeline, with the star pattern matcher of [`crate::register`] in place
//!    of image features).
//! 2. **Grows the canvas** to the bounding box of every placed frame's corners
//!    in reference coordinates. Pixels no frame covers hold no data.
//! 3. **Feathers.** Each frame's samples are weighted by a ramp that rises
//!    linearly from nearly zero at the frame edge to one at
//!    [`MosaicOptions::feather`] pixels inside it. Where panes overlap, each
//!    contributes in proportion to its distance from its own edge, so the seam
//!    is a smooth cross-fade, not a step; inside a pane, where many dithered
//!    frames overlap, the ramp changes almost nothing. Outlier rejection runs
//!    on the samples as usual and the weighted mean uses frame weight times
//!    ramp.
//!
//! Levels are matched to the reference per frame (the usual normalisation), so
//! a sky gradient that differs between panes can still show as a faint seam;
//! `valerian background` after the stack removes the large-scale part.
//!
//! # References
//!
//! - Burt, P. J. and Adelson, E. H. (1983), A multiresolution spline with application to image mosaics, ACM Trans. Graph. 2, 217-236, doi:10.1145/245.247 (why a seam is blended over a wide zone).
//! - Szeliski, R. (2006), Image alignment and stitching: a tutorial, Found. Trends Comput. Graph. Vis. 2, 1-104, doi:10.1561/0600000009 (feathering by distance to the image edge; compositing on a common canvas).
//! - Brown, M. and Lowe, D. G. (2007), Automatic panoramic image stitching using invariant features, Int. J. Comput. Vis. 74, 59-73, doi:10.1007/s11263-006-0002-3 (pairwise registration of overlapping images placed in one frame; gain compensation, which this module does not do).
//! - Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099 (the star matcher).

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::register::register;
use crate::stack::{
    Normalisation, Registered, StackOptions, Status, flux_ratio, initial_reports, pick_reference,
};
use crate::stars::Detection;
use crate::transform::Transform;
use helia_core::Frame;

/// Settings for a mosaic stack.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosaicOptions {
    /// Width in pixels of the ramp at each frame's edge. Zero turns feathering off.
    pub feather: f32,
    /// Fit one additive level per frame and colour plane from the overlaps so
    /// that panes with a different sky level meet without a step.
    pub match_levels: bool,
}

impl Default for MosaicOptions {
    fn default() -> Self {
        Self {
            feather: 120.0,
            match_levels: true,
        }
    }
}

/// The output grid of a mosaic, in the reference frame's pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canvas {
    /// Reference x of canvas column 0.
    pub x0: i64,
    /// Reference y of canvas row 0.
    pub y0: i64,
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
}

impl Canvas {
    /// Moves reference coordinates to canvas coordinates.
    #[must_use]
    pub fn shift(&self) -> Transform {
        Transform::similarity(1.0, 0.0, -(self.x0 as f64), -(self.y0 as f64))
    }
}

/// The canvas that holds every frame, given each frame's transform to the
/// reference and its size.
///
/// # Errors
/// [`Error::Invalid`] when the result is more than 12 times the area of the
/// largest frame or has a side over 40 times the longest frame side, which
/// means a registration put a frame in the wrong place.
pub fn canvas_for(frames: &[(Transform, (usize, usize))]) -> Result<Canvas> {
    let (mut x_min, mut y_min) = (f64::INFINITY, f64::INFINITY);
    let (mut x_max, mut y_max) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut biggest = (1_usize, 1_usize);
    for (t, (w, h)) in frames {
        biggest = (biggest.0.max(*w), biggest.1.max(*h));
        let (fw, fh) = (*w as f64, *h as f64);
        for c in [
            (-0.5, -0.5),
            (fw - 0.5, -0.5),
            (-0.5, fh - 0.5),
            (fw - 0.5, fh - 0.5),
        ] {
            let (x, y) = t.apply(c);
            x_min = x_min.min(x);
            y_min = y_min.min(y);
            x_max = x_max.max(x);
            y_max = y_max.max(y);
        }
    }
    if !(x_min.is_finite() && y_min.is_finite() && x_max.is_finite() && y_max.is_finite()) {
        return Err(Error::invalid("the mosaic canvas is not finite"));
    }
    let x0 = (x_min + 0.5).floor() as i64;
    let y0 = (y_min + 0.5).floor() as i64;
    let x1 = (x_max - 0.5).ceil() as i64;
    let y1 = (y_max - 0.5).ceil() as i64;
    let canvas = Canvas {
        x0,
        y0,
        width: (x1 - x0 + 1).max(1) as usize,
        height: (y1 - y0 + 1).max(1) as usize,
    };
    let area = canvas.width as f64 * canvas.height as f64;
    let longest = biggest.0.max(biggest.1) as f64;
    if area > 12.0 * (biggest.0 * biggest.1) as f64
        || canvas.width.max(canvas.height) as f64 > 40.0 * longest
    {
        return Err(Error::invalid(format!(
            "the mosaic canvas would be {} x {} pixels for frames of {} x {}: \
             a registration put a frame in the wrong place. Leave out the stray frames, \
             or stack the panes separately",
            canvas.width, canvas.height, biggest.0, biggest.1
        )));
    }
    Ok(canvas)
}

/// The feather weight of every canvas pixel for one frame: zero outside the
/// frame, rising linearly with the distance (in frame pixels) from the nearest
/// frame edge to one at `feather` pixels, never below 0.001 inside the frame
/// so that a pixel only this frame covers is still its own.
///
/// # Errors
/// [`Error::Invalid`] if `to_canvas` cannot be inverted.
pub fn feather_weights(
    to_canvas: &Transform,
    size: (usize, usize),
    canvas: (usize, usize),
    feather: f32,
) -> Result<Frame> {
    let inv = to_canvas
        .inverse()
        .ok_or_else(|| Error::invalid("the transform cannot be inverted"))?;
    let (sw, sh) = (size.0 as f64, size.1 as f64);
    let ramp = f64::from(feather.max(0.0));
    let mut data = vec![0.0_f32; canvas.0 * canvas.1];
    data.par_chunks_mut(canvas.0)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                let (sx, sy) = inv.apply((x as f64, y as f64));
                if !(sx >= -0.5 && sy >= -0.5 && sx <= sw - 0.5 && sy <= sh - 0.5) {
                    continue;
                }
                let edge = (sx + 0.5)
                    .min(sy + 0.5)
                    .min(sw - 0.5 - sx)
                    .min(sh - 0.5 - sy);
                *out = if ramp > 0.0 {
                    (edge / ramp).clamp(0.001, 1.0) as f32
                } else {
                    1.0
                };
            }
        });
    Frame::new(canvas.0, canvas.1, data).map_err(|e| Error::invalid(e.to_string()))
}

/// Keep only the part of a mosaic that at least `min_coverage` frames reached:
/// pixels below it become no data, and the canvas is cut to the bounding box of
/// what is left (the world coordinate reference pixel moves with it). Returns
/// the new size, or an error when nothing reaches that depth.
///
/// # Errors
/// [`Error::Invalid`] when no pixel has `min_coverage` frames.
pub fn crop_to_coverage(
    stacked: &mut crate::stack::Stacked,
    min_coverage: u16,
) -> Result<(usize, usize)> {
    use crate::fits::Value;
    let cov = &stacked.coverage;
    let (w, h) = (cov.width, cov.height);
    let enough = |x: usize, y: usize| cov.data[y * w + x] >= f32::from(min_coverage);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if enough(x, y) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return Err(Error::invalid(format!(
            "no pixel of the mosaic is covered by {min_coverage} frames; lower --mosaic-min-coverage"
        )));
    }
    let (nw, nh) = (x1 - x0 + 1, y1 - y0 + 1);
    let cut = |f: &Frame, mask: bool| -> Frame {
        let mut data = Vec::with_capacity(nw * nh);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let keep = !mask || enough(x, y);
                data.push(if keep { f.data[y * w + x] } else { f32::NAN });
            }
        }
        Frame {
            width: nw,
            height: nh,
            data,
            bayer: None,
        }
    };
    for p in &mut stacked.image.planes {
        *p = cut(p, true);
    }
    let cov_cut = {
        let mut c = cut(&stacked.coverage, false);
        for v in &mut c.data {
            if *v < f32::from(min_coverage) {
                *v = 0.0;
            }
        }
        c
    };
    stacked.coverage = cov_cut;
    let hdr = &mut stacked.image.header;
    for (key, shift) in [("CRPIX1", x0), ("CRPIX2", y0)] {
        if let Some(v) = hdr.get_f64(key) {
            hdr.set(key, Value::Float(v - shift as f64), "");
        }
    }
    hdr.set(
        "MOSMINCV",
        Value::Int(i64::from(min_coverage)),
        "least frames per pixel kept",
    );
    hdr.add_history(&format!(
        "mosaic cropped to {nw} x {nh} where at least {min_coverage} frames reach"
    ));
    Ok((nw, nh))
}

/// The fewest matched stars to trust a pane-to-pane registration. A plain stack
/// accepts 6; a wrong match there only drops one frame, but in a mosaic it
/// would put a frame in the wrong place on the canvas.
const MIN_MOSAIC_MATCHES: usize = 10;

/// Largest departure from a scale of 1 accepted: every pane comes from one
/// camera and lens, so a registration that rescales is a wrong match.
const MAX_SCALE_ERROR: f64 = 0.02;

/// Whether a registration is good enough to place a frame on the canvas.
fn trustworthy(reg: &crate::register::Registration) -> bool {
    reg.matched >= MIN_MOSAIC_MATCHES
        && (reg.transform.decompose().0 - 1.0).abs() <= MAX_SCALE_ERROR
}

/// Block size of the low-resolution copies the level fit works on.
pub(crate) const LEVEL_BIN: usize = 8;

/// Block-average every plane by `b` (non-finite pixels ignored; a block with
/// none is NaN).
pub(crate) fn bin_planes(planes: &[Frame], b: usize) -> Vec<Frame> {
    planes
        .iter()
        .map(|f| {
            let (bw, bh) = (f.width.div_ceil(b), f.height.div_ceil(b));
            let mut data = vec![f32::NAN; bw * bh];
            data.par_chunks_mut(bw).enumerate().for_each(|(by, row)| {
                for (bx, out) in row.iter_mut().enumerate() {
                    let (mut sum, mut n) = (0.0_f64, 0_u32);
                    for y in by * b..((by + 1) * b).min(f.height) {
                        let x1 = ((bx + 1) * b).min(f.width);
                        for &v in &f.data[y * f.width + bx * b..y * f.width + x1] {
                            if v.is_finite() {
                                sum += f64::from(v);
                                n += 1;
                            }
                        }
                    }
                    if n > 0 {
                        *out = (sum / f64::from(n)) as f32;
                    }
                }
            });
            Frame {
                width: bw,
                height: bh,
                data,
                bayer: None,
            }
        })
        .collect()
}

/// A frame on the low-resolution canvas: its planes (NaN outside it) and its
/// feather weight.
pub(crate) struct LowFrame {
    pub planes: Vec<Frame>,
    pub weight: Frame,
}

/// Additive level of each frame and plane that makes the frames agree where
/// they overlap, with the reference at zero.
///
/// Gauss-Seidel style gain compensation in the manner of Brown and Lowe, but
/// additive (the sky is an offset, and transparency is already scaled): each
/// round forms the weighted mean of all frames, then moves every frame by the
/// median of its difference from that mean over the pixels at least two frames
/// cover. The median ignores stars and a galaxy, which every frame sees alike.
#[allow(
    clippy::needless_range_loop,
    reason = "the plane index selects into several parallel structures"
)]
pub(crate) fn solve_offsets(
    frames: &[LowFrame],
    reference: usize,
    iterations: usize,
) -> Vec<Vec<f32>> {
    let n = frames.len();
    let np = frames.first().map_or(0, |f| f.planes.len());
    let mut off = vec![vec![0.0_f32; np]; n];
    if n < 2 {
        return off;
    }
    let npx = frames[0].weight.data.len();
    for p in 0..np {
        let mut o = vec![0.0_f32; n];
        for _ in 0..iterations {
            let mut sum_w = vec![0.0_f32; npx];
            let mut sum_wv = vec![0.0_f32; npx];
            let mut count = vec![0_u16; npx];
            sum_w
                .par_chunks_mut(4096)
                .zip(sum_wv.par_chunks_mut(4096))
                .zip(count.par_chunks_mut(4096))
                .enumerate()
                .for_each(|(c, ((sw, swv), cnt))| {
                    let base = c * 4096;
                    for (k, f) in frames.iter().enumerate() {
                        let (v, w) = (&f.planes[p].data, &f.weight.data);
                        for j in 0..sw.len() {
                            let (x, wt) = (v[base + j], w[base + j]);
                            if x.is_finite() && wt > 0.0 {
                                sw[j] += wt;
                                swv[j] += wt * (x + o[k]);
                                cnt[j] += 1;
                            }
                        }
                    }
                });
            let steps: Vec<f32> = (0..n)
                .into_par_iter()
                .map(|k| {
                    let (v, w) = (&frames[k].planes[p].data, &frames[k].weight.data);
                    let mut d: Vec<f32> = (0..npx)
                        .filter(|&i| v[i].is_finite() && w[i] > 0.0 && count[i] >= 2)
                        .map(|i| v[i] + o[k] - sum_wv[i] / sum_w[i])
                        .collect();
                    if d.len() < 50 {
                        0.0
                    } else {
                        crate::stats::median_in_place(&mut d)
                    }
                })
                .collect();
            let big = steps.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
            for (x, s) in o.iter_mut().zip(&steps) {
                *x -= s;
            }
            if big < 0.02 {
                break;
            }
        }
        let r = o[reference];
        for (k, x) in o.iter().enumerate() {
            off[k][p] = x - r;
        }
    }
    off
}

/// Rounds of anchor matching before the remaining frames are given up on.
const MAX_ROUNDS: usize = 12;

/// Distance between pane anchors, as a fraction of the shorter frame side: a
/// placed frame farther than this from every anchor starts a new pane.
const PANE_SEPARATION: f64 = 0.3;

/// Register frames to the reference first and then, round by round, to one
/// anchor per pane, so that panes which only overlap their neighbours are
/// reached. `sizes` are the frame sizes (width, height).
///
/// # Errors
/// As for [`crate::stack::register_detections`].
#[allow(clippy::too_many_lines, reason = "one loop that shares its state")]
pub(crate) fn register_mosaic(
    dets: &[Detection],
    sizes: &[(usize, usize)],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Registered> {
    let n = dets.len();
    let (reference, attempts) = pick_reference(dets, opts, log)?;
    log(&format!(
        "reference: frame {reference} (FWHM {:.2} px)",
        dets[reference].median_fwhm()
    ));
    let mut reports = initial_reports(dets, reference);
    let mut regs: Vec<Option<crate::register::Registration>> = vec![None; n];
    // Transform of each placed frame to the reference.
    let mut to_ref: Vec<Option<Transform>> = vec![None; n];
    to_ref[reference] = Some(Transform::identity());
    let mut why: Vec<String> = vec![String::new(); n];
    // Frames worth trying again against other anchors.
    let mut eligible = vec![false; n];
    let mut placed_now: Vec<usize> = Vec::new();
    for (i, attempt) in attempts.into_iter().enumerate() {
        match attempt {
            None => {}
            Some(Ok(reg)) if trustworthy(&reg) => {
                place(
                    i,
                    reference,
                    reg,
                    dets,
                    opts,
                    &mut reports,
                    &mut regs,
                    &mut to_ref,
                    log,
                );
                placed_now.push(i);
            }
            Some(Ok(reg)) => {
                eligible[i] = true;
                why[i] = format!("only {} stars matched, or the scale is wrong", reg.matched);
            }
            Some(Err(e)) => {
                eligible[i] = dets[i].stars.len() >= opts.min_stars
                    && !e.contains("too soft")
                    && !e.contains("cancel");
                why[i] = e;
            }
        }
    }
    let centre =
        |i: usize, t: &Transform| t.apply((sizes[i].0 as f64 / 2.0, sizes[i].1 as f64 / 2.0));
    let separation = PANE_SEPARATION * sizes[reference].0.min(sizes[reference].1) as f64;
    let mut anchors: Vec<usize> = vec![reference];
    let mut tried: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in (0..n).filter(|&i| eligible[i]) {
        tried[i].push(reference);
    }
    let mut anchor_pool = placed_now;
    for round in 1..=MAX_ROUNDS {
        // New anchors: the sharpest placed frame of each pane not yet anchored.
        anchor_pool.sort_by(|&a, &b| dets[a].median_fwhm().total_cmp(&dets[b].median_fwhm()));
        for &p in &anchor_pool {
            let Some(tp) = to_ref[p] else { continue };
            let cp = centre(p, &tp);
            let far = anchors.iter().all(|&a| {
                let ca = centre(a, &to_ref[a].unwrap_or_else(Transform::identity));
                (ca.0 - cp.0).hypot(ca.1 - cp.1) > separation
            });
            if far && dets[p].stars.len() >= opts.min_stars {
                anchors.push(p);
            }
        }
        let todo: Vec<usize> = (0..n)
            .filter(|&i| eligible[i] && to_ref[i].is_none())
            .filter(|&i| anchors.iter().any(|a| !tried[i].contains(a)))
            .collect();
        if todo.is_empty() {
            break;
        }
        opts.cancel.check()?;
        let found: Vec<Option<(usize, crate::register::Registration)>> = todo
            .par_iter()
            .map(|&i| {
                let mut best: Option<(usize, crate::register::Registration)> = None;
                for &a in anchors.iter().filter(|a| !tried[i].contains(a)) {
                    if let Ok(r) = register(&dets[a].stars, &dets[i].stars, &opts.register)
                        && trustworthy(&r)
                        && best.as_ref().is_none_or(|b| r.matched > b.1.matched)
                    {
                        best = Some((a, r));
                    }
                }
                best
            })
            .collect();
        anchor_pool = Vec::new();
        for (&i, f) in todo.iter().zip(found) {
            tried[i].extend(anchors.iter().copied());
            if let Some((a, reg)) = f {
                place(
                    i,
                    a,
                    reg,
                    dets,
                    opts,
                    &mut reports,
                    &mut regs,
                    &mut to_ref,
                    log,
                );
                anchor_pool.push(i);
            }
        }
        log(&format!(
            "mosaic round {round}: {} frames placed, {} anchors (one per pane)",
            anchor_pool.len(),
            anchors.len()
        ));
        if anchor_pool.is_empty() {
            break;
        }
    }
    let mut keep: Vec<usize> = Vec::new();
    for i in 0..n {
        if to_ref[i].is_some() {
            keep.push(i);
        } else {
            let reason = if why[i].is_empty() {
                "not placed".to_owned()
            } else if eligible[i] {
                format!("{}; no pane it overlaps either", why[i])
            } else {
                why[i].clone()
            };
            log(&format!("frame {i}: skipped, {reason}"));
            reports[i].status = Status::Skipped(reason);
        }
    }
    if keep.len() < 2 {
        return Err(Error::NoMatch(
            "fewer than two frames registered; nothing to stack".into(),
        ));
    }
    log(&format!(
        "mosaic: {} panes found (anchors), {} of {n} frames placed",
        anchors.len(),
        keep.len()
    ));
    Ok(Registered {
        regs,
        keep,
        reports,
        reference,
    })
}

/// Record frame `i`, registered to `anchor`, with its transform to the reference.
#[allow(
    clippy::too_many_arguments,
    reason = "the state of one registration round"
)]
fn place(
    i: usize,
    anchor: usize,
    mut reg: crate::register::Registration,
    dets: &[Detection],
    opts: &StackOptions,
    reports: &mut [crate::stack::FrameReport],
    regs: &mut [Option<crate::register::Registration>],
    to_ref: &mut [Option<Transform>],
    log: &mut dyn FnMut(&str),
) {
    let anchor_t = to_ref[anchor].unwrap_or_else(Transform::identity);
    let flux = match opts.normalisation {
        Normalisation::AdditiveScaling => {
            flux_ratio(&reg, &dets[anchor], &dets[i]).unwrap_or_else(|| {
                log(&format!(
                    "frame {i}: too few matched stars to measure its transparency; flux scale 1"
                ));
                1.0
            }) * reports[anchor].flux_scale
        }
        _ => 1.0,
    };
    let composed = anchor_t.after(&reg.transform);
    let (scale, angle, (tx, ty)) = composed.decompose();
    let r = &mut reports[i];
    r.status = Status::Stacked;
    r.matched = reg.matched;
    r.registration_rms = reg.rms;
    r.shift = (tx, ty);
    r.rotation_deg = angle.to_degrees();
    r.scale = scale;
    r.flux_scale = flux;
    log(&format!(
        "frame {i}: registered to frame {anchor}, {} stars matched, rms {:.3} px, shift ({tx:.2}, {ty:.2}), rotation {:.4} deg, flux x{flux:.4}",
        reg.matched,
        reg.rms,
        angle.to_degrees()
    ));
    reg.transform = composed;
    to_ref[i] = Some(composed);
    regs[i] = Some(reg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_is_the_union_of_the_frames() {
        let frames = [
            (Transform::identity(), (100, 80)),
            (Transform::similarity(1.0, 0.0, 60.0, 30.0), (100, 80)),
        ];
        let c = canvas_for(&frames).expect("canvas");
        assert_eq!((c.x0, c.y0, c.width, c.height), (0, 0, 160, 110));
        let frames = [
            (Transform::identity(), (100, 80)),
            (Transform::similarity(1.0, 0.0, -20.0, -5.0), (100, 80)),
        ];
        let c = canvas_for(&frames).expect("canvas");
        assert_eq!((c.x0, c.y0, c.width, c.height), (-20, -5, 120, 85));
    }

    #[test]
    fn a_runaway_registration_is_refused() {
        let frames = [
            (Transform::identity(), (100, 80)),
            (Transform::similarity(1.0, 0.0, 9000.0, 0.0), (100, 80)),
        ];
        assert!(canvas_for(&frames).is_err());
    }

    #[test]
    fn feather_rises_from_the_edge_and_is_zero_outside() {
        let t = Transform::similarity(1.0, 0.0, 10.0, 0.0);
        let f = feather_weights(&t, (50, 40), (80, 40), 10.0).expect("weights");
        let at = |x: usize, y: usize| f.data[y * 80 + x];
        assert_eq!(at(5, 20), 0.0, "left of the frame");
        assert_eq!(at(65, 20), 0.0, "right of the frame");
        assert!(
            at(10, 20) < 0.1,
            "first column is near zero: {}",
            at(10, 20)
        );
        assert!(
            (at(15, 20) - 0.55).abs() < 0.01,
            "half way up the ramp: {}",
            at(15, 20)
        );
        assert!((at(35, 20) - 1.0).abs() < 1e-6, "inside the ramp width");
        assert!(at(35, 0) < 0.1, "top row of the frame is on its edge");
    }

    fn low(w: usize, h: usize, x0: usize, x1: usize, level: f32) -> LowFrame {
        let mut data = vec![f32::NAN; w * h];
        let mut wt = vec![0.0_f32; w * h];
        for y in 0..h {
            for x in x0..x1 {
                data[y * w + x] = level + (x as f32 * 0.1).sin() * 3.0;
                wt[y * w + x] = 1.0;
            }
        }
        LowFrame {
            planes: vec![Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            }],
            weight: Frame {
                width: w,
                height: h,
                data: wt,
                bayer: None,
            },
        }
    }

    #[test]
    fn levels_are_matched_along_a_chain_of_panes() {
        // Three panes in a row, each overlapping the next; true offsets 0, +7, -4
        // relative to the first (the reference) on top of a shared sky signal
        // that is the same function of x in every pane.
        let frames = [
            low(300, 20, 0, 140, 100.0),
            low(300, 20, 100, 240, 107.0),
            low(300, 20, 200, 300, 96.0),
        ];
        let off = solve_offsets(&frames, 0, 400);
        assert!(off[0][0].abs() < 1e-4);
        assert!((off[1][0] + 7.0).abs() < 0.2, "{off:?}");
        assert!((off[2][0] - 4.0).abs() < 0.3, "{off:?}");
    }

    #[test]
    fn binning_averages_blocks_and_skips_no_data() {
        let f = Frame {
            width: 4,
            height: 2,
            data: vec![1.0, 3.0, f32::NAN, f32::NAN, 5.0, 7.0, f32::NAN, f32::NAN],
            bayer: None,
        };
        let b = bin_planes(&[f], 2);
        assert_eq!((b[0].width, b[0].height), (2, 1));
        assert!((b[0].data[0] - 4.0).abs() < 1e-6 && b[0].data[1].is_nan());
    }

    #[test]
    fn feather_zero_is_a_hard_edge() {
        let f = feather_weights(&Transform::identity(), (10, 10), (10, 10), 0.0).expect("weights");
        assert!(f.data.iter().all(|&v| (v - 1.0).abs() < 1e-6));
    }
}
