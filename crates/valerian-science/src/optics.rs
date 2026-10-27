// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Optics: what the stars of one image say about the telescope.
//!
//! * the star field: position, FWHM, eccentricity and orientation of every
//!   unsaturated star ([`measure_field`]);
//! * FWHM and eccentricity maps across the field, a polynomial surface fitted
//!   to the stars with outlier rejection, drawn as an interpolated image with
//!   contours ([`map_png`]);
//! * a tilt and backfocus hint from the FWHM at the corners and the centre
//!   ([`tilt`]);
//! * the aberration inspector, a 3 by 3 mosaic of corner, edge and centre
//!   crops ([`inspector`]);
//! * the radial profile of the brightest stars with a Moffat fit ([`psf`]).

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::{Deserialize, Serialize};
use valerian_core::fits::Image;
use valerian_core::stars::{DetectOptions, Star, detect, luminance};
use valerian_core::stretch::{AutoLevels, auto_mtf};

use crate::fit::{Surface, fit_surface, levmar, median_of};
use crate::plot::{self, Area, GREY, RED};
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// One star of the field.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct FieldStar {
    /// Centroid x, pixels.
    pub x: f64,
    /// Centroid y, pixels.
    pub y: f64,
    /// FWHM, pixels.
    pub fwhm: f64,
    /// `1 - b/a`.
    pub ellipticity: f64,
    /// Orientation of the major axis, radians from +x towards +y.
    pub theta: f64,
    /// Aperture flux above the sky, ADU.
    pub flux: f64,
    /// Peak over the noise.
    pub snr: f64,
    /// Peak above the sky, ADU.
    pub peak: f64,
}

/// The measured stars of an image.
#[derive(Clone, Debug)]
pub struct Field {
    /// Image width, pixels.
    pub width: usize,
    /// Image height, pixels.
    pub height: usize,
    /// The stars, brightest first.
    pub stars: Vec<FieldStar>,
}

impl Field {
    /// Median FWHM of the stars.
    #[must_use]
    pub fn median_fwhm(&self) -> f64 {
        median_of(&self.stars.iter().map(|s| s.fwhm).collect::<Vec<_>>())
    }

    /// Median eccentricity of the stars.
    #[must_use]
    pub fn median_ellipticity(&self) -> f64 {
        median_of(&self.stars.iter().map(|s| s.ellipticity).collect::<Vec<_>>())
    }
}

fn field_star(s: &Star) -> FieldStar {
    FieldStar {
        x: s.x,
        y: s.y,
        fwhm: f64::from(s.fwhm),
        ellipticity: f64::from(s.ellipticity),
        theta: f64::from(s.theta),
        flux: s.flux,
        snr: f64::from(s.snr),
        peak: f64::from(s.peak),
    }
}

/// Detect the stars of `img` (on its luminance) and keep those fit to measure:
/// not saturated, a finite FWHM, and a peak of at least `min_snr` times the noise.
///
/// # Errors
/// [`Error::Empty`] when fewer than 30 stars remain.
pub fn measure_field(img: &Image, threshold: f32, min_snr: f64) -> Result<Field> {
    let lum = luminance(&img.planes);
    let det = detect(
        &lum,
        &DetectOptions {
            threshold,
            ..DetectOptions::default()
        },
    );
    let stars: Vec<FieldStar> = det
        .stars
        .iter()
        .filter(|s| !s.saturated && s.fwhm.is_finite() && s.ellipticity.is_finite())
        .map(field_star)
        .filter(|s| s.snr >= min_snr)
        .collect();
    if stars.len() < 30 {
        return Err(Error::Empty(format!(
            "only {} usable stars were found; the optics plots need at least 30 \
             (lower --threshold or --min-snr, or use a sharper image)",
            stars.len()
        )));
    }
    Ok(Field {
        width: lum.width,
        height: lum.height,
        stars,
    })
}

/// Which quantity a map shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantity {
    /// FWHM in pixels.
    Fwhm,
    /// Eccentricity.
    Ellipticity,
}

impl Quantity {
    fn value(self, s: &FieldStar) -> f64 {
        match self {
            Self::Fwhm => s.fwhm,
            Self::Ellipticity => s.ellipticity,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Fwhm => "FWHM across the field (pixels)",
            Self::Ellipticity => "Eccentricity across the field (1 - b/a)",
        }
    }
}

/// Fit a surface of total degree `order` to one quantity of the stars.
#[must_use]
pub fn surface_of(field: &Field, q: Quantity, order: usize) -> Option<Surface> {
    let pts: Vec<(f64, f64, f64)> = field.stars.iter().map(|s| (s.x, s.y, q.value(s))).collect();
    fit_surface(&pts, field.width as f64, field.height as f64, order)
}

/// A viridis-like colour map over `0..=1`.
#[must_use]
pub fn colour_map(t: f64) -> RGBColor {
    const STOPS: [(f64, f64, f64); 5] = [
        (68.0, 1.0, 84.0),
        (59.0, 82.0, 139.0),
        (33.0, 145.0, 140.0),
        (94.0, 201.0, 98.0),
        (253.0, 231.0, 37.0),
    ];
    let t = t.clamp(0.0, 1.0) * 4.0;
    let i = (t.floor() as usize).min(3);
    let f = t - i as f64;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    RGBColor(
        (a.0 + (b.0 - a.0) * f) as u8,
        (a.1 + (b.1 - a.1) * f) as u8,
        (a.2 + (b.2 - a.2) * f) as u8,
    )
}

