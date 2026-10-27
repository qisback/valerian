// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Aperture photometry of one star across a set of frames: differential
//! magnitudes against an ensemble of comparison stars, a light curve and its
//! export.

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::{Deserialize, Serialize};
use valerian_core::Frame;
use valerian_core::fits::Image;
use valerian_core::stack::{StackOptions, register_frames};
use valerian_core::stars::{detect, luminance};
use valerian_core::stats;

use crate::fit::median_of;
use crate::plot;
use crate::subframe::epoch_of;
use crate::table::{csv_text, num};
use crate::timeutil::format_utc;
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// How to measure.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Aperture radius in units of the reference frame's median FWHM.
    pub aperture: f64,
    /// How many comparison stars to use at most.
    pub comparison: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            aperture: 2.0,
            comparison: 10,
        }
    }
}

/// One aperture measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ap {
    /// Flux above the sky inside the aperture, ADU.
    pub flux: f64,
    /// Sky level per pixel, ADU.
    pub sky: f64,
    /// Uncertainty of the flux from the sky noise, ADU.
    pub error: f64,
    /// Pixels in the aperture.
    pub pixels: f64,
}

/// Fraction of the pixel at `(x, y)` that lies inside a circle of radius `r`
/// about `(cx, cy)`, by 6 by 6 subpixels where the edge crosses it.
fn coverage(x: f64, y: f64, cx: f64, cy: f64, r: f64) -> f64 {
    let d = (x - cx).hypot(y - cy);
    if d + 0.71 <= r {
        return 1.0;
    }
    if d - 0.71 >= r {
        return 0.0;
    }
    let n = 6;
    let mut inside = 0;
    for i in 0..n {
        for j in 0..n {
            let sx = x - 0.5 + (f64::from(i) + 0.5) / f64::from(n);
            let sy = y - 0.5 + (f64::from(j) + 0.5) / f64::from(n);
            if (sx - cx).hypot(sy - cy) <= r {
                inside += 1;
            }
        }
    }
    f64::from(inside) / f64::from(n * n)
}

/// Aperture photometry of the star at `(cx, cy)`: the flux in a circle of
/// radius `r` (pixel fractions, exact to 1/36 at the edge) less the sigma-clipped
/// median of the annulus `r_in..r_out` times the aperture's pixels.
#[must_use]
pub fn aperture(f: &Frame, cx: f64, cy: f64, r: f64, r_in: f64, r_out: f64) -> Option<Ap> {
    let (w, h) = (f.width as i64, f.height as i64);
    let (x0, x1) = ((cx - r_out).floor() as i64, (cx + r_out).ceil() as i64);
    let (y0, y1) = ((cy - r_out).floor() as i64, (cy + r_out).ceil() as i64);
    if x0 < 0 || y0 < 0 || x1 >= w || y1 >= h {
        return None;
    }
    let mut ring = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = (x as f64 - cx).hypot(y as f64 - cy);
            let v = f.data[(y * w + x) as usize];
            if v.is_finite() && d >= r_in && d <= r_out {
                ring.push(v);
            }
        }
    }
    if ring.len() < 20 {
        return None;
    }
    let (m, s) = stats::median_sigma(&ring);
    let kept: Vec<f32> = ring
        .iter()
        .copied()
        .filter(|v| (v - m).abs() <= 3.0 * s.max(f32::MIN_POSITIVE))
        .collect();
    let sky = f64::from(stats::median_finite(&kept));
    let sigma = f64::from(stats::median_sigma(&kept).1);
    let (mut sum, mut pixels) = (0.0, 0.0);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let wgt = coverage(x as f64, y as f64, cx, cy, r);
            if wgt > 0.0 {
                let v = f.data[(y * w + x) as usize];
                if !v.is_finite() {
                    return None;
                }
                sum += wgt * f64::from(v);
                pixels += wgt;
            }
        }
    }
    let flux = sum - sky * pixels;
    let error = sigma * (pixels * (1.0 + pixels / kept.len().max(1) as f64)).sqrt();
    Some(Ap {
        flux,
        sky,
        error,
        pixels,
    })
}

