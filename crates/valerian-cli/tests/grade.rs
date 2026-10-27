// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `--grade` on `valerian subframes` and `valerian stack`, with the untrained
//! contract-test model: the scores mean nothing, the plumbing is what is checked.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::BayerPattern;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::testutil::Scene;
use valerian_core::transform::Transform;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn model() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../valerian-science/tests/data/untrained-seestar-subgrade.onnx")
}

/// Six made-up Seestar-sized subs: a star field painted onto a GRBG mosaic.
fn subs(dir: &Path, width: usize, height: usize, n: u64) {
    std::fs::create_dir_all(dir).expect("dir");
    let scene = Scene::random(250, width, height, 9);
    for k in 0..n {
        let view = Transform::similarity(1.0, 0.0, k as f64, -(k as f64));
        let mut img = Image::mono(
            scene.render(&view, 800.0, 4.0, 1.0, 0.0, 70 + k),
            Header::new(),
        );
        img.header.set("BAYERPAT", Value::Str("GRBG".into()), "");
        img.header.set("EXPTIME", Value::Float(10.0), "");
        img.planes[0].bayer = Some(BayerPattern::Grbg);
        fits::write(&dir.join(format!("s{k}.fits")), &img, Format::Float32).expect("write");
    }
}

#[test]
fn subframes_grade_adds_columns_and_rejects_below_min_grade() {
    let t = tempfile::tempdir().expect("tmp");
    subs(&t.path().join("in"), 1080, 1920, 4);
    let run = |min: &str, out: &str| {
        valerian()
            .args(["subframes", "--grade", "--min-grade", min])
            .arg("--grader-model")
            .arg(model())
            .arg("--report")
            .arg(t.path().join(out))
            .arg(t.path().join("in"))
            .assert()
            .success()
            .stderr(predicate::str::contains("4 of 4 frames graded"))
    };
    run("0", "keep");
    let csv = std::fs::read_to_string(t.path().join("keep/subframes.csv")).expect("csv");
    assert!(
        csv.lines()
            .next()
            .expect("header")
            .contains("grade_score,cloud_haze,trail,blur,dew,star_poor")
    );
    run("0.999", "drop");
    let csv = std::fs::read_to_string(t.path().join("drop/subframes.csv")).expect("csv");
    assert!(csv.contains("grade 0."), "{csv}");
    let json = std::fs::read_to_string(t.path().join("drop/subframes.json")).expect("json");
    assert!(json.contains("\"grade\""));
}

#[test]
fn frames_that_are_not_seestar_subs_are_skipped_not_fatal() {
    let t = tempfile::tempdir().expect("tmp");
    subs(&t.path().join("in"), 320, 240, 3);
    valerian()
        .args(["subframes", "--grade", "--grader-model"])
        .arg(model())
        .arg("--report")
        .arg(t.path().join("out"))
        .arg(t.path().join("in"))
        .assert()
        .success()
        .stderr(predicate::str::contains("not graded: it is 320 x 240"))
        .stderr(predicate::str::contains("nothing was graded"));
}

#[test]
fn stack_grade_filters_and_weights() {
    let t = tempfile::tempdir().expect("tmp");
    subs(&t.path().join("in"), 1080, 1920, 4);
    let stack = |min: &str| {
        let mut c = valerian();
        c.args(["stack", "--grade", "--grade-weights", "--min-grade", min])
            .arg("--grader-model")
            .arg(model())
            .arg("-o")
            .arg(t.path().join("out.fits"))
            .arg(t.path().join("in"));
        c
    };
    stack("0")
        .assert()
        .success()
        .stderr(predicate::str::contains("grade: 4 frames kept"));
    assert!(t.path().join("out.fits").exists());
    stack("0.999")
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipped: grade"))
        .stderr(predicate::str::contains("fewer than two frames"));
}

#[test]
fn the_built_in_model_grades_without_a_model_option() {
    let t = tempfile::tempdir().expect("tmp");
    subs(&t.path().join("in"), 1080, 1920, 3);
    valerian()
        .args(["subframes", "--grade", "--min-grade", "0", "--report"])
        .arg(t.path().join("out"))
        .arg(t.path().join("in"))
        .assert()
        .success()
        .stderr(predicate::str::contains("3 of 3 frames graded with"));
}

#[test]
fn a_missing_model_file_is_named() {
    let t = tempfile::tempdir().expect("tmp");
    subs(&t.path().join("in"), 320, 240, 2);
    valerian()
        .args([
            "subframes",
            "--grade",
            "--grader-model",
            "/nonexistent/model.onnx",
            "--report",
        ])
        .arg(t.path().join("out"))
        .arg(t.path().join("in"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("model.onnx"));
}
