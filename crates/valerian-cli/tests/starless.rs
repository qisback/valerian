// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian starless` through the binary.

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image};
use valerian_core::testutil::deep_sky;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn input(dir: &std::path::Path) -> (std::path::PathBuf, Image) {
    let sky = deep_sky(240, 160, 50, 9);
    let img = Image::mono(sky.frame, Header::new());
    let path = dir.join("in.fits");
    fits::write(&path, &img, Format::Float32).expect("write");
    (path, img)
}

#[test]
fn removes_stars_and_writes_every_output() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (path, img) = input(tmp.path());
    let (out, stars, mask, png) = (
        tmp.path().join("s.fits"),
        tmp.path().join("st.fits"),
        tmp.path().join("m.fits"),
        tmp.path().join("p.png"),
    );
    valerian()
        .arg("starless")
        .arg(&path)
        .arg("-o")
        .arg(&out)
        .arg("--stars-out")
        .arg(&stars)
        .arg("--mask-out")
        .arg(&mask)
        .arg("--preview")
        .arg(&png)
        .assert()
        .success()
        .stdout(predicate::str::contains("stars found"))
        .stdout(predicate::str::contains("wrote preview"));
    let s = fits::read(&out).expect("starless");
    let layer = fits::read(&stars).expect("stars");
    let m = fits::read(&mask).expect("mask");
    assert!(m.plane().data.iter().any(|&v| v > 0.5));
    let history = s
        .header
        .cards()
        .iter()
        .filter(|c| c.key == "HISTORY")
        .count();
    assert!(history >= 1);
    for i in 0..img.plane().data.len() {
        let sum = s.plane().data[i] + layer.plane().data[i];
        assert!((sum - img.plane().data[i]).abs() < 1e-2 * img.plane().data[i].abs().max(1.0));
    }
    assert_eq!(&std::fs::read(&png).expect("png")[1..4], b"PNG");
}

#[test]
fn strength_zero_returns_the_input() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (path, img) = input(tmp.path());
    let out = tmp.path().join("s.fits");
    valerian()
        .arg("starless")
        .arg(&path)
        .args(["--strength", "0", "-o"])
        .arg(&out)
        .assert()
        .success();
    assert_eq!(
        fits::read(&out).expect("read").plane().data,
        img.plane().data
    );
}

#[test]
fn bad_strength_is_refused_and_an_output_is_required() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (path, _) = input(tmp.path());
    let out = tmp.path().join("s.fits");
    valerian()
        .arg("starless")
        .arg(&path)
        .args(["--strength", "2", "-o"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("strength"));
    assert!(!out.exists());
    valerian().arg("starless").arg(&path).assert().failure();
}