/// Contour segments of a grid of values at one level, by marching squares.
/// The grid is `nx` by `ny` samples; the result is in grid units.
fn contour_segments(v: &[f64], nx: usize, ny: usize, level: f64) -> Vec<((f64, f64), (f64, f64))> {
    let at = |x: usize, y: usize| v[y * nx + x];
    let mut out = Vec::new();
    let lerp = |a: f64, b: f64| {
        if (b - a).abs() < 1e-300 {
            0.5
        } else {
            ((level - a) / (b - a)).clamp(0.0, 1.0)
        }
    };
    for y in 0..ny - 1 {
        for x in 0..nx - 1 {
            let (a, b, c, d) = (at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1));
            let (xf, yf) = (x as f64, y as f64);
            let mut pts = Vec::new();
            if (a < level) != (b < level) {
                pts.push((xf + lerp(a, b), yf));
            }
            if (b < level) != (c < level) {
                pts.push((xf + 1.0, yf + lerp(b, c)));
            }
            if (d < level) != (c < level) {
                pts.push((xf + lerp(d, c), yf + 1.0));
            }
            if (a < level) != (d < level) {
                pts.push((xf, yf + lerp(a, d)));
            }
            match pts.len() {
                2 => out.push((pts[0], pts[1])),
                4 => {
                    out.push((pts[0], pts[1]));
                    out.push((pts[2], pts[3]));
                }
                _ => {}
            }
        }
    }
    out
}

/// Pixels of the map sheet.
pub const MAP_SIZE: (u32, u32) = (1060, 780);

/// The surface sampled on a grid, for the CSV and the contours.
fn sample_grid(s: &Surface, nx: usize, ny: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity(nx * ny);
    for j in 0..ny {
        for i in 0..nx {
            v.push(s.eval(
                (i as f64 + 0.5) * s.width / nx as f64,
                (j as f64 + 0.5) * s.height / ny as f64,
            ));
        }
    }
    v
}

fn draw_map(
    area: &Area<'_>,
    field: &Field,
    q: Quantity,
    surface: &Surface,
    lo: f64,
    hi: f64,
) -> Result<()> {
    let (w, h) = (field.width as f64, field.height as f64);
    let (left, right) = area.split_horizontally(MAP_SIZE.0 as i32 - 110);
    let mut chart = ChartBuilder::on(&left)
        .caption(q.title(), (FAMILY, 17).into_font().style(FontStyle::Bold))
        .margin(8)
        .x_label_area_size(28)
        .y_label_area_size(52)
        .build_cartesian_2d(0.0..w, h..0.0)
        .map_err(Error::plot)?;
    chart
        .configure_mesh()
        .disable_mesh()
        .x_desc("x (pixels)")
        .y_desc("y (pixels)")
        .x_label_formatter(&|v| format!("{v:.0}"))
        .y_label_formatter(&|v| format!("{v:.0}"))
        .label_style((FAMILY, 11))
        .axis_desc_style((FAMILY, 12))
        .draw()
        .map_err(Error::plot)?;
    let nx = 64usize;
    let ny = ((nx as f64 * h / w).round() as usize).max(8);
    let grid = sample_grid(surface, nx, ny);
    let span = (hi - lo).max(1e-12);
    chart
        .draw_series(
            (0..ny)
                .flat_map(|j| (0..nx).map(move |i| (i, j)))
                .map(|(i, j)| {
                    let c = colour_map((grid[j * nx + i] - lo) / span);
                    Rectangle::new(
                        [
                            (i as f64 * w / nx as f64, j as f64 * h / ny as f64),
                            (
                                (i + 1) as f64 * w / nx as f64,
                                (j + 1) as f64 * h / ny as f64,
                            ),
                        ],
                        c.filled(),
                    )
                }),
        )
        .map_err(Error::plot)?;
    // Contours at seven levels, on the coarser grid of sample centres.
    for k in 1..=7 {
        let level = lo + span * f64::from(k) / 8.0;
        for (p0, p1) in contour_segments(&grid, nx, ny, level) {
            let to = |p: (f64, f64)| ((p.0 + 0.5) * w / nx as f64, (p.1 + 0.5) * h / ny as f64);
            chart
                .draw_series(LineSeries::new(
                    [to(p0), to(p1)],
                    BLACK.mix(0.45).stroke_width(1),
                ))
                .map_err(Error::plot)?;
        }
    }
    chart
        .draw_series(
            field
                .stars
                .iter()
                .map(|s| Circle::new((s.x, s.y), 2, WHITE.mix(0.8).stroke_width(1))),
        )
        .map_err(Error::plot)?;
    if q == Quantity::Ellipticity {
        // Whiskers: the mean orientation and size of the elongation on a coarse grid.
        let (gx, gy) = (10usize, ((10.0 * h / w).round() as usize).max(4));
        let r0 = 0.9 * w / gx as f64;
        for j in 0..gy {
            for i in 0..gx {
                let (cx, cy) = (
                    (i as f64 + 0.5) * w / gx as f64,
                    (j as f64 + 0.5) * h / gy as f64,
                );
                let (mut sc, mut ss, mut n) = (0.0, 0.0, 0.0);
                for s in field
                    .stars
                    .iter()
                    .filter(|s| (s.x - cx).hypot(s.y - cy) < r0)
                {
                    sc += s.ellipticity * (2.0 * s.theta).cos();
                    ss += s.ellipticity * (2.0 * s.theta).sin();
                    n += 1.0;
                }
                if n < 3.0 {
                    continue;
                }
                let (mc, ms) = (sc / n, ss / n);
                let mag = mc.hypot(ms);
                let psi = 0.5 * ms.atan2(mc);
                let half = (w / gx as f64) * 0.45 * (mag / 0.35).min(1.0);
                chart
                    .draw_series(LineSeries::new(
                        [
                            (cx - half * psi.cos(), cy - half * psi.sin()),
                            (cx + half * psi.cos(), cy + half * psi.sin()),
                        ],
                        WHITE.stroke_width(3),
                    ))
                    .map_err(Error::plot)?;
                chart
                    .draw_series(LineSeries::new(
                        [
                            (cx - half * psi.cos(), cy - half * psi.sin()),
                            (cx + half * psi.cos(), cy + half * psi.sin()),
                        ],
                        BLACK.stroke_width(1),
                    ))
                    .map_err(Error::plot)?;
            }
        }
    }
    // Colour bar.
    let (bw, bh) = right.dim_in_pixel();
    for k in 0..200 {
        let t = 1.0 - f64::from(k) / 199.0;
        let y0 = 50 + (f64::from(k) / 200.0 * (f64::from(bh) - 110.0)) as i32;
        let y1 = 50 + (f64::from(k + 1) / 200.0 * (f64::from(bh) - 110.0)) as i32 + 1;
        right
            .draw(&Rectangle::new(
                [(14, y0), (34, y1)],
                colour_map(t).filled(),
            ))
            .map_err(Error::plot)?;
    }
    let _ = bw;
    for (frac, v) in [(0.0, hi), (0.5, f64::midpoint(lo, hi)), (1.0, lo)] {
        let y = 50 + (frac * (f64::from(bh) - 110.0)) as i32;
        right
            .draw(&Text::new(
                plot::tick(v),
                (40, y - 6),
                (FAMILY, 12).into_font(),
            ))
            .map_err(Error::plot)?;
    }
    right
        .draw(&Text::new(
            format!("{} stars", field.stars.len()),
            (4, 10),
            (FAMILY, 11).into_font(),
        ))
        .map_err(Error::plot)?;
    Ok(())
}

