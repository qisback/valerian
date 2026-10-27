// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small least-squares tools: a polynomial surface over the frame with
//! outlier rejection, and a Levenberg-Marquardt fit of a function of one
//! variable.

/// Solve `a x = b` for a small dense system by Gaussian elimination with
/// partial pivoting; `None` when singular.
#[must_use]
pub fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            for k in col..n {
                let v = a[col][k];
                a[row][k] -= f * v;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// A polynomial surface `z(x, y)` in coordinates scaled to -1..1 over a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    /// Total degree.
    pub order: usize,
    /// Frame width, pixels.
    pub width: f64,
    /// Frame height, pixels.
    pub height: f64,
    /// Coefficients, in the order of [`terms`].
    pub coeffs: Vec<f64>,
    /// Robust scatter of the points about the surface (1.4826 MAD).
    pub scatter: f64,
    /// Points that were kept after rejection.
    pub used: usize,
}

/// The exponents `(i, j)` of `u^i v^j` for a total degree, lowest first.
#[must_use]
pub fn terms(order: usize) -> Vec<(usize, usize)> {
    let mut t = Vec::new();
    for d in 0..=order {
        for j in 0..=d {
            t.push((d - j, j));
        }
    }
    t
}

impl Surface {
    fn scaled(&self, x: f64, y: f64) -> (f64, f64) {
        (2.0 * x / self.width - 1.0, 2.0 * y / self.height - 1.0)
    }

    /// The surface at pixel `(x, y)`.
    #[must_use]
    pub fn eval(&self, x: f64, y: f64) -> f64 {
        let (u, v) = self.scaled(x, y);
        terms(self.order)
            .iter()
            .zip(&self.coeffs)
            .map(|(&(i, j), c)| c * u.powi(i as i32) * v.powi(j as i32))
            .sum()
    }

    /// The plane of the first-order terms: `(gx, gy)` change across the full
    /// width and height (so the value at the right edge minus the left).
    #[must_use]
    pub fn first_order(&self) -> (f64, f64) {
        let mut g = (0.0, 0.0);
        for (&(i, j), c) in terms(self.order).iter().zip(&self.coeffs) {
            match (i, j) {
                (1, 0) => g.0 = 2.0 * c,
                (0, 1) => g.1 = 2.0 * c,
                _ => {}
            }
        }
        g
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        f64::midpoint(v[n / 2 - 1], v[n / 2])
    }
}

/// Median of a slice (NaN when empty); sorts a copy.
#[must_use]
pub fn median_of(v: &[f64]) -> f64 {
    median(&mut v.to_vec())
}

/// Fit a polynomial surface of total degree `order` to `(x, y, z)` points over
/// a `width` by `height` frame, rejecting points more than 3 robust sigmas off
/// and refitting (up to 6 times). `None` with too few points for the order.
#[must_use]
pub fn fit_surface(
    pts: &[(f64, f64, f64)],
    width: f64,
    height: f64,
    order: usize,
) -> Option<Surface> {
    let t = terms(order);
    if pts.len() < 2 * t.len() {
        return None;
    }
    let mut s = Surface {
        order,
        width,
        height,
        coeffs: vec![0.0; t.len()],
        scatter: f64::NAN,
        used: 0,
    };
    let mut keep: Vec<bool> = vec![true; pts.len()];
    for _ in 0..6 {
        let n = t.len();
        let mut a = vec![vec![0.0; n]; n];
        let mut b = vec![0.0; n];
        let mut used = 0;
        for (p, _) in pts.iter().zip(&keep).filter(|(_, k)| **k) {
            let (u, v) = s.scaled(p.0, p.1);
            let basis: Vec<f64> = t
                .iter()
                .map(|&(i, j)| u.powi(i as i32) * v.powi(j as i32))
                .collect();
            for r in 0..n {
                for c in 0..n {
                    a[r][c] += basis[r] * basis[c];
                }
                b[r] += basis[r] * p.2;
            }
            used += 1;
        }
        if used < 2 * n {
            break;
        }
        // A tiny ridge keeps a sparsely covered corner from running away.
        for (r, row) in a.iter_mut().enumerate() {
            row[r] += 1e-9 * used as f64;
        }
        s.coeffs = solve(a, b)?;
        s.used = used;
        let res: Vec<f64> = pts.iter().map(|p| p.2 - s.eval(p.0, p.1)).collect();
        let mut kept: Vec<f64> = res
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(r, _)| *r)
            .collect();
        let m = median(&mut kept.clone());
        let mut dev: Vec<f64> = kept.iter().map(|r| (r - m).abs()).collect();
        let sigma = 1.4826 * median(&mut dev);
        kept.clear();
        s.scatter = sigma;
        if sigma <= 0.0 {
            break;
        }
        let new: Vec<bool> = res.iter().map(|r| (r - m).abs() <= 3.0 * sigma).collect();
        if new == keep {
            break;
        }
        keep = new;
    }
    Some(s)
}

/// Result of [`levmar`].
#[derive(Clone, Debug)]
pub struct LmFit {
    /// The fitted parameters.
    pub params: Vec<f64>,
    /// Root-mean-square residual.
    pub rms: f64,
    /// Iterations used.
    pub iterations: usize,
}

