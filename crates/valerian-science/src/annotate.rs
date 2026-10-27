// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plate-solve outputs: the annotated image (an RA/Dec grid, catalogue objects
//! and Gaia stars drawn on the picture) and the residuals of the solution.

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::Serialize;
use valerian_core::catalogue::Catalogue;
use valerian_core::fits::Image;
use valerian_core::preview::downsample;
use valerian_core::stars::{DetectOptions, detect, luminance};
use valerian_core::stretch::{AutoLevels, auto_mtf};
use valerian_core::wcs::Wcs;

use crate::objects::{self, Placed};
use crate::optics::colour_map;
use crate::plot::{self, Area};
use crate::table::{csv_text, num};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";

/// `hh mm ss` for a right ascension in degrees, with the seconds shown only
/// when the step needs them.
#[must_use]
pub fn ra_label(ra: f64, step_seconds: f64) -> String {
    let t = (ra.rem_euclid(360.0) / 15.0 * 3600.0 / step_seconds.max(1.0)).round()
        * step_seconds.max(1.0);
    let t = t.rem_euclid(86_400.0);
    let (h, m, s) = (
        (t / 3600.0).floor(),
        ((t % 3600.0) / 60.0).floor(),
        t % 60.0,
    );
    if step_seconds >= 3600.0 {
        format!("{h:.0}h")
    } else if step_seconds >= 60.0 {
        format!("{h:.0}h{m:02.0}m")
    } else {
        format!("{h:.0}h{m:02.0}m{s:02.0}s")
    }
}

/// `+dd mm ss` for a declination in degrees at a step in arc minutes.
#[must_use]
pub fn dec_label(dec: f64, step_arcmin: f64) -> String {
    let sign = if dec < 0.0 { '-' } else { '+' };
    let a = (dec.abs() * 60.0 / step_arcmin).round() * step_arcmin;
    let (d, m) = ((a / 60.0).floor(), a % 60.0);
    if step_arcmin >= 60.0 {
        format!("{sign}{d:.0}\u{b0}")
    } else if step_arcmin >= 1.0 {
        format!("{sign}{d:.0}\u{b0}{m:02.0}'")
    } else {
        format!(
            "{sign}{d:.0}\u{b0}{:02.0}'{:02.0}\"",
            m.floor(),
            (m % 1.0) * 60.0
        )
    }
}

/// Position as `hh:mm:ss.s +dd:mm:ss`.
#[must_use]
pub fn sexagesimal(ra: f64, dec: f64) -> String {
    let t = ra.rem_euclid(360.0) / 15.0;
    let (h, m) = (t.floor(), ((t % 1.0) * 60.0).floor());
    let s = ((t * 60.0) % 1.0) * 60.0;
    let sign = if dec < 0.0 { '-' } else { '+' };
    let a = dec.abs();
    let (d, dm) = (a.floor(), ((a % 1.0) * 60.0).floor());
    let ds = ((a * 60.0) % 1.0) * 60.0;
    format!("{h:02.0}:{m:02.0}:{s:04.1} {sign}{d:02.0}:{dm:02.0}:{ds:02.0}")
}

/// The first of `steps` that makes at most `lines` lines across `span`.
fn nice_step(span: f64, steps: &[f64], lines: f64) -> f64 {
    steps
        .iter()
        .copied()
        .find(|s| span / s <= lines)
        .unwrap_or(steps[steps.len() - 1])
}

const DEC_STEPS_ARCMIN: [f64; 16] = [
    0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1200.0,
    1800.0,
];
const RA_STEPS_SECONDS: [f64; 17] = [
    1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0,
    14_400.0, 21_600.0, 43_200.0,
];

/// What to draw.
#[derive(Clone, Copy, Debug)]
pub struct AnnotateOptions {
    /// Draw the RA/Dec grid.
    pub grid: bool,
    /// Draw the catalogue objects.
    pub objects: bool,
    /// Draw Gaia stars.
    pub stars: bool,
    /// Faintest Gaia star drawn (G).
    pub star_mag: f32,
    /// Gaia stars brighter than this (G) are labelled with their magnitude.
    pub label_mag: f32,
    /// Longest side of the picture, pixels.
    pub max_side: usize,
}