/// The range a map is coloured over: the 2nd to 98th percentile of the surface.
fn map_range(surface: &Surface) -> (f64, f64) {
    let mut v = sample_grid(surface, 48, 36);
    v.sort_by(f64::total_cmp);
    let (a, b) = (v[v.len() * 2 / 100], v[v.len() * 98 / 100]);
    if b - a < 1e-9 {
        (a - 0.05 * a.abs().max(1.0), b + 0.05 * b.abs().max(1.0))
    } else {
        (a, b)
    }
}

/// The map of FWHM or eccentricity as RGB bytes of [`MAP_SIZE`].
///
/// # Errors
/// If drawing fails, or the field has too few stars for `order`.
pub fn map_rgb(field: &Field, q: Quantity, order: usize) -> Result<Vec<u8>> {
    let surface = surface_of(field, q, order).ok_or_else(|| {
        Error::Empty("too few stars to fit a surface of that order across the field".into())
    })?;
    let (lo, hi) = map_range(&surface);
    plot::render_rgb(MAP_SIZE.0, MAP_SIZE.1, |a| {
        draw_map(a, field, q, &surface, lo, hi)
    })
}

/// The map as a PNG.
///
/// # Errors
/// As for [`map_rgb`].
pub fn map_png(field: &Field, q: Quantity, order: usize) -> Result<Vec<u8>> {
    plot::encode_png(&map_rgb(field, q, order)?, MAP_SIZE.0, MAP_SIZE.1)
}

/// The surface of a map sampled on a grid: `x, y, value` rows, for the CSV.
#[must_use]
pub fn map_csv(field: &Field, q: Quantity, order: usize) -> Option<String> {
    let s = surface_of(field, q, order)?;
    let (nx, ny) = (16usize, 12usize);
    let grid = sample_grid(&s, nx, ny);
    let rows: Vec<Vec<String>> = (0..ny)
        .flat_map(|j| (0..nx).map(move |i| (i, j)))
        .map(|(i, j)| {
            vec![
                num((i as f64 + 0.5) * s.width / nx as f64, 1),
                num((j as f64 + 0.5) * s.height / ny as f64, 1),
                num(grid[j * nx + i], 4),
            ]
        })
        .collect();
    Some(csv_text(&["x", "y", "value"], &rows))
}

/// Every measured star as CSV.
#[must_use]
pub fn stars_csv(field: &Field) -> String {
    let rows: Vec<Vec<String>> = field
        .stars
        .iter()
        .map(|s| {
            vec![
                num(s.x, 2),
                num(s.y, 2),
                num(s.fwhm, 3),
                num(s.ellipticity, 4),
                num(s.theta.to_degrees(), 1),
                num(s.flux, 1),
                num(s.snr, 1),
            ]
        })
        .collect();
    csv_text(
        &[
            "x",
            "y",
            "fwhm_px",
            "ellipticity",
            "theta_deg",
            "flux_adu",
            "snr",
        ],
        &rows,
    )
}

/// A zone of the field and what its stars measure.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Zone {
    /// Name: `top-left`, `top-right`, `bottom-left`, `bottom-right`, `centre`.
    pub name: String,
    /// Stars in the zone.
    pub stars: usize,
    /// Median FWHM, pixels; `None` with fewer than 5 stars.
    pub fwhm: Option<f64>,
    /// Median eccentricity.
    pub ellipticity: Option<f64>,
}

/// What the corner and centre FWHMs say about tilt and backfocus.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Tilt {
    /// Centre and four corner zones.
    pub zones: Vec<Zone>,
    /// Mean corner FWHM over the centre FWHM, minus 1 (positive: the corners are softer).
    pub curvature: Option<f64>,
    /// Largest minus smallest corner FWHM, over their mean.
    pub tilt: Option<f64>,
    /// Name of the softest corner.
    pub softest_corner: Option<String>,
    /// The change of FWHM across the full width and height from a plane
    /// fitted to all stars, pixels.
    pub plane_gradient: (f64, f64),
    /// Mean of `ecc x cos 2(theta - direction to centre)` over stars beyond
    /// half the half-diagonal: positive is elongation along the radius,
    /// negative across it; `None` with too few stars.
    pub radial_elongation: Option<f64>,
    /// The hints, in words.
    pub hints: Vec<String>,
}

