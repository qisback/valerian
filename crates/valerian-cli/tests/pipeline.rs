// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! End to end: calibrate, register and stack a synthetic session through the
//! `valerian` binary.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::Frame;
use valerian_core::fits::{self, Format, Image, Value};
use valerian_core::stats;
use valerian_core::testutil::{Rng, Scene, Sensor};
use valerian_core::transform::Transform;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn save(dir: &Path, name: &str, img: &Image) {
    std::fs::create_dir_all(dir).expect("dir");
    fits::write(&dir.join(name), img, Format::Float32).expect("write");
}

struct Session {
    root: tempfile::TempDir,
}

/// Bias, dark, flat and dark-flat frames, and six lights of one sky seen
/// through a different pointing each time.
fn session() -> Session {
    let root = tempfile::tempdir().expect("tempdir");
    let r = root.path();
    let (w, h) = (320, 240);
    let sensor = Sensor::new(w, h, false, 5);
    let mut rng = Rng::new(6);
    let zero = Frame::zeros(w, h);
    for i in 0..8 {
        save(
            &r.join("bias"),
            &format!("b{i}.fits"),
            &sensor.expose(&zero, 0.0, &mut rng),
        );
        save(
            &r.join("dark"),
            &format!("d{i}.fits"),
            &sensor.expose(&zero, 120.0, &mut rng),
        );
        save(
            &r.join("dflat"),
            &format!("df{i}.fits"),
            &sensor.expose(&zero, 2.0, &mut rng),
        );
    }
    let lamp = Frame::new(w, h, vec![20_000.0; w * h]).expect("lamp");
    for i in 0..8 {
        save(
            &r.join("flat"),
            &format!("f{i}.fits"),
            &sensor.expose(&lamp, 2.0, &mut rng),
        );
    }
    let scene = Scene::random(70, w, h, 7);
    for k in 0..6 {
        let view = Transform::rotation_about(
            w as f64 / 2.0,
            h as f64 / 2.0,
            (rng.uniform() - 0.5) * 0.006,
            (rng.uniform() - 0.5) * 14.0,
            (rng.uniform() - 0.5) * 14.0,
        );
        let sky = scene.render(&view, 40.0, 0.0, 1.0, 0.0, 1);
        save(
            &r.join("lights"),
            &format!("l{k}.fits"),
            &sensor.expose(&sky, 120.0, &mut rng),
        );
    }
    Session { root }
}

#[test]
fn calibrate_then_stack() {
    let s = session();
    let r = s.root.path();
    valerian()
        .arg("calibrate")
        .arg(r.join("lights"))
        .args(["--bias"])
        .arg(r.join("bias"))
        .args(["--dark"])
        .arg(r.join("dark"))
        .args(["--flat"])
        .arg(r.join("flat"))
        .args(["--dark-flat"])
        .arg(r.join("dflat"))
        .args(["-o"])
        .arg(r.join("cal"))
        .args(["--masters"])
        .arg(r.join("masters"))
        .assert()
        .success()
        .stdout(predicate::str::contains("l0_cal.fits"))
        .stdout(predicate::str::contains("pixels repaired"));
    for m in [
        "master_bias",
        "master_dark",
        "master_dark_flat",
        "master_flat",
    ] {
        assert!(r.join("masters").join(format!("{m}.fits")).is_file(), "{m}");
    }
    let cal = fits::read(&r.join("cal/l0_cal.fits")).expect("calibrated frame");
    assert_eq!(cal.header.get_str("IMAGETYP"), Some("Calibrated Light"));
    let (sky, _) = stats::sky_and_noise(cal.plane());
    // The flat-fielded sky is the 40 ADU of sky signal times the mean response.
    assert!((10.0..45.0).contains(&sky), "calibrated sky {sky}");

    valerian()
        .arg("stack")
        .arg(r.join("cal"))
        .args(["-o"])
        .arg(r.join("stack.fits"))
        .args(["--min-stars", "10", "--report"])
        .arg(r.join("report.txt"))
        .args(["--coverage"])
        .arg(r.join("coverage.fits"))
        .assert()
        .success()
        .stdout(predicate::str::contains("6 of 6 frames stacked"));
    let stacked = fits::read(&r.join("stack.fits")).expect("stack");
    assert_eq!(stacked.header.get("NCOMBINE"), Some(&Value::Int(6)));
    assert_eq!(stacked.header.get_f64("EXPTOTAL"), Some(720.0));
    let (_, single) = stats::sky_and_noise(cal.plane());
    let (_, combined) = stats::sky_and_noise(stacked.plane());
    assert!(combined < 0.7 * single, "noise {single} -> {combined}");
    let report = std::fs::read_to_string(r.join("report.txt")).expect("report");
    assert_eq!(
        report.matches("stacked").count() + report.matches("reference").count(),
        6
    );
    let coverage = fits::read(&r.join("coverage.fits")).expect("coverage");
    assert_eq!(coverage.plane().min_max().1, 6.0);
}

