// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::fits::{Header, Value};
use crate::testutil::{Rng, colour_scene};

fn scene() -> crate::testutil::ColourScene {
    colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5)
}

#[test]
fn the_balance_gains_are_recovered_from_the_star_colours() {
    let s = scene();
    let cal = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    assert!(cal.used >= 40, "{} stars used", cal.used);
    for c in [0, 2] {
        let rel = cal.gains[c] / s.true_gains[c] - 1.0;
        assert!(
            rel.abs() < 0.03,
            "channel {c}: gain {:.4}, true {:.4}",
            cal.gains[c],
            s.true_gains[c]
        );
    }
    assert!((cal.gains[1] - 1.0).abs() < f64::EPSILON);
    // The slopes are the response the scene was made with.
    assert!((cal.lines[0].slope + 0.55).abs() < 0.1, "{:?}", cal.lines);
    assert!((cal.lines[1].slope - 0.45).abs() < 0.1, "{:?}", cal.lines);
    assert!(cal.warnings.is_empty(), "{:?}", cal.warnings);
}

#[test]
fn the_background_is_made_neutral_without_moving_its_mean() {
    let s = scene();
    let cal = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    let out = cal.apply(&s.image).expect("apply");
    let after: Vec<f32> = out
        .planes
        .iter()
        .map(|p| stats::sky_and_noise(p).0)
        .collect();
    assert!(
        (after[0] - after[1]).abs() < 1.0 && (after[2] - after[1]).abs() < 1.0,
        "{after:?}"
    );
    let before_mean = (0..3)
        .map(|c| cal.sky_before[c] * cal.gains[c] as f32)
        .sum::<f32>()
        / 3.0;
    let after_mean = after.iter().sum::<f32>() / 3.0;
    assert!(
        (before_mean - after_mean).abs() < 1.0,
        "{before_mean} {after_mean}"
    );
    // Without neutralisation the offsets are zero and the skies stay apart.
    let off = calibrate(
        &s.image,
        &s.truth,
        &s.catalogue,
        &Options {
            neutralise_background: false,
            ..Options::default()
        },
    )
    .expect("calibrate");
    assert_eq!(off.offsets, [0.0; 3]);
}

#[test]
fn a_calibrated_image_calibrates_to_unit_gains() {
    let s = scene();
    let cal = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    let out = cal.apply(&s.image).expect("apply");
    let again = calibrate(&out, &s.truth, &s.catalogue, &Options::default()).expect("again");
    assert!((again.gains[0] - 1.0).abs() < 0.02, "{:?}", again.gains);
    assert!((again.gains[2] - 1.0).abs() < 0.02, "{:?}", again.gains);
}

#[test]
fn the_reference_colour_moves_the_balance() {
    let s = scene();
    let blue = calibrate(
        &s.image,
        &s.truth,
        &s.catalogue,
        &Options {
            reference_colour: 0.3,
            ..Options::default()
        },
    )
    .expect("calibrate");
    let sun = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    // A bluer neutral point needs less blue gain... and more red.
    assert!(blue.gains[0] > sun.gains[0] && blue.gains[2] < sun.gains[2]);
}

#[test]
fn planes_in_the_wrong_order_are_warned_about() {
    let mut s = scene();
    s.image.planes.swap(0, 2);
    let cal = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    assert_eq!(cal.warnings.len(), 2, "{:?}", cal.warnings);
    assert!(cal.warnings[0].contains("red, green, blue in that order"));
}

#[test]
fn unsuitable_images_are_refused() {
    let s = scene();
    let mono = Image::mono(s.image.planes[0].clone(), Header::new());
    let e = calibrate(&mono, &s.truth, &s.catalogue, &Options::default()).expect_err("mono");
    assert!(e.to_string().contains("1 plane"), "{e}");
    let mut stretched = s.image.clone();
    stretched.header.set("STRBLACK", Value::Float(0.0), "");
    stretched.header.set("STRWHITE", Value::Float(1.0), "");
    stretched
        .header
        .set("STRETCH", Value::Str("mtf".into()), "");
    stretched.header.set("STRMID", Value::Float(0.2), "");
    let e = calibrate(&stretched, &s.truth, &s.catalogue, &Options::default()).expect_err("str");
    assert!(e.to_string().contains("stretched"), "{e}");
    // A catalogue far from the image matches nothing.
    let far = colour_scene((10.0, -50.0), 1200, 800, 6.0, 150.0, 6);
    assert!(calibrate(&s.image, &s.truth, &far.catalogue, &Options::default()).is_err());
}

#[test]
fn the_fit_ignores_stars_that_are_wrong_in_colour() {
    let mut rng = Rng::new(3);
    let mut stars: Vec<StarPhot> = (0..120)
        .map(|i| {
            let colour = 0.3 + 2.0 * (i as f32 / 120.0);
            let x = f64::from(colour - 0.82);
            let (r, b) = (
                10f64.powf(-0.4 * (0.2 - 0.5 * x)),
                10f64.powf(-0.4 * (0.1 + 0.4 * x)),
            );
            let jitter = 1.0 + 0.01 * rng.normal();
            StarPhot {
                x: 0.0,
                y: 0.0,
                g: 9.0,
                colour,
                flux: [1000.0 * r * jitter, 1000.0, 1000.0 * b],
                snr: [100.0; 3],
                used: false,
            }
        })
        .collect();
    for s in stars.iter_mut().step_by(9) {
        s.flux[0] *= 2.5;
    }
    let (lines, _) = fit(&mut stars, &Options::default()).expect("fit");
    assert!((lines[0].zero - 0.2).abs() < 0.02, "{lines:?}");
    assert!((lines[0].slope + 0.5).abs() < 0.03, "{lines:?}");
    assert!((lines[1].zero - 0.1).abs() < 0.02 && (lines[1].slope - 0.4).abs() < 0.03);
    assert!(stars.iter().step_by(9).all(|s| !s.used));
}

#[test]
fn the_calibration_is_recorded_in_the_header() {
    let s = scene();
    let cal = calibrate(&s.image, &s.truth, &s.catalogue, &Options::default()).expect("calibrate");
    let mut img = cal.apply(&s.image).expect("apply");
    cal.record(&mut img);
    let g = img.header.get_f64("PCCGAINR").expect("PCCGAINR");
    assert!((g - cal.gains[0]).abs() < 1e-9);
    assert!(img.header.get_f64("PCCOFFB").is_some());
}

#[test]
fn a_cancelled_token_stops_the_colour_calibration() {
    let s = scene();
    let o = Options::default();
    assert!(calibrate(&s.image, &s.truth, &s.catalogue, &o).is_ok());
    o.cancel.cancel();
    assert!(matches!(
        calibrate(&s.image, &s.truth, &s.catalogue, &o),
        Err(Error::Cancelled)
    ));
}
