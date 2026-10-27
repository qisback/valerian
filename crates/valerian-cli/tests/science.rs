// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The science commands through the `valerian` binary: `subframes`, `plot`,
//! `stack --approve` and `optics`.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use valerian_core::fits::{self, Format, Image, Value};
use valerian_core::testutil::{Rng, Scene, gaussian_field};
use valerian_core::transform::Transform;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

/// Eight dithered frames of one sky; frame 5 is soft and frame 6 is noisy.
fn frames(dir: &Path) {
    std::fs::create_dir_all(dir).expect("dir");
    let mut rng = Rng::new(21);
    let scene = Scene::random(90, 360, 260, 22);
    for k in 0..8u64 {
        let (dx, dy) = (rng.uniform() * 4.0 - 2.0, rng.uniform() * 4.0 - 2.0);
        let blur = if k == 5 { 2.6 } else { 0.0 };
        let noise = if k == 6 { 30.0 } else { 4.0 };
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let mut img = Image::mono(
            scene.render(&view, 400.0, noise, 1.0, blur, 30 + k),
            fits::Header::new(),
        );
        img.header.set(
            "DATE-OBS",
            Value::Str(format!("2026-01-01T20:{:02}:00", 10 * k)),
            "",
        );
        fits::write(&dir.join(format!("f{k}.fits")), &img, Format::Float32).expect("write");
    }
}

#[test]
fn subframes_writes_the_report_and_stack_uses_the_approvals() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    frames(&lights);
    let report = dir.path().join("report");
    valerian()
        .args(["subframes"])
        .arg(&lights)
        .args(["--report"])
        .arg(&report)
        .args(["--max-fwhm", "4.5", "--min-star-count", "20"])
        .assert()
        .success()
        .stdout(predicate::str::contains("approved").and(predicate::str::contains("report.html")));
    for f in [
        "subframes.csv",
        "subframes.json",
        "subframes_index.png",
        "subframes_time.png",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    // Reject frame 3 by hand in the CSV; the stack leaves it out.
    let csv = std::fs::read_to_string(report.join("subframes.csv")).expect("csv");
    let edited: Vec<String> = csv
        .lines()
        .map(|l| {
            if l.contains("f3.fits") {
                l.replace(",yes,", ",no,")
            } else {
                l.to_owned()
            }
        })
        .collect();
    std::fs::write(report.join("edited.csv"), edited.join("\n")).expect("write");
    let out = dir.path().join("stack.fits");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(&out)
        .arg("--approve")
        .arg(report.join("edited.csv"))
        .arg("--science-report")
        .arg(dir.path().join("stack-report"))
        .assert()
        .success()
        .stderr(predicate::str::contains("rejected"));
    let stacked = fits::read(&out).expect("stack");
    let n = stacked.header.get_f64("NCOMBINE").expect("NCOMBINE");
    assert!(n < 8.0, "{n}");
    assert!(dir.path().join("stack-report/subframes.csv").is_file());
    // A plot from the saved table, on the time axis.
    let png = dir.path().join("p.png");
    valerian()
        .args(["plot", "subframes"])
        .arg(report.join("subframes.json"))
        .arg("-o")
        .arg(&png)
        .args(["--x", "time"])
        .assert()
        .success();
    assert!(
        png.is_file() && dir.path().join("p.csv").is_file() && dir.path().join("p.json").is_file()
    );
}

#[test]
fn a_table_that_rejects_everything_stops_the_stack() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    frames(&lights);
    let table = dir.path().join("t.csv");
    std::fs::write(
        &table,
        "name,approved
f0.fits,no
f1.fits,no
f2.fits,no
f3.fits,no
f4.fits,no
f5.fits,no
f6.fits,no
",
    )
    .expect("write");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(dir.path().join("s.fits"))
        .arg("--approve")
        .arg(&table)
        .assert()
        .failure()
        .stderr(predicate::str::contains("fewer than two"));
}

/// A star field whose stars widen to the right.
fn tilted(path: &Path) {
    let mut rng = Rng::new(40);
    let stars: Vec<(f64, f64, f64, f64)> = (0..300)
        .map(|_| {
            let x = 20.0 + rng.uniform() * 760.0;
            (
                x,
                20.0 + rng.uniform() * 560.0,
                4000.0 + 50000.0 * rng.uniform().powi(2),
                1.4 + 1.0 * x / 800.0,
            )
        })
        .collect();
    let mut f = gaussian_field(800, 600, 400.0, &stars);
    for v in &mut f.data {
        *v += rng.gauss(0.0, 4.0) as f32;
    }
    fits::write(path, &Image::mono(f, fits::Header::new()), Format::Float32).expect("write");
}

