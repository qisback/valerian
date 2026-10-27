// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::annotate::{Residual, Residuals};
use valerian_core::testutil::Rng;

/// Stars on a 2000 x 1000 frame whose residuals follow a known linear field
/// plus noise.
fn field(scale_ppm: f64, rot: f64, shift: (f64, f64), noise: f64) -> Residuals {
    let mut rng = Rng::new(9);
    let (w, h) = (2000usize, 1000usize);
    let stars: Vec<Residual> = (0..600)
        .map(|_| {
            let x = rng.uniform() * w as f64;
            let y = rng.uniform() * h as f64;
            let (u, v) = (x - 1000.0, y - 500.0);
            Residual {
                x,
                y,
                ra: 0.0,
                dec: 0.0,
                g: 9.0,
                dx: shift.0 + scale_ppm * 1e-6 * u - rot * v + rng.gauss(0.0, noise),
                dy: shift.1 + scale_ppm * 1e-6 * v + rot * u + rng.gauss(0.0, noise),
            }
        })
        .collect();
    let n = stars.len() as f64;
    let rms = (stars.iter().map(|s| s.dx * s.dx + s.dy * s.dy).sum::<f64>() / n).sqrt();
    Residuals {
        width: w,
        height: h,
        scale_arcsec: 2.0,
        rms_px: rms,
        rms_arcsec: rms * 2.0,
        mean_dx: 0.0,
        mean_dy: 0.0,
        median_px: rms,
        stars,
    }
}

#[test]
fn the_linear_terms_are_recovered() {
    // 400 ppm scale, 2e-4 rad rotation, a (0.1, -0.05) px shift, 0.05 px noise.
    let r = field(400.0, 2e-4, (0.1, -0.05), 0.05);
    let m = residual_map(&r, 6).expect("map");
    let l = &m.linear;
    assert!((l.scale_ppm - 400.0).abs() < 25.0, "{l:?}");
    assert!(
        l.anisotropy_ppm.abs() < 25.0 && l.shear_ppm.abs() < 25.0,
        "{l:?}"
    );
    // 2e-4 rad over the 1118 px half-diagonal at 2 arcsec/px is 0.447 arcsec.
    assert!((l.rotation_arcsec - 0.447).abs() < 0.04, "{l:?}");
    assert!((l.a[0] - 0.1).abs() < 0.01 && (l.b[0] + 0.05).abs() < 0.01);
    assert!(
        l.rms_after_px < 0.08 && l.rms_after_px < l.rms_before_px,
        "{l:?}"
    );
}

#[test]
fn the_grid_counts_every_star_and_leaves_thin_cells_empty() {
    let r = field(0.0, 0.0, (0.0, 0.0), 0.05);
    let m = residual_map(&r, 6).expect("map");
    assert_eq!(m.cells.len(), 36);
    assert_eq!(m.cells.iter().map(|c| c.n).sum::<usize>(), 600);
    assert!(
        m.cells
            .iter()
            .filter(|c| c.n >= MIN_CELL)
            .all(|c| c.length() < 0.1)
    );
    let mut thin = r.clone();
    thin.stars.retain(|s| s.x < 700.0 || s.y < 300.0);
    let t = residual_map(&thin, 6).expect("map");
    assert!(t.cells.iter().any(|c| c.n < MIN_CELL && c.dx.is_nan()));
}

#[test]
fn a_scale_error_is_radial_and_a_rotation_is_tangential() {
    let r = field(600.0, 0.0, (0.0, 0.0), 0.02);
    let m = residual_map(&r, 6).expect("map");
    let last = m
        .radial_rings
        .iter()
        .rev()
        .find(|v| v.is_finite())
        .copied()
        .expect("ring");
    assert!(last > 0.25, "{last}");
    assert!(
        m.tangential_rings
            .iter()
            .filter(|v| v.is_finite())
            .all(|v| v.abs() < 0.05)
    );
    let r = field(0.0, 3e-4, (0.0, 0.0), 0.02);
    let m = residual_map(&r, 6).expect("map");
    let last = m
        .tangential_rings
        .iter()
        .rev()
        .find(|v| v.is_finite())
        .copied()
        .expect("ring");
    assert!(last > 0.25, "{last}");
    assert!(
        m.radial_rings
            .iter()
            .filter(|v| v.is_finite())
            .all(|v| v.abs() < 0.05)
    );
}

#[test]
fn stars_on_a_line_cannot_fit_the_linear_terms() {
    let mut r = field(0.0, 0.0, (0.0, 0.0), 0.05);
    for s in &mut r.stars {
        s.y = 500.0;
    }
    assert!(residual_map(&r, 6).is_err());
}

#[test]
fn the_outputs_render() {
    let r = field(300.0, 1e-4, (0.0, 0.0), 0.05);
    let m = residual_map(&r, 8).expect("map");
    let png = plot_png(&m, &r).expect("plot");
    assert_eq!(&png[1..4], b"PNG");
    assert_eq!(to_csv(&m).lines().count(), 65);
    assert!(to_json(&m).expect("json").contains("scale_ppm"));
}
