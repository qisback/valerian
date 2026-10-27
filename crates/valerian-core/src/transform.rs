// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Planar transforms between frames: similarity, affine and homography, and
//! least-squares fitting of each to point pairs.
//!
//! A [`Transform`] is a 3x3 matrix acting on `(x, y, 1)`. For registration it
//! maps coordinates in a *source* frame to coordinates in the *reference*
//! frame. Pixel `(0, 0)` is centred on the coordinate `(0, 0)`.
//!
//! # References
//!
//! - Umeyama, S. (1991), Least-squares estimation of transformation parameters between two point patterns, IEEE Trans. Pattern Anal. Mach. Intell. 13, 376-380, doi:10.1109/34.88573. (the similarity fit)
//! - Hartley, R. I. (1997), In defense of the eight-point algorithm, IEEE Trans. Pattern Anal. Mach. Intell. 19, 580-593, doi:10.1109/34.601246 (point normalisation for the homography fit).

use crate::error::{Error, Result};

/// A point `(x, y)`.
pub type Point = (f64, f64);

/// A projective transform of the plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Row-major 3x3 matrix.
    pub m: [[f64; 3]; 3],
}

/// Which family of transforms to fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Model {
    /// Rotation, uniform scale and translation (4 parameters).
    #[default]
    Similarity,
    /// Any affine map: adds shear and unequal scale (6 parameters).
    Affine,
    /// A full homography (8 parameters), for wide fields and perspective.
    Homography,
}

impl Model {
    /// The fewest point pairs that determine this model.
    #[must_use]
    pub fn min_pairs(self) -> usize {
        match self {
            Self::Similarity => 2,
            Self::Affine => 3,
            Self::Homography => 4,
        }
    }
}

impl Transform {
    /// The identity.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// Rotation by `angle` radians (towards +y), uniform `scale`, then
    /// translation by `(tx, ty)`.
    #[must_use]
    pub fn similarity(scale: f64, angle: f64, tx: f64, ty: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            m: [
                [scale * c, -scale * s, tx],
                [scale * s, scale * c, ty],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    /// A rotation by `angle` about `(cx, cy)` followed by a translation.
    #[must_use]
    pub fn rotation_about(cx: f64, cy: f64, angle: f64, tx: f64, ty: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            m: [
                [c, -s, cx - c * cx + s * cy + tx],
                [s, c, cy - s * cx - c * cy + ty],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    /// Apply to a point.
    #[must_use]
    pub fn apply(&self, p: Point) -> Point {
        let m = &self.m;
        let w = m[2][0] * p.0 + m[2][1] * p.1 + m[2][2];
        (
            (m[0][0] * p.0 + m[0][1] * p.1 + m[0][2]) / w,
            (m[1][0] * p.0 + m[1][1] * p.1 + m[1][2]) / w,
        )
    }

    /// The inverse transform, or `None` if singular.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.m;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if det.abs() < 1e-300 {
            return None;
        }
        let c = |r0: usize, c0: usize, r1: usize, c1: usize| -> f64 {
            m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
        };
        let inv = [
            [c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)],
            [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)],
            [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)],
        ];
        let mut out = [[0.0; 3]; 3];
        for (o, i) in out.iter_mut().zip(&inv) {
            for (a, b) in o.iter_mut().zip(i) {
                *a = b / det;
            }
        }
        Some(Self { m: out })
    }

    /// `self` applied after `first`: `a.after(&b).apply(p) == a.apply(b.apply(p))`.
    #[must_use]
    pub fn after(&self, first: &Self) -> Self {
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[i][k] * first.m[k][j]).sum();
            }
        }
        Self { m }
    }

    /// Scale, rotation (radians) and translation, valid when the transform
    /// is (close to) a similarity.
    #[must_use]
    pub fn decompose(&self) -> (f64, f64, Point) {
        let a = &self.m;
        let scale = (a[0][0] * a[1][1] - a[0][1] * a[1][0]).abs().sqrt();
        (scale, a[1][0].atan2(a[0][0]), (a[0][2], a[1][2]))
    }

    /// Whether the last row is `(0, 0, 1)`, so the map is affine.
    #[must_use]
    pub fn is_affine(&self) -> bool {
        self.m[2][0].abs() < 1e-12
            && self.m[2][1].abs() < 1e-12
            && (self.m[2][2] - 1.0).abs() < 1e-12
    }
}

/// Solve `a x = b` by Gaussian elimination with partial pivoting.
#[allow(
    clippy::needless_range_loop,
    reason = "row and column indices are both needed in the elimination"
)]
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            for k in col..n {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|j| a[i][j] * x[j]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Some(x)
}

fn centroid(p: &[Point]) -> Point {
    let n = p.len() as f64;
    (
        p.iter().map(|q| q.0).sum::<f64>() / n,
        p.iter().map(|q| q.1).sum::<f64>() / n,
    )
}

