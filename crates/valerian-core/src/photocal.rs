// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Photometric colour calibration: white balance from the colours of the stars.
//!
//! A colour camera's red, green and blue do not see the sky the way a viewer
//! expects, and the balance of a stack depends on the filter, the sensor and
//! the sky. Gaia measured the colour of every bright star, so the stars in the
//! frame say what the balance ought to be. The method:
//!
//! 1. **Stars.** The plate solution ([`crate::solve`]) places Gaia stars
//!    (G between 6 and 11 by default, with a BP-RP colour) in the frame. Each
//!    is tied to a star detected in the image, and stars with a detected
//!    neighbour inside the background annulus are dropped.
//! 2. **Photometry.** Each star is measured in each colour plane through the
//!    same circular aperture (twice the median FWHM), with the sky taken from
//!    a clipped median of an annulus around it. Faint, saturated and
//!    non-positive measurements are dropped.
//! 3. **Fit.** For red and for blue, the instrumental colour against green,
//!    `-2.5 log10(F_red / F_green)`, is a straight line in Gaia BP-RP over the
//!    range of ordinary stars. A line is fitted with 3-sigma clipping (a star
//!    that is off in either channel goes from both).
//! 4. **Balance.** A reference colour (BP-RP 0.82, a G2V star like the Sun) is
//!    set as neutral: the gain of a channel is whatever makes the line's value
//!    at that colour equal to green's. Green's gain is 1, so brightness stays
//!    where it was.
//! 5. **Background.** Each plane's sky level, after the gain, is brought to
//!    the mean of the three, so the sky is neutral grey and not tinted by
//!    light pollution.
//!
//! The slopes of the two lines are the camera's colour response and double as
//! a check: red must brighten with BP-RP against green and blue must dim. If
//! they do not, the planes are not red, green and blue in that order or the
//! image is not linear, and a warning says so. The balance is only as good as
//! the image is linear: calibrate before stretching, after background
//! extraction.
//!
//! # References
//!
//! - Gaia Collaboration, Prusti, T. et al. (2016), The Gaia mission, Astron. Astrophys. 595, A1, doi:10.1051/0004-6361/201629272.
//! - Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940.
//! - Riello, M. et al. (2021), Gaia Early Data Release 3: photometric content and validation, Astron. Astrophys. 649, A3, doi:10.1051/0004-6361/202039587 (the BP and RP photometry; DR3 photometry is that of EDR3).
//! - Casagrande, L. and `VandenBerg`, D. A. (2018), On the use of Gaia magnitudes and new tables of bolometric corrections, Mon. Not. R. Astron. Soc. 479, L102-L107, doi:10.1093/mnrasl/sly104 (the solar BP-RP colour of 0.82; value to verify against the paper).
//! - Theil, H. (1950), reprinted in Henri Theil's Contributions to Economics and Econometrics, Springer (1992), doi:10.1007/978-94-011-2546-8_20, and Sen, P. K. (1968), Estimates of the regression coefficient based on Kendall's tau, J. Am. Stat. Assoc. 63, 1379-1389, doi:10.1080/01621459.1968.10480934 (the Theil-Sen line).
//! - Huber, P. J. (1964), Robust estimation of a location parameter, Ann. Math. Statist. 35, 73-101, doi:10.1214/aoms/1177703732.

use rayon::prelude::*;

use crate::CancelToken;
use crate::catalogue::Catalogue;
use crate::fits::{Image, Value};
use crate::stars::{self, DetectOptions, Star};
use crate::wcs::Wcs;
use crate::{Error, Result, stats};
use helia_core::Frame;

