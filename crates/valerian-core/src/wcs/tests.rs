// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::testutil::Rng;

fn truth(order: usize) -> Wcs {
    let mut w = Wcs::simple((83.82, -5.39), (1000.5, 750.5), 1.9, 37.0, false);
    if order >= 2 {
        // A mild distortion: a pixel or two at the corners of a 2000 by 1500 frame.
        w.sip = Some(Sip {
            order,
            a: vec![
                (2, 0, 2.0e-7),
                (1, 1, -1.5e-7),
                (0, 2, 1.0e-7),
                (3, 0, 1.0e-10),
            ],
            b: vec![
                (2, 0, -1.0e-7),
                (1, 1, 2.5e-7),
                (0, 2, -2.0e-7),
                (0, 3, -1.2e-10),
            ],
            ap: Vec::new(),
            bp: Vec::new(),
        });
    }
    w
}

type Pairs = (Vec<(f64, f64)>, Vec<(f64, f64)>);

fn points(w: &Wcs, n: usize, seed: u64) -> Pairs {
    let mut rng = Rng::new(seed);
    let px: Vec<(f64, f64)> = (0..n)
        .map(|_| (rng.uniform() * 2000.0, rng.uniform() * 1500.0))
        .collect();
    let sky = px.iter().map(|&(x, y)| w.pixel_to_world(x, y)).collect();
    (px, sky)
}

#[test]
fn projection_round_trips_and_keeps_the_axes() {
    for &(ra0, dec0) in &[(10.0, 20.0), (359.9, -45.0), (120.0, 89.0), (200.0, 0.0)] {
        for &(xi, eta) in &[(0.0, 0.0), (0.5, -0.3), (-1.2, 0.8)] {
            let (ra, dec) = deproject(xi, eta, ra0, dec0);
            let (x, y) = project(ra, dec, ra0, dec0).expect("near side");
            assert!((x - xi).abs() < 1e-10 && (y - eta).abs() < 1e-10);
        }
    }
    // Far side of the sky has no gnomonic image.
    assert!(project(190.0, -20.0, 10.0, 20.0).is_none());
    // North is +eta and east +xi.
    let (_, eta) = project(0.0, 1.0, 0.0, 0.0).expect("near side");
    let (xi, _) = project(1.0, 0.0, 0.0, 0.0).expect("near side");
    assert!(eta > 0.99 && xi > 0.99);
}

#[test]
fn a_simple_system_has_the_right_scale_orientation_and_centre() {
    let w = Wcs::simple((150.0, 30.0), (501.0, 401.0), 2.0, 0.0, false);
    let (ra, dec) = w.pixel_to_world(500.0, 400.0);
    assert!(separation(ra, dec, 150.0, 30.0) < 1e-9);
    assert!((w.scale_arcsec() - 2.0).abs() < 1e-9);
    // +y is north, +x is west, so right ascension falls along +x.
    let (_, d_up) = w.pixel_to_world(500.0, 500.0);
    let (r_right, _) = w.pixel_to_world(600.0, 400.0);
    assert!(d_up > dec && r_right < ra);
    assert!(!w.mirrored());
    assert!(w.rotation().abs() < 1e-9);
    let flipped = Wcs::simple((150.0, 30.0), (501.0, 401.0), 2.0, 25.0, true);
    assert!(flipped.mirrored() && (flipped.rotation() - 25.0).abs() < 1e-9);
}

#[test]
fn pixel_and_world_are_inverse_with_distortion() {
    let w = truth(3);
    for &(x, y) in &[
        (0.0, 0.0),
        (1999.0, 1499.0),
        (1000.0, 750.0),
        (30.0, 1400.0),
    ] {
        let (ra, dec) = w.pixel_to_world(x, y);
        let (px, py) = w.world_to_pixel(ra, dec).expect("near side");
        assert!(
            (px - x).abs() < 1e-6 && (py - y).abs() < 1e-6,
            "{x},{y} -> {px},{py}"
        );
    }
}

#[test]
fn a_header_round_trip_keeps_every_term() {
    let mut w = truth(3);
    let (px, sky) = points(&w, 120, 1);
    let (fitted, _) = Wcs::fit(&px, &sky, (2000, 1500), 3, &w).expect("fit");
    w = fitted;
    let mut h = Header::new();
    h.set("OBJECT", Value::Str("M42".into()), "");
    w.write(&mut h);
    assert_eq!(h.get_str("CTYPE1"), Some("RA---TAN-SIP"));
    assert!(h.get_f64("A_ORDER").is_some() && h.get_f64("AP_ORDER").is_some());
    let back = Wcs::from_header(&h).expect("read");
    for &(x, y) in &[(10.0, 10.0), (1900.0, 1400.0), (777.0, 321.0)] {
        let (a, b) = (w.pixel_to_world(x, y), back.pixel_to_world(x, y));
        assert!(separation(a.0, a.1, b.0, b.1) * 3600.0 < 1e-4);
    }
    remove_wcs(&mut h);
    assert!(h.get("CTYPE1").is_none() && h.get("A_1_1").is_none() && h.get("AP_0_0").is_none());
    assert_eq!(h.get_str("OBJECT"), Some("M42"));
}

