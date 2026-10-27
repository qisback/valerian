// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian deconvolve` through the binary.

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image};
use valerian_core::testutil::{Rng, gaussian_field_elliptical};

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn input(dir: &std::path::Path) -> std::path::PathBuf {
    let mut rng = Rng::new(7);
    let stars: Vec<(f64, f64, f64, f64, f64)> = (0..60)
        .map(|_| {
            (
                20.0 + rng.uniform() * 320.0,
                20.0 + rng.uniform() * 260.0,
                4000.0 + rng.uniform() * 20000.0,
                1.9,
                1.3,
            )
        })
        .collect();
    let mut f = gaussian_field_elliptical(360, 300, 500.0, &stars);
    for v in &mut f.data {
        *v += rng.gauss(0.0, 6.0) as f32;
    }
    let path = dir.join("in.fits");
    fits::write(&path, &Image::mono(f, Header::new()), Format::Float32).expect("write");
    path
}

#[test]
fn sharpens_and_writes_the_report() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = input(tmp.path());
    let (out, csv) = (tmp.path().join("o.fits"), tmp.path().join("t.csv"));
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .arg("-o")
        .arg(&out)
        .arg("--report")
        .arg(&csv)
        .assert()
        .success()
        .stdout(predicate::str::contains("deconvolved 1 plane(s)"))
        .stdout(predicate::str::contains("round, FWHM"));
    assert!(fits::read(&out).is_ok());
    let text = std::fs::read_to_string(&csv).expect("csv");
    assert!(text.starts_with("x0,y0,x1,y1,stars,psf_fwhm_px,psf_ellipticity"));
    assert!(text.lines().count() >= 2);
}

#[test]
fn bad_tiles_and_too_few_stars_are_plain_errors() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = input(tmp.path());
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "x.fits", "--tiles", "three"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("ACROSSxDOWN"));
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "x.fits", "--psf-min-snr", "100000"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--psf-fwhm"));
}

#[test]
fn roundness_saturation_and_ring_guard_are_accepted_and_checked() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = input(tmp.path());
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "a.fits", "--roundness", "1", "--saturation", "off"])
        .args(["--ring-guard", "0.5"])
        .current_dir(tmp.path())
        .assert()
        .success();
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "b.fits", "--saturation", "60000"])
        .current_dir(tmp.path())
        .assert()
        .success();
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "c.fits", "--saturation", "bright"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--saturation takes auto, off or"));
    valerian()
        .arg("deconvolve")
        .arg(&path)
        .args(["-o", "d.fits", "--roundness", "2"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("roundness"));
}