/// Settings for [`calibrate`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Brightest star used (Gaia G); brighter ones saturate.
    pub min_mag: f32,
    /// Faintest star used (Gaia G).
    pub max_mag: f32,
    /// Range of BP-RP used in the fit.
    pub colour_range: (f32, f32),
    /// The BP-RP colour set as neutral: 0.82 is the Sun.
    pub reference_colour: f32,
    /// Aperture radius in units of the median FWHM.
    pub aperture: f32,
    /// Least signal-to-noise of a star in each colour plane.
    pub min_snr: f64,
    /// The fewest stars a fit may rest on.
    pub min_stars: usize,
    /// Sigma-clipping limit of the fit.
    pub clip: f64,
    /// How far, in pixels, a detected star may lie from where Gaia puts it.
    pub match_radius: f64,
    /// Bring the sky of the three planes to the same level.
    pub neutralise_background: bool,
    /// Star detection settings.
    pub detect: DetectOptions,
    /// Stops the run between steps; never set by default.
    pub cancel: CancelToken,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_mag: 6.0,
            max_mag: 11.0,
            colour_range: (0.3, 2.5),
            reference_colour: 0.82,
            aperture: 2.0,
            min_snr: 15.0,
            min_stars: 15,
            clip: 3.0,
            match_radius: 3.0,
            neutralise_background: true,
            detect: DetectOptions {
                threshold: 6.0,
                border: 4,
                max_stars: 2000,
                ..DetectOptions::default()
            },
            cancel: CancelToken::default(),
        }
    }
}

/// One Gaia star as measured in the image.
#[derive(Clone, Debug, PartialEq)]
pub struct StarPhot {
    /// Pixel position of the detected star.
    pub x: f64,
    /// Pixel position of the detected star.
    pub y: f64,
    /// Gaia G magnitude.
    pub g: f32,
    /// Gaia BP-RP.
    pub colour: f32,
    /// Flux in red, green and blue (sky subtracted).
    pub flux: [f64; 3],
    /// Signal-to-noise in each plane.
    pub snr: [f64; 3],
    /// Whether the fit kept it.
    pub used: bool,
}

/// The line `instrumental colour = zero + slope * (BP-RP - reference)` of one
/// channel against green.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Line {
    /// Value at the reference colour, magnitudes.
    pub zero: f64,
    /// Change per magnitude of BP-RP.
    pub slope: f64,
    /// Scatter of the stars about the line, magnitudes.
    pub rms: f64,
}

/// The result of a calibration: what to multiply and subtract, and the evidence.
#[derive(Clone, Debug)]
pub struct Calibration {
    /// Multiplier of the red, green and blue planes.
    pub gains: [f64; 3],
    /// Level subtracted from each plane after the gain (zero without
    /// background neutralisation).
    pub offsets: [f64; 3],
    /// The red line and the blue line.
    pub lines: [Line; 2],
    /// Uncertainty of each gain as a fraction (red, blue; green is 1).
    pub gain_error: [f64; 2],
    /// Sky level of each plane before.
    pub sky_before: [f32; 3],
    /// Sky level of each plane after.
    pub sky_after: [f32; 3],
    /// Every star measured.
    pub stars: Vec<StarPhot>,
    /// Stars the fit kept.
    pub used: usize,
    /// Aperture radius, pixels.
    pub aperture_px: f64,
    /// The reference colour the balance is set at.
    pub reference_colour: f32,
    /// Things the user should know (a wrong-way slope, a narrow colour range).
    pub warnings: Vec<String>,
}

const MARGIN: f64 = 2.0;

fn plane_check(image: &Image) -> Result<()> {
    if image.planes.len() != 3 {
        return Err(Error::invalid(format!(
            "colour calibration needs a red, green and blue image; this one has {} plane{}",
            image.planes.len(),
            if image.planes.len() == 1 { "" } else { "s" }
        )));
    }
    if image.planes.iter().any(|p| p.bayer.is_some()) {
        return Err(Error::invalid(
            "colour calibration needs a demosaiced image, this one is a raw colour mosaic",
        ));
    }
    if crate::stretch::Stretch::from_header(&image.header).is_some() {
        return Err(Error::invalid(
            "this image has been stretched; colour calibration works on the linear image",
        ));
    }
    Ok(())
}