#[test]
fn register_writes_frames_on_the_reference_grid() {
    let s = session();
    let r = s.root.path();
    valerian()
        .arg("register")
        .arg(r.join("lights"))
        .args(["-o"])
        .arg(r.join("reg"))
        .assert()
        .success()
        .stdout(predicate::str::contains("registered 6 of 6 frames"));
    let n = std::fs::read_dir(r.join("reg")).expect("dir").count();
    assert_eq!(n, 6);
    let one = fits::read(&r.join("reg/l1_reg.fits")).expect("registered frame");
    assert!(one.header.get("REGREF").is_some());
    assert_eq!((one.width(), one.height()), (320, 240));
}

#[test]
fn info_describes_a_file_and_measures_it() {
    let s = session();
    let r = s.root.path();
    valerian()
        .arg("info")
        .arg(r.join("lights/l0.fits"))
        .assert()
        .success()
        .stdout(predicate::str::contains("EXPTIME"));
    valerian()
        .arg("info")
        .arg(r.join("lights/l0.fits"))
        .arg("--stats")
        .assert()
        .success()
        .stdout(predicate::str::contains("320 x 240 pixels"))
        .stdout(predicate::str::contains("stars:"));
}

#[test]
fn mistakes_are_reported_plainly() {
    let s = session();
    let r = s.root.path();
    valerian()
        .arg("stack")
        .arg(r.join("nowhere"))
        .args(["-o"])
        .arg(r.join("x.fits"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
    // Calibrating with nothing to calibrate with.
    valerian()
        .arg("calibrate")
        .arg(r.join("lights"))
        .args(["-o"])
        .arg(r.join("out"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to calibrate with"));
    // Too few frames to stack.
    valerian()
        .arg("stack")
        .arg(r.join("lights/l0.fits"))
        .args(["-o"])
        .arg(r.join("x.fits"))
        .args(["--min-stars", "10"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("fewer than two"));
}

#[test]
fn auto_cosmetic_alone_runs_a_cosmetic_only_pass() {
    let s = session();
    let r = s.root.path();
    valerian()
        .arg("calibrate")
        .arg(r.join("lights"))
        .arg("--auto-cosmetic")
        .args(["-o"])
        .arg(r.join("cosmetic"))
        .assert()
        .success()
        .stdout(predicate::str::contains("pixels repaired"));
    assert!(r.join("cosmetic").read_dir().expect("out").count() >= 6);
}

#[test]
fn low_and_high_apply_to_the_scheme_auto_picks() {
    let s = session();
    let r = s.root.path();
    // Six lights: auto picks Winsorised (3 and 3 by default).
    let stack = |extra: &[&str]| {
        let out = valerian()
            .arg("stack")
            .arg(r.join("lights"))
            .args(["-o"])
            .arg(r.join("s.fits"))
            .args(extra)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).expect("utf8")
    };
    assert!(stack(&[]).contains("Winsorised { low: 3.0, high: 3.0 }"));
    assert!(stack(&["--low", "2", "--high", "2.5"]).contains("Winsorised { low: 2.0, high: 2.5 }"));
    assert!(stack(&["--high", "1.5"]).contains("Winsorised { low: 3.0, high: 1.5 }"));
}

#[cfg(unix)]
#[test]
fn a_closed_pipe_ends_quietly() {
    use std::process::{Command as Std, Stdio};
    // `head -c0` exits at once, so `info --gpu` (which probes the adapters
    // first) writes into a pipe nobody reads.
    let mut v = Std::new(assert_cmd::cargo::cargo_bin("valerian"))
        .args(["info", "--gpu"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn valerian");
    let mut head = Std::new("head")
        .arg("-c0")
        .stdin(Stdio::from(v.stdout.take().expect("stdout")))
        .spawn()
        .expect("spawn head");
    head.wait().expect("head");
    let out = v.wait_with_output().expect("valerian");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "stderr: {err}");
    assert!(
        out.status.success(),
        "status {:?}, stderr: {err}",
        out.status
    );
}
