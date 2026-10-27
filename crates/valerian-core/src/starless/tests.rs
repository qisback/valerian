// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::fits::Header;
use crate::testutil::{Rng, deep_sky, gaussian_field};

fn mono(f: Frame) -> Image {
    Image::mono(f, Header::new())
}

/// `f` with Gaussian noise of `sigma`: a noise-free frame has no sky to measure.
fn grain(mut f: Frame, sigma: f64, seed: u64) -> Frame {
    let mut rng = Rng::new(seed);
    for v in &mut f.data {
        *v += rng.gauss(0.0, sigma) as f32;
    }
    f
}

fn quiet() -> Options {
    Options {
        noise: false,
        ..Options::default()
    }
}

fn mean_abs(a: &[f32], b: &[f32], mask: &[f32]) -> f64 {
    let (mut s, mut n) = (0.0f64, 0.0f64);
    for i in 0..a.len() {
        if mask[i] >= 0.5 {
            s += f64::from((a[i] - b[i]).abs());
            n += 1.0;
        }
    }
    s / n.max(1.0)
}

#[test]
fn stars_come_out_and_the_nebula_stays() {
    let sky = deep_sky(400, 300, 120, 3);
    let r = remove_stars(&mono(sky.frame.clone()), &Options::default()).expect("run");
    assert!(r.found > 60, "found {}", r.found);
    let out = &r.starless.plane().data;
    // Inside the mask the fill is close to the truth, to a few noise sigmas.
    let err = mean_abs(out, &sky.starless.data, &r.mask.data);
    assert!(err < 12.0, "mean error in the mask {err}");
    // Most of the star light is gone.
    let before: f64 = sky
        .frame
        .data
        .iter()
        .zip(&sky.starless.data)
        .map(|(a, b)| f64::from(a - b))
        .sum();
    let left: f64 = out
        .iter()
        .zip(&sky.starless.data)
        .map(|(a, b)| f64::from(a - b))
        .sum();
    assert!(left.abs() < 0.08 * before, "left {left} of {before}");
    // The star layer holds what was removed.
    let layer: f64 = r.stars.plane().data.iter().map(|&v| f64::from(v)).sum();
    assert!((layer - (before - left)).abs() < 1e-3 * before);
}

#[test]
fn pixels_outside_the_mask_are_untouched_and_the_layers_add_up() {
    let sky = deep_sky(300, 200, 60, 4);
    for strength in [1.0f32, 0.5] {
        let opts = Options {
            strength,
            ..Options::default()
        };
        let r = remove_stars(&mono(sky.frame.clone()), &opts).expect("run");
        let (a, s) = (&r.starless.plane().data, &r.stars.plane().data);
        let o = &sky.frame.data;
        for i in 0..o.len() {
            if r.mask.data[i] < 0.5 {
                assert_eq!(a[i].to_bits(), o[i].to_bits(), "pixel {i} moved");
            }
            assert!((a[i] + s[i] - o[i]).abs() <= 1e-3 * o[i].abs().max(1.0));
        }
    }
}

#[test]
fn zero_strength_changes_nothing() {
    let sky = deep_sky(200, 160, 40, 5);
    let opts = Options {
        strength: 0.0,
        ..Options::default()
    };
    let r = remove_stars(&mono(sky.frame.clone()), &opts).expect("run");
    assert_eq!(r.starless.plane().data, sky.frame.data);
}

#[test]
fn half_strength_halves_the_star_peak() {
    let f = grain(
        gaussian_field(160, 160, 100.0, &[(80.0, 80.0, 40_000.0, 2.0)]),
        2.0,
        1,
    );
    let full = remove_stars(&mono(f.clone()), &quiet()).expect("run");
    let half_opts = Options {
        strength: 0.5,
        ..quiet()
    };
    let half = remove_stars(&mono(f.clone()), &half_opts).expect("run");
    let at = 80 * 160 + 80;
    let peak = f.data[at] - 100.0;
    assert!((full.starless.plane().data[at] - 100.0).abs() < 0.02 * peak);
    let kept = half.starless.plane().data[at] - 100.0;
    assert!((kept / peak - 0.5).abs() < 0.03, "kept {kept} of {peak}");
}

#[test]
fn the_fill_follows_a_gradient() {
    let (w, h) = (160usize, 120usize);
    let ramp: Vec<f32> = (0..w * h)
        .map(|i| 300.0 + 2.0 * (i % w) as f32 + (i / w) as f32)
        .collect();
    let mut f = grain(
        gaussian_field(w, h, 0.0, &[(80.0, 60.0, 60_000.0, 2.0)]),
        1.0,
        3,
    );
    for (v, r) in f.data.iter_mut().zip(&ramp) {
        *v += r;
    }
    let r = remove_stars(&mono(f), &quiet()).expect("run");
    let err = mean_abs(&r.starless.plane().data, &ramp, &r.mask.data);
    assert!(r.masked_fraction > 0.0 && err < 0.5, "error {err}");
}