/// Flux in a circular aperture over the sky of an annulus.
struct Aperture {
    flux: f64,
    snr: f64,
    peak: f32,
}

fn aperture(f: &Frame, cx: f64, cy: f64, r: f64, r_in: f64, r_out: f64) -> Option<Aperture> {
    let (w, h) = (f.width as isize, f.height as isize);
    let (x0, x1) = ((cx - r_out).floor() as isize, (cx + r_out).ceil() as isize);
    let (y0, y1) = ((cy - r_out).floor() as isize, (cy + r_out).ceil() as isize);
    if x0 < 0 || y0 < 0 || x1 >= w || y1 >= h {
        return None;
    }
    let mut ring = Vec::new();
    let (mut sum, mut wsum, mut peak) = (0.0f64, 0.0f64, f32::MIN);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = (x as f64 - cx).hypot(y as f64 - cy);
            let v = f.data[y as usize * f.width + x as usize];
            if !v.is_finite() {
                continue;
            }
            if d >= r_in && d <= r_out {
                ring.push(v);
            }
            let wgt = (r + 0.5 - d).clamp(0.0, 1.0);
            if wgt > 0.0 {
                sum += wgt * f64::from(v);
                wsum += wgt;
                peak = peak.max(v);
            }
        }
    }
    if ring.len() < 20 || wsum < 4.0 {
        return None;
    }
    let (m, s) = stats::median_sigma(&ring);
    let kept: Vec<f32> = ring
        .iter()
        .copied()
        .filter(|v| (v - m).abs() <= 3.0 * s.max(f32::MIN_POSITIVE))
        .collect();
    let sky = f64::from(stats::median_finite(&kept));
    let flux = sum - sky * wsum;
    let noise = f64::from(s) * (wsum * (1.0 + wsum / kept.len().max(1) as f64)).sqrt();
    Some(Aperture {
        flux,
        snr: if noise > 0.0 { flux / noise } else { 0.0 },
        peak,
    })
}

