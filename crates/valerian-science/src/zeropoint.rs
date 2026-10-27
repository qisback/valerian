// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Photometric calibration of the whole field: the zero point and colour term
//! of each colour plane, a calibrated star catalogue with magnitudes and
//! errors, and the limiting magnitude of the image.
//!
//! The colour calibration of [`valerian_core::photocal`] measures bright Gaia
//! stars and fits the balance between the planes. This module uses the same
//! measurements for the absolute scale. No catalogue data is stored or added;
//! the Gaia values are those of the catalogue file the user already has.
//!
//! # Method
//!
//! 1. **Zero point.** Each Gaia star the colour fit kept gives, in plane `c`,
//!    `zp = G + 2.5 log10(F_c)` with `F_c` the aperture flux in the pixel
//!    units of the image. The stars follow the colour equation
//!    `zp = Z_c + k_c (BP-RP - BP-RP_ref)`, fitted by least squares with
//!    3-sigma clipping. `Z_c` is the zero point of a star of the reference
//!    colour and `k_c` the colour term (the usual transformation of
//!    instrumental to standard magnitudes, Stetson 2000). The standard error
//!    of `Z_c` is that of the intercept of the fit, and the scatter about the
//!    line is reported as it is. A trend of the residuals with `G` (saturation
//!    or non-linearity at the bright end) is fitted and reported.
//! 2. **Catalogue.** Every star detected in the image (not only Gaia's) is
//!    measured through the same aperture and sky annulus in each plane. Its
//!    colour is estimated by inverting the two instrumental-colour lines of the
//!    colour fit, weighting each by the inverse variance of its estimate, or
//!    set to the reference colour when a plane is too faint (flag `E`). The
//!    magnitude is `m_c = Z_c + k_c (BP-RP - BP-RP_ref) - 2.5 log10(F_c)`: a
//!    Gaia-G equivalent magnitude in each plane. The error adds, in
//!    quadrature, the photon and sky noise of the aperture
//!    (`1.0857 / SNR`, the first-order propagation of `-2.5 log10 F`), the
//!    standard error of `Z_c`, and the colour term times the colour error
//!    (Howell 1989; Merline and Howell 1995).
//! 3. **Limiting magnitude.** The `S`-sigma limit is the magnitude of a star
//!    whose flux is `S` times the noise of one aperture, `Z_c - 2.5 log10(S
//!    sigma_ap)`, with `sigma_ap` the median sky-noise error of the apertures
//!    of the catalogue stars (the standard definition of a point-source
//!    detection limit, Newberry 1991). Two empirical checks come from the
//!    stars themselves: the faintest star measured at least `S` sigma, and
//!    the peak of the differential counts, where the catalogue stops being
//!    complete.
//!
//! # References
//!
//! - Stetson, P. B. (2000), Homogeneous photometry for star clusters and resolved galaxies. II. Photometric standard stars, Publ. Astron. Soc. Pacific 112, 925-931, doi:10.1086/316595 (DOI to verify).
//! - Howell, S. B. (1989), Two-dimensional aperture photometry: signal-to-noise ratio of point-source observations and optimal data-extraction techniques, Publ. Astron. Soc. Pacific 101, 616-622, doi:10.1086/132477 (DOI to verify).
//! - Merline, W. J. and Howell, S. B. (1995), A realistic model for point-sources imaged on array detectors: the model and initial results, Experimental Astronomy 6, 163-210, doi:10.1007/BF00421131 (DOI to verify).
//! - Newberry, M. V. (1991), Signal-to-noise considerations for sky-subtracted CCD data, Publ. Astron. Soc. Pacific 103, 122-130, doi:10.1086/132802 (DOI to verify).
//! - Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940.
//! - Riello, M. et al. (2021), Gaia Early Data Release 3: photometric content and validation, Astron. Astrophys. 649, A3, doi:10.1051/0004-6361/202039587.

use plotters::prelude::*;
use plotters::style::FontStyle;
use rayon::prelude::*;
use serde::Serialize;
use valerian_core::fits::Image;
use valerian_core::photocal::Calibration;
use valerian_core::stars::{DetectOptions, detect, luminance};
use valerian_core::wcs::Wcs;

use crate::photometry::aperture;
use crate::plot;
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";
/// `2.5 / ln 10`: the magnitude error of a fractional flux error.
const MAG_PER_FRACTION: f64 = 1.085_736_204_758_129_6;
/// Names of the three planes.
pub const CHANNELS: [&str; 3] = ["red", "green", "blue"];
const NAN: f64 = f64::NAN;

/// Settings for [`calibrate_field`].
#[derive(Clone, Debug)]
pub struct Options {
    /// The signal-to-noise ratio that defines the limiting magnitude.
    pub limit_snr: f64,
    /// Detection threshold of the catalogue stars, in sigmas of the smoothed noise.
    pub threshold: f32,
    /// The most stars measured (the brightest).
    pub max_stars: usize,
    /// Clipping limit of the zero-point fit, in sigmas.
    pub clip: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limit_snr: 5.0,
            threshold: 5.0,
            max_stars: 5000,
            clip: 3.0,
        }
    }
}

