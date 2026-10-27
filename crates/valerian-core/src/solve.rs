// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plate solving: find where on the sky an image points, and write the world
//! coordinate system.
//!
//! The method, in the order it runs:
//!
//! 1. **Stars.** The image's stars are detected on the luminance
//!    ([`stars::detect`]).
//! 2. **Where to look.** A hint (a position, a search radius and a pixel scale
//!    range, from the FITS header or the command line) gives a list of trial
//!    patches of sky. Without a position the whole sky is tiled, which is a
//!    blind solve and takes longer; a scale range is still needed to size the
//!    tiles.
//! 3. **Patterns.** For each trial the catalogue stars of that patch are
//!    projected on the tangent plane and matched to the image stars by
//!    [`crate::register::register`], the triangle matching Valerian already uses to
//!    align frames: triangles of the brightest stars are compared by their side
//!    ratios, which do not change with rotation, scale or position, and the
//!    votes give a rotation, scale and shift. Both parities are tried, since a
//!    mirrored image (a diagonal in the light path) matches nothing otherwise.
//! 4. **Refine.** The system is fitted to every star that matches, with a match
//!    radius that tightens each round and 3-sigma clipping, as a gnomonic (TAN)
//!    projection about the image centre with optional SIP distortion
//!    ([`Wcs::fit`]).
//!
//! A solution is accepted only when enough stars agree (ten by default). With a
//! rotation, a scale and a shift that each had to be found, ten stars
//! landing within a couple of pixels of catalogue stars by chance is not a
//! thing that happens, so the match count is the evidence.
//!
//! Limits: the catalogue is G < 11 by default, about 30 stars per square degree,
//! which is plenty for a field of half a degree or more and too few for one much
//! narrower; build a deeper catalogue with `scripts/build_catalogue.py`. The
//! tangent-plane patches are good to about five degrees across.
//!
//! # References
//!
//! - Lang, D., Hogg, D. W., Mierle, K., Blanton, M. and Roweis, S. (2010), Astrometry.net: blind astrometric calibration of arbitrary astronomical images, Astron. J. 139, 1782-1800, doi:10.1088/0004-6256/139/5/1782 (the idea of blind solving by matching star patterns; Valerian matches triangles, not quads, and uses none of that project's code).
//! - Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099.
//! - Valdes, F. G. et al. (1995), FOCAS automatic catalog matching algorithms, Publ. Astron. Soc. Pac. 107, 1119, doi:10.1086/133667.
//! - Calabretta, M. R. and Greisen, E. W. (2002), Representations of celestial coordinates in FITS, Astron. Astrophys. 395, 1077-1122, doi:10.1051/0004-6361:20021327 (the TAN projection).
//! - Shupe, D. L. et al. (2005), The SIP convention for representing distortion in FITS image headers, ASP Conf. Ser. 347, 491 (no DOI; to verify).
//! - Shupe, D. L. et al. (2012), More flexibility in representing geometric distortion in astronomical images, Proc. SPIE 8451, 84511M, doi:10.1117/12.925460.
//! - Gaia Collaboration, Prusti, T. et al. (2016), The Gaia mission, Astron. Astrophys. 595, A1, doi:10.1051/0004-6361/201629272.
//! - Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940.

use rayon::prelude::*;

use crate::CancelToken;
use crate::catalogue::{CatStar, Catalogue};
use crate::fits::Image;
use crate::register::{RegisterOptions, register};
use crate::stars::{self, DetectOptions, Star};
use crate::transform::Model;
use crate::wcs::{Wcs, deproject, project, separation};
use crate::{Error, Result};

/// Where the image points, roughly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hint {
    /// Right ascension of the field centre, degrees.
    pub ra: f64,
    /// Declination of the field centre, degrees.
    pub dec: f64,
    /// How far the true centre may be from this, degrees.
    pub radius: f64,
}

/// Settings for [`solve`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Where to look; `None` searches the whole sky.
    pub hint: Option<Hint>,
    /// Range of the pixel scale in arc seconds per pixel.
    pub scale: (f64, f64),
    /// The fewest stars that must agree.
    pub min_matches: usize,
    /// SIP order: `Some(0)` for plain TAN, `Some(2..=5)`, or `None` to choose
    /// from the number of stars matched.
    pub sip_order: Option<usize>,
    /// The most trial patches of sky to try.
    pub max_trials: usize,
    /// Star detection settings.
    pub detect: DetectOptions,
    /// Most image stars used.
    pub max_stars: usize,
    /// Stops the search between trial patches; never set by default.
    pub cancel: CancelToken,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hint: None,
            scale: (0.5, 20.0),
            min_matches: 10,
            sip_order: None,
            max_trials: 400_000,
            detect: DetectOptions {
                threshold: 6.0,
                border: 4,
                max_stars: 400,
                ..DetectOptions::default()
            },
            max_stars: 400,
            cancel: CancelToken::default(),
        }
    }
}

