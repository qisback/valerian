// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Location and device-identifying headers are never written (#124).
//!
//! The inputs carry a made-up Seestar-style header: site coordinates, a site
//! name and device serial numbers. Every file the commands write must be free
//! of them, unless `--keep-location` is given.

use std::path::Path;

use assert_cmd::Command;
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::testutil::{Rng, Scene};
use valerian_core::transform::Transform;

/// Made-up values that appear nowhere else, searched for in every output.
const SENTINELS: &[&str] = &[
    "12.3456",
    "65.4321",
    "SYNTH-SN-0001",
    "SYNTH-CAM-77",
    "Synthville",
    "0badf00d",
    "0BADF00D",
    "77665544",
];

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

fn identifying() -> Vec<(&'static str, Value)> {
    vec![
        ("SITELAT", Value::Float(12.3456)),
        ("SITELONG", Value::Float(65.4321)),
        ("SITEELEV", Value::Float(123.0)),
        ("OBSGEO-X", Value::Float(1.0e6)),
        ("OBSGEO-Y", Value::Float(2.0e6)),
        ("OBSGEO-Z", Value::Float(3.0e6)),
        ("OBSGEO-L", Value::Float(65.4321)),
        ("OBSGEO-B", Value::Float(12.3456)),
        ("OBSGEO-H", Value::Float(123.0)),
        ("LAT-OBS", Value::Float(12.3456)),
        ("LONG-OBS", Value::Float(65.4321)),
        ("ALT-OBS", Value::Float(123.0)),
        ("OBSERVAT", Value::Str("Synthville".into())),
        ("SERIALNO", Value::Str("SYNTH-SN-0001".into())),
        ("CAMSERIA", Value::Str("SYNTH-CAM-77".into())),
    ]
}

fn frames(dir: &Path) {
    std::fs::create_dir_all(dir).expect("dir");
    let mut rng = Rng::new(5);
    let scene = Scene::random(70, 320, 240, 6);
    for k in 0..4u64 {
        let (dx, dy) = (rng.uniform() * 3.0 - 1.5, rng.uniform() * 3.0 - 1.5);
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let mut header = Header::new();
        header.set(
            "DATE-OBS",
            Value::Str(format!("2026-01-01T20:{:02}:00", 10 * k)),
            "",
        );
        // Seestar-style: the model, then the per-unit identifier (made up).
        header.set("INSTRUME", Value::Str("Synthetic S0".into()), "");
        header.set("TELESCOP", Value::Str("S50_0badf00d".into()), "");
        header.set("CAMNAME", Value::Str("Synthetic S0 SN 77665544".into()), "");
        header.add_history("captured by unit S50_0badf00d");
        for (key, v) in identifying() {
            header.set(key, v, "");
        }
        let img = Image::mono(scene.render(&view, 400.0, 4.0, 1.0, 0.0, 40 + k), header);
        // Written with the cards kept: this is the input a user would bring.
        let bytes = fits::to_bytes_with(&img, Format::Float32, true).expect("bytes");
        std::fs::write(dir.join(format!("f{k}.fits")), bytes).expect("write");
    }
}

fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).expect("dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// No file under `dir` contains a sentinel; FITS files also carry none of the keys.
fn assert_clean(dir: &Path) {
    let mut all = Vec::new();
    files(dir, &mut all);
    for p in all.iter().filter(|p| !p.starts_with(dir.join("lights"))) {
        let bytes = std::fs::read(p).expect("read");
        let text = String::from_utf8_lossy(&bytes);
        for s in SENTINELS {
            assert!(!text.contains(s), "{} holds {s}", p.display());
        }
        if p.extension().is_some_and(|e| e == "fits") {
            let h = fits::read_header(p).expect("header");
            for (key, _) in identifying() {
                assert!(h.get(key).is_none(), "{} holds {key}", p.display());
            }
            for c in h.cards() {
                let text = format!("{:?}{}", c.value, c.comment).to_ascii_lowercase();
                assert!(!text.contains("0badf00d"), "{} card {}", p.display(), c.key);
            }
            if let Some(t) = h.get_str("TELESCOP") {
                assert_eq!(t, "S50", "{} TELESCOP", p.display());
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "stack.fits" || name == "stretched.fits" {
                assert!(h.get("INSTRUME").is_some(), "{} lost INSTRUME", p.display());
                assert_eq!(h.get_str("TELESCOP"), Some("S50"), "{} model", p.display());
            }
        }
    }
}

#[test]
fn every_writer_drops_location_and_serials() {
    let dir = tempfile::tempdir().expect("tmp");
    let d = dir.path();
    frames(&d.join("lights"));
    let lights = d.join("lights");
    // FITS writer (stack), coverage map and the stack's science report.
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(d.join("stack.fits"))
        .arg("--coverage")
        .arg(d.join("cov.fits"))
        .arg("--report")
        .arg(d.join("stack-report.txt"))
        .arg("--science-report")
        .arg(d.join("sci"))
        .assert()
        .success();
    // FITS and PNG writers (stretch), starting from the stack.
    valerian()
        .arg("stretch")
        .arg(d.join("stack.fits"))
        .arg("-o")
        .arg(d.join("stretched.fits"))
        .arg("--preview")
        .arg(d.join("preview.png"))
        .assert()
        .success();
    // The subframe report: CSV, JSON, HTML, PNG.
    valerian()
        .arg("subframes")
        .arg(&lights)
        .arg("--report")
        .arg(d.join("sub"))
        .assert()
        .success();
    // The background-extraction writer.
    valerian()
        .arg("background")
        .arg(d.join("stack.fits"))
        .arg("-o")
        .arg(d.join("flat.fits"))
        .assert()
        .success();
    // The calibrate and register writers.
    valerian()
        .arg("calibrate")
        .arg(&lights)
        .arg("-o")
        .arg(d.join("cal"))
        .arg("--auto-cosmetic")
        .assert()
        .success();
    valerian()
        .arg("register")
        .arg(&lights)
        .arg("-o")
        .arg(d.join("reg"))
        .assert()
        .success();
    assert_clean(d);
}

#[test]
fn keep_location_keeps_the_headers() {
    let dir = tempfile::tempdir().expect("tmp");
    let d = dir.path();
    frames(&d.join("lights"));
    valerian()
        .arg("--keep-location")
        .arg("stack")
        .arg(d.join("lights"))
        .arg("-o")
        .arg(d.join("kept.fits"))
        .assert()
        .success();
    let h = fits::read_header(&d.join("kept.fits")).expect("header");
    assert_eq!(h.get_f64("SITELAT"), Some(12.3456));
    assert_eq!(h.get_str("SERIALNO"), Some("SYNTH-SN-0001"));
    assert_eq!(h.get_str("TELESCOP"), Some("S50_0badf00d"));
}

#[test]
fn the_default_stack_leaves_the_inputs_alone() {
    let dir = tempfile::tempdir().expect("tmp");
    let d = dir.path();
    frames(&d.join("lights"));
    valerian()
        .arg("stack")
        .arg(d.join("lights"))
        .arg("-o")
        .arg(d.join("s.fits"))
        .assert()
        .success();
    let h = fits::read_header(&d.join("lights/f0.fits")).expect("header");
    assert_eq!(h.get_f64("SITELAT"), Some(12.3456));
}

#[test]
fn which_keys_count_as_identifying() {
    for k in [
        "SITELAT",
        "SITELONG",
        "SITEELEV",
        "OBSGEO-X",
        "OBSGEO-L",
        "LAT-OBS",
        "LONG-OBS",
        "ALT-OBS",
        "SERIALNO",
        "CAMSERIA",
        "DEVSERIA",
        "OBSERVER",
        "CAMSERNO",
        "CAMSERNUM",
    ] {
        assert!(fits::is_identifying(k), "{k}");
    }
    for k in [
        "EXPTIME", "DATE-OBS", "INSTRUME", "TELESCOP", "OBJECT", "CRVAL1", "GAIN", "FILTER",
    ] {
        assert!(!fits::is_identifying(k), "{k}");
    }
}

#[test]
fn unit_ids_go_but_model_names_stay() {
    for (text, want) in [
        ("S50_0badf00d", "S50"),
        ("Seestar S50", "Seestar S50"),
        ("Seestar S50 SN 77665544", "Seestar S50"),
        ("S50-12345678", "S50"),
        ("ZWO ASI294MC Pro", "ZWO ASI294MC Pro"),
        ("QHY268M-PH", "QHY268M-PH"),
        ("registered by Valerian", "registered by Valerian"),
    ] {
        assert_eq!(fits::strip_unit_ids(text), want, "{text}");
    }
}

#[test]
fn every_fits_write_path_strips_unit_ids() {
    let mut header = Header::new();
    header.set(
        "TELESCOP",
        Value::Str("S50_0badf00d".into()),
        "unit S50_0badf00d",
    );
    header.set("DATE-OBS", Value::Str("2026-01-01T20:10:00".into()), "");
    header.add_history("from S50_0badf00d");
    let img = Image::mono(
        valerian_core::Frame::new(4, 4, vec![1.0; 16]).expect("frame"),
        header,
    );
    for format in [Format::Float32, Format::Uint16] {
        let bytes = fits::to_bytes(&img, format).expect("bytes");
        assert!(
            !String::from_utf8_lossy(&bytes)
                .to_ascii_lowercase()
                .contains("0badf00d")
        );
        let h = fits::parse(&bytes).expect("parse").header;
        assert_eq!(h.get_str("TELESCOP"), Some("S50"));
        assert_eq!(h.get_str("DATE-OBS"), Some("2026-01-01T20:10:00"));
    }
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("a.fits");
    fits::write(&path, &img, Format::Float32).expect("write");
    assert_eq!(
        fits::read_header(&path).expect("h").get_str("TELESCOP"),
        Some("S50")
    );
    let kept = fits::to_bytes_with(&img, Format::Float32, true).expect("bytes");
    assert!(String::from_utf8_lossy(&kept).contains("S50_0badf00d"));
    assert_eq!(img.header.scrubbed().get_str("TELESCOP"), Some("S50"));
}