#[test]
fn optics_writes_every_plot_and_says_which_side_is_soft() {
    let dir = tempfile::tempdir().expect("tmp");
    let img = dir.path().join("tilted.fits");
    tilted(&img);
    let report = dir.path().join("optics");
    valerian()
        .arg("optics")
        .arg(&img)
        .arg("--report")
        .arg(&report)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("softest at the top-right")
                .or(predicate::str::contains("softest at the bottom-right")),
        );
    for f in [
        "aberration_inspector.png",
        "fwhm_map.png",
        "fwhm_map.csv",
        "eccentricity_map.png",
        "tilt.png",
        "psf.png",
        "psf.csv",
        "field_stars.csv",
        "optics.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let json = std::fs::read_to_string(report.join("optics.json")).expect("json");
    assert!(json.contains("\"hints\"") && json.contains("\"fwhm_moffat\""));
    // One plot on its own, with its data.
    for (kind, ext) in [
        ("fwhm-map", "csv"),
        ("psf", "json"),
        ("tilt", "json"),
        ("aberration", "json"),
    ] {
        let out = dir.path().join(format!("{kind}.png"));
        valerian()
            .args(["plot", kind])
            .arg(&img)
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        assert!(out.is_file(), "{kind}");
        assert!(out.with_extension(ext).is_file(), "{kind}.{ext}");
    }
}

