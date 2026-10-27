// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian solve` through the binary.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::testutil::{PlateScene, plate_scene};
use valerian_core::wcs::{Wcs, separation};

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

struct Case {
    scene: PlateScene,
    image: PathBuf,
    catalogue: PathBuf,
}

/// A synthetic field on disk, with `header` cards chosen by the test.
fn case(dir: &Path, with_hint: bool) -> Case {
    let scene = plate_scene((150.0, 30.0), 900, 600, 6.0, 25.0, 31);
    let mut header = Header::new();
    if with_hint {
        header.set("RA", Value::Float(150.1), "");
        header.set("DEC", Value::Float(30.05), "");
        header.set("FOCALLEN", Value::Float(200.0), "");
        header.set("XPIXSZ", Value::Float(5.8), "");
    }
    let image = dir.join("field.fits");
    fits::write(
        &image,
        &Image::mono(scene.frame.clone(), header),
        Format::Float32,
    )
    .expect("write image");
    let catalogue = dir.join("cat.vcat");
    std::fs::write(&catalogue, scene.catalogue.as_bytes()).expect("write catalogue");
    Case {
        scene,
        image,
        catalogue,
    }
}

fn worst_arcsec(a: &Wcs, b: &Wcs) -> f64 {
    [
        (0.0, 0.0),
        (899.0, 0.0),
        (0.0, 599.0),
        (899.0, 599.0),
        (450.0, 300.0),
    ]
    .iter()
    .map(|&(x, y)| {
        let (p, q) = (a.pixel_to_world(x, y), b.pixel_to_world(x, y));
        separation(p.0, p.1, q.0, q.1) * 3600.0
    })
    .fold(0.0, f64::max)
}

#[test]
fn the_header_is_the_hint_and_the_wcs_is_written() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true);
    let out = tmp.path().join("solved.fits");
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("solved: centre RA"))
        .stdout(predicate::str::contains("stars matched"))
        .stdout(predicate::str::contains("wrote"));
    let img = fits::read(&out).expect("solved image");
    assert_eq!(img.header.get_str("CTYPE1"), Some("RA---TAN"));
    let wcs = Wcs::from_header(&img.header).expect("wcs");
    assert!(worst_arcsec(&wcs, &c.scene.truth) < 2.0);
    assert!(
        img.header
            .cards()
            .iter()
            .any(|k| k.key == "HISTORY" && k.comment.contains("valerian solve"))
    );
    // The pixels are untouched.
    assert_eq!(img.plane().data, c.scene.frame.data);
}

#[test]
fn a_position_and_scale_on_the_command_line_replace_the_header() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), false);
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--ra", "10:00:24", "--dec", "+30:03:00", "--scale", "6.0"])
        .args(["--radius", "0.5", "--sip-order", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plain TAN"));
}

#[test]
fn without_a_position_it_says_what_to_give_and_blind_works_with_a_scale() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), false);
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--scale"));
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--scale-range", "5", "7.5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("searching the whole sky"));
}

#[test]
fn a_wrong_position_fails_without_writing_anything() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true);
    let out = tmp.path().join("nope.fits");
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--ra", "150", "--dec", "-50", "--radius", "0.3", "-o"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no match"));
    assert!(!out.exists());
}

#[test]
fn bad_arguments_and_a_missing_catalogue_are_refused_with_a_reason() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true);
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--sip-order", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--sip-order"));
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(tmp.path().join("missing.vcat"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing.vcat"));
    valerian()
        .arg("solve")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--ra", "banana", "--dec", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("angle"));
}