/// Refine a star's position by the flux-weighted centroid of the pixels within
/// `r` above the local sky, up to 4 times; the start if it will not settle.
#[must_use]
pub fn centroid(f: &Frame, x: f64, y: f64, r: f64, sky: f64) -> (f64, f64) {
    let (mut cx, mut cy) = (x, y);
    for _ in 0..4 {
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        let ri = r.ceil() as i64;
        for yy in (cy.round() as i64 - ri)..=(cy.round() as i64 + ri) {
            for xx in (cx.round() as i64 - ri)..=(cx.round() as i64 + ri) {
                if xx < 0 || yy < 0 || xx >= f.width as i64 || yy >= f.height as i64 {
                    continue;
                }
                if (xx as f64 - cx).hypot(yy as f64 - cy) > r {
                    continue;
                }
                let v = f64::from(f.data[yy as usize * f.width + xx as usize]) - sky;
                if v > 0.0 {
                    sx += v * xx as f64;
                    sy += v * yy as f64;
                    sw += v;
                }
            }
        }
        if sw <= 0.0 {
            return (x, y);
        }
        (cx, cy) = (sx / sw, sy / sw);
    }
    if (cx - x).hypot(cy - y) > r.max(3.0) {
        (x, y)
    } else {
        (cx, cy)
    }
}

/// One frame of the light curve.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Row {
    /// Index in the input list.
    pub frame: usize,
    /// File name.
    pub name: String,
    /// Mid-exposure time, UTC.
    pub time_utc: Option<String>,
    /// Seconds since the Unix epoch.
    pub epoch_s: Option<f64>,
    /// Position of the star in this frame, pixels.
    pub x: f64,
    /// Position of the star in this frame, pixels.
    pub y: f64,
    /// Aperture radius used in this frame, pixels (it follows the frame's FWHM).
    pub aperture_px: f64,
    /// Flux above the sky, ADU.
    pub flux: f64,
    /// Flux error from the sky noise, ADU.
    pub flux_error: f64,
    /// Sky level per pixel, ADU.
    pub sky: f64,
    /// Signal to noise of the star.
    pub snr: f64,
    /// Summed flux of the comparison stars, ADU.
    pub ensemble_flux: f64,
    /// Instrumental magnitude `-2.5 log10 flux`.
    pub mag_instrumental: f64,
    /// Magnitude against the ensemble, median 0 (brighter is negative).
    pub mag_relative: f64,
    /// Its uncertainty.
    pub mag_error: f64,
    /// Magnitude of the first comparison star against the others, median 0.
    pub check_mag: f64,
    /// Whether the frame is a usable point.
    pub ok: bool,
    /// Why not, if not.
    pub note: String,
}

/// A light curve.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LightCurve {
    /// Position of the star in the reference frame.
    pub reference_xy: (f64, f64),
    /// Index of the reference frame.
    pub reference: usize,
    /// Aperture radius, pixels.
    pub aperture_px: f64,
    /// Sky annulus, pixels.
    pub annulus_px: (f64, f64),
    /// Positions of the comparison stars in the reference frame.
    pub comparison: Vec<(f64, f64)>,
    /// The points.
    pub rows: Vec<Row>,
    /// Robust scatter of the target about its median, magnitudes.
    pub scatter_mag: f64,
    /// The same for the check star, magnitudes.
    pub check_scatter_mag: f64,
}

/// Whether a star is saturated: a colour plane reaches the top of the 16-bit
/// range (60000 or more) or of a 0 to 1 scale (0.98 or more), and a pixel within
/// 2 pixels of the star is within 15 per cent of that top. (Demosaicing blurs
/// a clipped pixel into its neighbours, so the test is not for equality.)
fn saturated(image: &Image, x: f64, y: f64) -> bool {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    image.planes.iter().any(|p| {
        let top = p
            .data
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(f32::MIN, f32::max);
        if !(top >= 60_000.0 || (0.98..=1.0001).contains(&top)) {
            return false;
        }
        (-2..=2).any(|dy| {
            (-2..=2).any(|dx| {
                let (xx, yy) = (cx + dx, cy + dy);
                xx >= 0
                    && yy >= 0
                    && (xx as usize) < p.width
                    && (yy as usize) < p.height
                    && p.data[yy as usize * p.width + xx as usize] >= 0.85 * top
            })
        })
    })
}

