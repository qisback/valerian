// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::Frame;
use crate::fits::{Header, Value};
use crate::testutil::{Rng, gaussian_field};
use crate::wcs::Sip;

const SIZE: (usize, usize) = (1200, 800);

/// A made-up sky: `n` stars to G = 13 in a cap of `cap` degrees around the
/// centre, with the usual rise in numbers towards faint stars. The catalogue
/// has those brighter than G = 11.
struct Sky {
    all: Vec<CatStar>,
    cat: Catalogue,
}

fn sky(centre: (f64, f64), cap: f64, density: f64, seed: u64) -> Sky {
    let mut rng = Rng::new(seed);
    let n = (density * std::f64::consts::PI * cap * cap) as usize;
    let mut all = Vec::with_capacity(n);
    while all.len() < n {
        // Uniform over the cap by rejection in a box around it.
        let (dx, dy) = (
            (rng.uniform() * 2.0 - 1.0) * cap,
            (rng.uniform() * 2.0 - 1.0) * cap,
        );
        if dx.hypot(dy) > cap {
            continue;
        }
        let (ra, dec) = deproject(dx, dy, centre.0, centre.1);
        all.push(CatStar {
            ra,
            dec,
            mag: 7.0 + 6.0 * rng.uniform().sqrt() as f32,
            colour: None,
        });
    }
    let bright: Vec<CatStar> = all.iter().copied().filter(|s| s.mag < 11.0).collect();
    Sky {
        cat: Catalogue::build(&bright, 11.0),
        all,
    }
}

/// What a camera with the system `truth` sees of `sky`: stars to G = 12.5 with
/// 0.3 px of centroid noise, a tenth of them missed, and `spurious` detections
/// that are not stars of the catalogue.
fn observe(sky: &Sky, truth: &Wcs, spurious: usize, seed: u64) -> Vec<Star> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for s in &sky.all {
        if s.mag > 12.5 || rng.uniform() < 0.1 {
            continue;
        }
        let Some((x, y)) = truth.world_to_pixel(s.ra, s.dec) else {
            continue;
        };
        if x < 5.0 || y < 5.0 || x > SIZE.0 as f64 - 6.0 || y > SIZE.1 as f64 - 6.0 {
            continue;
        }
        out.push(star_at(
            x + rng.gauss(0.0, 0.3),
            y + rng.gauss(0.0, 0.3),
            s.mag,
        ));
    }
    for _ in 0..spurious {
        out.push(star_at(
            rng.uniform() * SIZE.0 as f64,
            rng.uniform() * SIZE.1 as f64,
            9.0 + 4.0 * rng.uniform() as f32,
        ));
    }
    out
}

fn distorted(mut w: Wcs) -> Wcs {
    // A few pixels at the frame corners, as a wide-field lens gives.
    w.sip = Some(Sip {
        order: 3,
        a: vec![
            (2, 0, 1.0e-5),
            (1, 1, -0.7e-5),
            (0, 2, 0.5e-5),
            (3, 0, 5.0e-9),
        ],
        b: vec![
            (2, 0, -0.4e-5),
            (1, 1, 1.0e-5),
            (0, 2, -0.8e-5),
            (0, 3, -5.0e-9),
        ],
        ap: Vec::new(),
        bp: Vec::new(),
    });
    w
}

/// The worst disagreement, in arc seconds, between two systems over the frame.
fn worst(a: &Wcs, b: &Wcs) -> f64 {
    let mut w = 0.0f64;
    for &(x, y) in &[
        (0.0, 0.0),
        (1199.0, 0.0),
        (0.0, 799.0),
        (1199.0, 799.0),
        (600.0, 400.0),
        (300.0, 650.0),
    ] {
        let (p, q) = (a.pixel_to_world(x, y), b.pixel_to_world(x, y));
        w = w.max(separation(p.0, p.1, q.0, q.1) * 3600.0);
    }
    w
}