/// A solved image.
#[derive(Clone, Debug)]
pub struct Solution {
    /// The world coordinate system.
    pub wcs: Wcs,
    /// Stars that agree with the catalogue.
    pub matched: usize,
    /// Stars detected in the image.
    pub detected: usize,
    /// Root-mean-square residual of the matched stars, pixels.
    pub rms_px: f64,
    /// The same in arc seconds.
    pub rms_arcsec: f64,
    /// Centre of the image, `(ra, dec)` degrees.
    pub centre: (f64, f64),
    /// Size of the field, `(width, height)` in degrees.
    pub field: (f64, f64),
    /// Patches of sky tried before this one matched.
    pub trials: usize,
    /// Pixel positions of the stars that matched, for drawing them.
    pub matched_pixels: Vec<(f64, f64)>,
}

const ARCSEC: f64 = 3600.0;

fn star_at(x: f64, y: f64, mag: f32) -> Star {
    let flux = 10f64.powf(-0.4 * f64::from(mag));
    Star {
        x,
        y,
        flux,
        peak: flux as f32,
        fwhm: 3.0,
        ellipticity: 0.0,
        theta: 0.0,
        snr: 10.0,
        saturated: false,
    }
}

/// Catalogue stars of a patch as pseudo-pixel stars on the tangent plane at
/// the nominal scale. `flip` puts east on +x (a mirrored image); otherwise west
/// is +x, so a normal image is a rotation of it.
fn patch_stars(cat: &[CatStar], ra0: f64, dec0: f64, nominal: f64, flip: bool) -> Vec<Star> {
    let sign = if flip { 1.0 } else { -1.0 };
    cat.iter()
        .filter_map(|s| {
            let (xi, eta) = project(s.ra, s.dec, ra0, dec0)?;
            Some(star_at(
                sign * xi * ARCSEC / nominal,
                eta * ARCSEC / nominal,
                s.mag,
            ))
        })
        .collect()
}

/// One trial of the pattern match: the starting system, or `None`.
fn trial(
    img: &[Star],
    cat: &Catalogue,
    centre: (f64, f64),
    radius: f64,
    scale: (f64, f64),
    size: (usize, usize),
    opts: &Options,
) -> Option<Wcs> {
    let nominal = (scale.0 * scale.1).sqrt();
    let near = cat.cone(centre.0, centre.1, radius, 99.0);
    if near.len() < 8 {
        return None;
    }
    // About as many catalogue stars as the image can hold of them.
    let keep = near.len().min(150);
    let near = &near[..keep];
    let n_img = img.len().min(80);
    if n_img < 8 {
        return None;
    }
    let ro = RegisterOptions {
        model: Model::Similarity,
        pattern_stars: 60,
        neighbours: 8,
        scale_range: (scale.0 / nominal * 0.9, scale.1 / nominal * 1.1),
        search_radius: 4.0,
        final_radius: 3.0,
        min_matches: opts.min_matches.min(8),
    };
    let mut best: Option<(usize, bool, crate::register::Registration)> = None;
    for flip in [false, true] {
        let reference = patch_stars(near, centre.0, centre.1, nominal, flip);
        if reference.len() < 8 {
            continue;
        }
        if let Ok(r) = register(&reference, &img[..n_img], &ro)
            && best.as_ref().is_none_or(|b| r.matched > b.0)
        {
            best = Some((r.matched, flip, r));
        }
    }
    let (_, flip, reg) = best?;
    // Image pixel to pseudo-pixel to standard coordinates about the patch.
    let t = reg.transform;
    let sign = if flip { 1.0 } else { -1.0 };
    let at = |x: f64, y: f64| {
        let (px, py) = t.apply((x, y));
        deproject(
            sign * px * nominal / ARCSEC,
            py * nominal / ARCSEC,
            centre.0,
            centre.1,
        )
    };
    let (cx, cy) = (size.0 as f64 / 2.0 - 0.5, size.1 as f64 / 2.0 - 0.5);
    let crval = at(cx, cy);
    // The linear part from the image axes (one pixel along each).
    let (r1, d1) = at(cx + 1.0, cy);
    let (r2, d2) = at(cx, cy + 1.0);
    let p1 = project(r1, d1, crval.0, crval.1)?;
    let p2 = project(r2, d2, crval.0, crval.1)?;
    Some(Wcs {
        crval,
        crpix: (cx + 1.0, cy + 1.0),
        cd: [[p1.0, p2.0], [p1.1, p2.1]],
        sip: None,
    })
}