#[test]
fn noise_is_added_in_the_hole_and_is_repeatable() {
    let sky = deep_sky(240, 180, 30, 6);
    let img = mono(sky.frame);
    let a = remove_stars(&img, &Options::default()).expect("run");
    let b = remove_stars(&img, &Options::default()).expect("run");
    assert_eq!(a.starless.plane().data, b.starless.plane().data);
    let smooth = remove_stars(&img, &quiet()).expect("run");
    let scatter = |f: &Frame, m: &Frame| {
        let v: Vec<f32> = (1..f.data.len() - 1)
            .filter(|&i| m.data[i] > 0.5 && m.data[i - 1] > 0.5 && m.data[i + 1] > 0.5)
            .map(|i| f.data[i] - f32::midpoint(f.data[i - 1], f.data[i + 1]))
            .collect();
        stats::median_sigma(&v).1
    };
    let (noisy, flat) = (
        scatter(a.starless.plane(), &a.mask),
        scatter(smooth.starless.plane(), &smooth.mask),
    );
    assert!(noisy > 3.0 * flat, "noisy {noisy}, smooth {flat}");
}

#[test]
fn colour_planes_share_one_mask() {
    let sky = deep_sky(200, 150, 40, 7);
    let planes = (0..3)
        .map(|k| {
            let mut p = sky.frame.clone();
            for v in &mut p.data {
                *v *= 1.0 + 0.2 * k as f32;
            }
            p
        })
        .collect();
    let img = Image {
        header: Header::new(),
        planes,
    };
    let r = remove_stars(&img, &Options::default()).expect("run");
    assert_eq!(r.starless.planes.len(), 3);
    let masked = r.mask.data.iter().filter(|&&v| v > 0.5).count();
    for (before, after) in img.planes.iter().zip(&r.starless.planes) {
        let changed = before
            .data
            .iter()
            .zip(&after.data)
            .filter(|(a, b)| (**a - **b).abs() > 0.0)
            .count();
        assert!(changed > 0 && changed <= masked);
    }
}

#[test]
fn a_star_on_the_frame_edge_is_removed_without_panicking() {
    let f = grain(
        gaussian_field(
            96,
            96,
            100.0,
            &[(4.0, 30.0, 20_000.0, 2.0), (91.0, 91.0, 20_000.0, 2.0)],
        ),
        2.0,
        2,
    );
    let r = remove_stars(&mono(f), &quiet()).expect("run");
    assert!(
        r.starless
            .plane()
            .data
            .iter()
            .all(|&v| (v - 100.0).abs() < 12.0)
    );
}

#[test]
fn a_frame_without_stars_is_returned_as_it_was() {
    let f = Frame::new(64, 64, vec![100.0; 64 * 64]).expect("frame");
    let r = remove_stars(&mono(f.clone()), &Options::default()).expect("run");
    assert_eq!(r.found, 0);
    assert_eq!(r.starless.plane().data, f.data);
}

#[test]
fn bad_input_is_an_error() {
    let f = Frame::new(32, 32, vec![1.0; 1024]).expect("frame");
    let bad = Options {
        strength: 1.5,
        ..Options::default()
    };
    assert!(remove_stars(&mono(f.clone()), &bad).is_err());
    let mut raw = f;
    raw.bayer = Some(crate::BayerPattern::Rggb);
    assert!(remove_stars(&mono(raw), &Options::default()).is_err());
    let tiny = Frame::new(4, 4, vec![1.0; 16]).expect("frame");
    assert!(remove_stars(&mono(tiny), &Options::default()).is_err());
}

#[test]
fn mask_radius_grows_with_brightness_and_is_capped() {
    let star = |peak: f32| Star {
        x: 0.0,
        y: 0.0,
        flux: 0.0,
        peak,
        fwhm: 4.0,
        ellipticity: 0.0,
        theta: 0.0,
        snr: 0.0,
        saturated: false,
    };
    let o = Options::default();
    let faint = radius(&star(30.0), 5.0, &o);
    let bright = radius(&star(30_000.0), 5.0, &o);
    assert!(faint >= 8.0 && bright > 1.5 * faint, "{faint} {bright}");
    let capped = Options {
        max_radius: 20.0,
        ..Options::default()
    };
    assert!((radius(&star(1e9), 5.0, &capped) - 20.0).abs() < 1e-3);
}

#[test]
fn a_cancelled_token_stops_star_removal_and_an_unset_one_changes_nothing() {
    let ds = deep_sky(300, 240, 40, 3);
    let img = mono(grain(ds.frame, 2.0, 4));
    let plain = remove_stars(&img, &quiet()).expect("plain");
    let o = quiet();
    let held = remove_stars(&img, &o).expect("held");
    assert_eq!(plain.starless.planes[0].data, held.starless.planes[0].data);
    o.cancel.cancel();
    assert!(matches!(remove_stars(&img, &o), Err(Error::Cancelled)));
}