#[test]
fn a_plain_tan_header_with_cdelt_and_pc_reads() {
    let mut h = Header::new();
    h.set("CTYPE1", Value::Str("RA---TAN".into()), "");
    h.set("CTYPE2", Value::Str("DEC--TAN".into()), "");
    for (k, v) in [
        ("CRVAL1", 10.0),
        ("CRVAL2", 20.0),
        ("CRPIX1", 50.0),
        ("CRPIX2", 60.0),
        ("CDELT1", -0.001),
        ("CDELT2", 0.001),
    ] {
        h.set(k, Value::Float(v), "");
    }
    let w = Wcs::from_header(&h).expect("read");
    assert!((w.scale_arcsec() - 3.6).abs() < 1e-9);
    assert!(Wcs::from_header(&Header::new()).is_none());
}

#[test]
fn the_fit_recovers_a_distorted_system_from_matched_stars() {
    let w = truth(3);
    let (px, sky) = points(&w, 150, 2);
    // Start from a poor guess: wrong centre by an arc minute, wrong scale by 3 %.
    let guess = Wcs::simple((83.83, -5.40), (1000.5, 750.5), 1.96, 36.5, false);
    let (fit, rms) = Wcs::fit(&px, &sky, (2000, 1500), 3, &guess).expect("fit");
    assert!(rms < 1e-3, "rms {rms} px");
    // Away from the stars used, the two systems still agree.
    for &(x, y) in &[(5.0, 5.0), (1990.0, 1490.0), (1000.0, 100.0)] {
        let (a, b) = (w.pixel_to_world(x, y), fit.pixel_to_world(x, y));
        assert!(separation(a.0, a.1, b.0, b.1) * 3600.0 < 0.01, "{x} {y}");
    }
    // The inverse polynomials undo the forward ones to well under a pixel.
    let s = fit.sip.as_ref().expect("sip");
    for &(u, v) in &[(-900.0, -700.0), (800.0, 600.0), (0.0, 0.0)] {
        let (up, vp) = (u + poly(&s.a, u, v), v + poly(&s.b, u, v));
        let (bu, bv) = (up + poly(&s.ap, up, vp), vp + poly(&s.bp, up, vp));
        assert!(
            (bu - u).abs() < 0.02 && (bv - v).abs() < 0.02,
            "{u} {v}: {bu} {bv}"
        );
    }
}

#[test]
fn order_zero_fits_a_plain_system_and_residual_shows_the_distortion() {
    let (px, sky) = points(&truth(0), 100, 3);
    let (_, rms) = Wcs::fit(&px, &sky, (2000, 1500), 0, &truth(0)).expect("fit");
    assert!(rms < 1e-6);
    let (px, sky) = points(&truth(3), 100, 4);
    let (_, plain) = Wcs::fit(&px, &sky, (2000, 1500), 0, &truth(0)).expect("fit");
    let (_, sip) = Wcs::fit(&px, &sky, (2000, 1500), 3, &truth(0)).expect("fit");
    assert!(
        plain > 0.05 && sip < 0.01 * plain,
        "plain {plain}, sip {sip}"
    );
}

#[test]
fn too_few_stars_and_bad_orders_are_refused() {
    let w = truth(0);
    let (px, sky) = points(&w, 8, 5);
    assert!(Wcs::fit(&px, &sky, (2000, 1500), 3, &w).is_err());
    let (px, sky) = points(&w, 50, 6);
    assert!(Wcs::fit(&px, &sky, (2000, 1500), 1, &w).is_err());
    assert!(Wcs::fit(&px, &sky, (2000, 1500), 9, &w).is_err());
    // Stars all on one line cannot fix a system.
    let line: Vec<(f64, f64)> = (0..30)
        .map(|i| (100.0 + 50.0 * f64::from(i), 700.0))
        .collect();
    let sky: Vec<(f64, f64)> = line.iter().map(|&(x, y)| w.pixel_to_world(x, y)).collect();
    assert!(Wcs::fit(&line, &sky, (2000, 1500), 0, &w).is_err());
}