/// The zero point and colour term of one plane.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ChannelZp {
    /// `red`, `green` or `blue`.
    pub channel: &'static str,
    /// Zero point of a star of the reference colour, magnitudes (flux in the image's pixel units).
    pub zero: f64,
    /// Standard error of the zero point.
    pub zero_error: f64,
    /// Colour term, magnitudes per magnitude of BP-RP.
    pub colour_term: f64,
    /// Standard error of the colour term.
    pub colour_term_error: f64,
    /// Scatter of the stars about the fitted line, magnitudes.
    pub scatter: f64,
    /// Stars the fit kept.
    pub stars: usize,
    /// Trend of the residuals with G, magnitudes per magnitude (0 when flat).
    pub trend_with_g: f64,
    /// Zero point once the colour-calibration gain is applied to the image.
    pub zero_after_gain: f64,
}

/// One Gaia star used for the zero point.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Calibrator {
    /// Pixel position.
    pub x: f64,
    /// Pixel position.
    pub y: f64,
    /// Gaia G.
    pub g: f32,
    /// Gaia BP-RP.
    pub bp_rp: f32,
    /// `G + 2.5 log10 F` in red, green and blue (NaN when the flux is not positive).
    pub zp: [f64; 3],
    /// Whether the zero-point fit kept it, per plane.
    pub kept: [bool; 3],
}

/// One star of the calibrated catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldStar {
    /// Running number, brightest first.
    pub id: usize,
    /// Pixel position.
    pub x: f64,
    /// Pixel position.
    pub y: f64,
    /// Right ascension, degrees.
    pub ra: f64,
    /// Declination, degrees.
    pub dec: f64,
    /// Aperture flux per plane, pixel units.
    pub flux: [f64; 3],
    /// Signal-to-noise per plane.
    pub snr: [f64; 3],
    /// Gaia-G equivalent magnitude per plane (NaN when the flux is not positive).
    pub mag: [f64; 3],
    /// Error of each magnitude.
    pub mag_error: [f64; 3],
    /// BP-RP used for the colour term: Gaia's, or estimated, or the reference.
    pub bp_rp: f64,
    /// Error of that colour.
    pub bp_rp_error: f64,
    /// Gaia G of the matched catalogue star.
    pub gaia_g: Option<f32>,
    /// Full width at half maximum, pixels.
    pub fwhm: f32,
    /// `S` saturated, `C` crowded, `E` colour assumed, `N` a plane has no positive flux, `B` brighter than every Gaia star the fit kept, `G` matched to Gaia.
    pub flags: String,
}

/// The limiting magnitude of the image.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Limit {
    /// The signal-to-noise ratio it is quoted at.
    pub snr: f64,
    /// Median noise of one aperture in each plane, pixel units.
    pub aperture_noise: [f64; 3],
    /// Point-source limit from the aperture noise, per plane.
    pub from_noise: [f64; 3],
    /// The same at 3 and at 10 sigma, green only.
    pub green_at_3_and_10: [f64; 2],
    /// Faintest catalogue star measured at the signal-to-noise ratio, green.
    pub faintest_measured: Option<f64>,
    /// Magnitude where the differential star counts peak, green.
    pub counts_peak: Option<f64>,
    /// Stars the empirical checks rest on.
    pub stars: usize,
}