/// Measure the Gaia stars in the image: the first half of [`calibrate`].
///
/// # Errors
/// [`Error::Invalid`] for an image that is not a linear RGB one, or when too few stars
/// when no star can be measured.
pub fn measure(
    image: &Image,
    wcs: &Wcs,
    cat: &Catalogue,
    opts: &Options,
) -> Result<(Vec<StarPhot>, f64)> {
    plane_check(image)?;
    let (w, h) = (image.width(), image.height());
    let luma = stars::luminance(&image.planes);
    let det = stars::detect(&luma, &opts.detect);
    let fwhm = f64::from(det.median_fwhm());
    if !fwhm.is_finite() || det.stars.len() < 8 {
        return Err(Error::Invalid(format!(
            "colour calibration found only {} stars in the image and needs at least 8:              use a linear stack of many frames, not a single short frame",
            det.stars.len()
        )));
    }
    let r = (f64::from(opts.aperture) * fwhm).clamp(2.0, 30.0);
    let (r_in, r_out) = (r + 2.0, r + 2.0 + (0.8 * r).max(3.0));
    let scale = wcs.scale_arcsec();
    let (cx, cy) = wcs.pixel_to_world(w as f64 / 2.0 - 0.5, h as f64 / 2.0 - 0.5);
    let radius = (w as f64).hypot(h as f64) * scale / 3600.0 / 2.0 * 1.05;
    let near: Vec<_> = cat
        .cone(cx, cy, radius, opts.max_mag)
        .into_iter()
        .filter(|s| s.mag >= opts.min_mag)
        .filter_map(|s| {
            let c = s
                .colour
                .filter(|c| (opts.colour_range.0..=opts.colour_range.1).contains(c))?;
            let (x, y) = wcs.world_to_pixel(s.ra, s.dec)?;
            (x > r_out + MARGIN
                && y > r_out + MARGIN
                && x < w as f64 - r_out - MARGIN
                && y < h as f64 - r_out - MARGIN)
                .then_some((x, y, s.mag, c))
        })
        .collect();
    let detected: &[Star] = &det.stars;
    let out: Vec<StarPhot> = near
        .par_iter()
        .filter_map(|&(px, py, g, colour)| {
            // The nearest detected star, if it is near enough.
            let (best, d) = detected
                .iter()
                .enumerate()
                .map(|(i, s)| (i, (s.x - px).hypot(s.y - py)))
                .min_by(|a, b| a.1.total_cmp(&b.1))?;
            if d > opts.match_radius || detected[best].saturated {
                return None;
            }
            let (sx, sy) = (detected[best].x, detected[best].y);
            // Another star in the aperture or the annulus spoils all three.
            let crowded = detected.iter().enumerate().any(|(i, s)| {
                i != best && (s.x - sx).hypot(s.y - sy) < r_out + 0.5 * f64::from(s.fwhm)
            });
            if crowded {
                return None;
            }
            let mut flux = [0.0; 3];
            let mut snr = [0.0; 3];
            for (c, plane) in image.planes.iter().enumerate() {
                let a = aperture(plane, sx, sy, r, r_in, r_out)?;
                if !a.peak.is_finite() {
                    return None;
                }
                flux[c] = a.flux;
                snr[c] = a.snr;
            }
            Some(StarPhot {
                x: sx,
                y: sy,
                g,
                colour,
                flux,
                snr,
                used: false,
            })
        })
        .collect();
    if out.is_empty() {
        return Err(Error::Invalid(
            "colour calibration could not measure any catalogue star in the image: check the              plate solution (run `valerian solve` again, or give `--wcs`)"
                .into(),
        ));
    }
    Ok((out, r))
}

/// Solve the 2x2 normal equations of `y = a + b x` over the indices in `idx`.
fn line_fit(x: &[f64], y: &[f64], idx: &[usize]) -> (f64, f64) {
    let n = idx.len() as f64;
    let (sx, sy) = idx
        .iter()
        .fold((0.0, 0.0), |(a, b), &i| (a + x[i], b + y[i]));
    let (mx, my) = (sx / n, sy / n);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for &i in idx {
        sxx += (x[i] - mx).powi(2);
        sxy += (x[i] - mx) * (y[i] - my);
    }
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (my - b * mx, b)
}

/// The Theil-Sen line (median of the pairwise slopes), which a minority of
/// wild points cannot move: the start of the clipped fit.
fn robust_line(x: &[f64], y: &[f64], idx: &[usize]) -> (f64, f64) {
    let step = idx.len().div_ceil(400).max(1);
    let sub: Vec<usize> = idx.iter().copied().step_by(step).collect();
    let mut slopes = Vec::new();
    for (k, &i) in sub.iter().enumerate() {
        for &j in &sub[k + 1..] {
            if (x[j] - x[i]).abs() > 1e-3 {
                slopes.push(((y[j] - y[i]) / (x[j] - x[i])) as f32);
            }
        }
    }
    let b = if slopes.is_empty() {
        0.0
    } else {
        f64::from(stats::median_in_place(&mut slopes))
    };
    let mut off: Vec<f32> = idx.iter().map(|&i| (y[i] - b * x[i]) as f32).collect();
    (f64::from(stats::median_in_place(&mut off)), b)
}

fn rms(x: &[f64], y: &[f64], idx: &[usize], a: f64, b: f64) -> f64 {
    let s: f64 = idx.iter().map(|&i| (y[i] - a - b * x[i]).powi(2)).sum();
    (s / idx.len().max(1) as f64).sqrt()
}