const CENTRE: (f64, f64) = (150.0, 30.0);

fn truth(rotation: f64, flip: bool) -> Wcs {
    Wcs::simple(CENTRE, (600.5, 400.5), 6.0, rotation, flip)
}

fn opts(hint: Option<Hint>) -> Options {
    Options {
        hint,
        scale: (5.0, 7.2),
        ..Options::default()
    }
}

fn hint(dra: f64, ddec: f64, radius: f64) -> Hint {
    Hint {
        ra: CENTRE.0 + dra,
        dec: CENTRE.1 + ddec,
        radius,
    }
}

#[test]
fn a_hinted_solve_finds_the_system() {
    let s = sky(CENTRE, 6.0, 90.0, 1);
    let t = truth(23.0, false);
    let stars = observe(&s, &t, 20, 2);
    let sol = solve_stars(&stars, SIZE, &s.cat, &opts(Some(hint(0.25, -0.2, 0.8)))).expect("solve");
    assert!(sol.matched >= 25, "matched {}", sol.matched);
    assert!(
        worst(&sol.wcs, &t) < 1.5,
        "worst {} arcsec",
        worst(&sol.wcs, &t)
    );
    assert!(sol.rms_px < 0.6 && !sol.wcs.mirrored());
    assert!((sol.wcs.scale_arcsec() - 6.0).abs() < 0.01);
    assert!((sol.wcs.rotation() - 23.0).abs() < 0.05);
}

#[test]
fn every_rotation_and_a_mirrored_image_solve() {
    let s = sky(CENTRE, 6.0, 90.0, 3);
    for (rot, flip) in [
        (0.0, false),
        (90.0, false),
        (181.0, false),
        (300.0, false),
        (40.0, true),
        (265.0, true),
    ] {
        let t = truth(rot, flip);
        let stars = observe(&s, &t, 10, 4);
        let sol = solve_stars(&stars, SIZE, &s.cat, &opts(Some(hint(0.0, 0.0, 0.3))))
            .unwrap_or_else(|e| panic!("rotation {rot} flip {flip}: {e}"));
        assert_eq!(sol.wcs.mirrored(), flip, "rotation {rot}");
        assert!(
            worst(&sol.wcs, &t) < 1.5,
            "rotation {rot}: {}",
            worst(&sol.wcs, &t)
        );
    }
}

#[test]
fn distortion_is_fitted_as_sip() {
    let s = sky(CENTRE, 6.0, 120.0, 5);
    let t = distorted(truth(-70.0, false));
    let stars = observe(&s, &t, 15, 6);
    let sol = solve_stars(&stars, SIZE, &s.cat, &opts(Some(hint(0.1, 0.1, 0.5)))).expect("solve");
    assert!(sol.wcs.sip.is_some(), "matched {}", sol.matched);
    assert!(sol.rms_px < 0.5, "rms {}", sol.rms_px);
    // Corner error of a cubic fitted to 0.3 px of centroid noise: about half a pixel.
    assert!(worst(&sol.wcs, &t) < 4.0, "worst {}", worst(&sol.wcs, &t));
    // Without SIP the same stars leave a visibly larger residual.
    let plain = solve_stars(
        &stars,
        SIZE,
        &s.cat,
        &Options {
            sip_order: Some(0),
            ..opts(Some(hint(0.1, 0.1, 0.5)))
        },
    )
    .expect("plain");
    assert!(
        plain.rms_px > 1.4 * sol.rms_px,
        "{} vs {}",
        plain.rms_px,
        sol.rms_px
    );
}

#[test]
fn a_blind_solve_needs_no_position() {
    let s = sky(CENTRE, 14.0, 60.0, 7);
    let t = truth(110.0, false);
    let stars = observe(&s, &t, 10, 8);
    let blind = Options {
        scale: (4.5, 8.0),
        ..Options::default()
    };
    let sol = solve_stars(&stars, SIZE, &s.cat, &blind).expect("blind solve");
    assert!(worst(&sol.wcs, &t) < 1.5);
    assert!(sol.trials > 1);
}