/// Everything the module produces for one image.
#[derive(Clone, Debug)]
pub struct Photometry {
    /// Image width, pixels.
    pub width: usize,
    /// Image height, pixels.
    pub height: usize,
    /// Zero point and colour term of red, green and blue.
    pub channels: [ChannelZp; 3],
    /// The BP-RP the zero points are quoted at.
    pub reference_colour: f64,
    /// Aperture radius, pixels.
    pub aperture_px: f64,
    /// The Gaia stars behind the fit.
    pub calibrators: Vec<Calibrator>,
    /// The calibrated catalogue.
    pub stars: Vec<FieldStar>,
    /// The limiting magnitude.
    pub limit: Limit,
    /// What the user should know.
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------------ fitting

struct LineFit {
    zero: f64,
    slope: f64,
    zero_error: f64,
    slope_error: f64,
    rms: f64,
}

/// Least-squares line `y = a + b x` over `idx` with its standard errors.
fn ols(x: &[f64], y: &[f64], idx: &[usize]) -> LineFit {
    let n = idx.len() as f64;
    let mx = idx.iter().map(|&i| x[i]).sum::<f64>() / n;
    let my = idx.iter().map(|&i| y[i]).sum::<f64>() / n;
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for &i in idx {
        sxx += (x[i] - mx).powi(2);
        sxy += (x[i] - mx) * (y[i] - my);
    }
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let a = my - b * mx;
    let ssr: f64 = idx.iter().map(|&i| (y[i] - a - b * x[i]).powi(2)).sum();
    let rms = (ssr / n).sqrt();
    let sigma = (ssr / (n - 2.0).max(1.0)).sqrt();
    let (zero_error, slope_error) = if sxx > 0.0 {
        (sigma * (1.0 / n + mx * mx / sxx).sqrt(), sigma / sxx.sqrt())
    } else {
        (sigma / n.sqrt(), f64::NAN)
    };
    LineFit {
        zero: a,
        slope: b,
        zero_error,
        slope_error,
        rms,
    }
}

/// Fit the colour equation of one plane with sigma clipping; returns the fit,
/// the kept indices and the trend with G.
fn fit_channel(
    x: &[f64],
    y: &[f64],
    g: &[f64],
    start: &[usize],
    clip: f64,
) -> Option<(LineFit, Vec<usize>, f64)> {
    let mut keep: Vec<usize> = start.to_vec();
    if keep.len() < 8 {
        return None;
    }
    let mut fit = ols(x, y, &keep);
    for _ in 0..8 {
        let sigma = fit.rms.max(0.002);
        let next: Vec<usize> = keep
            .iter()
            .copied()
            .filter(|&i| (y[i] - fit.zero - fit.slope * x[i]).abs() <= clip * sigma)
            .collect();
        if next.len() == keep.len() || next.len() < 8 {
            break;
        }
        keep = next;
        fit = ols(x, y, &keep);
    }
    // Trend of the residuals with G.
    let res: Vec<f64> = (0..y.len())
        .map(|i| y[i] - fit.zero - fit.slope * x[i])
        .collect();
    let trend = ols(g, &res, &keep).slope;
    Some((fit, keep, trend))
}

/// Estimate BP-RP from the instrumental colours of a star. Returns the colour
/// and its error, or `None` when the planes are too faint to say.
fn estimate_colour(cal: &Calibration, flux: [f64; 3], snr: [f64; 3]) -> Option<(f64, f64)> {
    if flux.iter().any(|&f| f <= 0.0) || snr.iter().any(|&s| s < 10.0) {
        return None;
    }
    let reference = f64::from(cal.reference_colour);
    let mut num_sum = 0.0;
    let mut den = 0.0;
    for (line, ch) in [(&cal.lines[0], 0usize), (&cal.lines[1], 2usize)] {
        if line.slope.abs() < 0.05 {
            continue;
        }
        let inst = -2.5 * (flux[ch] / flux[1]).log10();
        let est = reference + (inst - line.zero) / line.slope;
        let var = MAG_PER_FRACTION.powi(2) * (1.0 / snr[ch].powi(2) + 1.0 / snr[1].powi(2))
            / line.slope.powi(2);
        num_sum += est / var;
        den += 1.0 / var;
    }
    (den > 0.0).then(|| ((num_sum / den).clamp(-0.5, 4.0), (1.0 / den).sqrt()))
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return NAN;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        f64::midpoint(v[n / 2 - 1], v[n / 2])
    }
}

/// Calibrate the field: zero points, the catalogue and the limiting magnitude.
///
/// `cal` is the colour calibration of the same image
/// ([`valerian_core::photocal::calibrate`]); `wcs` its plate solution.
///
/// # Errors
/// [`Error::Empty`] when fewer than 8 Gaia stars were kept or no star can be
/// measured.
pub fn calibrate_field(
    image: &Image,
    wcs: &Wcs,
    cal: &Calibration,
    opts: &Options,
) -> Result<Photometry> {
    let reference = f64::from(cal.reference_colour);
    let used: Vec<usize> = (0..cal.stars.len())
        .filter(|&i| cal.stars[i].used)
        .collect();
    if used.len() < 8 {
        return Err(Error::Empty(format!(
            "only {} Gaia stars were kept by the colour fit; the zero point needs at least 8",
            used.len()
        )));
    }
    let xs: Vec<f64> = cal
        .stars
        .iter()
        .map(|s| f64::from(s.colour) - reference)
        .collect();
    let gs: Vec<f64> = cal.stars.iter().map(|s| f64::from(s.g)).collect();
    let mut warnings = Vec::new();
    let mut channels = Vec::with_capacity(3);
    let mut calibrators: Vec<Calibrator> = cal
        .stars
        .iter()
        .map(|s| Calibrator {
            x: s.x,
            y: s.y,
            g: s.g,
            bp_rp: s.colour,
            zp: std::array::from_fn(|c| {
                if s.flux[c] > 0.0 {
                    f64::from(s.g) + 2.5 * s.flux[c].log10()
                } else {
                    NAN
                }
            }),
            kept: [false; 3],
        })
        .collect();
    for c in 0..3 {
        let y: Vec<f64> = calibrators.iter().map(|s| s.zp[c]).collect();
        let start: Vec<usize> = used.iter().copied().filter(|&i| y[i].is_finite()).collect();
        let (fit, keep, trend) = fit_channel(&xs, &y, &gs, &start, opts.clip).ok_or_else(|| {
            Error::Empty(format!(
                "fewer than 8 Gaia stars have a positive {} flux",
                CHANNELS[c]
            ))
        })?;
        for &i in &keep {
            calibrators[i].kept[c] = true;
        }
        let grange = {
            let (lo, hi) = keep.iter().fold((f64::MAX, f64::MIN), |(l, h), &i| {
                (l.min(gs[i]), h.max(gs[i]))
            });
            hi - lo
        };
        if trend.abs() * grange > 0.1 && keep.len() >= 12 {
            warnings.push(format!(
                "the {} zero point drifts by {:+.2} mag across the {:.1} mag of G measured: the brightest stars may be saturated or the image not linear",
                CHANNELS[c],
                trend * grange,
                grange
            ));
        }
        channels.push(ChannelZp {
            channel: CHANNELS[c],
            zero: fit.zero,
            zero_error: fit.zero_error,
            colour_term: fit.slope,
            colour_term_error: fit.slope_error,
            scatter: fit.rms,
            stars: keep.len(),
            trend_with_g: trend,
            zero_after_gain: fit.zero + 2.5 * cal.gains[c].log10(),
        });
    }
    let channels: [ChannelZp; 3] = channels
        .try_into()
        .map_err(|_| Error::Empty("zero point".into()))?;

    let stars = measure_field(image, wcs, cal, &channels, &calibrators, opts)?;
    let limit = limiting_magnitude(&stars, &channels, opts.limit_snr);
    if channels.iter().any(|c| c.scatter > 0.15) {
        warnings.push(
            "the zero-point scatter is above 0.15 mag: the Gaia stars are noisy or blended, or the image is not linear".into(),
        );
    }
    Ok(Photometry {
        width: image.width(),
        height: image.height(),
        channels,
        reference_colour: reference,
        aperture_px: cal.aperture_px,
        calibrators,
        stars,
        limit,
        warnings,
    })
}

