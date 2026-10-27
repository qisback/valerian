// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Calibration of a real session: 300 s H-alpha lights from a mono ASI1600MM
//! with darks, flats and dark flats from the same nights.
//!
//! The data is never committed. Set `VALERIAN_TEST_DATA` to a directory laid
//! out as `calibration/2023-01-25/{LIGHT,FLAT,DARKFLAT}` and
//! `deepsky/2023-01-26/DARK` (the owner's NINA captures); the test is skipped
//! when it is unset.

#![allow(
    clippy::too_many_lines,
    reason = "these tests narrate a whole session, step by step"
)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;
use valerian_core::calibrate::{
    Calibrator, FlatNormalisation, PixelMap, flag, master_dark, master_flat,
};
use valerian_core::combine::Rejection;
use valerian_core::fits::{self, Image};
use valerian_core::register::{RegisterOptions, register};
use valerian_core::stars::{DetectOptions, detect};
use valerian_core::stats;
use valerian_core::warp::{Interpolation, warp};

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("VALERIAN_TEST_DATA").map(PathBuf::from)
}

fn list(dir: &Path, contains: &str, limit: usize) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let named = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(contains));
            let fits = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit"));
            named && fits
        })
        .collect();
    v.sort();
    v.truncate(limit);
    v
}

fn load(paths: &[PathBuf]) -> Vec<Image> {
    paths
        .par_iter()
        .map(|p| fits::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .collect()
}

/// Spread of the sky across the frame: the standard deviation, over an 8 by 8
/// grid of tiles, of each tile's median, relative to the mean of the medians.
fn tile_flatness(f: &valerian_core::Frame, above: f32) -> f64 {
    let (gx, gy) = (8, 8);
    let (tw, th) = (f.width / gx, f.height / gy);
    let mut m = Vec::new();
    for ty in 0..gy {
        for tx in 0..gx {
            let mut v = Vec::with_capacity(tw * th / 16);
            for y in (ty * th..(ty + 1) * th).step_by(4) {
                for x in (tx * tw..(tx + 1) * tw).step_by(4) {
                    v.push(f.data[y * f.width + x]);
                }
            }
            m.push(f64::from(stats::median_finite(&v) - above));
        }
    }
    let mean = m.iter().sum::<f64>() / m.len() as f64;
    let var = m.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / m.len() as f64;
    var.sqrt() / mean
}

#[test]
fn calibrate_a_real_halpha_session() {
    let Some(root) = data_dir() else {
        eprintln!("VALERIAN_TEST_DATA is unset: skipping the real-data calibration test");
        return;
    };
    let t0 = Instant::now();
    let lights = load(&list(
        &root.join("calibration/2023-01-25/LIGHT"),
        "300.00s",
        5,
    ));
    let darks = load(&list(
        &root.join("deepsky/2023-01-26/DARK"),
        "Ha_-10.00_300.00s",
        10,
    ));
    let flats = load(&list(
        &root.join("calibration/2023-01-25/FLAT"),
        "Ha_-10.00_2.04s",
        30,
    ));
    let dflats = load(&list(
        &root.join("calibration/2023-01-25/DARKFLAT"),
        "Ha_-10.00_2.04s",
        30,
    ));
    eprintln!(
        "loaded {} lights, {} darks, {} flats, {} dark flats ({}x{}) in {:.1?}",
        lights.len(),
        darks.len(),
        flats.len(),
        dflats.len(),
        lights[0].width(),
        lights[0].height(),
        t0.elapsed()
    );
    assert_eq!(
        (lights.len(), darks.len(), flats.len(), dflats.len()),
        (5, 10, 30, 30)
    );

    let t = Instant::now();
    // No bias frames exist for this session: the dark holds bias plus dark
    // current and is subtracted as it is (same exposure, same temperature).
    let dark = master_dark(&darks, None, Rejection::auto(darks.len())).expect("dark");
    let dflat = master_dark(&dflats, None, Rejection::auto(dflats.len())).expect("dark flat");
    let flat = master_flat(
        &flats,
        Some(&dflat),
        Rejection::auto(flats.len()),
        FlatNormalisation::Global,
    )
    .expect("flat");
    eprintln!("masters built in {:.1?}", t.elapsed());

    for k in [5.0, 8.0, 12.0, 20.0, 40.0] {
        let m = PixelMap::from_dark(dark.plane(), k, k);
        eprintln!(
            "dark pixels beyond {k} sigma of their neighbours: {} hot, {} cold",
            m.count_flag(flag::HOT),
            m.count_flag(flag::COLD)
        );
    }
    // This sensor has about one pixel in a hundred warm in a 300 s dark, which
    // dark subtraction already handles; only the extreme ones are repaired.
    let mut map = PixelMap::from_dark(dark.plane(), 40.0, 12.0);
    map.add_dead_from_flat(flat.plane(), 0.2);
    let n = (map.width * map.height) as f64;
    eprintln!(
        "pixel map: {} hot ({:.4} %), {} cold, {} dead",
        map.count_flag(flag::HOT),
        100.0 * map.count_flag(flag::HOT) as f64 / n,
        map.count_flag(flag::COLD),
        map.count_flag(flag::DEAD)
    );
    assert!(map.count_flag(flag::HOT) > 0);
    assert!(
        map.count() < (n * 0.01) as usize,
        "implausibly many defects"
    );

    let mut with_dark = Calibrator::new(None, Some(dark.clone()), None);
    with_dark.pixel_map = Some(map.clone());
    let mut full = Calibrator::new(None, Some(dark), Some(flat));
    full.pixel_map = Some(map);

    for (i, light) in lights.iter().enumerate() {
        let t = Instant::now();
        let (d_only, _) = with_dark.calibrate(light).expect("dark only");
        let (cal, report) = full.calibrate(light).expect("calibrate");
        let (sky_raw, noise_raw) = stats::sky_and_noise(light.plane());
        let (sky_d, noise_d) = stats::sky_and_noise(d_only.plane());
        let (sky_c, noise_c) = stats::sky_and_noise(cal.plane());
        let flat_before = tile_flatness(d_only.plane(), 0.0);
        let flat_after = tile_flatness(cal.plane(), 0.0);
        eprintln!(
            "light {i}: sky {sky_raw:.1} -> {sky_d:.1} -> {sky_c:.1} ADU, noise {noise_raw:.2} -> {noise_d:.2} -> {noise_c:.2}, \
             sky tile spread {:.2} % -> {:.2} % with the flat, {} px repaired, {:.1?}",
            100.0 * flat_before,
            100.0 * flat_after,
            report.repaired,
            t.elapsed()
        );
        assert!(sky_c.is_finite() && noise_c.is_finite());
        assert!(
            sky_raw > sky_d,
            "dark subtraction must lower the level ({sky_raw} -> {sky_d})"
        );
        assert!(report.repaired > 100, "cosmetic correction did nothing");
        assert!(
            flat_after < flat_before,
            "the flat should even out the sky ({flat_before} -> {flat_after})"
        );
    }
    eprintln!("total {:.1?}", t0.elapsed());
}

#[test]
fn register_real_halpha_frames() {
    let Some(root) = data_dir() else {
        eprintln!("VALERIAN_TEST_DATA is unset: skipping the real-data registration test");
        return;
    };
    let lights = load(&list(
        &root.join("calibration/2023-01-25/LIGHT"),
        "300.00s",
        6,
    ));
    let darks = load(&list(
        &root.join("deepsky/2023-01-26/DARK"),
        "Ha_-10.00_300.00s",
        10,
    ));
    let dark = master_dark(&darks, None, Rejection::auto(darks.len())).expect("dark");
    let map = PixelMap::from_dark(dark.plane(), 40.0, 12.0);
    let mut cal = Calibrator::new(None, Some(dark), None);
    cal.pixel_map = Some(map);
    let calibrated: Vec<Image> = lights
        .iter()
        .map(|l| cal.calibrate(l).expect("calibrate").0)
        .collect();

    let opts = DetectOptions::default();
    let t = Instant::now();
    let dets: Vec<_> = calibrated
        .iter()
        .map(|c| detect(c.plane(), &opts))
        .collect();
    eprintln!(
        "detected stars in {} frames in {:.1?}",
        dets.len(),
        t.elapsed()
    );
    for (i, d) in dets.iter().enumerate() {
        eprintln!(
            "registration frame {i}: {} stars, FWHM {:.2} px, ellipticity {:.3}, sky {:.1}, noise {:.1}",
            d.stars.len(),
            d.median_fwhm(),
            d.median_ellipticity(),
            d.sky,
            d.noise
        );
        assert!(
            d.stars.len() > 100,
            "frame {i}: only {} stars",
            d.stars.len()
        );
        assert!((1.5..12.0).contains(&d.median_fwhm()));
    }
    let reference = &dets[0];
    // Frames 0 to 4 are one block of the sequence; later blocks may be another
    // pointing, so they are reported but not asserted.
    for (i, d) in dets.iter().enumerate().skip(1) {
        let same_block = i < 5;
        let t = Instant::now();
        let reg = match register(&reference.stars, &d.stars, &RegisterOptions::default()) {
            Ok(r) => r,
            Err(e) if !same_block => {
                eprintln!("registration frame {i}: not registered to frame 0 ({e})");
                continue;
            }
            Err(e) => panic!("frame {i}: {e}"),
        };
        let (scale, angle, (tx, ty)) = reg.transform.decompose();
        // Compare the stars of the warped frame with the reference's.
        let warped = warp(
            calibrated[i].plane(),
            &reg.transform,
            reference_width(&calibrated),
            reference_height(&calibrated),
            Interpolation::Lanczos3,
        )
        .expect("warp");
        let now = detect(&warped, &opts);
        let (mut sq, mut n, mut sdx, mut sdy) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        let w = warped.width as f64;
        let h = warped.height as f64;
        for s in reference.stars.iter().filter(|s| {
            s.snr > 30.0 && s.x > 50.0 && s.y > 50.0 && s.x < w - 50.0 && s.y < h - 50.0
        }) {
            if let Some(m) = now
                .stars
                .iter()
                .find(|m| (m.x - s.x).hypot(m.y - s.y) < 2.0)
            {
                sq += (m.x - s.x).powi(2) + (m.y - s.y).powi(2);
                sdx += m.x - s.x;
                sdy += m.y - s.y;
                n += 1.0;
            }
        }
        let rms = (sq / n.max(1.0)).sqrt();
        let bias = (sdx / n.max(1.0)).hypot(sdy / n.max(1.0));
        eprintln!(
            "registration frame {i}: shift ({tx:.2}, {ty:.2}) px, rotation {:.4} deg, scale {scale:.5},              {} stars matched, fit rms {:.3} px, {n:.0} bright stars re-measured after the warp: mean offset {bias:.4} px, scatter {rms:.3} px (single-star centroid noise), {:.1?}",
            angle.to_degrees(),
            reg.matched,
            reg.rms,
            t.elapsed()
        );
        if !same_block {
            continue;
        }
        assert!(reg.matched > 80);
        assert!(n > 50.0, "only {n} stars compared");
        assert!(
            bias < 0.05,
            "frame {i}: systematic offset {bias} px after the warp"
        );
        assert!(rms < 0.3, "frame {i}: scatter {rms} px after the warp");
    }
}

fn reference_width(v: &[Image]) -> usize {
    v[0].width()
}

fn reference_height(v: &[Image]) -> usize {
    v[0].height()
}

/// Raw one-shot-colour sub-exposures from a Seestar S50 (1080 by 1920, GRBG,
/// ten seconds each): demosaiced, registered and stacked with no calibration
/// frames, as the telescope provides none. `VALERIAN_TEST_OSC` names a directory
/// of `Light_*.fit`; the test is skipped when it is unset.
#[test]
fn stack_real_colour_subs() {
    use valerian_core::debayer::{Method, demosaic_image};
    use valerian_core::stack::{StackOptions, Status, stack};

    let Some(dir) = std::env::var_os("VALERIAN_TEST_OSC").map(PathBuf::from) else {
        eprintln!("VALERIAN_TEST_OSC is unset: skipping the real colour-frame test");
        return;
    };
    let mut files = list(&dir, "Light_", 24);
    files.sort();
    let raw = load(&files);
    assert!(
        raw.iter().all(|f| f.plane().bayer.is_some()),
        "expected raw Bayer frames"
    );
    let t = Instant::now();
    let frames: Vec<Image> = raw
        .iter()
        .map(|f| demosaic_image(f, Method::Mhc).expect("demosaic"))
        .collect();
    eprintln!(
        "real colour: demosaiced {} frames in {:.1?}",
        frames.len(),
        t.elapsed()
    );
    let single = stats::sky_and_noise(&valerian_core::stars::luminance(&frames[0].planes)).1;
    let t = Instant::now();
    let result = stack(frames, &StackOptions::default(), &mut |_| {}).expect("stack");
    let used = result.used();
    let combined = stats::sky_and_noise(&valerian_core::stars::luminance(&result.image.planes)).1;
    eprintln!(
        "real colour: stacked {used} of {} frames in {:.1?}, noise {single:.1} -> {combined:.1} ADU ({:.2}x), rejection {:?}",
        files.len(),
        t.elapsed(),
        single / combined,
        result.rejection
    );
    for (i, f) in result.frames.iter().enumerate() {
        if let Status::Skipped(why) = &f.status {
            eprintln!("real colour: frame {i} skipped: {why}");
        }
    }
    assert_eq!(result.image.planes.len(), 3);
    assert!(
        used * 10 >= files.len() * 9,
        "only {used} of {} frames registered",
        files.len()
    );
    assert!(
        single / combined > 0.7 * (used as f32).sqrt(),
        "noise only fell by {:.2}",
        single / combined
    );
}

/// Plate solving of real stacks: `VALERIAN_TEST_SOLVE` names a directory with
/// stacked FITS images whose headers carry a pointing (`OBJCTRA`/`OBJCTDEC`
/// or `RA`/`DEC`) and `FOCALLEN`/`XPIXSZ`. Each is solved against the shipped
/// Gaia catalogue; the solution must put at least 25 stars within a pixel or
/// two and its centre within 0.4 degrees of where the header says it points.
/// The owner's stacks (the ASI1600MM H-alpha stack of the California Nebula
/// field and a Seestar S50 stack of IC 5070) solve with 38 to 41 stars and
/// 0.1 to 0.4 pixel of residual; `scripts/compare_python_solve.py` checks the
/// same files against an independent solve.
#[test]
fn real_stacks_plate_solve_near_where_the_header_says() {
    use valerian_core::catalogue;
    use valerian_core::solve::{Hint, Options, distance, hint_from_header, solve};
    let Some(dir) = std::env::var_os("VALERIAN_TEST_SOLVE").map(PathBuf::from) else {
        eprintln!("skipping: set VALERIAN_TEST_SOLVE to a directory of stacked FITS images");
        return;
    };
    let cat = catalogue::open_default().expect("the shipped catalogue");
    let files = list(&dir, "", 8);
    assert!(!files.is_empty(), "no FITS files in {}", dir.display());
    for f in files {
        let img = fits::read(&f).expect("read");
        let h = hint_from_header(&img.header, (img.width(), img.height()));
        let (Some(pos), Some(scale)) = (h.position, h.scale) else {
            eprintln!("{}: no pointing in the header, skipped", f.display());
            continue;
        };
        let opts = Options {
            hint: Some(Hint {
                ra: pos.0,
                dec: pos.1,
                radius: 1.5,
            }),
            scale: (scale * 0.85, scale * 1.15),
            ..Options::default()
        };
        let t = Instant::now();
        let sol = solve(&img, &cat, &opts).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        eprintln!(
            "{}: {} stars, rms {:.2} px, centre {:.4} {:+.4}, {:.0} ms",
            f.display(),
            sol.matched,
            sol.rms_px,
            sol.centre.0,
            sol.centre.1,
            t.elapsed().as_secs_f64() * 1e3
        );
        assert!(sol.matched >= 25, "{}: {} stars", f.display(), sol.matched);
        assert!(sol.rms_px < 1.0, "{}: rms {}", f.display(), sol.rms_px);
        let off = distance(pos, sol.centre);
        assert!(
            off < 0.4,
            "{}: centre {off} degrees from the header",
            f.display()
        );
    }
}