/// The target's position in the reference frame that is the brightest star
/// that is neither saturated nor at the edge: the default target.
///
/// # Errors
/// [`Error::Empty`] when no star qualifies.
pub fn brightest_star(image: &Image) -> Result<(f64, f64)> {
    let lum = luminance(&image.planes);
    let det = detect(&lum, &valerian_core::stars::DetectOptions::default());
    det.stars
        .iter()
        .find(|s| {
            !saturated(image, s.x, s.y)
                && s.x > 30.0
                && s.y > 30.0
                && s.x < lum.width as f64 - 30.0
                && s.y < lum.height as f64 - 30.0
        })
        .map(|s| (s.x, s.y))
        .ok_or_else(|| Error::Empty("no unsaturated star to measure".into()))
}

fn mag(flux: f64) -> f64 {
    if flux > 0.0 {
        -2.5 * flux.log10()
    } else {
        f64::NAN
    }
}

struct Raw {
    radius: f64,
    frame: usize,
    pos: (f64, f64),
    target: Ap,
    comps: Vec<Option<Ap>>,
    epoch: Option<f64>,
}

/// Measure the star at `target` of the reference frame in every frame that
/// registers to it, against an ensemble of comparison stars.
///
/// # Errors
/// [`Error::Empty`] when the frames do not register or the target or too few
/// comparison stars cannot be measured in the reference frame.
pub fn light_curve(
    frames: &[Image],
    names: &[String],
    target: Option<(f64, f64)>,
    opts: &Options,
    stack: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<LightCurve> {
    let reg = register_frames(frames, stack, log).map_err(|e| Error::Empty(e.to_string()))?;
    let reference = reg.reference;
    let target = match target {
        Some(t) => t,
        None => brightest_star(&frames[reference])?,
    };
    let ref_lum = luminance(&frames[reference].planes);
    let det = detect(&ref_lum, &stack.detect);
    let fwhm = f64::from(det.median_fwhm()).max(1.5);
    // The aperture follows each frame's own FWHM, so a soft frame loses no more of the
    // star than a sharp one; the annulus scales with it.
    let radii = |i: usize| {
        let f = f64::from(reg.reports[i].fwhm);
        let f = if f.is_finite() && f > 0.5 { f } else { fwhm };
        let r = (opts.aperture * f).max(2.0);
        (r, 1.5 * r, (2.2 * r).max(1.5 * r + 4.0))
    };
    let (r, r_in, r_out) = radii(reference);
    let worst_out = reg.keep.iter().map(|&i| radii(i).2).fold(r_out, f64::max);
    let (w, h) = (ref_lum.width as f64, ref_lum.height as f64);
    let margin = worst_out + 4.0;
    // Comparison stars: bright, unsaturated, isolated, away from the edges and the target.
    let comps: Vec<(f64, f64)> = det
        .stars
        .iter()
        .filter(|s| {
            !saturated(&frames[reference], s.x, s.y)
                && s.x > margin
                && s.y > margin
                && s.x < w - margin
                && s.y < h - margin
                && (s.x - target.0).hypot(s.y - target.1) > worst_out
                && det.stars.iter().all(|o| {
                    std::ptr::eq(*s, o)
                        || (o.x - s.x).hypot(o.y - s.y) > worst_out
                        || o.peak < 0.02 * s.peak
                })
        })
        .take(opts.comparison * 3)
        .map(|s| (s.x, s.y))
        .collect();
    if comps.len() < 3 {
        return Err(Error::Empty(format!(
            "only {} usable comparison stars in the reference frame; need 3",
            comps.len()
        )));
    }
    let tref = measure_at(&ref_lum, target, r, r_in, r_out).ok_or_else(|| {
        Error::Empty(
            "the target cannot be measured in the reference frame (too near an edge?)".into(),
        )
    })?;
    log(&format!(
        "target at ({:.1}, {:.1}): flux {:.0} ADU, {} comparison stars, aperture {r:.1} px",
        target.0,
        target.1,
        tref.2.flux,
        comps.len()
    ));
    // Every frame: positions through the registration, then the apertures.
    let mut measured = Vec::new();
    for &i in &reg.keep {
        let lum = if i == reference {
            ref_lum.clone()
        } else {
            luminance(&frames[i].planes)
        };
        let to_frame = |p: (f64, f64)| -> Option<(f64, f64)> {
            if i == reference {
                Some(p)
            } else {
                reg.regs[i]
                    .as_ref()?
                    .transform
                    .inverse()
                    .map(|t| t.apply(p))
            }
        };
        let Some(tp) = to_frame(target) else { continue };
        let (fr, fr_in, fr_out) = radii(i);
        let Some((cx, cy, ap)) = measure_at(&lum, tp, fr, fr_in, fr_out) else {
            continue;
        };
        let comp_aps = comps
            .iter()
            .map(|&c| {
                let p = to_frame(c)?;
                measure_at(&lum, p, fr, fr_in, fr_out).map(|m| m.2)
            })
            .collect();
        let epoch =
            epoch_of(&frames[i]).map(|t| t + frames[i].header.exposure().unwrap_or(0.0) / 2.0);
        measured.push(Raw {
            radius: fr,
            frame: i,
            pos: (cx, cy),
            target: ap,
            comps: comp_aps,
            epoch,
        });
    }
    // Comparison stars measured in every frame, brightest first; failing that,
    // those missing from at most a tenth of them (those frames are flagged).
    let n = measured.len().max(1);
    let missing = |k: usize| measured.iter().filter(|r| r.comps[k].is_none()).count();
    let mut good: Vec<usize> = (0..comps.len())
        .filter(|&k| missing(k) == 0)
        .take(opts.comparison)
        .collect();
    if good.len() < 3 {
        good = (0..comps.len())
            .filter(|&k| missing(k) * 10 <= n)
            .take(opts.comparison)
            .collect();
    }
    if good.len() < 3 {
        return Err(Error::Empty(
            "fewer than 3 comparison stars are measured in 90 per cent of the frames".into(),
        ));
    }
    let mut rows: Vec<Row> = measured
        .iter()
        .map(|raw| {
            let comp_flux: Option<Vec<f64>> =
                good.iter().map(|&k| raw.comps[k].map(|a| a.flux)).collect();
            let comp_err: Vec<f64> = good
                .iter()
                .filter_map(|&k| raw.comps[k].map(|a| a.error))
                .collect();
            let (ens, ens_err, check, note) = match comp_flux {
                Some(f) if f.iter().all(|x| *x > 0.0) => {
                    let sum: f64 = f.iter().sum();
                    let err = comp_err.iter().map(|e| e * e).sum::<f64>().sqrt();
                    let check = mag(f[0] / (sum - f[0]));
                    (sum, err, check, String::new())
                }
                _ => (
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    "a comparison star was not measured".to_owned(),
                ),
            };
            let t = raw.target;
            let ok = note.is_empty() && t.flux > 0.0;
            let mag_err = if ok {
                1.0857 * ((t.error / t.flux).powi(2) + (ens_err / ens).powi(2)).sqrt()
            } else {
                f64::NAN
            };
            Row {
                frame: raw.frame,
                name: names.get(raw.frame).cloned().unwrap_or_default(),
                time_utc: raw.epoch.map(format_utc),
                epoch_s: raw.epoch,
                x: raw.pos.0,
                y: raw.pos.1,
                aperture_px: raw.radius,
                flux: t.flux,
                flux_error: t.error,
                sky: t.sky,
                snr: if t.error > 0.0 { t.flux / t.error } else { 0.0 },
                ensemble_flux: ens,
                mag_instrumental: mag(t.flux),
                mag_relative: if ok { mag(t.flux / ens) } else { f64::NAN },
                mag_error: mag_err,
                check_mag: check,
                ok,
                note: if t.flux > 0.0 || !note.is_empty() {
                    note
                } else {
                    "no flux above the sky".to_owned()
                },
            }
        })
        .collect();
    let med = median_of(
        &rows
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.mag_relative)
            .collect::<Vec<_>>(),
    );
    let cmed = median_of(
        &rows
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.check_mag)
            .collect::<Vec<_>>(),
    );
    for r in &mut rows {
        r.mag_relative -= med;
        r.check_mag -= cmed;
    }
    let scatter = |v: Vec<f64>| {
        let m = median_of(&v);
        1.4826 * median_of(&v.iter().map(|x| (x - m).abs()).collect::<Vec<_>>())
    };
    let scatter_mag = scatter(
        rows.iter()
            .filter(|r| r.ok)
            .map(|r| r.mag_relative)
            .collect(),
    );
    let check_scatter_mag = scatter(rows.iter().filter(|r| r.ok).map(|r| r.check_mag).collect());
    Ok(LightCurve {
        reference_xy: target,
        reference,
        aperture_px: r,
        annulus_px: (r_in, r_out),
        comparison: good.iter().map(|&k| comps[k]).collect(),
        rows,
        scatter_mag,
        check_scatter_mag,
    })
}