/// Detect and measure every star of the image and calibrate it.
fn measure_field(
    image: &Image,
    wcs: &Wcs,
    cal: &Calibration,
    ch: &[ChannelZp; 3],
    calibrators: &[Calibrator],
    opts: &Options,
) -> Result<Vec<FieldStar>> {
    let reference = f64::from(cal.reference_colour);
    let luma = luminance(&image.planes);
    let det = detect(
        &luma,
        &DetectOptions {
            threshold: opts.threshold,
            border: 4,
            max_stars: opts.max_stars,
            ..DetectOptions::default()
        },
    );
    // Brighter than every star the green fit kept: the zero point is unverified there.
    let brightest = calibrators
        .iter()
        .filter(|c| c.kept[1])
        .map(|c| f64::from(c.g))
        .fold(f64::INFINITY, f64::min);
    let r = cal.aperture_px;
    let (r_in, r_out) = (r + 2.0, r + 2.0 + (0.8 * r).max(3.0));
    // A grid of the detections, to find neighbours without an n-squared scan.
    let cell = 64.0;
    let cols = (image.width() as f64 / cell).ceil() as usize + 1;
    let rows = (image.height() as f64 / cell).ceil() as usize + 1;
    let mut grid: Vec<Vec<usize>> = vec![Vec::new(); cols * rows];
    for (i, s) in det.stars.iter().enumerate() {
        grid[(s.y / cell) as usize * cols + (s.x / cell) as usize].push(i);
    }
    let crowded = |i: usize| -> bool {
        let s = &det.stars[i];
        let (cx, cy) = ((s.x / cell) as isize, (s.y / cell) as isize);
        for gy in (cy - 1).max(0)..=(cy + 1).min(rows as isize - 1) {
            for gx in (cx - 1).max(0)..=(cx + 1).min(cols as isize - 1) {
                for &j in &grid[gy as usize * cols + gx as usize] {
                    let o = &det.stars[j];
                    if j != i && (o.x - s.x).hypot(o.y - s.y) < r_out + 0.5 * f64::from(o.fwhm) {
                        return true;
                    }
                }
            }
        }
        false
    };
    let mut out: Vec<FieldStar> = det
        .stars
        .par_iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let mut flux = [0.0; 3];
            let mut snr = [0.0; 3];
            for (c, plane) in image.planes.iter().enumerate() {
                let a = aperture(plane, s.x, s.y, r, r_in, r_out)?;
                flux[c] = a.flux;
                snr[c] = if a.error > 0.0 { a.flux / a.error } else { 0.0 };
            }
            let gaia = calibrators
                .iter()
                .find(|g| (g.x - s.x).hypot(g.y - s.y) < 0.5);
            let mut flags = String::new();
            if s.saturated {
                flags.push('S');
            }
            if crowded(i) {
                flags.push('C');
            }
            let (bp, bp_err) = if let Some(g) = gaia {
                flags.push('G');
                (f64::from(g.bp_rp), 0.0)
            } else if let Some((b, e)) = estimate_colour(cal, flux, snr) {
                (b, e)
            } else {
                flags.push('E');
                (reference, 0.4)
            };
            if flux.iter().any(|&f| f <= 0.0) {
                flags.push('N');
            }
            if flux[1] > 0.0
                && ch[1].zero + ch[1].colour_term * (bp - reference) - 2.5 * flux[1].log10()
                    < brightest
            {
                flags.push('B');
            }
            let mut mag = [NAN; 3];
            let mut err = [NAN; 3];
            for c in 0..3 {
                if flux[c] > 0.0 {
                    mag[c] =
                        ch[c].zero + ch[c].colour_term * (bp - reference) - 2.5 * flux[c].log10();
                    err[c] = (MAG_PER_FRACTION / snr[c].max(1e-9)).powi(2)
                        + ch[c].zero_error.powi(2)
                        + (ch[c].colour_term * bp_err).powi(2);
                    err[c] = err[c].sqrt();
                }
            }
            let (ra, dec) = wcs.pixel_to_world(s.x, s.y);
            Some(FieldStar {
                id: 0,
                x: s.x,
                y: s.y,
                ra,
                dec,
                flux,
                snr,
                mag,
                mag_error: err,
                bp_rp: bp,
                bp_rp_error: bp_err,
                gaia_g: gaia.map(|g| g.g),
                fwhm: s.fwhm,
                flags,
            })
        })
        .collect();
    if out.is_empty() {
        return Err(Error::Empty(
            "no star could be measured for the catalogue".into(),
        ));
    }
    out.sort_by(|a, b| a.mag[1].total_cmp(&b.mag[1]));
    for (i, s) in out.iter_mut().enumerate() {
        s.id = i + 1;
    }
    Ok(out)
}

