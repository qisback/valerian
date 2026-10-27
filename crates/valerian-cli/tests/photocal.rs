// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian photocal` through the binary.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::stats::sky_and_noise;
use valerian_core::testutil::{ColourScene, colour_scene};

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

struct Case {
    scene: ColourScene,
    image: PathBuf,
    catalogue: PathBuf,
}

fn case(dir: &Path, with_wcs: bool, with_hint: bool) -> Case {
    let scene = colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    let mut header = Header::new();
    if with_wcs {
        scene.truth.write(&mut header);
    }
    if with_hint {
        header.set("RA", Value::Float(210.05), "");
        header.set("DEC", Value::Float(40.03), "");
        header.set("FOCALLEN", Value::Float(200.0), "");
        header.set("XPIXSZ", Value::Float(5.8), "");
    }
    let image = dir.join("colour.fits");
    fits::write(
        &image,
        &Image {
            header,
            planes: scene.image.planes.clone(),
        },
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

#[test]
fn the_gains_and_a_neutral_background_are_written() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true, false);
    let out = tmp.path().join("pcc.fits");
    let table = tmp.path().join("stars.csv");
    valerian()
        .arg("photocal")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .arg("-o")
        .arg(&out)
        .arg("--stars-out")
        .arg(&table)
        .assert()
        .success()
        .stdout(predicate::str::contains("Gaia stars measured"))
        .stdout(predicate::str::contains("red against green"))
        .stdout(predicate::str::contains("wrote"));
    let img = fits::read(&out).expect("calibrated image");
    let g = img.header.get_f64("PCCGAINR").expect("PCCGAINR");
    assert!((g / c.scene.true_gains[0] - 1.0).abs() < 0.03, "{g}");
    let b = img.header.get_f64("PCCGAINB").expect("PCCGAINB");
    assert!((b / c.scene.true_gains[2] - 1.0).abs() < 0.03, "{b}");
    let sky: Vec<f32> = img.planes.iter().map(|p| sky_and_noise(p).0).collect();
    assert!(
        (sky[0] - sky[1]).abs() < 1.0 && (sky[2] - sky[1]).abs() < 1.0,
        "{sky:?}"
    );
    let csv = std::fs::read_to_string(&table).expect("table");
    assert!(csv.starts_with("x,y,g,bp_rp,flux_r"));
    assert!(csv.lines().count() > 40);
}

/// Too few stars is a problem with the input, not a failed registration: the message says what was
/// found and which options to change (issue 126). Guards the wording against silently reverting.
#[test]
fn too_few_stars_for_the_fit_says_what_to_change_not_registration_failed() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true, false);
    valerian()
        .arg("photocal")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .args(["--min-stars", "100000"])
        .arg("-o")
        .arg(tmp.path().join("out.fits"))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "colour calibration needs at least 100000 usable stars and found",
        ))
        .stderr(predicate::str::contains("--min-stars"))
        .stderr(predicate::str::contains("--min-snr"))
        .stderr(predicate::str::contains("registration failed").not());
}

#[test]
fn an_image_without_a_wcs_is_solved_from_its_header() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), false, true);
    valerian()
        .arg("photocal")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .assert()
        .success()
        .stdout(predicate::str::contains("solving it from the header"))
        .stdout(predicate::str::contains("Gaia stars measured"));
}

#[test]
fn no_wcs_and_no_hint_says_to_solve_first() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), false, false);
    valerian()
        .arg("photocal")
        .arg(&c.image)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .assert()
        .failure()
        .stderr(predicate::str::contains("valerian solve"));
}

#[test]
fn a_mono_image_is_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true, false);
    let mono = tmp.path().join("mono.fits");
    let mut h = Header::new();
    c.scene.truth.write(&mut h);
    fits::write(
        &mono,
        &Image::mono(c.scene.image.planes[0].clone(), h),
        Format::Float32,
    )
    .expect("write");
    valerian()
        .arg("photocal")
        .arg(&mono)
        .arg("--catalogue")
        .arg(&c.catalogue)
        .assert()
        .failure()
        .stderr(predicate::str::contains("red, green and blue"));
}

#[test]
fn the_reference_colour_option_changes_the_gains() {
    let tmp = tempfile::tempdir().expect("tmp");
    let c = case(tmp.path(), true, false);
    let run = |extra: &[&str]| {
        let out = valerian()
            .arg("photocal")
            .arg(&c.image)
            .arg("--catalogue")
            .arg(&c.catalogue)
            .args(extra)
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let (sun, blue) = (run(&[]), run(&["--reference-colour", "0.3"]));
    assert_ne!(sun, blue);
}