impl Default for AnnotateOptions {
    fn default() -> Self {
        Self {
            grid: true,
            objects: true,
            stars: true,
            star_mag: 10.0,
            label_mag: 8.0,
            max_side: 1400,
        }
    }
}

/// A Gaia star found in the field.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FieldStar {
    /// Right ascension, degrees.
    pub ra: f64,
    /// Declination, degrees.
    pub dec: f64,
    /// Gaia G magnitude.
    pub g: f32,
    /// Gaia BP-RP, when known.
    pub bp_rp: Option<f32>,
    /// Pixel position.
    pub x: f64,
    /// Pixel position.
    pub y: f64,
}

/// An annotated image and what was drawn on it.
#[derive(Clone, Debug)]
pub struct Annotated {
    /// Width of the picture, pixels.
    pub width: u32,
    /// Height of the picture, pixels.
    pub height: u32,
    /// RGB bytes.
    pub rgb: Vec<u8>,
    /// The objects drawn.
    pub objects: Vec<Placed>,
    /// The Gaia stars drawn.
    pub stars: Vec<FieldStar>,
    /// Step of the Dec lines, arc minutes (0 without a grid).
    pub dec_step_arcmin: f64,
    /// Step of the RA lines, seconds of time (0 without a grid).
    pub ra_step_seconds: f64,
}

/// Sky bounds of an image: `(ra_min, ra_max, dec_min, dec_max)` with the right
/// ascensions unwrapped about the centre's.
fn sky_bounds(wcs: &Wcs, size: (usize, usize)) -> (f64, f64, f64, f64) {
    let (w, h) = (size.0 as f64, size.1 as f64);
    let (ra0, _) = wcs.pixel_to_world(w / 2.0, h / 2.0);
    let (mut a0, mut a1, mut d0, mut d1) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for i in 0..=10 {
        for j in 0..=10 {
            let (ra, dec) = wcs.pixel_to_world(w * f64::from(i) / 10.0, h * f64::from(j) / 10.0);
            let mut d = ra - ra0;
            while d > 180.0 {
                d -= 360.0;
            }
            while d < -180.0 {
                d += 360.0;
            }
            a0 = a0.min(ra0 + d);
            a1 = a1.max(ra0 + d);
            d0 = d0.min(dec);
            d1 = d1.max(dec);
        }
    }
    (a0, a1, d0, d1)
}

type Pt = (i32, i32);

fn shadow_text(a: &Area<'_>, text: &str, p: Pt, size: u32, colour: RGBColor) -> Result<()> {
    let style = |c: RGBColor| (FAMILY, size).into_font().color(&c);
    a.draw(&Text::new(
        text.to_owned(),
        (p.0 + 1, p.1 + 1),
        style(BLACK),
    ))
    .map_err(Error::plot)?;
    a.draw(&Text::new(text.to_owned(), p, style(colour)))
        .map_err(Error::plot)
}

fn colour_of(kind: &str) -> RGBColor {
    match kind {
        "G" | "GPair" | "GTrpl" | "GGroup" => RGBColor(255, 170, 60),
        "OCl" | "*Ass" => RGBColor(255, 240, 120),
        "GCl" => RGBColor(255, 200, 120),
        "PN" => RGBColor(120, 255, 200),
        "SNR" => RGBColor(255, 120, 200),
        _ => RGBColor(110, 230, 110),
    }
}