/// Fit the two lines to `stars`, marking which were used, and return them with
/// the colour spread of the stars kept.
fn fit(stars: &mut [StarPhot], opts: &Options) -> Result<([Line; 2], f64)> {
    let ok: Vec<usize> = (0..stars.len())
        .filter(|&i| {
            let s = &stars[i];
            s.flux.iter().all(|&f| f > 0.0) && s.snr.iter().all(|&q| q >= opts.min_snr)
        })
        .collect();
    if ok.len() < opts.min_stars {
        return Err(Error::Invalid(format!(
            "colour calibration needs at least {} usable stars and found {} of {} measured: the \
             field is sparse for the catalogue, or the stars are too faint for --min-snr {}. Lower \
             --min-stars or --min-snr, or build a deeper catalogue",
            opts.min_stars,
            ok.len(),
            stars.len(),
            opts.min_snr
        )));
    }
    let x: Vec<f64> = stars
        .iter()
        .map(|s| f64::from(s.colour - opts.reference_colour))
        .collect();
    let y = |c: usize| -> Vec<f64> {
        stars
            .iter()
            .map(|s| {
                if s.flux[c] > 0.0 && s.flux[1] > 0.0 {
                    -2.5 * (s.flux[c] / s.flux[1]).log10()
                } else {
                    0.0
                }
            })
            .collect()
    };
    let (yr, yb) = (y(0), y(2));
    let mut keep = ok;
    // Robust scatter of the residuals about a line, with a floor at what
    // photometry can reach.
    let sigma = |ys: &[f64], idx: &[usize], (a, b): (f64, f64)| -> f64 {
        let r: Vec<f32> = idx.iter().map(|&i| (ys[i] - a - b * x[i]) as f32).collect();
        f64::from(stats::median_sigma(&r).1).max(0.002)
    };
    for round in 0..8 {
        let fits = if round == 0 {
            [robust_line(&x, &yr, &keep), robust_line(&x, &yb, &keep)]
        } else {
            [line_fit(&x, &yr, &keep), line_fit(&x, &yb, &keep)]
        };
        let sig = [sigma(&yr, &keep, fits[0]), sigma(&yb, &keep, fits[1])];
        let next: Vec<usize> = keep
            .iter()
            .copied()
            .filter(|&i| {
                (yr[i] - fits[0].0 - fits[0].1 * x[i]).abs() <= opts.clip * sig[0]
                    && (yb[i] - fits[1].0 - fits[1].1 * x[i]).abs() <= opts.clip * sig[1]
            })
            .collect();
        if next.len() == keep.len() || next.len() < opts.min_stars {
            break;
        }
        keep = next;
    }
    let fits = [line_fit(&x, &yr, &keep), line_fit(&x, &yb, &keep)];
    for &i in &keep {
        stars[i].used = true;
    }
    let mean = keep.iter().map(|&i| x[i]).sum::<f64>() / keep.len() as f64;
    let spread =
        (keep.iter().map(|&i| (x[i] - mean).powi(2)).sum::<f64>() / keep.len() as f64).sqrt();
    let lines = [
        Line {
            zero: fits[0].0,
            slope: fits[0].1,
            rms: rms(&x, &yr, &keep, fits[0].0, fits[0].1),
        },
        Line {
            zero: fits[1].0,
            slope: fits[1].1,
            rms: rms(&x, &yb, &keep, fits[1].0, fits[1].1),
        },
    ];
    Ok((lines, spread))
}

/// The sky level of each plane after multiplying by `gains`.
fn skies(image: &Image) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (o, p) in out.iter_mut().zip(&image.planes) {
        *o = stats::sky_and_noise(p).0;
    }
    out
}