/// One-to-one nearest matches between image stars and catalogue stars
/// projected through `wcs`, within `radius` pixels, as `(image index,
/// catalogue index, distance)`.
fn match_stars(wcs: &Wcs, img: &[Star], cat: &[CatStar], radius: f64) -> Vec<(usize, usize, f64)> {
    let proj: Vec<Option<(f64, f64)>> = cat
        .iter()
        .map(|s| wcs.world_to_pixel(s.ra, s.dec))
        .collect();
    // Grid of catalogue positions for the lookup.
    let cell = radius.max(1.0) * 2.0;
    let mut grid: std::collections::HashMap<(i64, i64), Vec<usize>> =
        std::collections::HashMap::new();
    for (j, p) in proj.iter().enumerate() {
        if let Some((x, y)) = p {
            grid.entry(((x / cell).floor() as i64, (y / cell).floor() as i64))
                .or_default()
                .push(j);
        }
    }
    let mut cand = Vec::new();
    for (i, s) in img.iter().enumerate() {
        let (gx, gy) = ((s.x / cell).floor() as i64, (s.y / cell).floor() as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &j in grid.get(&(gx + dx, gy + dy)).into_iter().flatten() {
                    if let Some((x, y)) = proj[j] {
                        let d = (x - s.x).hypot(y - s.y);
                        if d <= radius {
                            cand.push((i, j, d));
                        }
                    }
                }
            }
        }
    }
    cand.sort_by(|a, b| a.2.total_cmp(&b.2));
    let (mut used_i, mut used_j) = (vec![false; img.len()], vec![false; cat.len()]);
    let mut out = Vec::new();
    for (i, j, d) in cand {
        if !used_i[i] && !used_j[j] {
            used_i[i] = true;
            used_j[j] = true;
            out.push((i, j, d));
        }
    }
    out
}

/// A refined system, its matches (image index, catalogue index, distance) and
/// the fit residual in pixels.
type Refined = (Wcs, Vec<(usize, usize, f64)>, f64);

/// Refine a starting system on every star that matches.
fn refine(
    start: &Wcs,
    img: &[Star],
    cat: &Catalogue,
    size: (usize, usize),
    opts: &Options,
) -> Result<Refined> {
    let diag = (size.0 as f64).hypot(size.1 as f64);
    let mut wcs = start.clone();
    let mut radius = 4.0f64.max(diag * 0.006);
    let mut last: Option<Refined> = None;
    for round in 0..8 {
        let centre = wcs.pixel_to_world(size.0 as f64 / 2.0 - 0.5, size.1 as f64 / 2.0 - 0.5);
        let field = diag * wcs.scale_arcsec() / ARCSEC / 2.0 * 1.15;
        let sky = cat.cone(centre.0, centre.1, field, 99.0);
        let mut m = match_stars(&wcs, img, &sky, radius);
        if m.len() < opts.min_matches {
            break;
        }
        // Clip at three times the scatter.
        let rms = (m.iter().map(|p| p.2 * p.2).sum::<f64>() / m.len() as f64).sqrt();
        let limit = (3.0 * rms).max(0.4);
        m.retain(|p| p.2 <= limit);
        if m.len() < opts.min_matches {
            break;
        }
        let px: Vec<(f64, f64)> = m.iter().map(|&(i, _, _)| (img[i].x, img[i].y)).collect();
        let world: Vec<(f64, f64)> = m.iter().map(|&(_, j, _)| (sky[j].ra, sky[j].dec)).collect();
        let fit_with = |order: usize| Wcs::fit(&px, &world, size, order, &wcs);
        let fitted = if let Some(order) = opts.sip_order {
            fit_with(order)
        } else {
            let order = if m.len() >= 100 {
                3
            } else if m.len() >= 50 {
                2
            } else {
                0
            };
            let plain = fit_with(0);
            // Distortion is kept only when it clearly beats a plain fit: a
            // polynomial fitted to noise wanders at the frame corners.
            let better = match &plain {
                Ok(p) if order >= 2 && round >= 1 => {
                    fit_with(order).ok().filter(|d| d.1 < 0.75 * p.1)
                }
                _ => None,
            };
            better.map_or(plain, Ok)
        };
        match fitted {
            Ok((w, rms_fit)) => {
                wcs = w;
                last = Some((wcs.clone(), m, rms_fit));
            }
            Err(_) => break,
        }
        radius = (radius * 0.6).max(1.2);
    }
    last.ok_or_else(|| Error::NoMatch("the stars matched too few catalogue stars to fit".into()))
}

