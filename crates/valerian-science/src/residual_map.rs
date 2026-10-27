// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The astrometric residual map: how the error of a plate solution varies
//! across the frame.
//!
//! [`crate::annotate::residuals`] matches detected stars to Gaia and gives one
//! vector per star. This module bins those vectors into a grid, so a
//! systematic pattern shows above the star-to-star scatter, and fits the
//! linear part of the pattern.
//!
//! # Method
//!
//! 1. **Grid.** The frame is divided into `n` by `n` equal cells; each cell
//!    holds the mean residual vector, its count and the rms of the vectors
//!    about the mean. A cell with fewer than 3 stars is left empty.
//! 2. **Linear terms.** With `u`, `v` the pixel offsets from the frame centre,
//!    `dx = a0 + a1 u + a2 v` and `dy = b0 + b1 u + b2 v` are fitted to the
//!    star residuals by least squares. A pure scale error of the solution is
//!    `a1 = b2`, a rotation is `b1 = -a2`, and unequal `a1`, `b2` or a
//!    symmetric `a2`, `b1` are an anisotropic scale or a shear. The fit gives
//!    the scale change in parts per million and the rotation in arc seconds
//!    across the frame, and the rms before and after, so the remainder is the
//!    distortion that a linear (TAN) solution cannot hold.
//! 3. **Radial and tangential.** Each residual is split along and across the
//!    direction from the centre. Optical distortion is radial; a rotation is
//!    tangential.
//!
//! The world coordinate system follows the FITS convention (Greisen and
//! Calabretta 2002; Calabretta and Greisen 2002).
//!
//! # References
//!
//! - Greisen, E. W. and Calabretta, M. R. (2002), Representations of world coordinates in FITS, Astron. Astrophys. 395, 1061-1075, doi:10.1051/0004-6361:20021326.
//! - Calabretta, M. R. and Greisen, E. W. (2002), Representations of celestial coordinates in FITS, Astron. Astrophys. 395, 1077-1122, doi:10.1051/0004-6361:20021327.
//! - Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940 (the positions the stars are compared with).

use plotters::prelude::*;
use plotters::style::FontStyle;
use serde::Serialize;

use crate::annotate::Residuals;
use crate::optics::colour_map;
use crate::plot;
use crate::table::{csv_text, num, signed};
use crate::{Error, Result};

const FAMILY: &str = "sans-serif";
/// The fewest stars a cell needs to be drawn.
pub const MIN_CELL: usize = 3;

/// One cell of the grid.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Cell {
    /// Column, from the left.
    pub ix: usize,
    /// Row, from the top.
    pub iy: usize,
    /// Centre of the cell, pixels.
    pub x: f64,
    /// Centre of the cell, pixels.
    pub y: f64,
    /// Stars in the cell.
    pub n: usize,
    /// Mean residual, pixels (NaN when the cell is empty).
    pub dx: f64,
    /// Mean residual, pixels.
    pub dy: f64,
    /// Rms of the star vectors about the mean, pixels.
    pub scatter_px: f64,
}

impl Cell {
    /// Length of the mean vector, pixels.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.dx.hypot(self.dy)
    }
}

/// The linear part of the residual field.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Linear {
    /// `dx = a0 + a1 u + a2 v`.
    pub a: [f64; 3],
    /// `dy = b0 + b1 u + b2 v`.
    pub b: [f64; 3],
    /// Scale change of the solution, parts per million (positive: stars lie farther out than the solution says).
    pub scale_ppm: f64,
    /// Rotation, arc seconds at the corner (positive: stars are turned from +x towards +y).
    pub rotation_arcsec: f64,
    /// Difference of the x and y scales, parts per million.
    pub anisotropy_ppm: f64,
    /// Shear, parts per million.
    pub shear_ppm: f64,
    /// Rms of the residuals before, pixels.
    pub rms_before_px: f64,
    /// Rms after the linear terms are removed, pixels.
    pub rms_after_px: f64,
}