#[test]
fn optics_refuses_an_empty_sky_and_a_bad_degree() {
    let dir = tempfile::tempdir().expect("tmp");
    let img = dir.path().join("blank.fits");
    let f = valerian_core::Frame::new(300, 300, vec![100.0; 90_000]).expect("frame");
    fits::write(&img, &Image::mono(f, fits::Header::new()), Format::Float32).expect("write");
    valerian()
        .arg("optics")
        .arg(&img)
        .arg("--report")
        .arg(dir.path().join("r"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("usable stars"));
    valerian()
        .arg("optics")
        .arg(&img)
        .args(["--order", "9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--order"));
}

#[test]
fn stack_science_report_writes_maps_noise_and_histograms() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    frames(&lights);
    let report = dir.path().join("report");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(dir.path().join("s.fits"))
        .arg("--science-report")
        .arg(&report)
        .assert()
        .success()
        .stderr(predicate::str::contains("CPU"));
    for f in [
        "rejection_low.png",
        "rejection_low.fits",
        "rejection_high.csv",
        "pixel_count.png",
        "weight_map.fits",
        "noise.png",
        "noise.csv",
        "histogram.png",
        "histogram.csv",
        "integration.json",
        "subframes.csv",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let json = std::fs::read_to_string(report.join("integration.json")).expect("json");
    assert!(json.contains("\"sigma1\"") && json.contains("\"histograms\""));
    // The count map holds the frames: no pixel has more than eight.
    let counts = fits::read(&report.join("pixel_count.fits")).expect("map");
    assert!(counts.plane().data.iter().all(|v| *v <= 8.0));
    // The maps on their own, and a histogram of the result.
    for (kind, ext) in [
        ("rejection-high", "csv"),
        ("count-map", "fits"),
        ("weight-map", "fits"),
        ("noise", "csv"),
    ] {
        let out = dir.path().join(format!("{kind}.png"));
        valerian()
            .args(["plot", kind])
            .arg(&lights)
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        assert!(out.is_file() && out.with_extension(ext).is_file(), "{kind}");
    }
    let hist = dir.path().join("h.png");
    valerian()
        .args(["plot", "histogram"])
        .arg(dir.path().join("s.fits"))
        .arg("-o")
        .arg(&hist)
        .assert()
        .success()
        .stdout(predicate::str::contains("median"));
    assert!(hist.is_file() && hist.with_extension("json").is_file());
}

#[test]
fn drizzle_science_report_writes_weight_count_and_rejection_maps() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    frames(&lights);
    let report = dir.path().join("report");
    valerian()
        .arg("drizzle")
        .arg(&lights)
        .arg("-o")
        .arg(dir.path().join("d.fits"))
        .args(["--scale", "1.5", "--pixfrac", "0.8", "--science-report"])
        .arg(&report)
        .assert()
        .success();
    for f in [
        "drizzle_weight_map.png",
        "drizzle_pixel_count.fits",
        "drizzle_rejected.png",
        "drizzle_rejected.csv",
        "drizzle_histogram.png",
        "drizzle_integration.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
}

/// A solved colour field with the catalogue that matches it, written to disk.
fn solved_colour(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let scene = valerian_core::testutil::colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    let mut image = scene.image.clone();
    scene.truth.write(&mut image.header);
    let (img, cat) = (dir.join("solved.fits"), dir.join("stars.vcat"));
    fits::write(&img, &image, Format::Float32).expect("write");
    std::fs::write(&cat, scene.catalogue.as_bytes()).expect("catalogue");
    (img, cat)
}

#[test]
fn annotate_draws_the_solution_and_writes_residuals() {
    let dir = tempfile::tempdir().expect("tmp");
    let (img, cat) = solved_colour(dir.path());
    let report = dir.path().join("astro");
    valerian()
        .arg("annotate")
        .arg(&img)
        .arg("--catalogue")
        .arg(&cat)
        .arg("--report")
        .arg(&report)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("residuals:").and(predicate::str::contains("report.html")),
        );
    for f in [
        "annotated.png",
        "annotated_objects.csv",
        "annotated_stars.csv",
        "wcs_residuals.png",
        "wcs_residuals.csv",
        "astrometry.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let csv = std::fs::read_to_string(report.join("wcs_residuals.csv")).expect("csv");
    assert!(csv.lines().count() > 20);
    // Plots on their own, and the colour fit from `photocal --report`.
    for (kind, ext) in [
        ("annotate", "stars.csv"),
        ("residuals", "json"),
        ("pcc", "json"),
    ] {
        let out = dir.path().join(format!("{kind}.png"));
        valerian()
            .args(["plot", kind])
            .arg(&img)
            .arg("--catalogue")
            .arg(&cat)
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        assert!(out.is_file() && out.with_extension(ext).is_file(), "{kind}");
    }
    let pcc = dir.path().join("pcc-report");
    valerian()
        .arg("photocal")
        .arg(&img)
        .arg("--catalogue")
        .arg(&cat)
        .arg("--report")
        .arg(&pcc)
        .assert()
        .success();
    assert!(
        pcc.join("pcc.png").is_file()
            && pcc.join("pcc.csv").is_file()
            && pcc.join("report.html").is_file()
    );
}

#[test]
fn annotate_without_a_solution_says_to_solve_first() {
    let dir = tempfile::tempdir().expect("tmp");
    let img = dir.path().join("blank.fits");
    let f = valerian_core::Frame::new(200, 200, vec![100.0; 40_000]).expect("frame");
    fits::write(&img, &Image::mono(f, fits::Header::new()), Format::Float32).expect("write");
    valerian()
        .arg("annotate")
        .arg(&img)
        .arg("--report")
        .arg(dir.path().join("r"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("world coordinate system"));
}

/// Eight dithered frames of a sparse sky, wide enough for comparison stars.
fn wide_frames(dir: &Path) {
    std::fs::create_dir_all(dir).expect("dir");
    let mut rng = Rng::new(51);
    let scene = Scene::random(45, 700, 500, 52);
    for k in 0..8u64 {
        let (dx, dy) = (rng.uniform() * 4.0 - 2.0, rng.uniform() * 4.0 - 2.0);
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let mut img = Image::mono(
            scene.render(&view, 400.0, 4.0, 1.0, 0.0, 60 + k),
            fits::Header::new(),
        );
        img.header.set(
            "DATE-OBS",
            Value::Str(format!("2026-01-01T21:{:02}:00", 5 * k)),
            "",
        );
        fits::write(&dir.join(format!("w{k}.fits")), &img, Format::Float32).expect("write");
    }
}

#[test]
fn photometry_measures_a_dimming_star_and_writes_the_light_curve() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    wide_frames(&lights);
    let report = dir.path().join("phot");
    valerian()
        .arg("photometry")
        .arg(&lights)
        .arg("--report")
        .arg(&report)
        .assert()
        .success()
        .stdout(predicate::str::contains("scatter").and(predicate::str::contains("report.html")));
    for f in [
        "lightcurve.png",
        "lightcurve.csv",
        "lightcurve.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let csv = std::fs::read_to_string(report.join("lightcurve.csv")).expect("csv");
    assert_eq!(csv.lines().count(), 9);
    let out = dir.path().join("lc.png");
    valerian()
        .args(["plot", "lightcurve"])
        .arg(&lights)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert!(out.is_file() && out.with_extension("csv").is_file());
    valerian()
        .arg("photometry")
        .arg(&lights)
        .args(["--star", "banana"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("X,Y"));
}

#[test]
fn stack_limits_leave_frames_out_of_the_stack() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    frames(&lights);
    let all = dir.path().join("all.fits");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(&all)
        .assert()
        .success();
    let n_all = fits::read(&all)
        .expect("all")
        .header
        .get_f64("NCOMBINE")
        .expect("NCOMBINE");
    // Frame 5 is soft: --max-fwhm must keep it out of the stack, not only the report.
    let limited = dir.path().join("limited.fits");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(&limited)
        .args(["--max-fwhm", "4.5"])
        .assert()
        .success()
        .stderr(predicate::str::contains("limits:").and(predicate::str::contains("f5.fits")));
    let n_lim = fits::read(&limited)
        .expect("limited")
        .header
        .get_f64("NCOMBINE")
        .expect("NCOMBINE");
    assert!(n_lim < n_all, "{n_lim} against {n_all}");
    // The same with a report: the report still lists every frame.
    let with_report = dir.path().join("r.fits");
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(&with_report)
        .args(["--max-fwhm", "4.5"])
        .arg("--science-report")
        .arg(dir.path().join("rep"))
        .assert()
        .success();
    let n_rep = fits::read(&with_report)
        .expect("r")
        .header
        .get_f64("NCOMBINE")
        .expect("NCOMBINE");
    assert!((n_rep - n_lim).abs() < 0.5, "{n_rep} against {n_lim}");
    let json = std::fs::read_to_string(dir.path().join("rep/subframes.json")).expect("json");
    assert!(json.contains("f5.fits") && json.contains("f0.fits"));
    // A limit nothing meets stops the stack.
    valerian()
        .arg("stack")
        .arg(&lights)
        .arg("-o")
        .arg(dir.path().join("none.fits"))
        .args(["--max-fwhm", "0.5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("need at least two"));
}

#[test]
fn photocal_writes_zero_points_a_calibrated_catalogue_and_the_limit() {
    let dir = tempfile::tempdir().expect("tmp");
    let (img, cat) = solved_colour(dir.path());
    let report = dir.path().join("photo");
    let stars = dir.path().join("calibrated.csv");
    valerian()
        .arg("photocal")
        .arg(&img)
        .arg("--catalogue")
        .arg(&cat)
        .arg("--report")
        .arg(&report)
        .arg("--calibrated-stars")
        .arg(&stars)
        .args(["--limit-snr", "3"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("zero point")
                .and(predicate::str::contains("3-sigma limiting magnitude"))
                .and(predicate::str::contains("calibrated star catalogue")),
        );
    for f in [
        "zeropoint.png",
        "zeropoint.csv",
        "zeropoint.json",
        "limiting_magnitude.png",
        "calibrated_stars.csv",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let csv = std::fs::read_to_string(&stars).expect("catalogue");
    let header = csv.lines().next().expect("header");
    assert!(
        header.starts_with("id,x,y,ra_deg,dec_deg,mag_g,mag_g_err"),
        "{header}"
    );
    assert!(csv.lines().count() > 50);
    let json = std::fs::read_to_string(report.join("zeropoint.json")).expect("json");
    assert!(json.contains("\"colour_term\"") && json.contains("\"from_noise\""));
}

#[test]
fn the_new_plots_write_their_data_beside_the_picture() {
    let dir = tempfile::tempdir().expect("tmp");
    let (img, cat) = solved_colour(dir.path());
    for kind in ["zeropoint", "limiting-mag", "residual-map"] {
        let out = dir.path().join(format!("{kind}.png"));
        valerian()
            .args(["plot", kind])
            .arg(&img)
            .arg("--catalogue")
            .arg(&cat)
            .args(["--grid", "5"])
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        assert!(
            out.is_file()
                && out.with_extension("csv").is_file()
                && out.with_extension("json").is_file(),
            "{kind}"
        );
    }
    // annotate adds the residual map to its report.
    let report = dir.path().join("astro-map");
    valerian()
        .arg("annotate")
        .arg(&img)
        .arg("--catalogue")
        .arg(&cat)
        .arg("--report")
        .arg(&report)
        .args(["--map-grid", "4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("residual map:"));
    assert!(report.join("residual_map.png").is_file() && report.join("residual_map.csv").is_file());
}

#[test]
fn a_mono_image_cannot_have_a_zero_point_plot() {
    let dir = tempfile::tempdir().expect("tmp");
    let (img, cat) = solved_colour(dir.path());
    let mono = dir.path().join("mono.fits");
    let full = fits::read(&img).expect("read");
    fits::write(
        &mono,
        &valerian_core::fits::Image::mono(full.planes[1].clone(), full.header.clone()),
        Format::Float32,
    )
    .expect("write");
    valerian()
        .args(["plot", "zeropoint"])
        .arg(&mono)
        .arg("--catalogue")
        .arg(&cat)
        .arg("-o")
        .arg(dir.path().join("x.png"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("red, green and blue"));
}