/// Evaluate the tilt and backfocus hint.
#[must_use]
pub fn tilt(field: &Field) -> Tilt {
    let (w, h) = (field.width as f64, field.height as f64);
    let corner = |name: &str, right: bool, bottom: bool| {
        let sel: Vec<&FieldStar> = field
            .stars
            .iter()
            .filter(|s| {
                let fx = s.x / w;
                let fy = s.y / h;
                (if right { fx > 0.75 } else { fx < 0.25 })
                    && (if bottom { fy > 0.75 } else { fy < 0.25 })
            })
            .collect();
        zone(name, &sel)
    };
    let centre_sel: Vec<&FieldStar> = field
        .stars
        .iter()
        .filter(|s| ((s.x / w - 0.5).abs() < 0.17) && ((s.y / h - 0.5).abs() < 0.17))
        .collect();
    let zones = vec![
        zone("centre", &centre_sel),
        corner("top-left", false, false),
        corner("top-right", true, false),
        corner("bottom-left", false, true),
        corner("bottom-right", true, true),
    ];
    let centre = zones[0].fwhm;
    let corners: Vec<(&str, f64)> = zones[1..]
        .iter()
        .filter_map(|z| z.fwhm.map(|f| (z.name.as_str(), f)))
        .collect();
    let mean_corner = (!corners.is_empty())
        .then(|| corners.iter().map(|c| c.1).sum::<f64>() / corners.len() as f64);
    let curvature = match (mean_corner, centre) {
        (Some(m), Some(c)) if c > 0.0 && corners.len() >= 3 => Some(m / c - 1.0),
        _ => None,
    };
    let (tilt_v, softest) = if corners.len() >= 3 {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut soft = "";
        for &(n, f) in &corners {
            lo = lo.min(f);
            if f > hi {
                hi = f;
                soft = n;
            }
        }
        (
            Some((hi - lo) / mean_corner.unwrap_or(1.0)),
            Some(soft.to_owned()),
        )
    } else {
        (None, None)
    };
    let pts: Vec<(f64, f64, f64)> = field.stars.iter().map(|s| (s.x, s.y, s.fwhm)).collect();
    let plane_gradient = fit_surface(&pts, w, h, 1).map_or((0.0, 0.0), |s| s.first_order());
    let half_diag = 0.5 * w.hypot(h);
    let (mut acc, mut n) = (0.0, 0usize);
    for s in &field.stars {
        let (dx, dy) = (s.x - 0.5 * w, s.y - 0.5 * h);
        if dx.hypot(dy) > 0.5 * half_diag {
            acc += s.ellipticity * (2.0 * (s.theta - dy.atan2(dx))).cos();
            n += 1;
        }
    }
    let radial_elongation = (n >= 20).then(|| acc / n as f64);
    let mut t = Tilt {
        zones,
        curvature,
        tilt: tilt_v,
        softest_corner: softest,
        plane_gradient,
        radial_elongation,
        hints: Vec::new(),
    };
    t.hints = hints(&t);
    t
}

fn zone(name: &str, sel: &[&FieldStar]) -> Zone {
    zone_min(name, sel, 5)
}

fn zone_min(name: &str, sel: &[&FieldStar], min: usize) -> Zone {
    let enough = sel.len() >= min;
    Zone {
        name: name.to_owned(),
        stars: sel.len(),
        fwhm: enough.then(|| median_of(&sel.iter().map(|s| s.fwhm).collect::<Vec<_>>())),
        ellipticity: enough
            .then(|| median_of(&sel.iter().map(|s| s.ellipticity).collect::<Vec<_>>())),
    }
}

fn hints(t: &Tilt) -> Vec<String> {
    let mut out = Vec::new();
    match (t.curvature, t.tilt) {
        (None, None) => out.push(
            "Too few stars in the corners to judge the field (at least 5 in each of three \
             corners and in the centre are needed)."
                .to_owned(),
        ),
        (c, tl) => {
            if let Some(c) = c {
                if c > 0.12 {
                    out.push(format!(
                        "The corners are {:.0} per cent softer than the centre: field curvature, or the \
                         distance from the flattener or reducer to the sensor (the backfocus) is wrong. \
                         The sign cannot be read from one image: change the spacing by a small step \
                         (0.1 to 0.2 mm), take another image and see whether the corners improve.",
                        100.0 * c
                    ));
                } else if c < -0.08 {
                    out.push(format!(
                        "The centre is {:.0} per cent softer than the corners, which is unusual: \
                         focus is probably set on the outer field, or the flattener over-corrects. \
                         Refocus on the centre stars.",
                        -100.0 * c
                    ));
                } else {
                    out.push(format!(
                        "The field is flat: the corners are within {:.0} per cent of the centre.",
                        (100.0 * c).abs()
                    ));
                }
            }
            if let (Some(tl), Some(name)) = (tl, &t.softest_corner) {
                if tl > 0.10 {
                    out.push(format!(
                        "The corners differ by {:.0} per cent of their mean, softest at the {name}: \
                         this is what a tilt of the sensor or focuser (or decentred optics) towards \
                         that side looks like; a tilt adjuster or shims on the opposite side move it. \
                         A corrector with an offset sensor can look the same.",
                        100.0 * tl
                    ));
                } else {
                    out.push(format!(
                        "The corners agree to {:.0} per cent: no tilt to speak of.",
                        100.0 * tl
                    ));
                }
            }
        }
    }
    if let Some(r) = t.radial_elongation
        && r.abs() > 0.05
    {
        out.push(format!(
            "Stars in the outer field are elongated {} (index {r:+.2}): the pattern is of \
             {} rather than of tracking or tilt.",
            if r > 0.0 {
                "along the radius"
            } else {
                "across the radius"
            },
            if r > 0.0 {
                "coma or a spacing error"
            } else {
                "astigmatism or a spacing error"
            }
        ));
    }
    out
}

/// A bar chart of the five zone FWHMs, as RGB bytes of [`TILT_SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn tilt_rgb(t: &Tilt) -> Result<Vec<u8>> {
    let bars: Vec<(&str, f64)> = [
        "centre",
        "top-left",
        "top-right",
        "bottom-left",
        "bottom-right",
    ]
    .iter()
    .filter_map(|n| {
        t.zones
            .iter()
            .find(|z| z.name == *n)
            .and_then(|z| z.fwhm.map(|f| (*n, f)))
    })
    .collect();
    if bars.is_empty() {
        return Err(Error::Empty("no zone has enough stars".into()));
    }
    let top = bars.iter().map(|b| b.1).fold(0.0, f64::max) * 1.25;
    plot::render_rgb(TILT_SIZE.0, TILT_SIZE.1, |a| {
        let mut chart = ChartBuilder::on(a)
            .caption(
                "FWHM by zone (pixels)",
                (FAMILY, 16).into_font().style(FontStyle::Bold),
            )
            .margin(10)
            .x_label_area_size(30)
            .y_label_area_size(48)
            .build_cartesian_2d(0.0..bars.len() as f64, 0.0..top)
            .map_err(Error::plot)?;
        chart
            .configure_mesh()
            .disable_x_mesh()
            .x_labels(0)
            .y_desc("FWHM (px)")
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .draw()
            .map_err(Error::plot)?;
        for (i, (name, f)) in bars.iter().enumerate() {
            let colour = if *name == "centre" {
                plot::GREEN
            } else {
                plot::BLUE
            };
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(i as f64 + 0.15, 0.0), (i as f64 + 0.85, *f)],
                    colour.filled(),
                )))
                .map_err(Error::plot)?;
            chart
                .draw_series(std::iter::once(Text::new(
                    format!("{f:.2}"),
                    (i as f64 + 0.3, *f + top * 0.02),
                    (FAMILY, 12).into_font(),
                )))
                .map_err(Error::plot)?;
            chart
                .draw_series(std::iter::once(Text::new(
                    (*name).to_owned(),
                    (i as f64 + 0.1, -top * 0.045),
                    (FAMILY, 11).into_font(),
                )))
                .map_err(Error::plot)?;
        }
        Ok(())
    })
}