/// Centroid and measure the star near `p`.
fn measure_at(f: &Frame, p: (f64, f64), r: f64, r_in: f64, r_out: f64) -> Option<(f64, f64, Ap)> {
    let first = aperture(f, p.0, p.1, r, r_in, r_out)?;
    let (cx, cy) = centroid(f, p.0, p.1, r, first.sky);
    aperture(f, cx, cy, r, r_in, r_out).map(|a| (cx, cy, a))
}

/// The light curve as CSV.
#[must_use]
pub fn to_csv(c: &LightCurve) -> String {
    let t0 = c
        .rows
        .iter()
        .filter_map(|r| r.epoch_s)
        .fold(f64::INFINITY, f64::min);
    let rows: Vec<Vec<String>> = c
        .rows
        .iter()
        .map(|r| {
            vec![
                r.frame.to_string(),
                r.name.clone(),
                r.time_utc.clone().unwrap_or_default(),
                r.epoch_s
                    .map_or(String::new(), |e| num((e - t0) / 3600.0, 5)),
                num(r.x, 3),
                num(r.y, 3),
                num(r.aperture_px, 2),
                num(r.flux, 2),
                num(r.flux_error, 2),
                num(r.sky, 2),
                num(r.snr, 1),
                num(r.ensemble_flux, 1),
                num(r.mag_instrumental, 4),
                num(r.mag_relative, 4),
                num(r.mag_error, 4),
                num(r.check_mag, 4),
                u8::from(r.ok).to_string(),
                r.note.clone(),
            ]
        })
        .collect();
    csv_text(
        &[
            "frame",
            "name",
            "time_utc",
            "hours",
            "x",
            "y",
            "aperture_px",
            "flux_adu",
            "flux_error_adu",
            "sky_adu",
            "snr",
            "ensemble_flux_adu",
            "mag_instrumental",
            "mag_relative",
            "mag_error",
            "check_mag",
            "ok",
            "note",
        ],
        &rows,
    )
}