/// The sky line at constant declination or right ascension, as polylines of
/// canvas points (broken where it leaves the picture).
fn line_points(
    wcs: &Wcs,
    size: (usize, usize),
    scale: f64,
    fixed: Fixed,
    range: (f64, f64),
) -> Vec<Vec<(f64, f64)>> {
    let (w, h) = (size.0 as f64, size.1 as f64);
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut cur = Vec::new();
    let n = 120;
    for k in 0..=n {
        let t = range.0 + (range.1 - range.0) * f64::from(k) / f64::from(n);
        let (ra, dec) = match fixed {
            Fixed::Dec(d) => (t, d),
            Fixed::Ra(r) => (r, t),
        };
        let inside = wcs
            .world_to_pixel(ra.rem_euclid(360.0), dec)
            .filter(|&(x, y)| x >= -0.5 && y >= -0.5 && x <= w - 0.5 && y <= h - 0.5);
        match inside {
            Some((x, y)) => cur.push((x / scale, y / scale)),
            None => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

#[derive(Clone, Copy)]
enum Fixed {
    Dec(f64),
    Ra(f64),
}

fn pt(p: (f64, f64)) -> Pt {
    (p.0.round() as i32, p.1.round() as i32)
}

fn ellipse(p: &Placed, wcs: &Wcs, scale: f64) -> Option<Vec<Pt>> {
    let o = &p.object;
    let major = o.major?;
    let minor = o.minor.unwrap_or(major);
    let pa = o.pos_angle.unwrap_or(0.0).to_radians();
    let half = major / 2.0 / 60.0;
    let cosd = o.dec.to_radians().cos().max(1e-3);
    let end = |sign: f64| {
        wcs.world_to_pixel(
            o.ra + sign * half * pa.sin() / cosd,
            o.dec + sign * half * pa.cos(),
        )
    };
    let (a, b) = (end(1.0)?, end(-1.0)?);
    let semi_major = ((a.0 - b.0).hypot(a.1 - b.1) / 2.0 / scale).max(5.0);
    let semi_minor = (semi_major * (minor / major)).max(4.0);
    let angle = (a.1 - b.1).atan2(a.0 - b.0);
    let (c, s) = (angle.cos(), angle.sin());
    let (cx, cy) = (p.x / scale, p.y / scale);
    Some(
        (0..=48)
            .map(|k| {
                let t = f64::from(k) / 48.0 * std::f64::consts::TAU;
                let (u, v) = (semi_major * t.cos(), semi_minor * t.sin());
                pt((cx + u * c - v * s, cy + u * s + v * c))
            })
            .collect(),
    )
}

/// Draw the plate solution on the image.
///
/// # Errors
/// If the image is empty or drawing fails.
pub fn annotate(
    img: &Image,
    wcs: &Wcs,
    cat: Option<&Catalogue>,
    opts: &AnnotateOptions,
) -> Result<Annotated> {
    let (w, h) = (img.width(), img.height());
    let small = downsample(img, opts.max_side);
    let scale = w as f64 / small.width() as f64;
    let (cw, ch) = (small.width(), small.height());
    let lum = luminance(&small.planes);
    let stretch =
        auto_mtf(&lum, 0.25, AutoLevels::default()).map_err(|e| Error::Empty(e.to_string()))?;
    let mut buf = vec![0u8; cw * ch * 3];
    for i in 0..cw * ch {
        for c in 0..3 {
            let plane = &small.planes[c.min(small.planes.len() - 1)];
            let v = stretch.apply(plane.data[i]);
            buf[i * 3 + c] = if v.is_finite() {
                (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            } else {
                0
            };
        }
    }
    let placed = if opts.objects {
        objects::in_field(wcs, (w, h), 0.0)
    } else {
        Vec::new()
    };
    let mut stars = Vec::new();
    if opts.stars
        && let Some(cat) = cat
    {
        let (ra0, dec0) = wcs.pixel_to_world(w as f64 / 2.0, h as f64 / 2.0);
        let (a, b) = (
            wcs.pixel_to_world(0.0, 0.0),
            wcs.pixel_to_world(w as f64, h as f64),
        );
        let radius = valerian_core::wcs::separation(a.0, a.1, b.0, b.1) / 2.0 * 1.05;
        for s in cat.cone(ra0, dec0, radius, opts.star_mag) {
            if let Some((x, y)) = wcs.world_to_pixel(s.ra, s.dec)
                && x >= 0.0
                && y >= 0.0
                && x < w as f64
                && y < h as f64
            {
                stars.push(FieldStar {
                    ra: s.ra,
                    dec: s.dec,
                    g: s.mag,
                    bp_rp: s.colour,
                    x,
                    y,
                });
            }
        }
    }
    let (ra_min, ra_max, dec_min, dec_max) = sky_bounds(wcs, (w, h));
    let (dec_step, ra_step) = if opts.grid {
        (
            nice_step((dec_max - dec_min) * 60.0, &DEC_STEPS_ARCMIN, 6.0),
            nice_step((ra_max - ra_min) * 240.0, &RA_STEPS_SECONDS, 6.0),
        )
    } else {
        (0.0, 0.0)
    };
    let (centre_ra, centre_dec) = wcs.pixel_to_world(w as f64 / 2.0, h as f64 / 2.0);
    let title = format!(
        "{}   {:.2} arcsec/px   rotation {:.1} deg{}",
        sexagesimal(centre_ra, centre_dec),
        wcs.scale_arcsec(),
        wcs.rotation(),
        if wcs.mirrored() { "   mirrored" } else { "" }
    );
    let rgb = plot::render_rgb_over(cw as u32, ch as u32, buf, |a| {
        if opts.grid {
            let grid = RGBColor(80, 200, 255);
            let mut d = (dec_min * 60.0 / dec_step).ceil() * dec_step / 60.0;
            while d <= dec_max {
                for line in line_points(wcs, (w, h), scale, Fixed::Dec(d), (ra_min, ra_max)) {
                    a.draw(&PathElement::new(
                        line.iter().map(|p| pt(*p)).collect::<Vec<_>>(),
                        grid.mix(0.55).stroke_width(1),
                    ))
                    .map_err(Error::plot)?;
                    if let Some(first) = line.iter().min_by(|p, q| p.0.total_cmp(&q.0)) {
                        shadow_text(
                            a,
                            &dec_label(d, dec_step),
                            (pt(*first).0 + 4, pt(*first).1 - 14),
                            13,
                            grid,
                        )?;
                    }
                }
                d += dec_step / 60.0;
            }
            let step_deg = ra_step / 240.0;
            let mut r = (ra_min / step_deg).ceil() * step_deg;
            while r <= ra_max {
                for line in line_points(wcs, (w, h), scale, Fixed::Ra(r), (dec_min, dec_max)) {
                    a.draw(&PathElement::new(
                        line.iter().map(|p| pt(*p)).collect::<Vec<_>>(),
                        grid.mix(0.55).stroke_width(1),
                    ))
                    .map_err(Error::plot)?;
                    if let Some(last) = line.iter().max_by(|p, q| p.1.total_cmp(&q.1)) {
                        shadow_text(
                            a,
                            &ra_label(r, ra_step),
                            (pt(*last).0 + 4, pt(*last).1 - 16),
                            13,
                            grid,
                        )?;
                    }
                }
                r += step_deg;
            }
        }
        for s in &stars {
            let radius = (7.0 - 0.45 * (f64::from(s.g) - 3.0)).clamp(3.0, 9.0) as i32;
            let c = pt((s.x / scale, s.y / scale));
            a.draw(&Circle::new(
                c,
                radius,
                RGBColor(255, 225, 90).mix(0.9).stroke_width(1),
            ))
            .map_err(Error::plot)?;
            if s.g < opts.label_mag {
                shadow_text(
                    a,
                    &format!("G {:.1}", s.g),
                    (c.0 + radius + 2, c.1 - 6),
                    11,
                    RGBColor(255, 225, 90),
                )?;
            }
        }
        for (k, p) in placed.iter().enumerate() {
            let col = colour_of(&p.object.kind);
            let c = pt((p.x / scale, p.y / scale));
            let outline = ellipse(p, wcs, scale).unwrap_or_else(|| {
                (0..=24)
                    .map(|i| {
                        let t = f64::from(i) / 24.0 * std::f64::consts::TAU;
                        (c.0 + (6.0 * t.cos()) as i32, c.1 + (6.0 * t.sin()) as i32)
                    })
                    .collect()
            });
            a.draw(&PathElement::new(outline.clone(), col.stroke_width(2)))
                .map_err(Error::plot)?;
            if k < 40 {
                let (lo, hi) = (
                    outline.iter().map(|q| q.0).min().unwrap_or(c.0),
                    outline.iter().map(|q| q.0).max().unwrap_or(c.0),
                );
                // A label to the right of the outline, or inside a big one that fills the picture.
                let at = if hi + 200 > cw as i32 {
                    (lo.max(8) + 12, c.1 - 7)
                } else {
                    (hi + 4, c.1 - 7)
                };
                shadow_text(a, &p.object.label(), at, 14, col)?;
            }
        }
        shadow_text(a, &title, (8, 8), 15, WHITE)?;
        Ok(())
    })?;
    Ok(Annotated {
        width: cw as u32,
        height: ch as u32,
        rgb,
        objects: placed,
        stars,
        dec_step_arcmin: dec_step,
        ra_step_seconds: ra_step,
    })
}

/// The annotated image as a PNG.
///
/// # Errors
/// If encoding fails.
pub fn annotated_png(a: &Annotated) -> Result<Vec<u8>> {
    plot::encode_png(&a.rgb, a.width, a.height)
}

/// The objects in the field as CSV.
#[must_use]
pub fn objects_csv(a: &Annotated) -> String {
    let rows: Vec<Vec<String>> = a
        .objects
        .iter()
        .map(|p| {
            let o = &p.object;
            vec![
                o.name.clone(),
                o.common.clone(),
                o.kind_word().to_owned(),
                num(o.ra, 5),
                num(o.dec, 5),
                num(p.x, 1),
                num(p.y, 1),
                o.major.map_or(String::new(), |v| num(v, 1)),
                o.mag.map_or(String::new(), |v| num(v, 1)),
            ]
        })
        .collect();
    csv_text(
        &[
            "name",
            "common_name",
            "kind",
            "ra_deg",
            "dec_deg",
            "x",
            "y",
            "major_arcmin",
            "mag",
        ],
        &rows,
    )
}

/// The Gaia stars in the field as CSV.
#[must_use]
pub fn stars_csv(a: &Annotated) -> String {
    let rows: Vec<Vec<String>> = a
        .stars
        .iter()
        .map(|s| {
            vec![
                num(s.ra, 6),
                num(s.dec, 6),
                num(f64::from(s.g), 2),
                s.bp_rp.map_or(String::new(), |c| num(f64::from(c), 3)),
                num(s.x, 2),
                num(s.y, 2),
            ]
        })
        .collect();
    csv_text(&["ra_deg", "dec_deg", "g", "bp_rp", "x", "y"], &rows)
}

// ---------------------------------------------------------------- residuals

/// One star matched to the catalogue.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Residual {
    /// Detected position, pixels.
    pub x: f64,
    /// Detected position, pixels.
    pub y: f64,
    /// Catalogue position, degrees.
    pub ra: f64,
    /// Catalogue position, degrees.
    pub dec: f64,
    /// Gaia G magnitude.
    pub g: f32,
    /// Detected minus where the solution puts the catalogue star, pixels.
    pub dx: f64,
    /// Detected minus where the solution puts the catalogue star, pixels.
    pub dy: f64,
}

impl Residual {
    /// Length of the vector, pixels.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.dx.hypot(self.dy)
    }
}

/// The residuals of a plate solution.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Residuals {
    /// Image width, pixels.
    pub width: usize,
    /// Image height, pixels.
    pub height: usize,
    /// Arc seconds per pixel.
    pub scale_arcsec: f64,
    /// Every matched star.
    pub stars: Vec<Residual>,
    /// Root-mean-square length, pixels.
    pub rms_px: f64,
    /// The same in arc seconds.
    pub rms_arcsec: f64,
    /// Mean of `dx`, pixels (a systematic shift).
    pub mean_dx: f64,
    /// Mean of `dy`, pixels.
    pub mean_dy: f64,
    /// Median length, pixels.
    pub median_px: f64,
}

/// Detect the stars of `img`, match them to the Gaia stars `wcs` predicts
/// within `radius_px` pixels, and measure the residuals.
///
/// # Errors
/// [`Error::Empty`] when fewer than 10 stars match.
pub fn residuals(
    img: &Image,
    wcs: &Wcs,
    cat: &Catalogue,
    mag_limit: f32,
    radius_px: f64,
) -> Result<Residuals> {
    let (w, h) = (img.width(), img.height());
    let lum = luminance(&img.planes);
    let det = detect(&lum, &DetectOptions::default());
    let (ra0, dec0) = wcs.pixel_to_world(w as f64 / 2.0, h as f64 / 2.0);
    let corner = wcs.pixel_to_world(0.0, 0.0);
    let radius = valerian_core::wcs::separation(corner.0, corner.1, ra0, dec0) * 1.05;
    let predicted: Vec<(f64, f64, f64, f64, f32)> = cat
        .cone(ra0, dec0, radius, mag_limit)
        .into_iter()
        .filter_map(|s| {
            let (x, y) = wcs.world_to_pixel(s.ra, s.dec)?;
            (x >= 0.0 && y >= 0.0 && x < w as f64 && y < h as f64)
                .then_some((x, y, s.ra, s.dec, s.mag))
        })
        .collect();
    let mut taken = vec![false; predicted.len()];
    let mut stars = Vec::new();
    // Brightest detections first, each to the nearest unclaimed catalogue star.
    for d in &det.stars {
        let best = predicted
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .map(|(i, p)| (i, (d.x - p.0).hypot(d.y - p.1)))
            .filter(|(_, r)| *r <= radius_px)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            taken[i] = true;
            let p = predicted[i];
            stars.push(Residual {
                x: d.x,
                y: d.y,
                ra: p.2,
                dec: p.3,
                g: p.4,
                dx: d.x - p.0,
                dy: d.y - p.1,
            });
        }
    }
    if stars.len() < 10 {
        return Err(Error::Empty(format!(
            "only {} stars match the catalogue within {radius_px} px; is the image solved?",
            stars.len()
        )));
    }
    let n = stars.len() as f64;
    let rms = (stars.iter().map(|s| s.dx * s.dx + s.dy * s.dy).sum::<f64>() / n).sqrt();
    let mut len: Vec<f64> = stars.iter().map(Residual::length).collect();
    len.sort_by(f64::total_cmp);
    let scale = wcs.scale_arcsec();
    Ok(Residuals {
        width: w,
        height: h,
        scale_arcsec: scale,
        rms_px: rms,
        rms_arcsec: rms * scale,
        mean_dx: stars.iter().map(|s| s.dx).sum::<f64>() / n,
        mean_dy: stars.iter().map(|s| s.dy).sum::<f64>() / n,
        median_px: len[len.len() / 2],
        stars,
    })
}