/// Pixels of the tilt chart.
pub const TILT_SIZE: (u32, u32) = (760, 420);

// ---------------------------------------------------------------- inspector

/// What one panel of the inspector shows.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct InspectorPanel {
    /// Place in the field: `top-left` to `bottom-right` and `centre`.
    pub name: String,
    /// Centre of the crop, pixels.
    pub x: usize,
    /// Centre of the crop, pixels.
    pub y: usize,
    /// Stars measured in the neighbourhood.
    pub stars: usize,
    /// Their median FWHM, pixels.
    pub fwhm: Option<f64>,
    /// Their median eccentricity.
    pub ellipticity: Option<f64>,
}

/// The mosaic and what it shows.
#[derive(Clone, Debug)]
pub struct Inspector {
    /// RGB bytes.
    pub rgb: Vec<u8>,
    /// Width, pixels.
    pub width: u32,
    /// Height, pixels.
    pub height: u32,
    /// The nine panels, row by row.
    pub panels: Vec<InspectorPanel>,
}

const NAMES: [&str; 9] = [
    "top-left",
    "top",
    "top-right",
    "left",
    "centre",
    "right",
    "bottom-left",
    "bottom",
    "bottom-right",
];

/// The 3 by 3 aberration inspector: crops of `crop` pixels (an even number) at
/// the corners, edges and centre, shown at `zoom` times and stretched alike
/// (an automatic stretch of the whole image, so panels compare), with the FWHM
/// and eccentricity of the stars around each.
///
/// # Errors
/// If the image is smaller than a crop, or drawing fails.
pub fn inspector(
    img: &Image,
    field: Option<&Field>,
    crop: usize,
    zoom: usize,
) -> Result<Inspector> {
    let (w, h) = (img.width(), img.height());
    let crop = crop.clamp(32, 512) & !1;
    let zoom = zoom.clamp(1, 6);
    if w < 2 * crop || h < 2 * crop {
        return Err(Error::Empty(format!(
            "the image ({w} x {h}) is too small for {crop} pixel crops"
        )));
    }
    let lum = luminance(&img.planes);
    let stretch =
        auto_mtf(&lum, 0.25, AutoLevels::default()).map_err(|e| Error::Plot(e.to_string()))?;
    let side = crop * zoom;
    let (gap, label) = (8usize, 24usize);
    let (cw, ch) = (3 * side + 4 * gap, 3 * (side + label) + 4 * gap);
    let mut buf = vec![255u8; cw * ch * 3];
    let mut panels = Vec::new();
    for (k, name) in NAMES.iter().enumerate() {
        let (col, row) = (k % 3, k / 3);
        let cx = [crop / 2, w / 2, w - crop / 2][col];
        let cy = [crop / 2, h / 2, h - crop / 2][row];
        let (x0, y0) = (cx - crop / 2, cy - crop / 2);
        let (ox, oy) = (gap + col * (side + gap), gap + row * (side + label + gap));
        for y in 0..side {
            for x in 0..side {
                let (sx, sy) = (x0 + x / zoom, y0 + y / zoom);
                let px = ((oy + y) * cw + ox + x) * 3;
                for c in 0..3 {
                    let plane = &img.planes[c.min(img.planes.len() - 1)];
                    let v = stretch.apply(plane.data[sy * w + sx]);
                    buf[px + c] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
        let near: Vec<&FieldStar> = field
            .map(|f| {
                f.stars
                    .iter()
                    .filter(|s| {
                        (s.x - cx as f64).abs() < crop as f64
                            && (s.y - cy as f64).abs() < crop as f64
                    })
                    .collect()
            })
            .unwrap_or_default();
        let z = zone_min(name, &near, 3);
        panels.push(InspectorPanel {
            name: (*name).to_owned(),
            x: cx,
            y: cy,
            stars: near.len(),
            fwhm: z.fwhm,
            ellipticity: z.ellipticity,
        });
    }
    let texts: Vec<(i32, i32, String)> = panels
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let (col, row) = (k % 3, k / 3);
            let ox = gap + col * (side + gap);
            let oy = gap + row * (side + label + gap) + side;
            let text = match (p.fwhm, p.ellipticity) {
                (Some(f), Some(e)) => format!("{}  FWHM {f:.2}  e {e:.2}  n {}", p.name, p.stars),
                _ => format!("{}  ({} stars)", p.name, p.stars),
            };
            (ox as i32 + 2, oy as i32 + 5, text)
        })
        .collect();
    let rgb = plot::render_rgb_over(cw as u32, ch as u32, buf, |area| {
        for (x, y, t) in &texts {
            area.draw(&Text::new(t.clone(), (*x, *y), (FAMILY, 13).into_font()))
                .map_err(Error::plot)?;
        }
        Ok(())
    })?;
    Ok(Inspector {
        rgb,
        width: cw as u32,
        height: ch as u32,
        panels,
    })
}

/// The inspector as a PNG.
///
/// # Errors
/// As for [`inspector`].
pub fn inspector_png(i: &Inspector) -> Result<Vec<u8>> {
    plot::encode_png(&i.rgb, i.width, i.height)
}

// ---------------------------------------------------------------- PSF

/// One radial bin of the stacked profile.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Bin {
    /// Radius, pixels (bin centre).
    pub r: f64,
    /// Median of the normalised pixel values.
    pub value: f64,
    /// Pixels in the bin.
    pub n: usize,
}