fn limiting_magnitude(stars: &[FieldStar], ch: &[ChannelZp; 3], snr: f64) -> Limit {
    let clean = |s: &&FieldStar| !s.flags.contains(['S', 'C']);
    // Aperture noise: flux / SNR for the stars whose flux is positive.
    let noise: [f64; 3] = std::array::from_fn(|c| {
        let mut v: Vec<f64> = stars
            .iter()
            .filter(clean)
            .filter(|s| s.flux[c] > 0.0 && s.snr[c] > 0.0)
            .map(|s| s.flux[c] / s.snr[c])
            .collect();
        median(&mut v)
    });
    let at = |c: usize, s: f64| ch[c].zero - 2.5 * (s * noise[c]).log10();
    let from_noise = std::array::from_fn(|c| at(c, snr));
    let good: Vec<&FieldStar> = stars
        .iter()
        .filter(clean)
        .filter(|s| s.mag[1].is_finite())
        .collect();
    let faintest = good
        .iter()
        .filter(|s| s.snr[1] >= snr)
        .map(|s| s.mag[1])
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    // Differential counts in half-magnitude bins; the peak, once there are enough stars.
    let counts_peak = if good.len() >= 30 {
        let lo = good.iter().map(|s| s.mag[1]).fold(f64::MAX, f64::min);
        let hi = good.iter().map(|s| s.mag[1]).fold(f64::MIN, f64::max);
        let bins = (((hi - lo) / 0.5).ceil() as usize).max(1);
        let mut n = vec![0usize; bins];
        for s in &good {
            n[(((s.mag[1] - lo) / 0.5) as usize).min(bins - 1)] += 1;
        }
        n.iter()
            .enumerate()
            .max_by_key(|(_, c)| **c)
            .map(|(i, _)| lo + 0.5 * i as f64 + 0.25)
    } else {
        None
    };
    Limit {
        snr,
        aperture_noise: noise,
        from_noise,
        green_at_3_and_10: [at(1, 3.0), at(1, 10.0)],
        faintest_measured: faintest,
        counts_peak,
        stars: good.len(),
    }
}

// ------------------------------------------------------------------ tables

/// The calibrated star catalogue as CSV.
#[must_use]
pub fn stars_csv(p: &Photometry) -> String {
    let rows: Vec<Vec<String>> = p
        .stars
        .iter()
        .map(|s| {
            vec![
                s.id.to_string(),
                num(s.x, 2),
                num(s.y, 2),
                num(s.ra, 6),
                num(s.dec, 6),
                num(s.mag[1], 3),
                num(s.mag_error[1], 3),
                num(s.mag[0], 3),
                num(s.mag_error[0], 3),
                num(s.mag[2], 3),
                num(s.mag_error[2], 3),
                num(s.snr[1], 1),
                num(s.flux[1], 2),
                num(s.bp_rp, 2),
                num(s.bp_rp_error, 2),
                s.gaia_g.map_or(String::new(), |g| num(f64::from(g), 2)),
                s.gaia_g
                    .map_or(String::new(), |g| num(s.mag[1] - f64::from(g), 3)),
                num(f64::from(s.fwhm), 2),
                s.flags.clone(),
            ]
        })
        .collect();
    csv_text(
        &[
            "id",
            "x",
            "y",
            "ra_deg",
            "dec_deg",
            "mag_g",
            "mag_g_err",
            "mag_r",
            "mag_r_err",
            "mag_b",
            "mag_b_err",
            "snr_g",
            "flux_g",
            "bp_rp",
            "bp_rp_err",
            "gaia_g",
            "mag_g_minus_gaia_g",
            "fwhm_px",
            "flags",
        ],
        &rows,
    )
}

