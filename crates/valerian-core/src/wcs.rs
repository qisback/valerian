// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A world coordinate system for an image: gnomonic (TAN) projection with the
//! optional SIP distortion polynomials, as FITS headers carry them.
//!
//! Pixel coordinates here are 0-based with the centre of the first pixel at
//! `(0, 0)`, like the star positions in [`crate::stars`]; FITS counts from 1, so
//! `CRPIX` is stored as `x + 1`. The standard coordinates `(xi, eta)` are in
//! degrees, east and north of the tangent point.
//!
//! The distortion follows the SIP convention: with `u = x + 1 - CRPIX1` and
//! `v = y + 1 - CRPIX2`, the corrected offsets are `u + A(u, v)` and
//! `v + B(u, v)`, and `(xi, eta) = CD * (u', v')`. [`Wcs::fit`] finds all of it
//! from matched pixel and sky positions by linear least squares.
//!
//! # References
//!
//! - Calabretta, M. R. and Greisen, E. W. (2002), Representations of celestial coordinates in FITS, Astron. Astrophys. 395, 1077-1122, doi:10.1051/0004-6361:20021327 (the TAN projection).
//! - Greisen, E. W. and Calabretta, M. R. (2002), Representations of world coordinates in FITS, Astron. Astrophys. 395, 1061-1075, doi:10.1051/0004-6361:20021326.
//! - Shupe, D. L. et al. (2005), The SIP convention for representing distortion in FITS image headers, ASP Conf. Ser. 347, 491 (no DOI; to verify).
//! - Shupe, D. L. et al. (2012), More flexibility in representing geometric distortion in astronomical images, Proc. SPIE 8451, 84511M, doi:10.1117/12.925460.
//! - Pence, W. D. et al. (2010), Definition of the Flexible Image Transport System (FITS), version 3.0, Astron. Astrophys. 524, A42, doi:10.1051/0004-6361/201015362.

use crate::fits::{Header, Value};
use crate::{Error, Result};

/// SIP distortion: polynomial corrections to the pixel offsets.
#[derive(Clone, Debug, PartialEq)]
pub struct Sip {
    /// Polynomial order of `A` and `B` (2 or more).
    pub order: usize,
    /// `A` coefficients as `(p, q, value)` for the term `u^p v^q`, `p + q >= 2`.
    pub a: Vec<(usize, usize, f64)>,
    /// `B` coefficients.
    pub b: Vec<(usize, usize, f64)>,
    /// Inverse `AP` coefficients, `p + q <= order + 1`.
    pub ap: Vec<(usize, usize, f64)>,
    /// Inverse `BP` coefficients.
    pub bp: Vec<(usize, usize, f64)>,
}

/// A TAN (optionally TAN-SIP) world coordinate system.
#[derive(Clone, Debug, PartialEq)]
pub struct Wcs {
    /// Sky coordinates of the reference point, degrees.
    pub crval: (f64, f64),
    /// Reference pixel in FITS coordinates (1-based).
    pub crpix: (f64, f64),
    /// Degrees per pixel: `[[CD1_1, CD1_2], [CD2_1, CD2_2]]`.
    pub cd: [[f64; 2]; 2],
    /// Distortion, if any.
    pub sip: Option<Sip>,
}

fn poly(terms: &[(usize, usize, f64)], u: f64, v: f64) -> f64 {
    terms
        .iter()
        .map(|&(p, q, c)| c * u.powi(p as i32) * v.powi(q as i32))
        .sum()
}