/// The light curve as JSON.
///
/// # Errors
/// If serialisation fails.
pub fn to_json(c: &LightCurve) -> Result<String> {
    serde_json::to_string_pretty(c).map_err(|e| Error::Parse(e.to_string()))
}

/// Pixels of the light-curve sheet.
pub const SIZE: (u32, u32) = (1100, 760);

/// The light curve and the check star under it, as RGB bytes of [`SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn plot_rgb(c: &LightCurve) -> Result<Vec<u8>> {
    let t0 = c
        .rows
        .iter()
        .filter_map(|r| r.epoch_s)
        .fold(f64::INFINITY, f64::min);
    let timed = c.rows.iter().all(|r| r.epoch_s.is_some()) && t0.is_finite();
    let x = |r: &Row| {
        if timed {
            (r.epoch_s.unwrap_or(t0) - t0) / 3600.0
        } else {
            r.frame as f64
        }
    };
    let pts: Vec<&Row> = c.rows.iter().filter(|r| r.ok).collect();
    if pts.is_empty() {
        return Err(Error::Empty("no frame could be measured".into()));
    }
    let (x0, x1) = plot::padded_range(pts.iter().map(|r| x(r)));
    let (y0, y1) = plot::padded_range(pts.iter().flat_map(|r| {
        [
            r.mag_relative - r.mag_error,
            r.mag_relative + r.mag_error,
            r.check_mag,
        ]
    }));
    plot::render_rgb(SIZE.0, SIZE.1, |root| {
        let (top, bottom) = root.split_vertically(SIZE.1 * 62 / 100);
        let label = if timed {
            "hours since the first frame"
        } else {
            "frame"
        };
        let mut chart = ChartBuilder::on(&top)
            .caption(
                format!(
                    "Star at ({:.0}, {:.0}) against {} comparison stars: scatter {:.4} mag",
                    c.reference_xy.0,
                    c.reference_xy.1,
                    c.comparison.len(),
                    c.scatter_mag
                ),
                (FAMILY, 15).into_font().style(FontStyle::Bold),
            )
            .margin(10)
            .x_label_area_size(10)
            .y_label_area_size(64)
            .build_cartesian_2d(x0..x1, y1..y0)
            .map_err(Error::plot)?;
        chart
            .configure_mesh()
            .light_line_style(WHITE)
            .y_desc("relative magnitude (brighter up)")
            .x_label_formatter(&|_| String::new())
            .y_label_formatter(&|v| format!("{v:+.3}"))
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .draw()
            .map_err(Error::plot)?;
        for r in &pts {
            chart
                .draw_series(LineSeries::new(
                    [
                        (x(r), r.mag_relative - r.mag_error),
                        (x(r), r.mag_relative + r.mag_error),
                    ],
                    plot::BLUE.stroke_width(1),
                ))
                .map_err(Error::plot)?;
        }
        chart
            .draw_series(
                pts.iter()
                    .map(|r| Circle::new((x(r), r.mag_relative), 3, plot::BLUE.filled())),
            )
            .map_err(Error::plot)?;
        let mut check = ChartBuilder::on(&bottom)
            .caption(
                format!(
                    "Check star (the first comparison star against the others): scatter {:.4} mag",
                    c.check_scatter_mag
                ),
                (FAMILY, 14).into_font().style(FontStyle::Bold),
            )
            .margin(10)
            .x_label_area_size(36)
            .y_label_area_size(64)
            .build_cartesian_2d(x0..x1, y1..y0)
            .map_err(Error::plot)?;
        check
            .configure_mesh()
            .light_line_style(WHITE)
            .x_desc(label)
            .y_desc("magnitude")
            .x_label_formatter(&|v| plot::tick(*v))
            .y_label_formatter(&|v| format!("{v:+.3}"))
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .draw()
            .map_err(Error::plot)?;
        check
            .draw_series(
                pts.iter()
                    .map(|r| Circle::new((x(r), r.check_mag), 3, plot::GREEN.filled())),
            )
            .map_err(Error::plot)?;
        Ok(())
    })
}