/// The Gaia stars behind the zero point as CSV.
#[must_use]
pub fn zeropoint_csv(p: &Photometry) -> String {
    let rows: Vec<Vec<String>> = p
        .calibrators
        .iter()
        .map(|s| {
            let colour = f64::from(s.bp_rp) - p.reference_colour;
            let res = |c: usize| {
                num(
                    s.zp[c] - p.channels[c].zero - p.channels[c].colour_term * colour,
                    4,
                )
            };
            vec![
                num(s.x, 2),
                num(s.y, 2),
                num(f64::from(s.g), 2),
                num(f64::from(s.bp_rp), 3),
                num(s.zp[0], 4),
                num(s.zp[1], 4),
                num(s.zp[2], 4),
                res(0),
                res(1),
                res(2),
                s.kept
                    .iter()
                    .map(|k| char::from(b"01"[usize::from(*k)]))
                    .collect::<String>(),
            ]
        })
        .collect();
    csv_text(
        &[
            "x",
            "y",
            "g",
            "bp_rp",
            "zp_r",
            "zp_g",
            "zp_b",
            "residual_r",
            "residual_g",
            "residual_b",
            "kept_rgb",
        ],
        &rows,
    )
}

#[derive(Serialize)]
struct Summary<'a> {
    reference_bp_rp: f64,
    aperture_px: f64,
    pixel_units: &'static str,
    channels: &'a [ChannelZp; 3],
    limit: &'a Limit,
    catalogue_stars: usize,
    gaia_matches: usize,
    warnings: &'a [String],
}

/// The zero points, limits and counts as JSON (the stars are in the CSV).
///
/// # Errors
/// If serialisation fails.
pub fn to_json(p: &Photometry) -> Result<String> {
    serde_json::to_string_pretty(&Summary {
        reference_bp_rp: p.reference_colour,
        aperture_px: p.aperture_px,
        pixel_units: "the pixel values of the image as measured, before the colour gains",
        channels: &p.channels,
        limit: &p.limit,
        catalogue_stars: p.stars.len(),
        gaia_matches: p.stars.iter().filter(|s| s.gaia_g.is_some()).count(),
        warnings: &p.warnings,
    })
    .map_err(|e| Error::Parse(e.to_string()))
}

// ------------------------------------------------------------------- plots

/// Pixels of the zero-point sheet.
pub const ZP_SIZE: (u32, u32) = (1500, 520);
/// Pixels of the limiting-magnitude sheet.
pub const LIMIT_SIZE: (u32, u32) = (1200, 520);

const CHANNEL_COLOUR: [RGBColor; 3] = [plot::RED, plot::GREEN, plot::BLUE];

macro_rules! mesh {
    ($chart:expr, $x:expr, $y:expr) => {
        $chart
            .configure_mesh()
            .light_line_style(WHITE)
            .bold_line_style(RGBColor(215, 215, 215))
            .x_desc($x)
            .y_desc($y)
            .x_label_formatter(&|v| plot::tick(*v))
            .y_label_formatter(&|v| plot::tick(*v))
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .draw()
            .map_err(Error::plot)?
    };
}