#[test]
fn a_wrong_hint_or_scale_fails_instead_of_guessing() {
    let s = sky(CENTRE, 6.0, 90.0, 9);
    let t = truth(10.0, false);
    let stars = observe(&s, &t, 10, 10);
    // Thirty degrees off, in empty sky.
    assert!(solve_stars(&stars, SIZE, &s.cat, &opts(Some(hint(0.0, -30.0, 0.5)))).is_err());
    // Right place, scale range that excludes the truth (6 arcsec per pixel).
    let wrong = Options {
        scale: (1.0, 1.6),
        hint: Some(hint(0.0, 0.0, 0.5)),
        ..Options::default()
    };
    assert!(solve_stars(&stars, SIZE, &s.cat, &wrong).is_err());
}

#[test]
fn random_stars_do_not_solve() {
    let s = sky(CENTRE, 6.0, 90.0, 11);
    let mut rng = Rng::new(12);
    let stars: Vec<Star> = (0..80)
        .map(|_| {
            star_at(
                rng.uniform() * SIZE.0 as f64,
                rng.uniform() * SIZE.1 as f64,
                8.0 + 4.0 * rng.uniform() as f32,
            )
        })
        .collect();
    let e = solve_stars(&stars, SIZE, &s.cat, &opts(Some(hint(0.0, 0.0, 0.6))))
        .expect_err("no solution");
    assert!(e.to_string().contains("no match"), "{e}");
}

#[test]
fn too_few_stars_and_bad_scale_are_refused() {
    let s = sky(CENTRE, 3.0, 90.0, 13);
    let few: Vec<Star> = (0..5)
        .map(|i| star_at(10.0 * f64::from(i), 20.0, 9.0))
        .collect();
    let e = solve_stars(&few, SIZE, &s.cat, &opts(None)).expect_err("few");
    assert!(e.to_string().contains("stars were detected"));
    let bad = Options {
        scale: (3.0, 1.0),
        ..Options::default()
    };
    assert!(solve_stars(&few, SIZE, &s.cat, &bad).is_err());
    // A blind search at a tiny scale would need millions of patches.
    let many: Vec<Star> = (0..30)
        .map(|i| star_at(f64::from(i) * 30.0, f64::from(i) * 20.0, 9.0))
        .collect();
    let tiny = Options {
        scale: (0.2, 0.3),
        max_trials: 1000,
        ..Options::default()
    };
    let e = solve_stars(&many, SIZE, &s.cat, &tiny).expect_err("too many patches");
    assert!(e.to_string().contains("patches of sky"), "{e}");
}

#[test]
fn an_image_solves_end_to_end_and_the_wcs_is_recorded() {
    let s = sky(CENTRE, 6.0, 90.0, 14);
    let t = truth(33.0, false);
    // Render the stars into pixels: flux from magnitude, a little noise.
    let stars = observe(&s, &t, 0, 15);
    let specs: Vec<(f64, f64, f64, f64)> = stars
        .iter()
        .map(|st| (st.x, st.y, st.flux * 4.0e6, 1.8))
        .collect();
    let mut frame: Frame = gaussian_field(SIZE.0, SIZE.1, 200.0, &specs);
    let mut rng = Rng::new(16);
    for v in &mut frame.data {
        *v += rng.gauss(0.0, 5.0) as f32;
    }
    let mut image = Image::mono(frame, Header::new());
    let sol = solve(&image, &s.cat, &opts(Some(hint(0.1, 0.1, 0.5)))).expect("solve the image");
    assert!(worst(&sol.wcs, &t) < 1.5, "worst {}", worst(&sol.wcs, &t));
    record(&mut image, &sol);
    let back = Wcs::from_header(&image.header).expect("wcs in the header");
    assert!(worst(&back, &sol.wcs) < 1e-3);
    assert!(
        image
            .header
            .cards()
            .iter()
            .any(|c| c.key == "HISTORY" && c.comment.contains("valerian solve"))
    );
    // The recorded system is now the hint for the next run.
    let h = hint_from_header(&image.header, SIZE);
    let pos = h.position.expect("position");
    assert!(distance(pos, sol.centre) < 1e-6);
    assert!((h.scale.expect("scale") - 6.0).abs() < 0.02);
}