/// The trial patches for a hint: its centre first, then rings around it.
fn trials_for(hint: &Hint, tile: f64) -> Vec<(f64, f64)> {
    let mut out = vec![(hint.ra, hint.dec)];
    let step = tile * 0.8;
    if hint.radius > step * 0.5 {
        let n = (hint.radius / step).ceil() as i32;
        let mut ring = Vec::new();
        for i in -n..=n {
            for j in -n..=n {
                let (dx, dy) = (f64::from(i) * step, f64::from(j) * step);
                let r = dx.hypot(dy);
                if r > 1e-9 && r <= hint.radius {
                    let dec = (hint.dec + dy).clamp(-89.9, 89.9);
                    let ra = hint.ra + dx / dec.to_radians().cos().max(0.05);
                    ring.push((r, ra.rem_euclid(360.0), dec));
                }
            }
        }
        ring.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.extend(ring.into_iter().map(|(_, ra, dec)| (ra, dec)));
    }
    out
}

/// Tile centres over the whole sky, a spherical Fibonacci lattice with about
/// `spacing` degrees between neighbours.
fn sky_tiles(spacing: f64) -> Vec<(f64, f64)> {
    let n = ((41_253.0 / (spacing * spacing)).ceil() as usize).max(8);
    let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
    (0..n)
        .map(|i| {
            let z = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
            (
                (i as f64 * golden).to_degrees().rem_euclid(360.0),
                z.asin().to_degrees(),
            )
        })
        .collect()
}

/// Solve from detected stars. `size` is the image size in pixels.
///
/// # Errors
/// [`Error::NoMatch`] when no patch of sky matches, or the stars are too few.
pub fn solve_stars(
    stars: &[Star],
    size: (usize, usize),
    cat: &Catalogue,
    opts: &Options,
) -> Result<Solution> {
    if opts.scale.0 <= 0.0 || opts.scale.1 < opts.scale.0 {
        return Err(Error::invalid(
            "the scale range must be positive and ordered",
        ));
    }
    if stars.len() < opts.min_matches.max(8) {
        return Err(Error::NoMatch(format!(
            "only {} stars were detected; at least {} are needed",
            stars.len(),
            opts.min_matches.max(8)
        )));
    }
    let mut img: Vec<Star> = stars.to_vec();
    img.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    img.truncate(opts.max_stars);
    let diag = (size.0 as f64).hypot(size.1 as f64);
    // The patch radius: half the field diagonal at the largest scale, so a
    // patch centred anywhere inside the nominal field covers the image.
    let nominal = (opts.scale.0 * opts.scale.1).sqrt();
    let radius = diag * nominal / ARCSEC / 2.0 * 1.1;
    let tiles = match &opts.hint {
        Some(h) => trials_for(h, radius),
        None => sky_tiles(radius * 1.2),
    };
    if tiles.len() > opts.max_trials {
        return Err(Error::invalid(format!(
            "a blind search at this scale needs {} patches of sky (limit {}); give a position or a \
             narrower scale range",
            tiles.len(),
            opts.max_trials
        )));
    }
    let total = tiles.len();
    let found = tiles
        .par_iter()
        .enumerate()
        .find_map_first(|(k, &(ra, dec))| {
            if opts.cancel.is_cancelled() {
                return None;
            }
            let guess = trial(&img, cat, (ra, dec), radius, opts.scale, size, opts)?;
            let (wcs, m, resid) = refine(&guess, &img, cat, size, opts).ok()?;
            (m.len() >= opts.min_matches).then_some((k, wcs, m, resid))
        });
    opts.cancel.check()?;
    let Some((first, wcs, pairs, rms)) = found else {
        return Err(Error::NoMatch(format!(
            "no match in {total} patch{} of sky: the field is not where the hint says, the scale \
             is outside {:.2} to {:.2} arc seconds per pixel, or there are too few catalogue \
             stars in the field (this catalogue goes to G = {:.0})",
            if total == 1 { "" } else { "es" },
            opts.scale.0,
            opts.scale.1,
            cat.mag_limit()
        )));
    };
    let scale = wcs.scale_arcsec();
    let centre = wcs.pixel_to_world(size.0 as f64 / 2.0 - 0.5, size.1 as f64 / 2.0 - 0.5);
    Ok(Solution {
        field: (
            size.0 as f64 * scale / ARCSEC,
            size.1 as f64 * scale / ARCSEC,
        ),
        rms_arcsec: rms * scale,
        wcs,
        matched: pairs.len(),
        detected: stars.len(),
        rms_px: rms,
        centre,
        trials: first + 1,
        matched_pixels: pairs
            .iter()
            .map(|&(i, _, _)| (img[i].x, img[i].y))
            .collect(),
    })
}