/// A fitted radial profile.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PsfFit {
    /// Stars stacked.
    pub stars: usize,
    /// Moffat `A (1 + (r/alpha)^2)^-beta`: amplitude.
    pub amplitude: f64,
    /// Moffat alpha, pixels.
    pub alpha: f64,
    /// Moffat beta.
    pub beta: f64,
    /// Moffat FWHM, `2 alpha sqrt(2^(1/beta) - 1)`, pixels.
    pub fwhm_moffat: f64,
    /// Gaussian `A exp(-r^2 / 2 sigma^2)`: sigma, pixels.
    pub sigma_gauss: f64,
    /// Gaussian FWHM, `2.3548 sigma`, pixels.
    pub fwhm_gauss: f64,
    /// RMS residual of the Moffat against the bins.
    pub rms_moffat: f64,
    /// RMS residual of the Gaussian against the bins.
    pub rms_gauss: f64,
    /// The binned profile.
    pub bins: Vec<Bin>,
}

/// Moffat FWHM from alpha and beta.
#[must_use]
pub fn moffat_fwhm(alpha: f64, beta: f64) -> f64 {
    2.0 * alpha * (2.0_f64.powf(1.0 / beta) - 1.0).sqrt()
}

/// The raw points `(r, value)` of the stacked stars and the fit.
#[derive(Clone, Debug)]
pub struct Psf {
    /// The fit.
    pub fit: PsfFit,
    /// Every pixel of every star, as `(r, normalised value)`.
    pub points: Vec<(f64, f64)>,
}

/// Stack the radial profile of up to `max_stars` of the brightest isolated
/// stars and fit a Moffat and a Gaussian to it. Each star's pixels are taken
/// relative to its sub-pixel centroid, the local sky is the median of an
/// annulus just outside the window, and the value is divided by the star's
/// peak above the sky.
///
/// # Errors
/// [`Error::Empty`] when fewer than 8 isolated stars are available, or the
/// fit fails.
pub fn psf(img: &Image, field: &Field, max_stars: usize) -> Result<Psf> {
    let plane = &img.planes[img.planes.len() / 2];
    let (w, h) = (plane.width as i64, plane.height as i64);
    let fw = field.median_fwhm().max(1.5);
    let radius = (3.0 * fw).ceil().clamp(6.0, 20.0);
    let ri = radius as i64;
    let isolated: Vec<&FieldStar> = field
        .stars
        .iter()
        .filter(|s| {
            field.stars.iter().all(|o| {
                std::ptr::eq(*s, o)
                    || (o.x - s.x).hypot(o.y - s.y) > radius + 4.0
                    || o.peak < 0.02 * s.peak
            })
        })
        .filter(|s| {
            s.x > (ri + 6) as f64
                && s.y > (ri + 6) as f64
                && s.x < (w - ri - 7) as f64
                && s.y < (h - ri - 7) as f64
        })
        .take(max_stars)
        .collect();
    if isolated.len() < 8 {
        return Err(Error::Empty(format!(
            "only {} isolated stars for a profile; need 8",
            isolated.len()
        )));
    }
    let mut points = Vec::new();
    for s in &isolated {
        let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
        let mut ring = Vec::new();
        for y in cy - ri - 5..=cy + ri + 5 {
            for x in cx - ri - 5..=cx + ri + 5 {
                let r = ((x as f64 - s.x).powi(2) + (y as f64 - s.y).powi(2)).sqrt();
                if r >= radius + 1.0 && r <= radius + 5.0 {
                    ring.push(f64::from(plane.data[(y * w + x) as usize]));
                }
            }
        }
        let sky = median_of(&ring);
        let amp = f64::from(plane.data[(cy * w + cx) as usize]) - sky;
        let norm = if amp > 0.0 { amp } else { s.peak };
        if !sky.is_finite() || norm <= 0.0 {
            continue;
        }
        for y in cy - ri..=cy + ri {
            for x in cx - ri..=cx + ri {
                let r = ((x as f64 - s.x).powi(2) + (y as f64 - s.y).powi(2)).sqrt();
                if r <= radius {
                    points.push((
                        r,
                        (f64::from(plane.data[(y * w + x) as usize]) - sky) / norm,
                    ));
                }
            }
        }
    }
    let width = 0.5;
    let nb = (radius / width) as usize;
    let mut cols: Vec<Vec<f64>> = vec![Vec::new(); nb];
    for &(r, v) in &points {
        let k = ((r / width) as usize).min(nb - 1);
        cols[k].push(v);
    }
    let bins: Vec<Bin> = cols
        .iter()
        .enumerate()
        .filter(|(_, c)| c.len() >= 5)
        .map(|(k, c)| Bin {
            r: (k as f64 + 0.5) * width,
            value: median_of(c),
            n: c.len(),
        })
        .collect();
    let pts: Vec<(f64, f64, f64)> = bins.iter().map(|b| (b.r, b.value, 1.0)).collect();
    let moffat = |p: &[f64], r: f64| p[0] * (1.0 + (r / p[1].abs().max(1e-3)).powi(2)).powf(-p[2]);
    let gauss = |p: &[f64], r: f64| p[0] * (-0.5 * (r / p[1].abs().max(1e-3)).powi(2)).exp();
    let fm = levmar(moffat, &[1.0, fw / 2.0, 3.0], &pts)
        .ok_or_else(|| Error::Empty("the Moffat fit failed".into()))?;
    let fg = levmar(gauss, &[1.0, fw / 2.3548], &pts)
        .ok_or_else(|| Error::Empty("the Gaussian fit failed".into()))?;
    let (amplitude, alpha, beta) = (fm.params[0], fm.params[1].abs(), fm.params[2]);
    let sigma = fg.params[1].abs();
    Ok(Psf {
        fit: PsfFit {
            stars: isolated.len(),
            amplitude,
            alpha,
            beta,
            fwhm_moffat: moffat_fwhm(alpha, beta),
            sigma_gauss: sigma,
            fwhm_gauss: 2.3548 * sigma,
            rms_moffat: fm.rms,
            rms_gauss: fg.rms,
            bins,
        },
        points,
    })
}