/// Least-squares similarity mapping each `pairs[i].0` to `pairs[i].1`.
///
/// # Errors
/// [`Error::Invalid`] with fewer than two pairs or coincident points.
pub fn fit_similarity(pairs: &[(Point, Point)]) -> Result<Transform> {
    if pairs.len() < 2 {
        return Err(Error::invalid(
            "a similarity needs at least two point pairs",
        ));
    }
    let src: Vec<Point> = pairs.iter().map(|p| p.0).collect();
    let dst: Vec<Point> = pairs.iter().map(|p| p.1).collect();
    let (cs, cd) = (centroid(&src), centroid(&dst));
    // Complex least squares: dst - cd = a * (src - cs), a = scale * e^{i theta}.
    let (mut num_re, mut num_im, mut den) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(&dst) {
        let (sx, sy) = (s.0 - cs.0, s.1 - cs.1);
        let (dx, dy) = (d.0 - cd.0, d.1 - cd.1);
        num_re += sx * dx + sy * dy;
        num_im += sx * dy - sy * dx;
        den += sx * sx + sy * sy;
    }
    if den < 1e-18 {
        return Err(Error::invalid("point pairs are coincident"));
    }
    let (a, b) = (num_re / den, num_im / den);
    Ok(Transform {
        m: [
            [a, -b, cd.0 - a * cs.0 + b * cs.1],
            [b, a, cd.1 - b * cs.0 - a * cs.1],
            [0.0, 0.0, 1.0],
        ],
    })
}

