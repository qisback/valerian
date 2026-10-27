// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian stack --memory` and `--scratch-dir`: the stack does not depend on them.

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::testutil::{Rng, Scene};
use valerian_core::transform::Transform;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn frames(dir: &std::path::Path) {
    let scene = Scene::random(100, 300, 220, 3);
    let mut rng = Rng::new(4);
    for k in 0..9 {
        let view = Transform::rotation_about(
            150.0,
            110.0,
            (rng.uniform() - 0.5) * 0.01,
            (rng.uniform() - 0.5) * 20.0,
            (rng.uniform() - 0.5) * 20.0,
        );
        let f = scene.render(&view, 500.0, 8.0, 1.0, 0.0, 30 + k);
        let mut img = Image::mono(f, Header::new());
        img.header.set("EXPTIME", Value::Float(10.0), "");
        fits::write(&dir.join(format!("f{k}.fits")), &img, Format::Float32).expect("write");
    }
}

#[test]
fn a_tight_memory_limit_streams_through_a_scratch_file_and_gives_the_same_stack() {
    let tmp = tempfile::tempdir().expect("tmp");
    let input = tmp.path().join("in");
    std::fs::create_dir(&input).expect("dir");
    frames(&input);
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("dir");
    let (a, b) = (tmp.path().join("a.fits"), tmp.path().join("b.fits"));
    valerian()
        .args(["stack", "--backend", "cpu", "-o"])
        .arg(&a)
        .arg(&input)
        .assert()
        .success();
    valerian()
        .args([
            "stack",
            "--backend",
            "cpu",
            "--memory",
            "1",
            "--scratch-dir",
        ])
        .arg(&scratch)
        .arg("-o")
        .arg(&b)
        .arg(&input)
        .assert()
        .success();
    let (x, y) = (fits::read(&a).expect("a"), fits::read(&b).expect("b"));
    for (p, q) in x.planes[0].data.iter().zip(&y.planes[0].data) {
        assert!(p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan()));
    }
    assert_eq!(
        std::fs::read_dir(&scratch).expect("dir").count(),
        0,
        "staging file removed"
    );
}

#[test]
fn stacking_a_folder_does_not_report_reading_one_frame() {
    let tmp = tempfile::tempdir().expect("tmp");
    let input = tmp.path().join("in");
    std::fs::create_dir(&input).expect("dir");
    frames(&input);
    valerian()
        .args(["stack", "--backend", "cpu", "-o"])
        .arg(tmp.path().join("a.fits"))
        .arg(&input)
        .assert()
        .success()
        .stderr(predicate::str::contains("read 1 frames").not());
}