/// Detect the stars of `image` and solve it. A raw colour mosaic is refused.
///
/// # Errors
/// As [`solve_stars`], and [`Error::Invalid`] for a raw mosaic.
pub fn solve(image: &Image, cat: &Catalogue, opts: &Options) -> Result<Solution> {
    if image.planes.iter().any(|p| p.bayer.is_some()) {
        return Err(Error::invalid(
            "plate solving needs a demosaiced image, this one is a raw colour mosaic",
        ));
    }
    let luma = stars::luminance(&image.planes);
    opts.cancel.check()?;
    let det = stars::detect(&luma, &opts.detect);
    solve_stars(&det.stars, (image.width(), image.height()), cat, opts)
}

/// Write `solution` into the image's header: the WCS cards, and `HISTORY` lines
/// saying how it was found.
pub fn record(image: &mut Image, solution: &Solution) {
    solution.wcs.write(&mut image.header);
    image.header.add_history(&format!(
        "valerian solve: {} stars matched of {} detected, rms {:.2} arcsec ({:.2} px), \
         scale {:.3} arcsec/px, field {:.2} x {:.2} deg",
        solution.matched,
        solution.detected,
        solution.rms_arcsec,
        solution.rms_px,
        solution.wcs.scale_arcsec(),
        solution.field.0,
        solution.field.1,
    ));
}

/// What a header says of where the image points and its scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeaderHint {
    /// Field centre, degrees.
    pub position: Option<(f64, f64)>,
    /// Pixel scale, arc seconds per pixel.
    pub scale: Option<f64>,
}

/// An angle from text: a number is degrees; `hh:mm:ss` or `hh mm ss` is hours
/// for a right ascension (`hours`) and degrees otherwise.
#[must_use]
pub fn parse_angle(text: &str, hours: bool) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .or_else(|| sexagesimal(text, hours))
}

fn sexagesimal(text: &str, hours: bool) -> Option<f64> {
    let t = text.trim();
    let neg = t.starts_with('-');
    let parts: Vec<f64> = t
        .trim_start_matches(['+', '-'])
        .split([' ', ':'])
        .filter(|p| !p.is_empty())
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    let v = parts.first()?
        + parts.get(1).unwrap_or(&0.0) / 60.0
        + parts.get(2).unwrap_or(&0.0) / 3600.0;
    let v = if hours { v * 15.0 } else { v };
    Some(if neg { -v } else { v })
}

/// Read a position and scale from FITS header keywords: an existing WCS, else
/// `RA`/`DEC` (degrees, or sexagesimal text) or `OBJCTRA`/`OBJCTDEC` (hours and
/// degrees as text), and `CDELT`, or `FOCALLEN` with `XPIXSZ`.
#[must_use]
pub fn hint_from_header(h: &crate::fits::Header, size: (usize, usize)) -> HeaderHint {
    let mut out = HeaderHint::default();
    if let Some(w) = Wcs::from_header(h) {
        out.position = Some(w.pixel_to_world(size.0 as f64 / 2.0 - 0.5, size.1 as f64 / 2.0 - 0.5));
        out.scale = Some(w.scale_arcsec());
        return out;
    }
    // Text is sexagesimal (hours for right ascension), numbers are degrees.
    let ra_of = |key: &str| -> Option<f64> {
        match h.get(key)? {
            crate::fits::Value::Str(s) => sexagesimal(s, true),
            v => v.as_f64(),
        }
    };
    let dec_of = |key: &str| -> Option<f64> {
        match h.get(key)? {
            crate::fits::Value::Str(s) => sexagesimal(s, false),
            v => v.as_f64(),
        }
    };
    out.position = match (ra_of("OBJCTRA"), dec_of("OBJCTDEC")) {
        (Some(ra), Some(dec)) => Some((ra, dec)),
        _ => match (ra_of("RA"), dec_of("DEC")) {
            (Some(ra), Some(dec)) => Some((ra, dec)),
            _ => None,
        },
    };
    out.scale = match (h.get_f64("FOCALLEN"), h.get_f64("XPIXSZ")) {
        (Some(f), Some(p)) if f > 0.0 && p > 0.0 => Some(206.264_806 * p / f),
        _ => None,
    };
    out
}

/// Where `pos` falls from `other`, degrees (a convenience for callers that
/// compare solutions).
#[must_use]
pub fn distance(pos: (f64, f64), other: (f64, f64)) -> f64 {
    separation(pos.0, pos.1, other.0, other.1)
}

#[cfg(test)]
mod tests;