/// The grid and the linear fit.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ResidualMap {
    /// Cells per side.
    pub grid: usize,
    /// Image width, pixels.
    pub width: usize,
    /// Image height, pixels.
    pub height: usize,
    /// Arc seconds per pixel.
    pub scale_arcsec: f64,
    /// The cells, row by row.
    pub cells: Vec<Cell>,
    /// The linear terms.
    pub linear: Linear,
    /// Stars behind the map.
    pub stars: usize,
    /// Mean radial residual in five rings from the centre out, pixels.
    pub radial_rings: Vec<f64>,
    /// Mean tangential residual in the same rings, pixels.
    pub tangential_rings: Vec<f64>,
}

/// Solve the 3x3 normal equations `m c = r` by Cramer's rule.
fn solve3(m: [[f64; 3]; 3], r: [f64; 3]) -> Option<[f64; 3]> {
    let det = |a: [[f64; 3]; 3]| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-9 * (m[0][0] * m[1][1] * m[2][2]).abs().max(1.0) {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut t = m;
        for row in 0..3 {
            t[row][k] = r[row];
        }
        *o = det(t) / d;
    }
    Some(out)
}

fn cell_of(x: f64, y: f64, w: f64, h: f64, grid: usize) -> (usize, usize) {
    (
        ((x / w * grid as f64) as usize).min(grid - 1),
        ((y / h * grid as f64) as usize).min(grid - 1),
    )
}

/// Bin the residuals into a `grid` by `grid` map and fit the linear terms.
///
/// # Errors
/// [`Error::Empty`] when the stars do not span the frame well enough to fit
/// the linear terms.
pub fn residual_map(r: &Residuals, grid: usize) -> Result<ResidualMap> {
    let grid = grid.clamp(2, 24);
    let (w, h) = (r.width as f64, r.height as f64);
    let mut acc = vec![(0usize, 0.0, 0.0); grid * grid];
    for s in &r.stars {
        let (ix, iy) = cell_of(s.x, s.y, w, h, grid);
        let a = &mut acc[iy * grid + ix];
        a.0 += 1;
        a.1 += s.dx;
        a.2 += s.dy;
    }
    let mut cells = Vec::with_capacity(grid * grid);
    for iy in 0..grid {
        for ix in 0..grid {
            let (n, sx, sy) = acc[iy * grid + ix];
            let ok = n >= MIN_CELL;
            let (dx, dy) = if ok {
                (sx / n as f64, sy / n as f64)
            } else {
                (f64::NAN, f64::NAN)
            };
            let scatter = if ok {
                let ss: f64 = r
                    .stars
                    .iter()
                    .filter(|s| cell_of(s.x, s.y, w, h, grid) == (ix, iy))
                    .map(|s| (s.dx - dx).powi(2) + (s.dy - dy).powi(2))
                    .sum();
                (ss / n as f64).sqrt()
            } else {
                f64::NAN
            };
            cells.push(Cell {
                ix,
                iy,
                x: (ix as f64 + 0.5) * w / grid as f64,
                y: (iy as f64 + 0.5) * h / grid as f64,
                n,
                dx,
                dy,
                scatter_px: scatter,
            });
        }
    }
    // Linear terms about the frame centre.
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (mut m, mut rx, mut ry) = ([[0.0; 3]; 3], [0.0; 3], [0.0; 3]);
    for s in &r.stars {
        let t = [1.0, s.x - cx, s.y - cy];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += t[i] * t[j];
            }
            rx[i] += t[i] * s.dx;
            ry[i] += t[i] * s.dy;
        }
    }
    let (a, b) = solve3(m, rx).zip(solve3(m, ry)).ok_or_else(|| {
        Error::Empty(
            "the matched stars lie on a line or a point; the linear terms cannot be fitted".into(),
        )
    })?;
    let n = r.stars.len() as f64;
    let after: f64 = r
        .stars
        .iter()
        .map(|s| {
            let (u, v) = (s.x - cx, s.y - cy);
            (s.dx - a[0] - a[1] * u - a[2] * v).powi(2)
                + (s.dy - b[0] - b[1] * u - b[2] * v).powi(2)
        })
        .sum();
    let half_diag = cx.hypot(cy);
    let linear = Linear {
        a,
        b,
        scale_ppm: f64::midpoint(a[1], b[2]) * 1e6,
        rotation_arcsec: 0.5 * (b[1] - a[2]) * half_diag * r.scale_arcsec,
        anisotropy_ppm: 0.5 * (a[1] - b[2]) * 1e6,
        shear_ppm: f64::midpoint(a[2], b[1]) * 1e6,
        rms_before_px: r.rms_px,
        rms_after_px: (after / n).sqrt(),
    };
    // Five rings of mean radial and tangential residual.
    let mut ring = [(0usize, 0.0, 0.0); 5];
    for s in &r.stars {
        let (u, v) = (s.x - cx, s.y - cy);
        let d = u.hypot(v);
        let k = ((d / half_diag * 5.0) as usize).min(4);
        let (ur, vr) = if d > 0.0 { (u / d, v / d) } else { (0.0, 0.0) };
        ring[k].0 += 1;
        ring[k].1 += s.dx * ur + s.dy * vr;
        ring[k].2 += -s.dx * vr + s.dy * ur;
    }
    let mean = |pick: fn(&(usize, f64, f64)) -> f64| -> Vec<f64> {
        ring.iter()
            .map(|q| {
                if q.0 >= MIN_CELL {
                    pick(q) / q.0 as f64
                } else {
                    f64::NAN
                }
            })
            .collect()
    };
    Ok(ResidualMap {
        grid,
        width: r.width,
        height: r.height,
        scale_arcsec: r.scale_arcsec,
        cells,
        linear,
        stars: r.stars.len(),
        radial_rings: mean(|q| q.1),
        tangential_rings: mean(|q| q.2),
    })
}