/// Gnomonic projection of `(ra, dec)` about `(ra0, dec0)`: `(xi, eta)` in
/// degrees, or `None` on the far side of the sky.
#[must_use]
pub fn project(ra: f64, dec: f64, ra0: f64, dec0: f64) -> Option<(f64, f64)> {
    let (a, d, a0, d0) = (
        ra.to_radians(),
        dec.to_radians(),
        ra0.to_radians(),
        dec0.to_radians(),
    );
    let dr = a - a0;
    let c = d0.sin() * d.sin() + d0.cos() * d.cos() * dr.cos();
    if c <= 1e-9 {
        return None;
    }
    let xi = d.cos() * dr.sin() / c;
    let eta = (d0.cos() * d.sin() - d0.sin() * d.cos() * dr.cos()) / c;
    Some((xi.to_degrees(), eta.to_degrees()))
}

/// The inverse of [`project`].
#[must_use]
pub fn deproject(xi: f64, eta: f64, ra0: f64, dec0: f64) -> (f64, f64) {
    let (x, y) = (xi.to_radians(), eta.to_radians());
    let (a0, d0) = (ra0.to_radians(), dec0.to_radians());
    let rho = x.hypot(y);
    if rho < 1e-15 {
        return (ra0, dec0);
    }
    let c = rho.atan();
    let dec = (c.cos() * d0.sin() + y * c.sin() * d0.cos() / rho).asin();
    let ra = a0 + (x * c.sin()).atan2(rho * d0.cos() * c.cos() - y * d0.sin() * c.sin());
    (ra.to_degrees().rem_euclid(360.0), dec.to_degrees())
}

/// Angular distance between two sky positions, degrees.
#[must_use]
pub fn separation(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let (a, b, c, d) = (
        ra1.to_radians(),
        dec1.to_radians(),
        ra2.to_radians(),
        dec2.to_radians(),
    );
    // Haversine: stable for small angles.
    let h = ((d - b) / 2.0).sin().powi(2) + b.cos() * d.cos() * ((c - a) / 2.0).sin().powi(2);
    (2.0 * h.sqrt().min(1.0).asin()).to_degrees()
}

impl Wcs {
    /// A plain TAN system with a given scale and rotation.
    ///
    /// `scale` is arc seconds per pixel, `rotation` the angle in degrees of
    /// north from the image's +y axis towards +x (0 puts north up when `flip`
    /// is false and east is to the left), `flip` mirrors the image so that east
    /// is on the right.
    #[must_use]
    pub fn simple(
        crval: (f64, f64),
        crpix: (f64, f64),
        scale: f64,
        rotation: f64,
        flip: bool,
    ) -> Self {
        let s = scale / 3600.0;
        let (sn, cs) = rotation.to_radians().sin_cos();
        let e = if flip { 1.0 } else { -1.0 };
        Self {
            crval,
            crpix,
            cd: [[e * s * cs, s * sn], [e * s * -sn, s * cs]],
            sip: None,
        }
    }

    /// Pixel `(x, y)` (0-based) to sky `(ra, dec)` in degrees.
    #[must_use]
    pub fn pixel_to_world(&self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = (x + 1.0 - self.crpix.0, y + 1.0 - self.crpix.1);
        let (u, v) = match &self.sip {
            Some(s) => (u + poly(&s.a, u, v), v + poly(&s.b, u, v)),
            None => (u, v),
        };
        let xi = self.cd[0][0] * u + self.cd[0][1] * v;
        let eta = self.cd[1][0] * u + self.cd[1][1] * v;
        deproject(xi, eta, self.crval.0, self.crval.1)
    }

    /// Sky `(ra, dec)` to pixel `(x, y)` (0-based), or `None` on the far side.
    #[must_use]
    pub fn world_to_pixel(&self, ra: f64, dec: f64) -> Option<(f64, f64)> {
        let (xi, eta) = project(ra, dec, self.crval.0, self.crval.1)?;
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        let up = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let vp = (-self.cd[1][0] * xi + self.cd[0][0] * eta) / det;
        let (mut u, mut v) = (up, vp);
        if let Some(s) = &self.sip {
            // Fixed point on the forward polynomials: u' = u + A(u, v).
            for _ in 0..20 {
                let (nu, nv) = (up - poly(&s.a, u, v), vp - poly(&s.b, u, v));
                let moved = (nu - u).abs().max((nv - v).abs());
                u = nu;
                v = nv;
                if moved < 1e-10 {
                    break;
                }
            }
        }
        Some((u + self.crpix.0 - 1.0, v + self.crpix.1 - 1.0))
    }