/// Levenberg-Marquardt least squares of `f(params, x)` to `(x, y, weight)`
/// points, starting from `p0`; the Jacobian is numerical. `None` when the
/// normal equations are singular from the start.
pub fn levmar<F>(f: F, p0: &[f64], pts: &[(f64, f64, f64)]) -> Option<LmFit>
where
    F: Fn(&[f64], f64) -> f64,
{
    let n = p0.len();
    let chi = |p: &[f64]| -> f64 {
        pts.iter()
            .map(|&(x, y, w)| {
                let r = (f(p, x) - y) * w;
                r * r
            })
            .sum()
    };
    let mut p = p0.to_vec();
    let mut c = chi(&p);
    let mut lambda = 1e-3;
    let mut it = 0;
    while it < 200 {
        it += 1;
        let mut jac = vec![vec![0.0; n]; pts.len()];
        for k in 0..n {
            let h = 1e-6 * p[k].abs().max(1e-3);
            let mut q = p.clone();
            q[k] += h;
            for (row, &(x, _, w)) in jac.iter_mut().zip(pts) {
                row[k] = (f(&q, x) - f(&p, x)) / h * w;
            }
        }
        let mut a = vec![vec![0.0; n]; n];
        let mut g = vec![0.0; n];
        for (row, &(x, y, w)) in jac.iter().zip(pts) {
            let r = (y - f(&p, x)) * w;
            for i in 0..n {
                g[i] += row[i] * r;
                for j in 0..n {
                    a[i][j] += row[i] * row[j];
                }
            }
        }
        let mut improved = false;
        for _ in 0..12 {
            let mut m = a.clone();
            for (i, row) in m.iter_mut().enumerate() {
                row[i] += lambda * a[i][i].max(1e-12);
            }
            let Some(step) = solve(m, g.clone()) else {
                lambda *= 10.0;
                continue;
            };
            let q: Vec<f64> = p.iter().zip(&step).map(|(a, b)| a + b).collect();
            let cq = chi(&q);
            if cq.is_finite() && cq < c {
                let rel = (c - cq) / c.max(1e-300);
                p = q;
                c = cq;
                lambda = (lambda / 5.0).max(1e-12);
                improved = true;
                if rel < 1e-10 {
                    return Some(finish(p, c, pts, it));
                }
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    Some(finish(p, c, pts, it))
}

fn finish(params: Vec<f64>, chi: f64, pts: &[(f64, f64, f64)], iterations: usize) -> LmFit {
    LmFit {
        params,
        rms: (chi / pts.len().max(1) as f64).sqrt(),
        iterations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_plane_is_recovered_through_outliers() {
        let mut pts = Vec::new();
        for i in 0..20 {
            for j in 0..15 {
                let (x, y) = (f64::from(i) * 50.0, f64::from(j) * 50.0);
                pts.push((x, y, 3.0 + 0.002 * x - 0.001 * y));
            }
        }
        pts[7].2 += 40.0;
        pts[100].2 -= 25.0;
        let s = fit_surface(&pts, 1000.0, 750.0, 1).expect("fit");
        assert!(
            (s.eval(0.0, 0.0) - 3.0).abs() < 1e-6,
            "{}",
            s.eval(0.0, 0.0)
        );
        assert!((s.eval(1000.0, 750.0) - (3.0 + 2.0 - 0.75)).abs() < 1e-6);
        let (gx, gy) = s.first_order();
        assert!((gx - 2.0).abs() < 1e-6 && (gy + 0.75).abs() < 1e-6);
        assert!(s.used < pts.len());
    }

    #[test]
    fn a_quadratic_bowl_needs_order_two() {
        let pts: Vec<(f64, f64, f64)> = (0..400)
            .map(|k| {
                let (x, y) = (f64::from(k % 20) * 50.0, f64::from(k / 20) * 40.0);
                let (u, v) = (x / 500.0 - 1.0, y / 400.0 - 1.0);
                (x, y, 2.0 + 1.5 * (u * u + v * v))
            })
            .collect();
        let s = fit_surface(&pts, 1000.0, 800.0, 2).expect("fit");
        assert!((s.eval(500.0, 400.0) - 2.0).abs() < 1e-6);
        assert!((s.eval(0.0, 0.0) - 5.0).abs() < 1e-6);
        assert!(fit_surface(&pts[..5], 1000.0, 800.0, 2).is_none());
    }

    #[test]
    fn levmar_fits_a_moffat_from_a_poor_start() {
        let f = |p: &[f64], r: f64| p[0] * (1.0 + (r / p[1]).powi(2)).powf(-p[2]);
        let truth = [1.0, 2.0, 3.0];
        let pts: Vec<(f64, f64, f64)> = (0..40)
            .map(|k| {
                let r = f64::from(k) * 0.4;
                (r, f(&truth, r), 1.0)
            })
            .collect();
        let fit = levmar(f, &[0.8, 3.0, 1.5], &pts).expect("fit");
        for (a, b) in fit.params.iter().zip(truth) {
            assert!((a.abs() - b).abs() < 1e-3, "{:?}", fit.params);
        }
    }

    #[test]
    fn a_singular_system_is_none() {
        assert!(solve(vec![vec![1.0, 2.0], vec![2.0, 4.0]], vec![1.0, 2.0]).is_none());
    }
}
