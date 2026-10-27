// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian stretch` through the binary.

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::Frame;
use valerian_core::fits::{self, Format, Header, Image};
use valerian_core::stats;
use valerian_core::stretch::Stretch;
use valerian_core::testutil::Rng;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

/// A noisy sky at 300 ADU with a ramp of bright structure.
fn linear_image(dir: &std::path::Path) -> (std::path::PathBuf, Frame) {
    let mut rng = Rng::new(21);
    let (w, h) = (96, 64);
    let data: Vec<f32> = (0..w * h)
        .map(|i| {
            let ramp = if i % w > 70 {
                (i % w - 70) as f32 * 40.0
            } else {
                0.0
            };
            300.0 + ramp + rng.gauss(0.0, 8.0) as f32
        })
        .collect();
    let frame = Frame::new(w, h, data).expect("frame");
    let path = dir.join("linear.fits");
    fits::write(
        &path,
        &Image::mono(frame.clone(), Header::new()),
        Format::Float32,
    )
    .expect("write");
    (path, frame)
}

#[test]
fn auto_puts_the_sky_at_the_target_and_records_the_stretch() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (input, _) = linear_image(tmp.path());
    let out = tmp.path().join("out.fits");
    valerian()
        .args(["stretch", "-o"])
        .arg(&out)
        .arg(&input)
        .args(["--target", "0.25"])
        .assert()
        .success()
        .stdout(predicate::str::contains("median"));
    let img = fits::read(&out).expect("read");
    let med = stats::median_finite(&img.plane().data);
    assert!((med - 0.25).abs() < 0.02, "sky at {med}");
    assert!(Stretch::from_header(&img.header).is_some());
}

#[test]
fn ghs_then_invert_gets_the_data_back() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (input, frame) = linear_image(tmp.path());
    let stretched = tmp.path().join("s.fits");
    let restored = tmp.path().join("r.fits");
    valerian()
        .args([
            "stretch",
            "--curve",
            "ghs",
            "--ghs-stretch",
            "4",
            "--ghs-b",
            "-1",
        ])
        .args([
            "--ghs-sp", "0.05", "--black", "100", "--white", "3500", "-o",
        ])
        .arg(&stretched)
        .arg(&input)
        .assert()
        .success();
    valerian()
        .args(["stretch", "--invert", "-o"])
        .arg(&restored)
        .arg(&stretched)
        .assert()
        .success();
    let back = fits::read(&restored).expect("read");
    assert!(Stretch::from_header(&back.header).is_none());
    let worst = back
        .plane()
        .data
        .iter()
        .zip(&frame.data)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(worst < 0.5, "worst difference {worst} ADU");
}

#[test]
fn bad_options_are_refused_with_a_reason() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (input, _) = linear_image(tmp.path());
    let out = tmp.path().join("o.fits");
    valerian()
        .args(["stretch", "--black", "10", "-o"])
        .arg(&out)
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("both --black and --white"));
    valerian()
        .args([
            "stretch", "--curve", "ghs", "--ghs-lp", "0.9", "--ghs-sp", "0.1", "-o",
        ])
        .arg(&out)
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("LP <= SP <= HP"));
    // An unstretched image has nothing to undo.
    valerian()
        .args(["stretch", "--invert", "-o"])
        .arg(&out)
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("records no stretch"));
    assert!(!out.exists());
}

#[test]
fn preview_alone_writes_a_png_and_no_fits() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (input, _) = linear_image(tmp.path());
    let png = tmp.path().join("p.png");
    valerian()
        .args(["stretch", "--preview"])
        .arg(&png)
        .args(["--preview-size", "48"])
        .arg(&input)
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote preview"));
    let bytes = std::fs::read(&png).expect("png");
    assert_eq!(&bytes[1..4], b"PNG");
    let dec = std::fs::File::open(&png).expect("open");
    let r = png::Decoder::new(std::io::BufReader::new(dec))
        .read_info()
        .expect("decode");
    assert_eq!((r.info().width, r.info().height), (48, 32));
}

#[test]
fn stretch_needs_an_output_or_a_preview() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (input, _) = linear_image(tmp.path());
    valerian().arg("stretch").arg(&input).assert().failure();
}