    /// Pixel scale in arc seconds per pixel at the reference point.
    #[must_use]
    pub fn scale_arcsec(&self) -> f64 {
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        det.abs().sqrt() * 3600.0
    }

    /// True when east is to the right of north (the image is mirrored against
    /// the usual sky view of east to the left).
    #[must_use]
    pub fn mirrored(&self) -> bool {
        self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0] > 0.0
    }

    /// Position angle of the image's +y axis, degrees east of north (the usual
    /// "camera angle").
    #[must_use]
    pub fn rotation(&self) -> f64 {
        // The +y axis in standard coordinates is the second column of CD.
        let (xi, eta) = (self.cd[0][1], self.cd[1][1]);
        xi.atan2(eta).to_degrees()
    }

    /// Fit a TAN system to matched positions: `pixels[i]` (0-based) is the
    /// star seen at the sky position `sky[i]`. `centre` is the pixel to put the
    /// reference point on (0-based), `size` the image size, `sip_order` 0 for a
    /// plain TAN fit or 2 or more for distortion. Returns the system and the
    /// root-mean-square residual in pixels.
    ///
    /// # Errors
    /// [`Error::Invalid`] when there are too few pairs for the order asked, or
    /// the fit is degenerate.
    pub fn fit(
        pixels: &[(f64, f64)],
        sky: &[(f64, f64)],
        size: (usize, usize),
        sip_order: usize,
        start: &Self,
    ) -> Result<(Self, f64)> {
        let order = sip_order;
        if order == 1 || order > 5 {
            return Err(Error::invalid("the SIP order must be 0 or 2 to 5"));
        }
        let terms: Vec<(usize, usize)> = (0..=order.max(1))
            .flat_map(|t| (0..=t).map(move |p| (p, t - p)))
            .filter(|&(p, q)| p + q >= 1)
            .collect();
        let n_params = terms.len() + 1;
        if pixels.len() != sky.len() || pixels.len() < n_params + 3 {
            return Err(Error::invalid(format!(
                "{} matched stars are too few for a fit of order {order}",
                pixels.len()
            )));
        }
        let scale = (size.0.max(size.1) as f64 / 2.0).max(1.0);
        let crpix = (size.0 as f64 / 2.0 + 0.5, size.1 as f64 / 2.0 + 0.5);
        let mut crval = start.pixel_to_world(crpix.0 - 1.0, crpix.1 - 1.0);
        let mut coef = (Vec::new(), Vec::new());
        for _ in 0..4 {
            let mut rows = Vec::with_capacity(pixels.len());
            let (mut ys_xi, mut ys_eta) = (Vec::new(), Vec::new());
            for (&(x, y), &(ra, dec)) in pixels.iter().zip(sky) {
                let Some((xi, eta)) = project(ra, dec, crval.0, crval.1) else {
                    return Err(Error::invalid(
                        "a matched star is on the far side of the sky",
                    ));
                };
                let (u, v) = ((x + 1.0 - crpix.0) / scale, (y + 1.0 - crpix.1) / scale);
                let mut row = vec![1.0];
                row.extend(
                    terms
                        .iter()
                        .map(|&(p, q)| u.powi(p as i32) * v.powi(q as i32)),
                );
                rows.push(row);
                ys_xi.push(xi);
                ys_eta.push(eta);
            }
            let cx = lstsq(&rows, &ys_xi)?;
            let ce = lstsq(&rows, &ys_eta)?;
            // The constant term is a shift of the tangent point.
            let (c0x, c0e) = (cx[0], ce[0]);
            coef = (cx, ce);
            if c0x.abs().max(c0e.abs()) < 1e-10 {
                break;
            }
            crval = deproject(c0x, c0e, crval.0, crval.1);
        }
        let (cx, ce) = coef;
        // Linear part: terms (1,0) and (0,1) come first in `terms`.
        let idx = |p: usize, q: usize| 1 + terms.iter().position(|&t| t == (p, q)).unwrap_or(0);
        let cd = [
            [cx[idx(1, 0)] / scale, cx[idx(0, 1)] / scale],
            [ce[idx(1, 0)] / scale, ce[idx(0, 1)] / scale],
        ];
        let det = cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0];
        if det.abs() < 1e-30 {
            return Err(Error::invalid("the fit is degenerate"));
        }
        let inv = [
            [cd[1][1] / det, -cd[0][1] / det],
            [-cd[1][0] / det, cd[0][0] / det],
        ];
        let sip = if order >= 2 {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for &(p, q) in terms.iter().filter(|&&(p, q)| p + q >= 2) {
                let k = scale.powi((p + q) as i32);
                let (cxx, cee) = (cx[idx(p, q)] / k, ce[idx(p, q)] / k);
                a.push((p, q, inv[0][0] * cxx + inv[0][1] * cee));
                b.push((p, q, inv[1][0] * cxx + inv[1][1] * cee));
            }
            Some(Sip {
                order,
                a,
                b,
                ap: Vec::new(),
                bp: Vec::new(),
            })
        } else {
            None
        };
        let mut wcs = Self {
            crval,
            crpix,
            cd,
            sip,
        };
        if wcs.sip.is_some() {
            wcs.fit_inverse(size)?;
        }
        let mut sum = 0.0;
        for (&(x, y), &(ra, dec)) in pixels.iter().zip(sky) {
            let (px, py) = wcs
                .world_to_pixel(ra, dec)
                .ok_or_else(|| Error::invalid("a matched star is on the far side of the sky"))?;
            sum += (px - x).powi(2) + (py - y).powi(2);
        }
        Ok((wcs, (sum / pixels.len() as f64).sqrt()))
    }

    /// Fit the `AP` and `BP` polynomials that map corrected offsets back to
    /// pixel offsets, over a grid covering the image.
    fn fit_inverse(&mut self, size: (usize, usize)) -> Result<()> {
        let Some(sip) = self.sip.clone() else {
            return Ok(());
        };
        let order = sip.order + 1;
        let terms: Vec<(usize, usize)> = (0..=order)
            .flat_map(|t| (0..=t).map(move |p| (p, t - p)))
            .collect();
        let scale = (size.0.max(size.1) as f64 / 2.0).max(1.0);
        let n = 21_u32;
        let (mut rows, mut yu, mut yv) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..n {
            for j in 0..n {
                let u = (f64::from(i) / f64::from(n - 1) - 0.5) * size.0 as f64;
                let v = (f64::from(j) / f64::from(n - 1) - 0.5) * size.1 as f64;
                let (up, vp) = (u + poly(&sip.a, u, v), v + poly(&sip.b, u, v));
                let (us, vs) = (up / scale, vp / scale);
                rows.push(
                    terms
                        .iter()
                        .map(|&(p, q)| us.powi(p as i32) * vs.powi(q as i32))
                        .collect::<Vec<_>>(),
                );
                yu.push(u - up);
                yv.push(v - vp);
            }
        }
        let (cu, cv) = (lstsq(&rows, &yu)?, lstsq(&rows, &yv)?);
        let pack = |c: &[f64]| -> Vec<(usize, usize, f64)> {
            terms
                .iter()
                .zip(c)
                .map(|(&(p, q), &v)| (p, q, v / scale.powi((p + q) as i32)))
                .collect()
        };
        if let Some(s) = &mut self.sip {
            s.ap = pack(&cu);
            s.bp = pack(&cv);
        }
        Ok(())
    }

    /// Write the system into a header: `WCSAXES`, `CTYPE`, `CRVAL`, `CRPIX`,
    /// `CD` and, with distortion, the SIP keywords. Any older WCS cards are
    /// removed first.
    pub fn write(&self, h: &mut Header) {
        remove_wcs(h);
        let tan = if self.sip.is_some() { "TAN-SIP" } else { "TAN" };
        h.set("WCSAXES", Value::Int(2), "");
        h.set("CTYPE1", Value::Str(format!("RA---{tan}")), "");
        h.set("CTYPE2", Value::Str(format!("DEC--{tan}")), "");
        h.set("CUNIT1", Value::Str("deg".into()), "");
        h.set("CUNIT2", Value::Str("deg".into()), "");
        h.set(
            "CRVAL1",
            Value::Float(self.crval.0),
            "[deg] RA of the reference point",
        );
        h.set(
            "CRVAL2",
            Value::Float(self.crval.1),
            "[deg] Dec of the reference point",
        );
        h.set(
            "CRPIX1",
            Value::Float(self.crpix.0),
            "[px] reference pixel x (1-based)",
        );
        h.set(
            "CRPIX2",
            Value::Float(self.crpix.1),
            "[px] reference pixel y (1-based)",
        );
        h.set("CD1_1", Value::Float(self.cd[0][0]), "[deg/px]");
        h.set("CD1_2", Value::Float(self.cd[0][1]), "[deg/px]");
        h.set("CD2_1", Value::Float(self.cd[1][0]), "[deg/px]");
        h.set("CD2_2", Value::Float(self.cd[1][1]), "[deg/px]");
        h.set("RADESYS", Value::Str("ICRS".into()), "");
        h.set("EQUINOX", Value::Float(2000.0), "");
        if let Some(s) = &self.sip {
            h.set(
                "A_ORDER",
                Value::Int(s.order as i64),
                "SIP polynomial order",
            );
            h.set(
                "B_ORDER",
                Value::Int(s.order as i64),
                "SIP polynomial order",
            );
            for (name, terms) in [("A", &s.a), ("B", &s.b)] {
                for &(p, q, c) in terms {
                    h.set(&format!("{name}_{p}_{q}"), Value::Float(c), "");
                }
            }
            h.set(
                "AP_ORDER",
                Value::Int(s.order as i64 + 1),
                "SIP inverse order",
            );
            h.set(
                "BP_ORDER",
                Value::Int(s.order as i64 + 1),
                "SIP inverse order",
            );
            for (name, terms) in [("AP", &s.ap), ("BP", &s.bp)] {
                for &(p, q, c) in terms {
                    h.set(&format!("{name}_{p}_{q}"), Value::Float(c), "");
                }
            }
        }
    }

    /// Read a TAN or TAN-SIP system from a header, or `None` when it has none.
    #[must_use]
    pub fn from_header(h: &Header) -> Option<Self> {
        let ctype = h.get_str("CTYPE1")?;
        if !ctype.starts_with("RA---TAN") {
            return None;
        }
        let f = |k: &str| h.get_f64(k);
        let crval = (f("CRVAL1")?, f("CRVAL2")?);
        let crpix = (f("CRPIX1")?, f("CRPIX2")?);
        let cd = if let (Some(a), Some(b), Some(c), Some(d)) =
            (f("CD1_1"), f("CD1_2"), f("CD2_1"), f("CD2_2"))
        {
            [[a, b], [c, d]]
        } else {
            let (d1, d2) = (f("CDELT1")?, f("CDELT2")?);
            let (a, b, c, d) = (
                f("PC1_1").unwrap_or(1.0),
                f("PC1_2").unwrap_or(0.0),
                f("PC2_1").unwrap_or(0.0),
                f("PC2_2").unwrap_or(1.0),
            );
            [[d1 * a, d1 * b], [d2 * c, d2 * d]]
        };
        let sip = f("A_ORDER").map(|o| {
            let read = |name: &str, max: usize| -> Vec<(usize, usize, f64)> {
                let mut v = Vec::new();
                for p in 0..=max {
                    for q in 0..=max - p {
                        if let Some(c) = h.get_f64(&format!("{name}_{p}_{q}")) {
                            v.push((p, q, c));
                        }
                    }
                }
                v
            };
            let order = o as usize;
            let ap_order = f("AP_ORDER").map_or(order + 1, |x| x as usize);
            Sip {
                order,
                a: read("A", order),
                b: read("B", order),
                ap: read("AP", ap_order),
                bp: read("BP", ap_order),
            }
        });
        Some(Self {
            crval,
            crpix,
            cd,
            sip,
        })
    }
}