/// The zero-point sheet as RGB bytes of [`ZP_SIZE`]: the colour-corrected zero
/// point against G, the zero point against BP-RP with the colour term, and the
/// green zero-point residual across the field.
///
/// # Errors
/// If drawing fails.
pub fn zeropoint_rgb(p: &Photometry) -> Result<Vec<u8>> {
    plot::render_rgb(ZP_SIZE.0, ZP_SIZE.1, |root| {
        let areas = root.split_evenly((1, 3));
        let reference = p.reference_colour;
        let caption = (FAMILY, 14).into_font().style(FontStyle::Bold);
        // Panel 1: colour-corrected zero point against G.
        {
            let pts = |c: usize, kept: bool| -> Vec<(f64, f64)> {
                p.calibrators
                    .iter()
                    .filter(|s| s.zp[c].is_finite() && s.kept[c] == kept)
                    .map(|s| {
                        (
                            f64::from(s.g),
                            s.zp[c] - p.channels[c].colour_term * (f64::from(s.bp_rp) - reference),
                        )
                    })
                    .collect()
            };
            let all: Vec<(f64, f64)> = (0..3).flat_map(|c| pts(c, true)).collect();
            let (x0, x1) = plot::padded_range(all.iter().map(|q| q.0));
            let (y0, y1) = plot::padded_range(
                all.iter()
                    .map(|q| q.1)
                    .chain((0..3).map(|c| p.channels[c].zero)),
            );
            let mut chart = ChartBuilder::on(&areas[0])
                .caption("Zero point against G, colour term removed", caption.clone())
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(x0..x1, y0..y1)
                .map_err(Error::plot)?;
            mesh!(chart, "Gaia G", "zero point (mag)");
            for c in 0..3 {
                let col = CHANNEL_COLOUR[c];
                chart
                    .draw_series(
                        pts(c, true)
                            .into_iter()
                            .map(|q| Circle::new(q, 3, col.mix(0.55).filled())),
                    )
                    .map_err(Error::plot)?
                    .label(format!(
                        "{} {:.3} \u{b1} {:.3}",
                        CHANNELS[c], p.channels[c].zero, p.channels[c].zero_error
                    ))
                    .legend(move |(x, y)| Circle::new((x + 9, y), 3, col.filled()));
                chart
                    .draw_series(
                        pts(c, false)
                            .into_iter()
                            .map(|q| Cross::new(q, 4, plot::GREY.stroke_width(1))),
                    )
                    .map_err(Error::plot)?;
                chart
                    .draw_series(LineSeries::new(
                        [(x0, p.channels[c].zero), (x1, p.channels[c].zero)],
                        col.stroke_width(2),
                    ))
                    .map_err(Error::plot)?;
            }
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        // Panel 2: zero point against BP-RP with the colour term.
        {
            let all: Vec<(f64, f64)> = (0..3)
                .flat_map(|c| {
                    p.calibrators
                        .iter()
                        .filter(move |s| s.kept[c])
                        .map(move |s| (f64::from(s.bp_rp), s.zp[c]))
                })
                .collect();
            let (x0, x1) = plot::padded_range(all.iter().map(|q| q.0));
            let line =
                |c: usize, x: f64| p.channels[c].zero + p.channels[c].colour_term * (x - reference);
            let (y0, y1) = plot::padded_range(
                all.iter()
                    .map(|q| q.1)
                    .chain((0..3).flat_map(|c| [line(c, x0), line(c, x1)])),
            );
            let mut chart = ChartBuilder::on(&areas[1])
                .caption("Zero point against BP-RP: the colour term", caption.clone())
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(x0..x1, y0..y1)
                .map_err(Error::plot)?;
            mesh!(chart, "Gaia BP-RP", "zero point (mag)");
            for c in 0..3 {
                let col = CHANNEL_COLOUR[c];
                chart
                    .draw_series(p.calibrators.iter().filter(|s| s.kept[c]).map(|s| {
                        Circle::new((f64::from(s.bp_rp), s.zp[c]), 3, col.mix(0.55).filled())
                    }))
                    .map_err(Error::plot)?;
                chart
                    .draw_series(LineSeries::new(
                        [(x0, line(c, x0)), (x1, line(c, x1))],
                        col.stroke_width(2),
                    ))
                    .map_err(Error::plot)?
                    .label(format!(
                        "{} {:+.3} \u{b1} {:.3} mag per colour",
                        CHANNELS[c], p.channels[c].colour_term, p.channels[c].colour_term_error
                    ))
                    .legend(move |(x, y)| {
                        PathElement::new([(x, y), (x + 18, y)], col.stroke_width(2))
                    });
            }
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        // Panel 3: green residual across the field (flatness of the zero point).
        {
            let (cx, cy) = (p.width as f64 / 2.0, p.height as f64 / 2.0);
            let rmax = cx.hypot(cy);
            let pts: Vec<(f64, f64)> = p
                .calibrators
                .iter()
                .filter(|s| s.kept[1])
                .map(|s| {
                    (
                        (s.x - cx).hypot(s.y - cy),
                        s.zp[1]
                            - p.channels[1].zero
                            - p.channels[1].colour_term * (f64::from(s.bp_rp) - reference),
                    )
                })
                .collect();
            let (y0, y1) = plot::padded_range(pts.iter().map(|q| q.1).chain([0.0]));
            let mut chart = ChartBuilder::on(&areas[2])
                .caption("Green residual across the field", caption.clone())
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(0.0..rmax, y0..y1)
                .map_err(Error::plot)?;
            mesh!(chart, "distance from the centre (pixels)", "residual (mag)");
            chart
                .draw_series(
                    pts.iter()
                        .map(|&q| Circle::new(q, 3, plot::GREEN.mix(0.55).filled())),
                )
                .map_err(Error::plot)?;
            chart
                .draw_series(LineSeries::new(
                    [(0.0, 0.0), (rmax, 0.0)],
                    plot::GREY.stroke_width(1),
                ))
                .map_err(Error::plot)?;
            // Median in five rings.
            let mut med = Vec::new();
            for k in 0..5 {
                let (a, b) = (rmax * f64::from(k) / 5.0, rmax * f64::from(k + 1) / 5.0);
                let mut v: Vec<f64> = pts
                    .iter()
                    .filter(|q| q.0 >= a && q.0 < b)
                    .map(|q| q.1)
                    .collect();
                if v.len() >= 3 {
                    med.push((f64::midpoint(a, b), median(&mut v)));
                }
            }
            chart
                .draw_series(LineSeries::new(med.clone(), plot::ORANGE.stroke_width(3)))
                .map_err(Error::plot)?
                .label("median of five rings")
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::ORANGE.stroke_width(3))
                });
            chart
                .draw_series(
                    med.iter()
                        .map(|&q| Circle::new(q, 5, plot::ORANGE.filled())),
                )
                .map_err(Error::plot)?;
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        Ok(())
    })
}

/// The zero-point sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn zeropoint_png(p: &Photometry) -> Result<Vec<u8>> {
    plot::encode_png(&zeropoint_rgb(p)?, ZP_SIZE.0, ZP_SIZE.1)
}

