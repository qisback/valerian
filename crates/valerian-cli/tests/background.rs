// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian background` through the binary.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::Frame;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::stats;
use valerian_core::testutil::Rng;

const W: usize = 320;
const H: usize = 240;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

/// Sky 800 with a tilt of `slope` ADU per pixel across x and a few stars.
fn tilted(seed: u64, slope: f64) -> Frame {
    let mut rng = Rng::new(seed);
    let mut data: Vec<f32> = (0..W * H)
        .map(|i| (800.0 + slope * (i % W) as f64 + rng.gauss(0.0, 6.0)) as f32)
        .collect();
    for k in 0..40 {
        let (cx, cy) = ((k * 37 + 11) % (W - 12) + 6, (k * 53 + 7) % (H - 12) + 6);
        for dy in 0..9 {
            for dx in 0..9 {
                let r2 = (dx as f64 - 4.0).powi(2) + (dy as f64 - 4.0).powi(2);
                data[(cy + dy - 4) * W + cx + dx - 4] += (4000.0 * (-r2 / 4.0).exp()) as f32;
            }
        }
    }
    Frame::new(W, H, data).expect("frame")
}

fn save(dir: &Path, name: &str, planes: Vec<Frame>, header: Header) -> PathBuf {
    let path = dir.join(name);
    fits::write(&path, &Image { header, planes }, Format::Float32).expect("write");
    path
}

fn edge_difference(f: &Frame) -> f64 {
    let strip = |x0: usize| {
        let mut v = Vec::new();
        for y in 60..180 {
            for x in x0..x0 + 30 {
                v.push(f.get(x, y));
            }
        }
        f64::from(stats::median_finite(&v))
    };
    strip(W - 40) - strip(10)
}

#[test]
fn removes_a_tilt_from_every_plane_and_can_save_the_model() {
    let tmp = tempfile::tempdir().expect("tmp");
    let input = save(
        tmp.path(),
        "in.fits",
        vec![tilted(1, 0.5), tilted(2, -0.3)],
        Header::new(),
    );
    let out = tmp.path().join("out.fits");
    let model = tmp.path().join("model.fits");
    valerian()
        .args(["background", "--degree", "1", "-o"])
        .arg(&out)
        .arg("--save-model")
        .arg(&model)
        .arg(&input)
        .assert()
        .success()
        .stdout(predicate::str::contains("plane 0:"))
        .stdout(predicate::str::contains("plane 1:"));
    let before = fits::read(&input).expect("read");
    let after = fits::read(&out).expect("read");
    assert!(edge_difference(&before.planes[0]) > 100.0);
    assert!(edge_difference(&after.planes[0]).abs() < 4.0);
    assert!(edge_difference(&after.planes[1]).abs() < 4.0);
    // The model is the gradient: it carries the tilt, with no stars in it.
    let m = fits::read(&model).expect("model");
    assert_eq!(m.planes.len(), 2);
    assert!(edge_difference(&m.planes[0]) > 100.0);
    let history = after
        .header
        .cards()
        .iter()
        .any(|c| c.key == "HISTORY" && c.comment.contains("valerian background"));
    assert!(history);
}

#[test]
fn zero_level_takes_the_sky_to_zero_and_divide_keeps_it_near_one() {
    let tmp = tempfile::tempdir().expect("tmp");
    let input = save(tmp.path(), "in.fits", vec![tilted(3, 0.4)], Header::new());
    let zero = tmp.path().join("zero.fits");
    valerian()
        .args(["background", "--degree", "1", "--level", "zero", "-o"])
        .arg(&zero)
        .arg(&input)
        .assert()
        .success();
    let z = fits::read(&zero).expect("read");
    let med = stats::median_finite(&z.plane().data);
    assert!(med.abs() < 5.0, "sky {med}");
    let ratio = tmp.path().join("ratio.fits");
    valerian()
        .args([
            "background",
            "--degree",
            "1",
            "--mode",
            "divide",
            "--level",
            "zero",
            "-o",
        ])
        .arg(&ratio)
        .arg(&input)
        .assert()
        .success();
    let r = fits::read(&ratio).expect("read");
    let med = stats::median_finite(&r.plane().data);
    assert!((med - 1.0).abs() < 0.02, "sky {med}");
}

#[test]
fn a_target_that_fills_the_frame_is_left_alone_with_a_warning() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut f = tilted(4, 0.0);
    for y in 0..H {
        for x in 0..W {
            let r2 = (x as f64 - 160.0).powi(2) + (y as f64 - 120.0).powi(2);
            f.data[y * W + x] += (2500.0 * (-r2 / (2.0 * 150.0 * 150.0)).exp()) as f32;
        }
    }
    let input = save(tmp.path(), "in.fits", vec![f.clone()], Header::new());
    let out = tmp.path().join("out.fits");
    valerian()
        .args(["background", "--degree", "1", "--exclude-target", "-o"])
        .arg(&out)
        .arg(&input)
        .assert()
        .success()
        .stderr(predicate::str::contains("declined"));
    let after = fits::read(&out).expect("read");
    assert_eq!(after.plane().data, f.data, "a declined plane is unchanged");
}

#[test]
fn raw_mosaics_and_bad_options_are_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut header = Header::new();
    header.set("BAYERPAT", Value::Str("RGGB".into()), "");
    let mosaic = save(tmp.path(), "mosaic.fits", vec![tilted(5, 0.1)], header);
    let out = tmp.path().join("o.fits");
    valerian()
        .args(["background", "-o"])
        .arg(&out)
        .arg(&mosaic)
        .assert()
        .failure()
        .stderr(predicate::str::contains("raw colour mosaic"));
    let plain = save(
        tmp.path(),
        "plain.fits",
        vec![tilted(6, 0.1)],
        Header::new(),
    );
    valerian()
        .args(["background", "--degree", "9", "-o"])
        .arg(&out)
        .arg(&plain)
        .assert()
        .failure()
        .stderr(predicate::str::contains("degree"));
    assert!(!out.exists());
}