/// Remove every WCS card a solve writes (and the SIP ones) from a header.
pub fn remove_wcs(h: &mut Header) {
    const KEYS: &[&str] = &[
        "WCSAXES", "CTYPE1", "CTYPE2", "CUNIT1", "CUNIT2", "CRVAL1", "CRVAL2", "CRPIX1", "CRPIX2",
        "CD1_1", "CD1_2", "CD2_1", "CD2_2", "CDELT1", "CDELT2", "PC1_1", "PC1_2", "PC2_1", "PC2_2",
        "CROTA1", "CROTA2", "RADESYS", "EQUINOX", "A_ORDER", "B_ORDER", "AP_ORDER", "BP_ORDER",
        "LONPOLE", "LATPOLE",
    ];
    let sip: Vec<String> = h
        .cards()
        .iter()
        .map(|c| c.key.clone())
        .filter(|k| {
            ["A_", "B_", "AP_", "BP_"].iter().any(|p| {
                k.strip_prefix(p).is_some_and(|rest| {
                    rest.split('_')
                        .all(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                        && rest.contains('_')
                })
            })
        })
        .collect();
    for k in KEYS {
        h.remove(k);
    }
    for k in sip {
        h.remove(&k);
    }
}

/// Least squares by the normal equations, with the columns scaled to unit
/// length. The terms here are monomials of coordinates scaled to about one, so
/// the conditioning is adequate up to the orders used.
fn lstsq(rows: &[Vec<f64>], y: &[f64]) -> Result<Vec<f64>> {
    let m = rows[0].len();
    let norm: Vec<f64> = (0..m)
        .map(|j| {
            rows.iter()
                .map(|r| r[j] * r[j])
                .sum::<f64>()
                .sqrt()
                .max(1e-300)
        })
        .collect();
    let mut a = vec![vec![0.0f64; m + 1]; m];
    for (r, &yy) in rows.iter().zip(y) {
        for i in 0..m {
            let ri = r[i] / norm[i];
            for (aij, (rj, nj)) in a[i].iter_mut().zip(r.iter().zip(&norm)) {
                *aij += ri * rj / nj;
            }
            a[i][m] += ri * yy;
        }
    }
    for c in 0..m {
        let p = (c..m)
            .max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))
            .unwrap_or(c);
        if a[p][c].abs() < 1e-12 {
            return Err(Error::invalid("the stars are too clustered for this fit"));
        }
        a.swap(c, p);
        let pivot = a[c].clone();
        for row in &mut a[c + 1..] {
            let f = row[c] / pivot[c];
            for (x, pj) in row[c..].iter_mut().zip(&pivot[c..]) {
                *x -= f * pj;
            }
        }
    }
    let mut x = vec![0.0; m];
    for i in (0..m).rev() {
        let s: f64 = (i + 1..m).map(|j| a[i][j] * x[j]).sum();
        x[i] = (a[i][m] - s) / a[i][i];
    }
    Ok(x.iter().zip(&norm).map(|(v, n)| v / n).collect())
}

#[cfg(test)]
mod tests;