/// Least-squares affine map from `pairs[i].0` to `pairs[i].1`.
///
/// # Errors
/// [`Error::Invalid`] with fewer than three pairs or collinear points.
pub fn fit_affine(pairs: &[(Point, Point)]) -> Result<Transform> {
    if pairs.len() < 3 {
        return Err(Error::invalid(
            "an affine map needs at least three point pairs",
        ));
    }
    let src: Vec<Point> = pairs.iter().map(|p| p.0).collect();
    let cs = centroid(&src);
    let cd = centroid(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
    // Normal equations for x' = a x + b y + c on centred coordinates.
    let mut ata = vec![vec![0.0; 3]; 3];
    let (mut bx, mut by) = (vec![0.0; 3], vec![0.0; 3]);
    for (s, d) in pairs {
        let row = [s.0 - cs.0, s.1 - cs.1, 1.0];
        for i in 0..3 {
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
            bx[i] += row[i] * (d.0 - cd.0);
            by[i] += row[i] * (d.1 - cd.1);
        }
    }
    let px = solve(ata.clone(), bx).ok_or_else(|| Error::invalid("point pairs are collinear"))?;
    let py = solve(ata, by).ok_or_else(|| Error::invalid("point pairs are collinear"))?;
    // Undo the centring: x' = a (x - cs.x) + b (y - cs.y) + c + cd.x.
    Ok(Transform {
        m: [
            [px[0], px[1], px[2] + cd.0 - px[0] * cs.0 - px[1] * cs.1],
            [py[0], py[1], py[2] + cd.1 - py[0] * cs.0 - py[1] * cs.1],
            [0.0, 0.0, 1.0],
        ],
    })
}

/// Hartley normalisation: translate to the centroid and scale so the mean
/// distance is sqrt(2). Returns the points and the normalising transform.
fn normalise(p: &[Point]) -> (Vec<Point>, Transform) {
    let c = centroid(p);
    let mean = p.iter().map(|q| (q.0 - c.0).hypot(q.1 - c.1)).sum::<f64>() / p.len() as f64;
    let s = if mean > 0.0 {
        std::f64::consts::SQRT_2 / mean
    } else {
        1.0
    };
    let t = Transform {
        m: [[s, 0.0, -s * c.0], [0.0, s, -s * c.1], [0.0, 0.0, 1.0]],
    };
    (p.iter().map(|&q| t.apply(q)).collect(), t)
}

/// Least-squares homography (direct linear transform on normalised points,
/// with the bottom-right entry fixed) from `pairs[i].0` to `pairs[i].1`.
///
/// # Errors
/// [`Error::Invalid`] with fewer than four pairs or a degenerate layout.
pub fn fit_homography(pairs: &[(Point, Point)]) -> Result<Transform> {
    if pairs.len() < 4 {
        return Err(Error::invalid(
            "a homography needs at least four point pairs",
        ));
    }
    let (src, ts) = normalise(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
    let (dst, td) = normalise(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
    let mut ata = vec![vec![0.0; 8]; 8];
    let mut atb = vec![0.0; 8];
    for (s, d) in src.iter().zip(&dst) {
        let (x, y, u, v) = (s.0, s.1, d.0, d.1);
        let rows = [
            ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
            ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
        ];
        for (r, rhs) in rows {
            for i in 0..8 {
                for j in 0..8 {
                    ata[i][j] += r[i] * r[j];
                }
                atb[i] += r[i] * rhs;
            }
        }
    }
    let h = solve(ata, atb).ok_or_else(|| Error::invalid("point pairs are degenerate"))?;
    let hn = Transform {
        m: [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]],
    };
    let tdi = td
        .inverse()
        .ok_or_else(|| Error::invalid("degenerate destination points"))?;
    let full = tdi.after(&hn).after(&ts);
    let k = full.m[2][2];
    if k.abs() < 1e-300 {
        return Err(Error::invalid("degenerate homography"));
    }
    let mut m = full.m;
    for row in &mut m {
        for v in row {
            *v /= k;
        }
    }
    Ok(Transform { m })
}

/// Fit the chosen model.
///
/// # Errors
/// As for the individual fitting functions.
pub fn fit(model: Model, pairs: &[(Point, Point)]) -> Result<Transform> {
    match model {
        Model::Similarity => fit_similarity(pairs),
        Model::Affine => fit_affine(pairs),
        Model::Homography => fit_homography(pairs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn grid_points() -> Vec<Point> {
        let mut rng = Rng::new(2);
        (0..40)
            .map(|_| (rng.uniform() * 3000.0, rng.uniform() * 2000.0))
            .collect()
    }

    fn pairs_through(t: &Transform, noise: f64, seed: u64) -> Vec<(Point, Point)> {
        let mut rng = Rng::new(seed);
        grid_points()
            .into_iter()
            .map(|p| {
                let q = t.apply(p);
                (
                    p,
                    (q.0 + rng.gauss(0.0, noise), q.1 + rng.gauss(0.0, noise)),
                )
            })
            .collect()
    }

    fn max_err(a: &Transform, b: &Transform) -> f64 {
        grid_points()
            .into_iter()
            .map(|p| {
                let (u, v) = (a.apply(p), b.apply(p));
                (u.0 - v.0).hypot(u.1 - v.1)
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn inverse_and_composition() {
        let t = Transform::similarity(1.02, 0.31, 120.5, -33.25);
        let inv = t.inverse().expect("invertible");
        assert!(max_err(&inv.after(&t), &Transform::identity()) < 1e-9);
        let p = (12.0, 34.0);
        let q = t.apply(p);
        let r = inv.apply(q);
        assert!((r.0 - p.0).abs() < 1e-9 && (r.1 - p.1).abs() < 1e-9);
        let (s, a, (tx, ty)) = t.decompose();
        assert!((s - 1.02).abs() < 1e-12 && (a - 0.31).abs() < 1e-12);
        assert!((tx - 120.5).abs() < 1e-12 && (ty + 33.25).abs() < 1e-12);
    }

    #[test]
    fn rotation_about_a_point_keeps_that_point_fixed() {
        let t = Transform::rotation_about(100.0, 50.0, 0.4, 0.0, 0.0);
        let p = t.apply((100.0, 50.0));
        assert!((p.0 - 100.0).abs() < 1e-9 && (p.1 - 50.0).abs() < 1e-9);
    }

    #[test]
    fn exact_fits_recover_each_model() {
        let sim = Transform::similarity(0.5, 3.1, -40.0, 900.0);
        let aff = Transform {
            m: [[1.01, 0.02, 15.0], [-0.015, 0.99, -8.0], [0.0, 0.0, 1.0]],
        };
        let hom = Transform {
            m: [[1.0, 0.01, 12.0], [-0.02, 1.02, -5.0], [1e-5, -2e-5, 1.0]],
        };
        for (model, truth) in [
            (Model::Similarity, sim),
            (Model::Affine, aff),
            (Model::Homography, hom),
        ] {
            let got = fit(model, &pairs_through(&truth, 0.0, 1)).expect("fit");
            assert!(
                max_err(&got, &truth) < 1e-6,
                "{model:?}: {}",
                max_err(&got, &truth)
            );
        }
    }

    #[test]
    fn noisy_fits_average_the_noise_down() {
        let truth = Transform::similarity(1.0, 0.05, 20.0, -10.0);
        let got = fit_similarity(&pairs_through(&truth, 0.1, 5)).expect("fit");
        // 40 pairs at 0.1 px noise: the fitted map is good to a few hundredths.
        assert!(max_err(&got, &truth) < 0.1, "{}", max_err(&got, &truth));
    }

    #[test]
    fn degenerate_input_is_an_error() {
        assert!(fit_similarity(&[((0.0, 0.0), (1.0, 1.0))]).is_err());
        let same = [((1.0, 1.0), (2.0, 2.0)), ((1.0, 1.0), (3.0, 3.0))];
        assert!(fit_similarity(&same).is_err());
        let line: Vec<(Point, Point)> = (0..5)
            .map(|i| ((f64::from(i), f64::from(i)), (f64::from(i), 0.0)))
            .collect();
        assert!(fit_affine(&line).is_err());
        assert!(fit_homography(&line[..3]).is_err());
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let t = Transform {
            m: [[1.0, 2.0, 0.0], [2.0, 4.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(t.inverse().is_none());
    }
}