/// The limiting-magnitude sheet as RGB bytes of [`LIMIT_SIZE`]: signal-to-noise
/// against magnitude, and the differential star counts.
///
/// # Errors
/// If drawing fails.
pub fn limit_rgb(p: &Photometry) -> Result<Vec<u8>> {
    plot::render_rgb(LIMIT_SIZE.0, LIMIT_SIZE.1, |root| {
        let (left, right) = root.split_horizontally(LIMIT_SIZE.0 / 2);
        let l = &p.limit;
        let m_lim = l.from_noise[1];
        let good: Vec<&FieldStar> = p
            .stars
            .iter()
            .filter(|s| !s.flags.contains(['S', 'C']) && s.mag[1].is_finite() && s.snr[1] > 0.0)
            .collect();
        let bold = (FAMILY, 14).into_font().style(FontStyle::Bold);
        {
            let (x0, x1) = plot::padded_range(good.iter().map(|s| s.mag[1]).chain([m_lim]));
            let ys: Vec<f64> = good.iter().map(|s| s.snr[1].log10()).collect();
            let (y0, y1) = plot::padded_range(ys.iter().copied().chain([l.snr.log10()]));
            let mut chart = ChartBuilder::on(&left)
                .caption(
                    format!(
                        "Signal-to-noise against magnitude (green), {:.0}-sigma limit {m_lim:.2}",
                        l.snr
                    ),
                    bold.clone(),
                )
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(x0..x1, y0..y1)
                .map_err(Error::plot)?;
            mesh!(
                chart,
                "magnitude (Gaia-G equivalent)",
                "log10 signal-to-noise"
            );
            chart
                .draw_series(good.iter().filter(|s| s.gaia_g.is_none()).map(|s| {
                    Circle::new(
                        (s.mag[1], s.snr[1].log10()),
                        2,
                        plot::BLUE.mix(0.45).filled(),
                    )
                }))
                .map_err(Error::plot)?
                .label("detected stars")
                .legend(|(x, y)| Circle::new((x + 9, y), 3, plot::BLUE.filled()));
            chart
                .draw_series(
                    good.iter().filter(|s| s.gaia_g.is_some()).map(|s| {
                        Circle::new((s.mag[1], s.snr[1].log10()), 3, plot::ORANGE.filled())
                    }),
                )
                .map_err(Error::plot)?
                .label("matched to Gaia")
                .legend(|(x, y)| Circle::new((x + 9, y), 3, plot::ORANGE.filled()));
            chart
                .draw_series(LineSeries::new(
                    [(x0, l.snr.log10()), (x1, l.snr.log10())],
                    plot::RED.stroke_width(1),
                ))
                .map_err(Error::plot)?
                .label(format!("signal-to-noise {:.0}", l.snr))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::RED.stroke_width(1))
                });
            chart
                .draw_series(LineSeries::new(
                    [(m_lim, y0), (m_lim, y1)],
                    plot::GREEN.stroke_width(2),
                ))
                .map_err(Error::plot)?
                .label(format!("limit from the noise {m_lim:.2}"))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::GREEN.stroke_width(2))
                });
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        {
            let lo = good.iter().map(|s| s.mag[1]).fold(f64::MAX, f64::min);
            let hi = good
                .iter()
                .map(|s| s.mag[1])
                .fold(f64::MIN, f64::max)
                .max(m_lim);
            let bins = (((hi - lo) / 0.5).ceil() as usize).max(1);
            let mut n = vec![0usize; bins];
            for s in &good {
                n[(((s.mag[1] - lo) / 0.5) as usize).min(bins - 1)] += 1;
            }
            let top = n.iter().copied().max().unwrap_or(1).max(1) as f64 * 1.1;
            let mut chart = ChartBuilder::on(&right)
                .caption("Star counts per half magnitude (green)", bold)
                .margin(10)
                .x_label_area_size(36)
                .y_label_area_size(60)
                .build_cartesian_2d(lo..lo + 0.5 * bins as f64, 0.0..top)
                .map_err(Error::plot)?;
            mesh!(chart, "magnitude (Gaia-G equivalent)", "stars");
            chart
                .draw_series(n.iter().enumerate().map(|(i, &c)| {
                    let a = lo + 0.5 * i as f64;
                    Rectangle::new(
                        [(a, 0.0), (a + 0.5, c as f64)],
                        plot::BLUE.mix(0.5).filled(),
                    )
                }))
                .map_err(Error::plot)?;
            chart
                .draw_series(LineSeries::new(
                    [(m_lim, 0.0), (m_lim, top)],
                    plot::GREEN.stroke_width(2),
                ))
                .map_err(Error::plot)?
                .label(format!("{:.0}-sigma limit {m_lim:.2}", l.snr))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::GREEN.stroke_width(2))
                });
            if let Some(pk) = l.counts_peak {
                chart
                    .draw_series(LineSeries::new(
                        [(pk, 0.0), (pk, top)],
                        plot::RED.stroke_width(2),
                    ))
                    .map_err(Error::plot)?
                    .label(format!("counts peak {pk:.2}"))
                    .legend(|(x, y)| {
                        PathElement::new([(x, y), (x + 18, y)], plot::RED.stroke_width(2))
                    });
            }
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(plot::GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperLeft)
                .draw()
                .map_err(Error::plot)?;
        }
        Ok(())
    })
}

/// The limiting-magnitude sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn limit_png(p: &Photometry) -> Result<Vec<u8>> {
    plot::encode_png(&limit_rgb(p)?, LIMIT_SIZE.0, LIMIT_SIZE.1)
}

#[cfg(test)]
mod tests;