/// Calibrate the colour of `image` against Gaia.
///
/// `wcs` is the plate solution of the image ([`crate::solve`]).
///
/// # Errors
/// [`Error::Invalid`] for an image that is not a linear RGB one, or when too few stars
/// can be measured or fitted.
pub fn calibrate(image: &Image, wcs: &Wcs, cat: &Catalogue, opts: &Options) -> Result<Calibration> {
    opts.cancel.check()?;
    let (mut stars, aperture_px) = measure(image, wcs, cat, opts)?;
    opts.cancel.check()?;
    let (lines, spread) = fit(&mut stars, opts)?;
    let used = stars.iter().filter(|s| s.used).count();
    let gains = [
        10f64.powf(0.4 * lines[0].zero),
        1.0,
        10f64.powf(0.4 * lines[1].zero),
    ];
    let gain_error = [
        0.4 * std::f64::consts::LN_10 * lines[0].rms / (used as f64).sqrt(),
        0.4 * std::f64::consts::LN_10 * lines[1].rms / (used as f64).sqrt(),
    ];
    let mut warnings = Vec::new();
    if lines[0].slope >= -0.05 {
        warnings.push(format!(
            "red does not brighten against green with BP-RP (slope {:+.2}): check the planes are \
             red, green, blue in that order and the image is linear",
            lines[0].slope
        ));
    }
    if lines[1].slope <= 0.05 {
        warnings.push(format!(
            "blue does not dim against green with BP-RP (slope {:+.2}): check the planes are \
             red, green, blue in that order and the image is linear",
            lines[1].slope
        ));
    }
    if spread < 0.2 {
        warnings.push(format!(
            "the stars span a narrow colour range (sigma {spread:.2} in BP-RP); the balance rests on \
             the average colour"
        ));
    }
    let sky_before = skies(image);
    let scaled: [f64; 3] = std::array::from_fn(|c| f64::from(sky_before[c]) * gains[c]);
    let target = scaled.iter().sum::<f64>() / 3.0;
    let offsets: [f64; 3] = if opts.neutralise_background {
        std::array::from_fn(|c| scaled[c] - target)
    } else {
        [0.0; 3]
    };
    let sky_after: [f32; 3] = std::array::from_fn(|c| (scaled[c] - offsets[c]) as f32);
    Ok(Calibration {
        gains,
        offsets,
        lines,
        gain_error,
        sky_before,
        sky_after,
        stars,
        used,
        aperture_px,
        reference_colour: opts.reference_colour,
        warnings,
    })
}

impl Calibration {
    /// A copy of `image` with the gains applied and the background offsets
    /// removed.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the image does not have three planes.
    pub fn apply(&self, image: &Image) -> Result<Image> {
        if image.planes.len() != 3 {
            return Err(Error::invalid("colour calibration needs three planes"));
        }
        let mut out = image.clone();
        for (c, plane) in out.planes.iter_mut().enumerate() {
            let (g, o) = (self.gains[c] as f32, self.offsets[c] as f32);
            plane.data.par_iter_mut().for_each(|v| *v = *v * g - o);
        }
        Ok(out)
    }

    /// Note the calibration in the header: keywords for the gains and offsets
    /// and a `HISTORY` line.
    pub fn record(&self, image: &mut Image) {
        for (c, name) in ["R", "G", "B"].into_iter().enumerate() {
            image.header.set(
                &format!("PCCGAIN{name}"),
                Value::Float(self.gains[c]),
                "photometric colour calibration gain",
            );
            image.header.set(
                &format!("PCCOFF{name}"),
                Value::Float(self.offsets[c]),
                "background offset subtracted after the gain",
            );
        }
        image.header.add_history(&format!(
            "valerian photocal: gains R {:.4} G 1 B {:.4} from {} Gaia stars (BP-RP {:.2} neutral), \
             scatter {:.3} / {:.3} mag, background offsets {:.2} {:.2} {:.2}",
            self.gains[0],
            self.gains[2],
            self.used,
            self.reference_colour,
            self.lines[0].rms,
            self.lines[1].rms,
            self.offsets[0],
            self.offsets[1],
            self.offsets[2],
        ));
    }
}

#[cfg(test)]
mod tests;