/// The cells as CSV.
#[must_use]
pub fn to_csv(m: &ResidualMap) -> String {
    let rows: Vec<Vec<String>> = m
        .cells
        .iter()
        .map(|c| {
            vec![
                c.ix.to_string(),
                c.iy.to_string(),
                num(c.x, 1),
                num(c.y, 1),
                c.n.to_string(),
                num(c.dx, 4),
                num(c.dy, 4),
                num(c.length(), 4),
                num(c.length() * m.scale_arcsec, 3),
                num(c.scatter_px, 4),
            ]
        })
        .collect();
    csv_text(
        &[
            "ix",
            "iy",
            "x",
            "y",
            "stars",
            "mean_dx_px",
            "mean_dy_px",
            "mean_length_px",
            "mean_length_arcsec",
            "scatter_px",
        ],
        &rows,
    )
}

/// The map and its linear terms as JSON (empty cells and rings are `null`).
///
/// # Errors
/// If serialisation fails.
pub fn to_json(m: &ResidualMap) -> Result<String> {
    serde_json::to_string_pretty(m).map_err(|e| Error::Parse(e.to_string()))
}

/// Pixels of the sheet.
pub const SIZE: (u32, u32) = (1500, 760);

/// The residual map sheet as RGB bytes of [`SIZE`]: the grid of mean vectors
/// (arrows exaggerated, colour by length), and beside it the radial and
/// tangential residual of every star against distance from the centre.
///
/// # Errors
/// If drawing fails.
pub fn plot_rgb(m: &ResidualMap, r: &Residuals) -> Result<Vec<u8>> {
    let mut lens: Vec<f64> = m
        .cells
        .iter()
        .filter(|c| c.n >= MIN_CELL)
        .map(Cell::length)
        .collect();
    lens.sort_by(f64::total_cmp);
    let top = lens.last().copied().unwrap_or(0.1).max(0.02);
    let (cw, ch) = (
        m.width as f64 / m.grid as f64,
        m.height as f64 / m.grid as f64,
    );
    let gain = (0.45 * cw.min(ch) / top).clamp(1.0, 2000.0);
    let (cx, cy) = (m.width as f64 / 2.0, m.height as f64 / 2.0);
    let half_diag = cx.hypot(cy);
    plot::render_rgb(SIZE.0, SIZE.1, |root| {
        let (left, right) = root.split_horizontally(SIZE.0 * 11 / 20);
        let bold = (FAMILY, 15).into_font().style(FontStyle::Bold);
        let mut chart = ChartBuilder::on(&left)
            .caption(
                format!(
                    "Mean residual per cell, arrows x{gain:.0}; longest {top:.3} px = {:.2} arcsec",
                    top * m.scale_arcsec
                ),
                bold.clone(),
            )
            .margin(10)
            .x_label_area_size(32)
            .y_label_area_size(56)
            .build_cartesian_2d(0.0..m.width as f64, m.height as f64..0.0)
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
        for c in &m.cells {
            let (x0, y0) = (c.ix as f64 * cw, c.iy as f64 * ch);
            let (x1, y1) = (x0 + cw, y0 + ch);
            if c.n < MIN_CELL {
                chart
                    .draw_series(std::iter::once(Rectangle::new(
                        [(x0, y0), (x1, y1)],
                        RGBColor(235, 235, 235).filled(),
                    )))
                    .map_err(Error::plot)?;
                continue;
            }
            let col = colour_map(c.length() / top);
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(x0, y0), (x1, y1)],
                    col.mix(0.18).filled(),
                )))
                .map_err(Error::plot)?;
            chart
                .draw_series(LineSeries::new(
                    [(c.x, c.y), (c.x + c.dx * gain, c.y + c.dy * gain)],
                    col.stroke_width(3),
                ))
                .map_err(Error::plot)?;
            chart
                .draw_series(std::iter::once(Circle::new((c.x, c.y), 3, col.filled())))
                .map_err(Error::plot)?;
            chart
                .draw_series(std::iter::once(Text::new(
                    c.n.to_string(),
                    (x0 + 4.0, y0 + 14.0),
                    (FAMILY, 10).into_font().color(&plot::GREY),
                )))
                .map_err(Error::plot)?;
        }
        // Right: radial and tangential against radius.
        let areas = right.split_evenly((2, 1));
        let l = &m.linear;
        for (k, area) in areas.iter().enumerate() {
            let (name, slope) = if k == 0 {
                (
                    "Radial residual (positive: outwards)",
                    f64::midpoint(l.a[1], l.b[2]),
                )
            } else {
                (
                    "Tangential residual (positive: clockwise on screen)",
                    0.5 * (l.b[1] - l.a[2]),
                )
            };
            let pts: Vec<(f64, f64)> = r
                .stars
                .iter()
                .map(|s| {
                    let (u, v) = (s.x - cx, s.y - cy);
                    let d = u.hypot(v).max(1e-9);
                    let val = if k == 0 {
                        (s.dx * u + s.dy * v) / d
                    } else {
                        (-s.dx * v + s.dy * u) / d
                    };
                    (d, val)
                })
                .collect();
            let (y0, y1) =
                plot::padded_range(pts.iter().map(|q| q.1).chain([slope * half_diag, 0.0]));
            let mut sc = ChartBuilder::on(area)
                .caption(name, bold.clone())
                .margin(10)
                .x_label_area_size(32)
                .y_label_area_size(56)
                .build_cartesian_2d(0.0..half_diag, y0..y1)
                .map_err(Error::plot)?;
            sc.configure_mesh()
                .light_line_style(WHITE)
                .bold_line_style(RGBColor(215, 215, 215))
                .x_desc("distance from the centre (pixels)")
                .y_desc("residual (pixels)")
                .x_label_formatter(&|v| plot::tick(*v))
                .y_label_formatter(&|v| plot::tick(*v))
                .label_style((FAMILY, 11))
                .axis_desc_style((FAMILY, 12))
                .draw()
                .map_err(Error::plot)?;
            sc.draw_series(
                pts.iter()
                    .map(|&q| Circle::new(q, 3, plot::BLUE.mix(0.5).filled())),
            )
            .map_err(Error::plot)?;
            sc.draw_series(LineSeries::new(
                [(0.0, 0.0), (half_diag, slope * half_diag)],
                plot::RED.stroke_width(2),
            ))
            .map_err(Error::plot)?
            .label(if k == 0 {
                format!("linear fit: scale {} ppm", signed(l.scale_ppm, 0))
            } else {
                format!(
                    "linear fit: rotation {} arcsec at the corner",
                    signed(l.rotation_arcsec, 2)
                )
            })
            .legend(|(x, y)| PathElement::new([(x, y), (x + 18, y)], plot::RED.stroke_width(2)));
            sc.configure_series_labels()
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

/// The residual map sheet as a PNG.
///
/// # Errors
/// If drawing fails.
pub fn plot_png(m: &ResidualMap, r: &Residuals) -> Result<Vec<u8>> {
    plot::encode_png(&plot_rgb(m, r)?, SIZE.0, SIZE.1)
}

#[cfg(test)]
mod tests;