/// Pixels of the PSF sheet.
pub const PSF_SIZE: (u32, u32) = (1200, 620);

/// The profile plot, linear and logarithmic, as RGB bytes of [`PSF_SIZE`].
///
/// # Errors
/// If drawing fails.
pub fn psf_rgb(p: &Psf) -> Result<Vec<u8>> {
    let f = &p.fit;
    let rmax = f.bins.last().map_or(10.0, |b| b.r + 0.5);
    let moffat = |r: f64| f.amplitude * (1.0 + (r / f.alpha).powi(2)).powf(-f.beta);
    let gauss = |r: f64| f.amplitude * (-0.5 * (r / f.sigma_gauss).powi(2)).exp();
    plot::render_rgb(PSF_SIZE.0, PSF_SIZE.1, |root| {
        let (a, b) = root.split_horizontally(PSF_SIZE.0 / 2);
        for (area, log) in [(&a, false), (&b, true)] {
            let (ylo, yhi) = if log { (-3.0, 0.15) } else { (-0.15, 1.25) };
            let yf = |v: f64| if log { v.max(1e-3).log10() } else { v };
            let mut chart = ChartBuilder::on(area)
                .caption(
                    if log {
                        "Radial profile, logarithmic"
                    } else {
                        "Radial profile of the stacked stars"
                    },
                    (FAMILY, 15).into_font().style(FontStyle::Bold),
                )
                .margin(8)
                .x_label_area_size(30)
                .y_label_area_size(52)
                .build_cartesian_2d(0.0..rmax, ylo..yhi)
                .map_err(Error::plot)?;
            chart
                .configure_mesh()
                .light_line_style(WHITE)
                .x_label_formatter(&|v| plot::tick(*v))
                .y_label_formatter(&|v| plot::tick(*v))
                .x_desc("radius (pixels)")
                .y_desc(if log {
                    "log10 of the peak-normalised value"
                } else {
                    "value / star peak"
                })
                .label_style((FAMILY, 11))
                .axis_desc_style((FAMILY, 12))
                .draw()
                .map_err(Error::plot)?;
            let step = (p.points.len() / 6000).max(1);
            chart
                .draw_series(
                    p.points
                        .iter()
                        .step_by(step)
                        .filter(|(_, v)| !log || *v > 1e-3)
                        .map(|&(r, v)| {
                            Circle::new((r, yf(v)), 1, RGBColor(170, 190, 210).filled())
                        }),
                )
                .map_err(Error::plot)?;
            chart
                .draw_series(LineSeries::new(
                    (0..=200).map(|k| {
                        let r = rmax * f64::from(k) / 200.0;
                        (r, yf(gauss(r)))
                    }),
                    plot::ORANGE.stroke_width(2),
                ))
                .map_err(Error::plot)?
                .label(format!("Gaussian, FWHM {:.2} px", f.fwhm_gauss))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::ORANGE.stroke_width(2))
                });
            chart
                .draw_series(LineSeries::new(
                    (0..=200).map(|k| {
                        let r = rmax * f64::from(k) / 200.0;
                        (r, yf(moffat(r)))
                    }),
                    plot::GREEN.stroke_width(2),
                ))
                .map_err(Error::plot)?
                .label(format!(
                    "Moffat, FWHM {:.2} px, beta {}",
                    f.fwhm_moffat,
                    if f.beta > 50.0 {
                        "above 50".to_owned()
                    } else {
                        format!("{:.2}", f.beta)
                    }
                ))
                .legend(|(x, y)| {
                    PathElement::new([(x, y), (x + 18, y)], plot::GREEN.stroke_width(2))
                });
            chart
                .draw_series(
                    f.bins
                        .iter()
                        .filter(|bn| !log || bn.value > 1e-3)
                        .map(|bn| Circle::new((bn.r, yf(bn.value)), 4, plot::BLUE.filled())),
                )
                .map_err(Error::plot)?
                .label(format!("median of {} stars", f.stars))
                .legend(|(x, y)| Circle::new((x + 9, y), 4, plot::BLUE.filled()));
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.85))
                .border_style(GREY)
                .label_font((FAMILY, 12))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(Error::plot)?;
        }
        let _ = RED;
        Ok(())
    })
}

