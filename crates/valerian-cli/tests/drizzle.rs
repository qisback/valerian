// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian drizzle` through the binary.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::testutil::{Rng, Scene};
use valerian_core::transform::Transform;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

/// Twelve dithered renderings of a made-up undersampled sky.
fn frames(dir: &Path) {
    let mut rng = Rng::new(4);
    let scene = Scene {
        width: 320,
        height: 240,
        stars: (0..60)
            .map(|_| {
                (
                    8.0 + rng.uniform() * 304.0,
                    8.0 + rng.uniform() * 224.0,
                    6000.0 + 30000.0 * rng.uniform(),
                    0.7,
                )
            })
            .collect(),
    };
    std::fs::create_dir_all(dir).expect("dir");
    for k in 0..12u64 {
        let (dx, dy) = if k == 0 {
            (0.0, 0.0)
        } else {
            (rng.uniform() * 6.0 - 3.0, rng.uniform() * 6.0 - 3.0)
        };
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let mut img = Image::mono(
            scene.render(&view, 300.0, 3.0, 1.0, 0.0, 40 + k),
            Header::new(),
        );
        img.header.set("EXPTIME", Value::Float(60.0), "");
        fits::write(&dir.join(format!("f{k:02}.fits")), &img, Format::Float32).expect("write");
    }
}

#[test]
fn frames_are_drizzled_onto_a_finer_grid() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = tmp.path().join("frames");
    frames(&dir);
    let out = tmp.path().join("drz.fits");
    let weight = tmp.path().join("weight.fits");
    let report = tmp.path().join("report.txt");
    valerian()
        .arg("drizzle")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .arg("--weight-map")
        .arg(&weight)
        .arg("--report")
        .arg(&report)
        .args(["--scale", "2", "--pixfrac", "0.8", "--reference", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "12 of 12 frames drizzled to 640 x 480",
        ))
        .stdout(predicate::str::contains("wrote"));
    let img = fits::read(&out).expect("output");
    assert_eq!((img.width(), img.height()), (640, 480));
    assert_eq!(img.header.get_f64("DRIZSCL"), Some(2.0));
    assert_eq!(img.header.get_f64("DRIZPIXF"), Some(0.8));
    assert_eq!(img.header.get("NCOMBINE"), Some(&Value::Int(12)));
    assert_eq!(img.header.get_str("STACKALG"), Some("drizzle"));
    let w = fits::read(&weight).expect("weight map");
    assert_eq!(w.width(), 640);
    assert!(
        std::fs::read_to_string(&report)
            .expect("report")
            .contains("stacked")
    );
}

#[test]
fn rejection_can_be_turned_off_and_its_limit_set() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = tmp.path().join("frames");
    frames(&dir);
    let out = tmp.path().join("drz.fits");
    valerian()
        .arg("drizzle")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .args(["--scale", "1", "--pixfrac", "1", "--no-reject"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "0.00 % of the input samples rejected",
        ));
    valerian()
        .arg("drizzle")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .args([
            "--scale",
            "1",
            "--pixfrac",
            "1",
            "--reject",
            "3",
            "--no-reject",
        ])
        .assert()
        .failure();
}

#[test]
fn bad_settings_are_refused_with_the_reason() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = tmp.path().join("frames");
    frames(&dir);
    let out = tmp.path().join("drz.fits");
    valerian()
        .arg("drizzle")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .args(["--pixfrac", "1.5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pixfrac"));
    assert!(!out.exists(), "nothing is written on failure");
}
