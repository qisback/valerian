// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use valerian_core::photocal;
use valerian_core::testutil::colour_scene;

/// The scene's stars have flux `3e7 * 10^(-0.4 G)` in green, so the green zero
/// point is `2.5 log10(3e7)` and the colour terms are the scene's response.
const TRUE_ZP: f64 = 18.693_941_5;

fn run() -> Photometry {
    let s = colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    let cal = photocal::calibrate(
        &s.image,
        &s.truth,
        &s.catalogue,
        &photocal::Options::default(),
    )
    .expect("calibrate");
    calibrate_field(&s.image, &s.truth, &cal, &Options::default()).expect("field")
}

#[test]
fn the_zero_points_and_colour_terms_are_recovered() {
    let p = run();
    // Aperture loss on a Gaussian of sigma 1.8 px with a 2 FWHM aperture is under 0.03 mag.
    assert!(
        (p.channels[1].zero - TRUE_ZP).abs() < 0.05,
        "{:?}",
        p.channels[1]
    );
    assert!(
        p.channels[1].colour_term.abs() < 0.03,
        "{:?}",
        p.channels[1]
    );
    assert!(
        (p.channels[0].zero - (TRUE_ZP - 0.35)).abs() < 0.05,
        "{:?}",
        p.channels[0]
    );
    assert!(
        (p.channels[0].colour_term - 0.55).abs() < 0.05,
        "{:?}",
        p.channels[0]
    );
    assert!(
        (p.channels[2].zero - (TRUE_ZP + 0.2)).abs() < 0.05,
        "{:?}",
        p.channels[2]
    );
    assert!(
        (p.channels[2].colour_term + 0.45).abs() < 0.05,
        "{:?}",
        p.channels[2]
    );
    assert!(p.channels[1].zero_error < 0.02 && p.channels[1].scatter < 0.05);
    assert!(p.channels.iter().all(|c| c.stars >= 30));
}

#[test]
fn the_catalogue_agrees_with_gaia_and_reaches_fainter() {
    let p = run();
    let matched: Vec<&FieldStar> = p.stars.iter().filter(|s| s.gaia_g.is_some()).collect();
    assert!(matched.len() >= 30, "{}", matched.len());
    let mut d: Vec<f64> = matched
        .iter()
        .map(|s| s.mag[1] - f64::from(s.gaia_g.expect("gaia")))
        .collect();
    let med = median(&mut d);
    assert!(med.abs() < 0.05, "median offset {med}");
    // Stars past the catalogue's G = 11 are measured too, with a growing error.
    let faint: Vec<&FieldStar> = p
        .stars
        .iter()
        .filter(|s| s.mag[1] > 11.5 && s.mag[1] < 13.0)
        .collect();
    assert!(faint.len() >= 20, "{}", faint.len());
    let bright_err = matched
        .iter()
        .map(|s| s.mag_error[1])
        .fold(f64::MAX, f64::min);
    let faint_err = faint.iter().map(|s| s.mag_error[1]).fold(0.0, f64::max);
    assert!(faint_err > bright_err);
    // Stars without a Gaia match get an estimated colour near the truth range.
    assert!(
        p.stars
            .iter()
            .any(|s| s.gaia_g.is_none() && !s.flags.contains('E'))
    );
    // Sorted brightest first, numbered from 1.
    assert_eq!(p.stars[0].id, 1);
    assert!(
        p.stars
            .windows(2)
            .all(|w| w[0].mag[1] <= w[1].mag[1] || w[1].mag[1].is_nan())
    );
}

#[test]
fn the_limit_follows_the_noise() {
    let p = run();
    // Aperture noise of about 5 ADU per pixel over a 25 px radius is ~ 5*sqrt(pi*8.4^2) ~ 75 ADU.
    let expect = p.channels[1].zero - 2.5 * (5.0 * p.limit.aperture_noise[1]).log10();
    assert!((p.limit.from_noise[1] - expect).abs() < 1e-9);
    assert!(
        p.limit.from_noise[1] > 11.0 && p.limit.from_noise[1] < 15.0,
        "{:?}",
        p.limit
    );
    // More signal-to-noise is a brighter limit.
    assert!(p.limit.green_at_3_and_10[0] > p.limit.from_noise[1]);
    assert!(p.limit.green_at_3_and_10[1] < p.limit.from_noise[1]);
    if let Some(f) = p.limit.faintest_measured {
        assert!(f < p.limit.from_noise[1] + 0.8, "{f}");
    }
}

#[test]
fn the_outputs_render_and_hold_every_star() {
    let p = run();
    assert_eq!(stars_csv(&p).lines().count(), p.stars.len() + 1);
    assert_eq!(zeropoint_csv(&p).lines().count(), p.calibrators.len() + 1);
    let json = to_json(&p).expect("json");
    assert!(json.contains("\"colour_term\"") && json.contains("\"from_noise\""));
    for png in [
        zeropoint_png(&p).expect("zp"),
        limit_png(&p).expect("limit"),
    ] {
        assert_eq!(&png[1..4], b"PNG");
        assert!(png.len() > 5000);
    }
}

#[test]
fn a_line_fit_has_the_textbook_standard_errors() {
    let x: Vec<f64> = (0..20).map(|i| f64::from(i) * 0.1).collect();
    let y: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, v)| 2.0 + 0.5 * v + if i % 2 == 0 { 0.01 } else { -0.01 })
        .collect();
    let idx: Vec<usize> = (0..20).collect();
    let f = ols(&x, &y, &idx);
    assert!((f.zero - 2.0).abs() < 0.01 && (f.slope - 0.5).abs() < 0.02);
    assert!(f.zero_error > 0.0 && f.slope_error > 0.0 && f.rms > 0.005);
}

#[test]
fn too_few_gaia_stars_is_an_error_that_says_so() {
    let s = colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    let mut cal = photocal::calibrate(
        &s.image,
        &s.truth,
        &s.catalogue,
        &photocal::Options::default(),
    )
    .expect("calibrate");
    for st in &mut cal.stars {
        st.used = false;
    }
    let e = calibrate_field(&s.image, &s.truth, &cal, &Options::default()).expect_err("error");
    assert!(e.to_string().contains("at least 8"), "{e}");
}