#[test]
fn raw_mosaics_are_refused() {
    let s = sky(CENTRE, 3.0, 50.0, 17);
    let mut f = Frame::new(64, 64, vec![1.0; 4096]).expect("frame");
    f.bayer = Some(crate::BayerPattern::Rggb);
    assert!(solve(&Image::mono(f, Header::new()), &s.cat, &Options::default()).is_err());
}

#[test]
fn headers_give_a_position_and_scale() {
    let mut h = Header::new();
    h.set("OBJCTRA", Value::Str("04 04 38".into()), "");
    h.set("OBJCTDEC", Value::Str("+35 36 58".into()), "");
    h.set("FOCALLEN", Value::Float(840.0), "");
    h.set("XPIXSZ", Value::Float(3.8), "");
    let hint = hint_from_header(&h, SIZE);
    let (ra, dec) = hint.position.expect("position");
    assert!((ra - 61.158_33).abs() < 1e-3 && (dec - 35.616_11).abs() < 1e-3);
    assert!((hint.scale.expect("scale") - 0.933).abs() < 0.001);

    // A plain RA and DEC in degrees, as a Seestar writes them.
    let mut h = Header::new();
    h.set("RA", Value::Float(313.020_84), "");
    h.set("DEC", Value::Float(44.4975), "");
    assert_eq!(
        hint_from_header(&h, SIZE).position,
        Some((313.020_84, 44.4975))
    );
    // Sexagesimal RA text is hours.
    let mut h = Header::new();
    h.set("RA", Value::Str("12:00:00".into()), "");
    h.set("DEC", Value::Str("-10:30:00".into()), "");
    assert_eq!(hint_from_header(&h, SIZE).position, Some((180.0, -10.5)));
    assert_eq!(
        hint_from_header(&Header::new(), SIZE),
        HeaderHint::default()
    );
}

#[test]
fn the_trial_list_starts_at_the_hint_and_spreads_out() {
    let h = hint(0.0, 0.0, 1.0);
    let t = trials_for(&h, 0.5);
    assert_eq!(t[0], (CENTRE.0, CENTRE.1));
    assert!(t.len() > 5);
    let d: Vec<f64> = t
        .iter()
        .map(|&(r, d)| separation(r, d, h.ra, h.dec))
        .collect();
    assert!(d[0] < 1e-9 && d[1] < d[d.len() - 1]);
    assert!(d.iter().all(|&x| x <= 1.05));
    assert_eq!(trials_for(&hint(0.0, 0.0, 0.05), 0.5).len(), 1);
    // The whole-sky lattice covers every part of the sky evenly.
    let tiles = sky_tiles(10.0);
    assert!(
        tiles.len() > 300 && tiles.iter().any(|t| t.1 > 80.0) && tiles.iter().any(|t| t.1 < -80.0)
    );
}

#[test]
fn a_cancelled_token_stops_the_search() {
    let s = sky(CENTRE, 6.0, 90.0, 1);
    let t = truth(23.0, false);
    let stars = observe(&s, &t, 20, 2);
    let o = opts(Some(hint(0.25, -0.2, 0.8)));
    assert!(solve_stars(&stars, SIZE, &s.cat, &o).is_ok());
    o.cancel.cancel();
    assert!(matches!(
        solve_stars(&stars, SIZE, &s.cat, &o),
        Err(Error::Cancelled)
    ));
}