/// The light-curve sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn plot_png(c: &LightCurve) -> Result<Vec<u8>> {
    plot::encode_png(&plot_rgb(c)?, SIZE.0, SIZE.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::{Header, Value};
    use valerian_core::testutil::{Rng, Scene};
    use valerian_core::transform::Transform;

    #[test]
    fn the_aperture_recovers_the_flux_of_a_gaussian_star() {
        // A star of 50000 ADU total, sigma 2 px, on 300 ADU of sky.
        let f =
            valerian_core::testutil::gaussian_field(80, 80, 300.0, &[(40.3, 39.7, 50000.0, 2.0)]);
        let ap = aperture(&f, 40.3, 39.7, 8.0, 12.0, 20.0).expect("aperture");
        assert!((ap.flux - 50000.0).abs() / 50000.0 < 0.005, "{}", ap.flux);
        assert!((ap.sky - 300.0).abs() < 0.5);
        let (cx, cy) = centroid(&f, 41.0, 39.0, 6.0, 300.0);
        assert!(
            (cx - 40.3).abs() < 0.05 && (cy - 39.7).abs() < 0.05,
            "{cx} {cy}"
        );
        assert!(aperture(&f, 3.0, 3.0, 8.0, 12.0, 20.0).is_none());
    }

    #[test]
    fn the_coverage_of_a_pixel_adds_to_the_area_of_the_circle() {
        let r = 4.3;
        let area: f64 = (-8..=8)
            .flat_map(|y| (-8..=8).map(move |x| (x, y)))
            .map(|(x, y)| coverage(f64::from(x), f64::from(y), 0.0, 0.0, r))
            .sum();
        assert!((area - std::f64::consts::PI * r * r).abs() < 0.5, "{area}");
    }

    /// Frames of one sky where one star dims by 0.3 mag in the middle.
    fn frames() -> (Vec<Image>, usize) {
        let scene = Scene::random(70, 400, 300, 5);
        let target = 0usize;
        let mut rng = Rng::new(8);
        let mut out = Vec::new();
        for k in 0..16 {
            let (dx, dy) = (rng.uniform() * 6.0 - 3.0, rng.uniform() * 6.0 - 3.0);
            let mut sc = scene.clone();
            sc.stars[target].0 = 200.0;
            sc.stars[target].1 = 150.0;
            sc.stars[target].2 = 400_000.0;
            let dim = if (6..10).contains(&k) { 0.3 } else { 0.0 };
            sc.stars[target].2 *= 10f64.powf(-0.4 * dim);
            let view = Transform::similarity(1.0, 0.0, dx, dy);
            let mut img = Image::mono(
                sc.render(&view, 400.0, 4.0, 1.0, 0.0, 40 + k as u64),
                Header::new(),
            );
            img.header.set(
                "DATE-OBS",
                Value::Str(format!("2026-02-01T21:{:02}:00", k * 3)),
                "",
            );
            img.header.set("EXPTIME", Value::Float(60.0), "");
            out.push(img);
        }
        (out, target)
    }

    #[test]
    fn a_dimming_star_shows_in_the_light_curve_and_the_check_star_stays_flat() {
        let (frames, _) = frames();
        let names: Vec<String> = (0..16).map(|k| format!("f{k}.fits")).collect();
        let reg =
            register_frames(&frames, &StackOptions::default(), &mut |_| {}).expect("register");
        let (tx, ty) = {
            // The target in the reference frame: its place in frame 0 moved by that frame's transform.
            let t = reg.regs[0]
                .as_ref()
                .map_or(Transform::similarity(1.0, 0.0, 0.0, 0.0), |r| r.transform);
            let _ = t;
            brightest_star(&frames[reg.reference]).expect("brightest")
        };
        let lc = light_curve(
            &frames,
            &names,
            Some((tx, ty)),
            &Options::default(),
            &StackOptions::default(),
            &mut |_| {},
        )
        .expect("light curve");
        assert_eq!(lc.rows.len(), 16);
        let level = |k: usize| {
            lc.rows
                .iter()
                .find(|r| r.frame == k)
                .expect("row")
                .mag_relative
        };
        let dim = (6..10).map(level).sum::<f64>() / 4.0;
        let flat = [0, 1, 2, 12, 13, 14].iter().map(|&k| level(k)).sum::<f64>() / 6.0;
        assert!((dim - flat - 0.3).abs() < 0.05, "dim {dim} flat {flat}");
        assert!(lc.check_scatter_mag < 0.03, "{}", lc.check_scatter_mag);
        assert!(
            to_csv(&lc).lines().count() == 17
                && to_json(&lc).expect("json").contains("scatter_mag")
        );
        assert_eq!(&plot_png(&lc).expect("plot")[1..4], b"PNG");
    }
}