/// The residuals as CSV.
#[must_use]
pub fn residuals_csv(r: &Residuals) -> String {
    let rows: Vec<Vec<String>> = r
        .stars
        .iter()
        .map(|s| {
            vec![
                num(s.x, 3),
                num(s.y, 3),
                num(s.ra, 6),
                num(s.dec, 6),
                num(f64::from(s.g), 2),
                num(s.dx, 4),
                num(s.dy, 4),
                num(s.length(), 4),
                num(s.length() * r.scale_arcsec, 3),
            ]
        })
        .collect();
    csv_text(
        &[
            "x",
            "y",
            "ra_deg",
            "dec_deg",
            "g",
            "dx_px",
            "dy_px",
            "length_px",
            "length_arcsec",
        ],
        &rows,
    )
}

/// Height of the residual sheet, pixels.
pub const RESIDUAL_HEIGHT: u32 = 900;

/// The residual sheet: a vector field over the frame (arrows exaggerated so
/// the longest common arrow is about a twentieth of the short side, with a
/// scale bar) and the residual length against magnitude. Returns the size and
/// the RGB bytes.
///
/// # Errors
/// If drawing fails.
pub fn residuals_rgb(r: &Residuals) -> Result<(u32, u32, Vec<u8>)> {
    let ph = f64::from(RESIDUAL_HEIGHT);
    let aspect = r.width as f64 / r.height as f64;
    let left_w = (ph * aspect).clamp(520.0, 1000.0);
    let total_w = (left_w + 520.0) as u32;
    let mut len: Vec<f64> = r.stars.iter().map(Residual::length).collect();
    len.sort_by(f64::total_cmp);
    let p90 = len[len.len() * 9 / 10].max(0.02);
    let short = r.width.min(r.height) as f64;
    let gain = (short / 20.0 / p90).clamp(1.0, 500.0);
    let rgb = plot::render_rgb(total_w, RESIDUAL_HEIGHT, |root| {
        let (left, right) = root.split_horizontally(left_w as i32);
        let mut chart = ChartBuilder::on(&left)
            .caption(
                format!(
                    "WCS residuals (detected minus catalogue), arrows x{gain:.0}   rms {:.2} px = {:.2} arcsec",
                    r.rms_px, r.rms_arcsec
                ),
                (FAMILY, 15).into_font().style(FontStyle::Bold),
            )
            .margin(8)
            .x_label_area_size(30)
            .y_label_area_size(52)
            .build_cartesian_2d(0.0..r.width as f64, r.height as f64..0.0)
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
        for s in &r.stars {
            let c = colour_map((s.length() / (2.0 * p90)).min(1.0));
            let (ex, ey) = (s.x + s.dx * gain, s.y + s.dy * gain);
            chart
                .draw_series(LineSeries::new([(s.x, s.y), (ex, ey)], c.stroke_width(2)))
                .map_err(Error::plot)?;
            chart
                .draw_series(std::iter::once(Circle::new((s.x, s.y), 2, c.filled())))
                .map_err(Error::plot)?;
        }
        // The scale bar: half a pixel at the exaggeration.
        let bar = 0.5 * gain;
        let (bx, by) = (short * 0.05, r.height as f64 - short * 0.04);
        chart
            .draw_series(LineSeries::new(
                [(bx, by), (bx + bar, by)],
                BLACK.stroke_width(3),
            ))
            .map_err(Error::plot)?;
        chart
            .draw_series(std::iter::once(Text::new(
                "0.5 px".to_owned(),
                (bx, by - short * 0.015),
                (FAMILY, 12).into_font(),
            )))
            .map_err(Error::plot)?;
        let mut sc = ChartBuilder::on(&right)
            .caption(
                "Residual against magnitude",
                (FAMILY, 15).into_font().style(FontStyle::Bold),
            )
            .margin(8)
            .x_label_area_size(36)
            .y_label_area_size(56)
            .build_cartesian_2d(
                plot::padded_range(r.stars.iter().map(|s| f64::from(s.g))).0
                    ..plot::padded_range(r.stars.iter().map(|s| f64::from(s.g))).1,
                0.0..(len[len.len() - 1] * 1.1).max(0.1),
            )
            .map_err(Error::plot)?;
        sc.configure_mesh()
            .light_line_style(WHITE)
            .x_desc("Gaia G")
            .y_desc("residual (pixels)")
            .x_label_formatter(&|v| plot::tick(*v))
            .y_label_formatter(&|v| plot::tick(*v))
            .label_style((FAMILY, 11))
            .axis_desc_style((FAMILY, 12))
            .draw()
            .map_err(Error::plot)?;
        sc.draw_series(r.stars.iter().map(|s| {
            Circle::new(
                (f64::from(s.g), s.length()),
                3,
                plot::BLUE.mix(0.7).filled(),
            )
        }))
        .map_err(Error::plot)?;
        sc.draw_series(LineSeries::new(
            [
                (
                    f64::from(r.stars.iter().map(|s| s.g).fold(f32::INFINITY, f32::min)),
                    r.rms_px,
                ),
                (
                    f64::from(
                        r.stars
                            .iter()
                            .map(|s| s.g)
                            .fold(f32::NEG_INFINITY, f32::max),
                    ),
                    r.rms_px,
                ),
            ],
            plot::RED.stroke_width(1),
        ))
        .map_err(Error::plot)?;
        right
            .draw(&Text::new(
                format!(
                    "{} stars; mean shift ({:+.3}, {:+.3}) px; median {:.3} px",
                    r.stars.len(),
                    r.mean_dx,
                    r.mean_dy,
                    r.median_px
                ),
                (10, 30),
                (FAMILY, 12).into_font(),
            ))
            .map_err(Error::plot)?;
        Ok(())
    })?;
    Ok((total_w, RESIDUAL_HEIGHT, rgb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::Frame;
    use valerian_core::fits::Header;
    use valerian_core::testutil::{Rng, gaussian_field};

    #[test]
    fn labels_are_sexagesimal_at_the_step_asked() {
        assert_eq!(ra_label(10.6848, 60.0), "0h43m");
        assert_eq!(ra_label(10.6848, 10.0), "0h42m40s");
        assert_eq!(dec_label(41.2691, 5.0), "+41\u{b0}15'");
        assert_eq!(dec_label(-5.39, 60.0), "-5\u{b0}");
        assert_eq!(sexagesimal(83.8187, -5.3897), "05:35:16.5 -05:23:23");
    }

    #[test]
    fn steps_make_at_most_six_lines() {
        assert!((nice_step(100.0, &DEC_STEPS_ARCMIN, 6.0) - 20.0).abs() < 1e-9);
        assert!((nice_step(3.0, &DEC_STEPS_ARCMIN, 6.0) - 0.5).abs() < 1e-9);
    }

    /// A star field on a known WCS with a catalogue built from the same stars.
    fn field() -> (Image, Wcs, Catalogue, Vec<(f64, f64)>) {
        let wcs = Wcs::simple((150.0, 30.0), (600.0, 400.0), 6.0, 20.0, false);
        let mut rng = Rng::new(9);
        let (w, h) = (1200usize, 800usize);
        let mut sky = Vec::new();
        let mut stars = Vec::new();
        for _ in 0..160 {
            let (x, y) = (30.0 + rng.uniform() * 1140.0, 30.0 + rng.uniform() * 740.0);
            let (ra, dec) = wcs.pixel_to_world(x, y);
            let mag = 8.0 + 3.0 * rng.uniform();
            // A deliberate 0.4 px shift east-west so the residuals have a mean.
            stars.push((x + 0.4, y, 30000.0 * 10f64.powf(-0.4 * (mag - 8.0)), 1.6));
            sky.push(valerian_core::catalogue::CatStar {
                ra,
                dec,
                mag: mag as f32,
                colour: Some(0.8),
            });
        }
        let mut f: Frame = gaussian_field(w, h, 300.0, &stars);
        for v in &mut f.data {
            *v += rng.gauss(0.0, 3.0) as f32;
        }
        let cat = Catalogue::build(&sky, 12.0);
        let pos = stars.iter().map(|s| (s.0, s.1)).collect();
        (Image::mono(f, Header::new()), wcs, cat, pos)
    }

    #[test]
    fn residuals_find_the_deliberate_shift() {
        let (img, wcs, cat, _) = field();
        let r = residuals(&img, &wcs, &cat, 12.0, 3.0).expect("residuals");
        assert!(r.stars.len() > 60, "{}", r.stars.len());
        assert!(
            (r.mean_dx - 0.4).abs() < 0.08 && r.mean_dy.abs() < 0.08,
            "{} {}",
            r.mean_dx,
            r.mean_dy
        );
        assert!((r.rms_arcsec - r.rms_px * 6.0).abs() < 1e-9);
        let (w, h, rgb) = residuals_rgb(&r).expect("plot");
        assert_eq!(rgb.len(), (w * h * 3) as usize);
        assert!(residuals_csv(&r).lines().count() > 60);
    }

    #[test]
    fn an_unsolved_image_has_too_few_matches() {
        let (img, _, cat, _) = field();
        let wrong = Wcs::simple((151.0, 31.0), (600.0, 400.0), 6.0, 0.0, false);
        assert!(residuals(&img, &wrong, &cat, 12.0, 3.0).is_err());
    }

    #[test]
    fn the_annotated_picture_has_a_grid_stars_and_objects() {
        let (img, wcs, cat, _) = field();
        let a = annotate(&img, &wcs, Some(&cat), &AnnotateOptions::default()).expect("annotate");
        assert_eq!(a.rgb.len(), (a.width * a.height * 3) as usize);
        assert!(a.dec_step_arcmin > 0.0 && a.ra_step_seconds > 0.0);
        assert!(a.stars.len() > 60);
        assert!(stars_csv(&a).lines().count() > 60);
        assert_eq!(&annotated_png(&a).expect("png")[1..4], b"PNG");
        // Without the optional layers nothing but the picture is drawn.
        let bare = annotate(
            &img,
            &wcs,
            None,
            &AnnotateOptions {
                grid: false,
                objects: false,
                stars: false,
                ..AnnotateOptions::default()
            },
        )
        .expect("bare");
        assert!(bare.stars.is_empty() && bare.objects.is_empty() && bare.dec_step_arcmin == 0.0);
    }
}