/// The bins and fit as CSV.
#[must_use]
pub fn psf_csv(p: &Psf) -> String {
    let f = &p.fit;
    let rows: Vec<Vec<String>> = f
        .bins
        .iter()
        .map(|b| {
            let m = f.amplitude * (1.0 + (b.r / f.alpha).powi(2)).powf(-f.beta);
            let g = f.amplitude * (-0.5 * (b.r / f.sigma_gauss).powi(2)).exp();
            vec![
                num(b.r, 2),
                b.n.to_string(),
                num(b.value, 5),
                num(m, 5),
                num(g, 5),
            ]
        })
        .collect();
    csv_text(&["r_px", "pixels", "median", "moffat", "gaussian"], &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::Header;
    use valerian_core::testutil::{Rng, gaussian_field};

    /// A star field whose FWHM grows to the right (tilt) with a known law.
    fn tilted_image(w: usize, h: usize, seed: u64) -> Image {
        let mut rng = Rng::new(seed);
        let stars: Vec<(f64, f64, f64, f64)> = (0..400)
            .map(|_| {
                let x = 20.0 + rng.uniform() * (w as f64 - 40.0);
                let y = 20.0 + rng.uniform() * (h as f64 - 40.0);
                let sigma = 1.3 + 1.2 * x / w as f64;
                (x, y, 4000.0 + 60000.0 * rng.uniform().powi(2), sigma)
            })
            .collect();
        let mut f = gaussian_field(w, h, 500.0, &stars);
        for v in &mut f.data {
            *v += rng.gauss(0.0, 4.0) as f32;
        }
        Image::mono(f, Header::new())
    }

    #[test]
    fn a_tilted_field_is_found_tilted_with_the_right_side_soft() {
        let img = tilted_image(1200, 800, 5);
        let field = measure_field(&img, 5.0, 10.0).expect("field");
        let t = tilt(&field);
        // The left corners are sharp and the right ones soft.
        let z = |n: &str| t.zones.iter().find(|z| z.name == n).and_then(|z| z.fwhm);
        let (tl, tr) = (z("top-left").expect("tl"), z("top-right").expect("tr"));
        assert!(tr > 1.5 * tl, "{tl} {tr}");
        assert!(t.tilt.expect("tilt") > 0.3, "{:?}", t.tilt);
        assert!(
            t.softest_corner
                .as_deref()
                .is_some_and(|n| n.ends_with("right"))
        );
        // The plane gradient across the width: sigma goes 1.3 to 2.5, FWHM 3.06 to 5.9.
        assert!(
            (t.plane_gradient.0 - 2.8).abs() < 0.5 && t.plane_gradient.1.abs() < 0.4,
            "{:?}",
            t.plane_gradient
        );
        assert!(t.hints.iter().any(|h| h.contains("tilt of the sensor")));
    }

    #[test]
    fn a_surface_of_the_fwhm_follows_the_gradient() {
        let img = tilted_image(1200, 800, 6);
        let field = measure_field(&img, 5.0, 10.0).expect("field");
        let s = surface_of(&field, Quantity::Fwhm, 2).expect("surface");
        let (left, right) = (s.eval(60.0, 400.0), s.eval(1140.0, 400.0));
        let want = |x: f64| 2.3548 * (1.3 + 1.2 * x / 1200.0);
        assert!((left - want(60.0)).abs() < 0.35, "{left} vs {}", want(60.0));
        assert!(
            (right - want(1140.0)).abs() < 0.35,
            "{right} vs {}",
            want(1140.0)
        );
    }

    #[test]
    fn the_maps_render_to_pngs() {
        let img = tilted_image(1200, 800, 7);
        let field = measure_field(&img, 5.0, 10.0).expect("field");
        for q in [Quantity::Fwhm, Quantity::Ellipticity] {
            let png = map_png(&field, q, 3).expect("map");
            assert_eq!(&png[1..4], b"PNG");
        }
        assert!(
            map_csv(&field, Quantity::Fwhm, 2)
                .expect("csv")
                .lines()
                .count()
                > 100
        );
        assert!(tilt_rgb(&tilt(&field)).is_ok());
    }

    #[test]
    fn the_inspector_has_nine_panels_with_local_fwhm() {
        let img = tilted_image(1200, 800, 8);
        let field = measure_field(&img, 5.0, 10.0).expect("field");
        let i = inspector(&img, Some(&field), 128, 2).expect("inspector");
        assert_eq!(i.panels.len(), 9);
        assert_eq!(i.rgb.len(), (i.width * i.height * 3) as usize);
        let f = |k: usize| i.panels[k].fwhm.expect("fwhm");
        assert!(f(5) > f(3), "right {} left {}", f(5), f(3));
        assert!(inspector(&img, None, 4000, 2).is_err());
    }

    #[test]
    fn the_moffat_fit_recovers_alpha_and_beta_of_moffat_stars() {
        // Moffat stars with alpha 2.2 and beta 2.8, rendered at pixel centres
        // plus a sub-pixel offset, on noise.
        let (w, h) = (900usize, 700usize);
        let (alpha, beta) = (2.2_f64, 2.8_f64);
        let mut rng = Rng::new(11);
        let mut data = vec![300.0_f32; w * h];
        let mut centres = Vec::new();
        while centres.len() < 60 {
            let (x, y) = (
                40.0 + rng.uniform() * (w as f64 - 80.0),
                40.0 + rng.uniform() * (h as f64 - 80.0),
            );
            if centres
                .iter()
                .all(|&(a, b): &(f64, f64)| (a - x).hypot(b - y) > 60.0)
            {
                centres.push((x, y));
            }
        }
        for &(cx, cy) in &centres {
            for y in (cy as i64 - 30)..(cy as i64 + 30) {
                for x in (cx as i64 - 30)..(cx as i64 + 30) {
                    let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                    data[y as usize * w + x as usize] +=
                        (20000.0 * (1.0 + r2 / (alpha * alpha)).powf(-beta)) as f32;
                }
            }
        }
        for v in &mut data {
            *v += rng.gauss(0.0, 5.0) as f32;
        }
        let img = Image::mono(
            valerian_core::Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            },
            Header::new(),
        );
        let field = measure_field(&img, 5.0, 10.0).expect("field");
        let p = psf(&img, &field, 50).expect("psf");
        let truth = moffat_fwhm(alpha, beta);
        assert!(
            (p.fit.fwhm_moffat - truth).abs() / truth < 0.05,
            "{} vs {truth}",
            p.fit.fwhm_moffat
        );
        assert!((p.fit.beta - beta).abs() < 0.5, "beta {}", p.fit.beta);
        assert!(p.fit.rms_moffat < p.fit.rms_gauss);
        assert_eq!(
            &psf_rgb(&p).expect("plot").len(),
            &((PSF_SIZE.0 * PSF_SIZE.1 * 3) as usize)
        );
        assert!(psf_csv(&p).lines().count() > 10);
    }

    #[test]
    fn a_flat_field_gets_no_tilt_hint() {
        let mut rng = Rng::new(3);
        let stars: Vec<(f64, f64, f64, f64)> = (0..500)
            .map(|_| {
                (
                    20.0 + rng.uniform() * 1160.0,
                    20.0 + rng.uniform() * 760.0,
                    4000.0 + 50000.0 * rng.uniform().powi(2),
                    1.8,
                )
            })
            .collect();
        let mut f = gaussian_field(1200, 800, 500.0, &stars);
        for v in &mut f.data {
            *v += rng.gauss(0.0, 4.0) as f32;
        }
        let field = measure_field(&Image::mono(f, Header::new()), 5.0, 10.0).expect("field");
        let t = tilt(&field);
        assert!(t.tilt.expect("tilt") < 0.1, "{:?}", t.tilt);
        assert!(t.curvature.expect("curvature").abs() < 0.1);
        assert!(t.hints.iter().any(|h| h.contains("flat")));
    }
}
