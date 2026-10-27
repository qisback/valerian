// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Behaviour tests of the workspace and image snapshots for the user guide.
//!
//! The picture is synthetic (`valerian_core::testutil::deep_sky`), so nothing
//! depends on data files or the machine. Snapshots are drawn by the software
//! adapter and run only when `VALERIAN_GPU_TESTS=software`; the PNGs live in
//! `docs/book/src/images/ui/`. Regenerate them with
//! `UPDATE_SNAPSHOTS=1 VALERIAN_GPU_TESTS=software cargo test -p valerian-gui snapshots`.

use std::path::{Path, PathBuf};

use eframe::egui::{self, vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, SnapshotOptions};
use valerian_core::fits::{Header, Image};
use valerian_core::testutil::deep_sky;

use crate::app::App;
use crate::stretch_tool::CurveKind;

/// Per-pixel colour tolerance and the number of pixels allowed over it, for
/// rasteriser differences between software adapters.
const THRESHOLD: f32 = 0.6;
const MAX_FAILED_PIXELS: usize = 60;
/// The usual allowance plus one picture-wide row (647 px) that WARP draws white (odd scissor top).
const WARP_EDGE_ROW_PIXELS: usize = MAX_FAILED_PIXELS + 650;

pub(crate) fn enabled() -> bool {
    if std::env::var("VALERIAN_GPU_TESTS").as_deref() == Ok("software") {
        true
    } else {
        eprintln!("skipping GUI snapshots: set VALERIAN_GPU_TESTS=software to render them");
        false
    }
}

/// One window at a time: each snapshot opens its own software GPU device, and
/// lavapipe has crashed when several were made and torn down together.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn options() -> SnapshotOptions {
    SnapshotOptions::new()
        .threshold(THRESHOLD)
        .max_failed_pixels(MAX_FAILED_PIXELS)
        .output_path(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/book/src/images/ui"),
        )
}

/// The synthetic linear picture all the tests open.
pub(crate) fn picture() -> Image {
    let sky = deep_sky(720, 480, 260, 11);
    Image::mono(sky.frame, Header::new())
}

pub(crate) fn window(w: f32, h: f32) -> Harness<'static, App> {
    Harness::builder()
        .with_size(vec2(w, h))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::new(cc, None))
}

/// A window for the behaviour tests, tall enough that no panel needs scrolling.
pub(crate) fn headless(w: f32, h: f32) -> Harness<'static, App> {
    Harness::builder()
        .with_size(vec2(w, h.max(1400.0)))
        .build_eframe(|cc| App::new(cc, None))
}

pub(crate) fn shown(h: &Harness<'_, App>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

/// Open the Drizzle panel, which sits in the Stack step.
fn go_drizzle(h: &mut Harness<'_, App>) {
    h.state_mut().ws.select(crate::rail::Step::Stack);
    h.run_steps(2);
    h.get_by_label("Drizzle").click();
    h.run_steps(2);
}

fn open(h: &mut Harness<'_, App>) {
    h.state_mut()
        .ws
        .load(Path::new("/data/NGC_demo.fits"), picture());
    h.run_steps(3);
}

#[test]
fn an_empty_workspace_says_what_to_do() {
    let mut h = headless(1000.0, 640.0);
    h.run();
    h.get_by_label("No image open");
    h.get_by_label_contains("Press Open (Ctrl+O)");
    // The Process step with nothing open says the same in its own panel.
    h.state_mut().ws.select(crate::rail::Step::Process);
    h.run_steps(2);
    h.get_by_label_contains("Open a FITS image to work on it");
}

#[test]
fn opening_an_image_shows_a_preview_with_the_measured_levels() {
    let mut h = headless(1000.0, 700.0);
    open(&mut h);
    let ws = &h.state().ws;
    assert!(ws.doc.is_some());
    assert!(ws.measured.is_some());
    assert!(shown(&h, "NGC_demo.fits: 720 x 480 px, 1 plane"));
    assert!(shown(&h, "black "));
}

/// The synthetic picture at a size that needs the pyramid built on a thread.
fn big_picture() -> Image {
    let sky = deep_sky(2400, 1800, 600, 5);
    Image::mono(sky.frame, Header::new())
}

#[test]
fn a_big_picture_builds_its_zoom_levels_in_the_background() {
    let mut h = headless(1000.0, 700.0);
    h.state_mut()
        .ws
        .load(Path::new("/data/big.fits"), big_picture());
    h.run_steps(1);
    // While it builds the notice says so; it may already be done on a fast machine.
    let building = h.state().ws.tiles.building();
    if building {
        let n = h.state().ws.notice().expect("a notice while building");
        assert!(n.running && n.text.contains("zoom levels"), "{n:?}");
    }
    for _ in 0..400 {
        h.run_steps(1);
        if !h.state().ws.tiles.building() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.run_steps(2);
    let ws = &h.state().ws;
    assert!(!ws.tiles.building());
    assert_eq!(ws.tiles.pyramid().map(|p| p.count()), Some(4));
    assert!(
        ws.status
            .as_ref()
            .is_some_and(|s| s.0.contains("Full-resolution zoom ready") && !s.1),
        "{:?}",
        ws.status
    );
}

#[test]
fn zoomed_in_the_viewer_draws_real_pixel_tiles_and_reads_the_pixel() {
    let mut h = headless(1200.0, 800.0);
    h.state_mut()
        .ws
        .load(Path::new("/data/big.fits"), big_picture());
    for _ in 0..400 {
        h.run_steps(1);
        if h.state().ws.measured.is_some() && !h.state().ws.tiles.building() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Fitted, the preview is enough: no tile is made.
    h.run_steps(3);
    assert_eq!(h.state().ws.tiles.made, 0);
    // 4:1 about the middle of the picture.
    h.state_mut()
        .ws
        .view
        .centre_on(4.0, 1200.0, 900.0, vec2(2400.0, 1800.0));
    h.run_steps(6);
    h.hover_at(egui::pos2(640.0, 430.0));
    h.run_steps(3);
    let ws = &h.state().ws;
    assert!(ws.tiles.made > 0, "no tile was made");
    assert!(ws.tiles.cached_bytes() > 0);
    assert!(
        ws.tiles.cached_bytes() <= crate::tiles::CACHE_BYTES,
        "the cache is bounded"
    );
    // A window needs only a few tiles, not the picture.
    assert!(ws.tiles.made <= crate::tiles::tiles_for_window((1200.0, 800.0)));
    let r = ws.reading.as_ref().expect("a reading under the pointer");
    let doc = ws.doc.as_ref().expect("doc");
    assert_eq!(r.raw, crate::readout::raw_at(&doc.full, r.x, r.y));
    assert!(r.shown.as_ref().is_some_and(|s| s.len() == 1));
    // At 4:1 the pointer is within a few tens of pixels of the centre pixel.
    assert!(r.x.abs_diff(1200) < 200 && r.y.abs_diff(900) < 200, "{r:?}");
}

#[test]
fn choosing_a_curve_shows_its_controls() {
    let mut h = headless(1000.0, 900.0);
    open(&mut h);
    h.get_by_label("GHS").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.stretch.kind, CurveKind::Ghs);
    assert!(shown(&h, "Stretch factor") && shown(&h, "Symmetry point"));
    h.get_by_label("Arcsinh").click();
    h.run_steps(3);
    assert!(shown(&h, "Strength"));
    assert!(!shown(&h, "Stretch factor"));
}

#[test]
fn reset_returns_to_the_automatic_stretch() {
    let mut h = headless(1000.0, 900.0);
    open(&mut h);
    h.get_by_label("GHS").click();
    h.run_steps(3);
    h.get_by_label("Reset").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.stretch.kind, CurveKind::Auto);
}

#[test]
fn apply_writes_the_same_stretch_as_the_preview() {
    let dir = tempfile::tempdir().expect("tmp");
    let out = dir.path().join("out.fits");
    let mut h = headless(1000.0, 900.0);
    open(&mut h);
    h.state_mut().ws.output_text = out.to_string_lossy().into_owned();
    h.get_by_label("Apply and save").click();
    for _ in 0..200 {
        h.run_steps(2);
        if out.exists()
            && h.state()
                .ws
                .status
                .as_ref()
                .is_some_and(|s| s.0.starts_with("Wrote"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let written = valerian_core::fits::read(&out).expect("output");
    assert!(valerian_core::stretch::Stretch::from_header(&written.header).is_some());
    let med = valerian_core::stats::median_finite(&written.plane().data);
    assert!((med - 0.10).abs() < 0.02, "sky at {med}");
}

#[test]
fn a_missing_file_is_reported_not_fatal() {
    let mut h = headless(1000.0, 700.0);
    h.state_mut().ws.open(Path::new("/nonexistent/none.fits"));
    h.run_steps(2);
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("Cannot open"));
}

#[test]
fn stretch_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1200.0, 960.0);
    open(&mut h);
    h.snapshot("stretch-auto");

    {
        let t = &mut h.state_mut().ws.stretch;
        t.kind = CurveKind::Ghs;
        t.ghs.stretch = 4.0;
        t.ghs.b = 2.0;
        t.ghs.sp = 0.05;
        t.ghs.lp = 0.0;
        t.ghs.hp = 0.9;
    }
    h.state_mut().ws.mark_dirty();
    h.run_steps(4);
    h.snapshot("stretch-ghs");
}

/// Run star removal through the panel and wait for the background thread.
fn remove(h: &mut Harness<'_, App>) {
    h.get_by_label("Star removal").click();
    h.run_steps(3);
    h.get_by_label("Remove stars").click();
    for _ in 0..400 {
        h.run_steps(2);
        if h.state().ws.stars.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
}

#[test]
fn star_removal_runs_and_offers_the_layers() {
    let mut h = headless(1000.0, 1000.0);
    open(&mut h);
    remove(&mut h);
    let r = h.state().ws.stars.result.clone().expect("a result");
    assert!(r.removal.found > 100, "found {}", r.removal.found);
    for name in [
        "Starless",
        "Original",
        "Stars",
        "Mask",
        "Save starless and stars",
    ] {
        assert!(shown(&h, name), "{name} is missing");
    }
    h.get_by_label("Mask").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.stars.view, crate::star_tool::View::Mask);
}

#[test]
fn saving_writes_the_starless_image_and_the_star_layer() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 1000.0);
    open(&mut h);
    remove(&mut h);
    let out = dir.path().join("x_starless.fits");
    h.state_mut().ws.star_output_text = out.to_string_lossy().into_owned();
    h.run_steps(2);
    h.get_by_label("Save starless and stars").click();
    let stars = dir.path().join("x_stars.fits");
    for _ in 0..400 {
        h.run_steps(2);
        if stars.exists()
            && h.state()
                .ws
                .status
                .as_ref()
                .is_some_and(|s| s.0.starts_with("Wrote"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let a = valerian_core::fits::read(&out).expect("starless");
    let b = valerian_core::fits::read(&stars).expect("stars");
    let o = picture();
    for i in (0..o.plane().data.len()).step_by(97) {
        let sum = a.plane().data[i] + b.plane().data[i];
        assert!((sum - o.plane().data[i]).abs() < 1e-2 * o.plane().data[i].abs().max(1.0));
    }
}

#[test]
fn star_removal_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1200.0, 960.0);
    open(&mut h);
    h.get_by_label("Star removal").click();
    h.run_steps(3);
    h.snapshot("stars-panel");
    h.get_by_label("Remove stars").click();
    for _ in 0..400 {
        h.run_steps(2);
        if h.state().ws.stars.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
    h.snapshot("stars-starless");
    h.get_by_label("Original").click();
    h.run_steps(4);
    h.snapshot("stars-original");
}

/// Open a synthetic star field with a header that names roughly where it points,
/// and give the Plate solve tool the matching catalogue.
fn open_plate(h: &mut Harness<'_, App>) -> valerian_core::testutil::PlateScene {
    use valerian_core::fits::Value;
    let scene = valerian_core::testutil::plate_scene((150.0, 30.0), 900, 600, 6.0, 25.0, 21);
    let mut header = Header::new();
    header.set("RA", Value::Float(150.12), "");
    header.set("DEC", Value::Float(30.08), "");
    header.set("FOCALLEN", Value::Float(200.0), "");
    header.set("XPIXSZ", Value::Float(5.8), "");
    header.set("OBJECT", Value::Str("Synthetic field".into()), "");
    let image = Image::mono(scene.frame.clone(), header);
    h.state_mut().ws.load(Path::new("/data/field.fits"), image);
    h.state_mut().ws.solve.catalogue = Some(std::sync::Arc::new(scene.catalogue.clone()));
    h.run_steps(3);
    scene
}

fn solve_now(h: &mut Harness<'_, App>) {
    if h.state().ws.tool != crate::workspace::Tool::Solve {
        h.get_by_label("Plate solve").click();
        h.run_steps(3);
    }
    h.get_by_label("Solve").click();
    for _ in 0..400 {
        h.run_steps(2);
        if h.state().ws.solve.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
}

#[test]
fn the_solve_tool_takes_its_hint_from_the_header_and_solves() {
    let mut h = headless(1000.0, 1000.0);
    let scene = open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    {
        let t = &h.state().ws.solve;
        assert_eq!(t.ra, "150.1200");
        assert!((t.scale - 5.981).abs() < 0.01, "{}", t.scale);
    }
    solve_now(&mut h);
    let sol = h.state().ws.solve.result.clone().expect("a solution");
    assert!(sol.matched >= 15, "matched {}", sol.matched);
    let (ra, dec) = sol.wcs.pixel_to_world(450.0, 300.0);
    let truth = scene.truth.pixel_to_world(450.0, 300.0);
    assert!(valerian_core::wcs::separation(ra, dec, truth.0, truth.1) * 3600.0 < 1.5);
    assert!(shown(&h, "rotation "));
    assert!(shown(&h, "Save with WCS"));
}

#[test]
fn a_solve_that_cannot_match_says_so() {
    let mut h = headless(1000.0, 1000.0);
    open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    // Point the hint at empty sky.
    h.state_mut().ws.solve.dec = "-60".into();
    h.get_by_label("Solve").click();
    for _ in 0..200 {
        h.run_steps(2);
        if h.state().ws.status.as_ref().is_some_and(|s| s.1) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("no match"), "{}", st.0);
    assert!(h.state().ws.solve.result.is_none());
}

#[test]
fn a_blind_solve_needs_no_position() {
    let mut h = headless(1000.0, 1000.0);
    open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    h.state_mut().ws.solve.blind = true;
    h.state_mut().ws.solve.scale = 6.0;
    h.get_by_label("Solve").click();
    for _ in 0..800 {
        h.run_steps(2);
        if h.state().ws.solve.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(h.state().ws.solve.result.is_some(), "no blind solution");
}

#[test]
fn saving_writes_the_wcs_into_a_copy() {
    let dir = tempfile::tempdir().expect("tmp");
    let out = dir.path().join("solved.fits");
    let mut h = headless(1000.0, 1000.0);
    open_plate(&mut h);
    solve_now(&mut h);
    h.state_mut().ws.solve_output_text = out.to_string_lossy().into_owned();
    h.run_steps(2);
    h.get_by_label("Save with WCS").click();
    for _ in 0..400 {
        h.run_steps(2);
        if out.exists()
            && h.state()
                .ws
                .status
                .as_ref()
                .is_some_and(|s| s.0.starts_with("Wrote"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let img = valerian_core::fits::read(&out).expect("written");
    let wcs = valerian_core::wcs::Wcs::from_header(&img.header).expect("a WCS");
    let sol = h.state().ws.solve.result.clone().expect("solution");
    let (a, b) = (
        wcs.pixel_to_world(100.0, 100.0),
        sol.wcs.pixel_to_world(100.0, 100.0),
    );
    assert!(valerian_core::wcs::separation(a.0, a.1, b.0, b.1) * 3600.0 < 1e-3);
    // The pixels are the same ones.
    assert_eq!(img.plane().data.len(), 900 * 600);
}

#[test]
fn solve_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1200.0, 960.0);
    open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    h.snapshot("solve-panel");
    h.get_by_label("Solve").click();
    for _ in 0..400 {
        h.run_steps(2);
        if h.state().ws.solve.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
    h.snapshot("solve-result");
}

/// Open a synthetic colour field whose header carries its plate solution, and
/// give the Plate solve tool the matching catalogue (which the Colour tool
/// shares).
fn open_colour(h: &mut Harness<'_, App>) -> valerian_core::testutil::ColourScene {
    let scene = valerian_core::testutil::colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    let mut image = scene.image.clone();
    scene.truth.write(&mut image.header);
    image.header.set(
        "OBJECT",
        valerian_core::fits::Value::Str("Synthetic field".into()),
        "",
    );
    h.state_mut().ws.load(Path::new("/data/colour.fits"), image);
    h.state_mut().ws.solve.catalogue = Some(std::sync::Arc::new(scene.catalogue.clone()));
    h.run_steps(3);
    scene
}

fn calibrate_now(h: &mut Harness<'_, App>) {
    if h.state().ws.tool != crate::workspace::Tool::Colour {
        h.get_by_label("Colour").click();
        h.run_steps(3);
    }
    h.get_by_label("Calibrate colour").click();
    for _ in 0..400 {
        h.run_steps(2);
        if h.state().ws.colour.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
}

#[test]
fn the_colour_tool_calibrates_with_the_wcs_in_the_header() {
    let mut h = headless(1000.0, 1100.0);
    let scene = open_colour(&mut h);
    calibrate_now(&mut h);
    let cal = h.state().ws.colour.result.clone().expect("a calibration");
    assert!(cal.used >= 40, "{} stars", cal.used);
    for c in [0, 2] {
        assert!((cal.gains[c] / scene.true_gains[c] - 1.0).abs() < 0.03);
    }
    assert!(shown(&h, "Save calibrated"));
    assert!(shown(&h, "Calibrated") && shown(&h, "Original"));
    h.get_by_label("Original").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.colour.view, crate::colour_tool::View::Original);
}

#[test]
fn without_a_plate_solution_the_colour_tool_says_what_to_do() {
    let mut h = headless(1000.0, 1100.0);
    let scene = valerian_core::testutil::colour_scene((210.0, 40.0), 1200, 800, 6.0, 150.0, 5);
    h.state_mut()
        .ws
        .load(Path::new("/data/colour.fits"), scene.image.clone());
    h.run_steps(3);
    h.get_by_label("Colour").click();
    h.run_steps(3);
    h.get_by_label("Calibrate colour").click();
    h.run_steps(3);
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("Plate solve"), "{}", st.0);
}

#[test]
fn a_mono_image_cannot_be_colour_calibrated() {
    let mut h = headless(1000.0, 900.0);
    open(&mut h);
    h.get_by_label("Colour").click();
    h.run_steps(3);
    assert!(shown(&h, "not a three-plane colour image"));
}

#[test]
fn saving_writes_the_calibrated_image_with_its_gains() {
    let dir = tempfile::tempdir().expect("tmp");
    let out = dir.path().join("cal.fits");
    let mut h = headless(1000.0, 1100.0);
    open_colour(&mut h);
    calibrate_now(&mut h);
    h.state_mut().ws.colour_output_text = out.to_string_lossy().into_owned();
    h.run_steps(2);
    h.get_by_label("Save calibrated").click();
    for _ in 0..400 {
        h.run_steps(2);
        if out.exists()
            && h.state()
                .ws
                .status
                .as_ref()
                .is_some_and(|s| s.0.starts_with("Wrote"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let img = valerian_core::fits::read(&out).expect("written");
    let cal = h.state().ws.colour.result.clone().expect("calibration");
    assert!((img.header.get_f64("PCCGAINR").expect("gain") - cal.gains[0]).abs() < 1e-6);
    let sky: Vec<f32> = img
        .planes
        .iter()
        .map(|p| valerian_core::stats::sky_and_noise(p).0)
        .collect();
    assert!((sky[0] - sky[2]).abs() < 1.0, "{sky:?}");
}

#[test]
fn colour_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1200.0, 960.0);
    open_colour(&mut h);
    h.get_by_label("Colour").click();
    h.run_steps(3);
    h.snapshot("colour-panel");
    calibrate_now(&mut h);
    h.snapshot("colour-result");
    h.get_by_label("Original").click();
    h.run_steps(4);
    h.snapshot("colour-original");
}

/// A picker that answers with `answer` and records what it was asked.
pub(crate) fn fake_picker(
    answer: Option<PathBuf>,
) -> (
    crate::dialogs::Picker,
    std::sync::Arc<std::sync::Mutex<Vec<crate::dialogs::Request>>>,
) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&seen);
    let picker: crate::dialogs::Picker = std::sync::Arc::new(move |r| {
        log.lock().expect("log").push(r.clone());
        answer.clone()
    });
    (picker, seen)
}

/// Step the window until the dialog has answered.
pub(crate) fn settle(h: &mut Harness<'_, App>) {
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().ws.chooser.busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.run_steps(3);
}

pub(crate) fn write_fits(path: &Path) {
    valerian_core::fits::write(path, &picture(), valerian_core::fits::Format::Float32)
        .expect("write fits");
}

#[test]
fn browsing_opens_the_chosen_image() {
    let dir = tempfile::tempdir().expect("tmp");
    let file = dir.path().join("chosen.fits");
    write_fits(&file);
    let mut h = headless(1000.0, 800.0);
    h.run();
    let (picker, seen) = fake_picker(Some(file.clone()));
    h.state_mut().ws.chooser.picker = picker;
    // The Open button of the top bar, with nothing open yet.
    h.get_by_label("Open").click();
    settle(&mut h);
    let ws = &h.state().ws;
    assert_eq!(ws.doc.as_ref().expect("opened").path, file);
    assert_eq!(ws.path_text, file.to_string_lossy());
    let asked = seen.lock().expect("log");
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].save && asked[0].filter.1.contains(&"fits"));
}

#[test]
fn cancelling_the_dialog_changes_nothing() {
    let mut h = headless(1000.0, 800.0);
    open(&mut h);
    let before = h.state().ws.path_text.clone();
    let (picker, seen) = fake_picker(None);
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Browse...").click();
    settle(&mut h);
    assert_eq!(h.state().ws.path_text, before);
    assert_eq!(seen.lock().expect("log").len(), 1);
    assert!(h.state().ws.status.is_none());
}

#[test]
fn control_o_opens_the_dialog() {
    let dir = tempfile::tempdir().expect("tmp");
    let file = dir.path().join("keyed.fits");
    write_fits(&file);
    let mut h = headless(1000.0, 800.0);
    h.run();
    let (picker, _seen) = fake_picker(Some(file.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.key_press_modifiers(eframe::egui::Modifiers::COMMAND, eframe::egui::Key::O);
    h.run_steps(2);
    settle(&mut h);
    assert!(h.state().ws.doc.is_some(), "Ctrl+O did not open the dialog");
}

#[test]
fn save_as_fills_the_output_box_and_adds_the_extension() {
    let dir = tempfile::tempdir().expect("tmp");
    let target = dir.path().join("my_result");
    let mut h = headless(1000.0, 900.0);
    open(&mut h);
    let before = h.state().ws.output_text.clone();
    let (picker, seen) = fake_picker(Some(target.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Save as...").click();
    settle(&mut h);
    assert_eq!(
        h.state().ws.output_text,
        target.with_extension("fits").to_string_lossy()
    );
    let asked = seen.lock().expect("log");
    assert!(asked[0].save);
    // It offers the name the box held.
    assert_eq!(
        asked[0].file_name.as_deref(),
        Path::new(&before).file_name().and_then(|n| n.to_str())
    );
}

#[test]
fn each_tool_saves_to_its_own_box() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 1000.0);
    open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    solve_now(&mut h);
    let target = dir.path().join("solved_here.fits");
    let (picker, _seen) = fake_picker(Some(target.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Save as...").click();
    settle(&mut h);
    assert_eq!(h.state().ws.solve_output_text, target.to_string_lossy());
}

#[test]
fn the_catalogue_can_be_chosen_with_the_dialog() {
    let dir = tempfile::tempdir().expect("tmp");
    let cat = dir.path().join("mine.vcat");
    let mut h = headless(1000.0, 1000.0);
    open_plate(&mut h);
    h.get_by_label("Plate solve").click();
    h.run_steps(3);
    let (picker, seen) = fake_picker(Some(cat.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Choose catalogue...").click();
    settle(&mut h);
    let ws = &h.state().ws;
    assert_eq!(ws.solve.catalogue_path, cat.to_string_lossy());
    // The catalogue held in memory no longer overrides the file named.
    assert!(ws.solve.catalogue.is_none());
    assert_eq!(seen.lock().expect("log")[0].filter.1, ["vcat"]);
}

#[test]
fn the_colour_tool_saves_to_its_own_box() {
    let dir = tempfile::tempdir().expect("tmp");
    let target = dir.path().join("balanced.fits");
    let mut h = headless(1000.0, 1100.0);
    open_colour(&mut h);
    calibrate_now(&mut h);
    let (picker, seen) = fake_picker(Some(target.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Save as...").click();
    settle(&mut h);
    assert_eq!(h.state().ws.colour_output_text, target.to_string_lossy());
    assert!(seen.lock().expect("log")[0].title.contains("calibrated"));
}

/// A folder of dithered frames of one made-up undersampled sky.
fn write_dithered_frames(dir: &Path, n: u64) {
    use valerian_core::fits::{Format, Value};
    use valerian_core::testutil::{Rng, Scene};
    use valerian_core::transform::Transform;
    let mut rng = Rng::new(33);
    let scene = Scene {
        width: 480,
        height: 320,
        stars: (0..110)
            .map(|_| {
                (
                    8.0 + rng.uniform() * 464.0,
                    8.0 + rng.uniform() * 304.0,
                    5000.0 + 40000.0 * rng.uniform().powi(2),
                    0.7,
                )
            })
            .collect(),
    };
    std::fs::create_dir_all(dir).expect("dir");
    for k in 0..n {
        let (dx, dy) = if k == 0 {
            (0.0, 0.0)
        } else {
            (rng.uniform() * 6.0 - 3.0, rng.uniform() * 6.0 - 3.0)
        };
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let mut img = Image::mono(
            scene.render(&view, 300.0, 4.0, 1.0, 0.0, 90 + k),
            Header::new(),
        );
        img.header.set("EXPTIME", Value::Float(60.0), "");
        valerian_core::fits::write(&dir.join(format!("sub_{k:02}.fits")), &img, Format::Float32)
            .expect("write frame");
    }
}

fn drizzle_now(h: &mut Harness<'_, App>) {
    if h.state().ws.tool != crate::workspace::Tool::Drizzle {
        go_drizzle(h);
    }
    h.get_by_label("Drizzle frames").click();
    for _ in 0..800 {
        h.run_steps(2);
        if h.state().ws.drizzle.result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
}

#[test]
fn the_drizzle_tool_works_with_nothing_open() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 14);
    let mut h = headless(1000.0, 1100.0);
    h.run();
    go_drizzle(&mut h);
    assert!(shown(&h, "Drizzle frames"));
    h.state_mut().ws.drizzle.folder = dir.path().to_string_lossy().into_owned();
    drizzle_now(&mut h);
    let run = h.state().ws.drizzle.result.clone().expect("a result");
    assert_eq!(run.found, 14);
    assert_eq!(run.drizzled.used(), 14);
    assert_eq!(run.drizzled.image.width(), 960);
    assert!(shown(&h, "Save drizzled") && shown(&h, "Open it"));
    assert!(h.state().ws.doc.is_none());
}

#[test]
fn an_empty_or_missing_folder_is_reported() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 900.0);
    h.run();
    go_drizzle(&mut h);
    h.get_by_label("Drizzle frames").click();
    h.run_steps(3);
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("Enter the folder"), "{}", st.0);
    h.state_mut().ws.drizzle.folder = dir.path().to_string_lossy().into_owned();
    h.get_by_label("Drizzle frames").click();
    for _ in 0..200 {
        h.run_steps(2);
        if h.state()
            .ws
            .status
            .as_ref()
            .is_some_and(|s| s.0.contains("no FITS"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("no FITS files"), "{}", st.0);
    assert!(h.state().ws.drizzle.result.is_none());
}

#[test]
fn saving_and_opening_the_drizzled_image() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 12);
    let out = dir.path().join("out").join("drz.fits");
    std::fs::create_dir_all(out.parent().expect("dir")).expect("mkdir");
    let mut h = headless(1000.0, 1100.0);
    h.run();
    go_drizzle(&mut h);
    h.state_mut().ws.drizzle.folder = dir.path().to_string_lossy().into_owned();
    h.state_mut().ws.drizzle.scale = 2.0;
    h.state_mut().ws.drizzle_output_text = out.to_string_lossy().into_owned();
    drizzle_now(&mut h);
    h.get_by_label("Save drizzled").click();
    for _ in 0..400 {
        h.run_steps(2);
        if out.exists()
            && h.state()
                .ws
                .status
                .as_ref()
                .is_some_and(|s| s.0.starts_with("Wrote"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let img = valerian_core::fits::read(&out).expect("written");
    assert_eq!((img.width(), img.height()), (960, 640));
    assert_eq!(img.header.get_f64("DRIZSCL"), Some(2.0));
    h.get_by_label("Open it").click();
    h.run_steps(4);
    let ws = &h.state().ws;
    assert_eq!(ws.doc.as_ref().expect("opened").full.width(), 960);
    assert_eq!(ws.tool, crate::workspace::Tool::Stretch);
}

#[test]
fn rejection_and_pixfrac_reach_the_run() {
    let mut h = headless(1000.0, 1100.0);
    h.run();
    go_drizzle(&mut h);
    {
        let t = &mut h.state_mut().ws.drizzle;
        t.scale = 3.0;
        t.pixfrac = 0.5;
        t.reject = false;
    }
    let o = h.state().ws.drizzle.options();
    assert_eq!((o.scale, o.pixfrac, o.reject), (3.0, 0.5, None));
    h.state_mut().ws.drizzle.reject = true;
    h.state_mut().ws.drizzle.sigma = 5.0;
    assert_eq!(h.state().ws.drizzle.options().reject, Some(5.0));
}

#[test]
fn drizzle_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 16);
    let mut h = window(1200.0, 960.0);
    h.run();
    go_drizzle(&mut h);
    h.state_mut().ws.drizzle.folder = "/data/IC5070/subs".into();
    h.run_steps(3);
    h.snapshot("drizzle-panel");
    h.state_mut().ws.drizzle.folder = dir.path().to_string_lossy().into_owned();
    drizzle_now(&mut h);
    h.state_mut().ws.drizzle.folder = "/data/IC5070/subs".into();
    h.state_mut().ws.drizzle_output_text = "/data/IC5070/subs/drizzled.fits".into();
    h.run_steps(3);
    h.snapshot("drizzle-result");
}

#[test]
fn the_drizzle_folder_and_output_can_be_chosen_with_the_dialog() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 1100.0);
    h.run();
    go_drizzle(&mut h);
    let (picker, seen) = fake_picker(Some(dir.path().to_path_buf()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Choose folder...").click();
    settle(&mut h);
    assert_eq!(h.state().ws.drizzle.folder, dir.path().to_string_lossy());
    {
        let asked = seen.lock().expect("log");
        assert!(asked[0].folder && !asked[0].save);
    }
    // The save box appears with a result, and its dialog saves a FITS file.
    write_dithered_frames(dir.path(), 12);
    drizzle_now(&mut h);
    let target = dir.path().join("mine");
    let (picker, seen) = fake_picker(Some(target.clone()));
    h.state_mut().ws.chooser.picker = picker;
    h.get_by_label("Save as...").click();
    settle(&mut h);
    assert_eq!(
        h.state().ws.drizzle_output_text,
        target.with_extension("fits").to_string_lossy()
    );
    assert!(seen.lock().expect("log")[0].save);
}

#[test]
fn the_gpu_tool_has_no_benchmark_until_it_is_run() {
    let mut h = headless(1000.0, 900.0);
    h.run();
    h.get_by_label("GPU").click();
    h.run_steps(3);
    assert!(shown(&h, "No benchmark yet"));
    assert!(shown(&h, "Run benchmark"));
    assert!(h.state().ws.gpu.result.is_none());
}

#[test]
fn the_gpu_tool_benchmarks_the_kernels_and_reports_agreement() {
    let Some(request) = valerian_gpu::test_adapter() else {
        eprintln!("VALERIAN_GPU_TESTS is unset or 'none': skipping the GPU benchmark");
        return;
    };
    // Metal has no fallback (software) adapter, so a software run cannot open one there.
    if request == valerian_gpu::AdapterRequest::Software && cfg!(target_os = "macos") {
        eprintln!("macOS has no software adapter: skipping the GPU benchmark");
        return;
    }
    let _serial = serial();
    let mut h = headless(1000.0, 1100.0);
    h.run();
    h.get_by_label("GPU").click();
    h.run_steps(3);
    {
        let t = &mut h.state_mut().ws.gpu;
        t.adapter = if request == valerian_gpu::AdapterRequest::Software {
            crate::gpu_tool::Adapter::Software
        } else {
            crate::gpu_tool::Adapter::Hardware
        };
        t.width = 256;
        t.height = 256;
        t.frames = 4;
    }
    h.get_by_label("Run benchmark").click();
    for _ in 0..1200 {
        h.run_steps(2);
        if h.state().ws.gpu.result.is_some() || h.state().ws.status.as_ref().is_some_and(|s| s.1) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    h.run_steps(4);
    let run = h.state().ws.gpu.result.clone().expect("a benchmark");
    assert_eq!(run.rows.len(), 6);
    assert!(run.rows.iter().all(|r| r.worst < 0.06 && r.coverage < 1e-3));
    assert!(shown(&h, "Largest difference from the CPU"));
}

/// The figures measured on the build server's A100 lane (GPU 1, Vulkan) with a 24-core CPU
/// that other jobs were sharing, `valerian bench`, frames of 4096 x 3072 and 20
/// frames in the combine. The screenshots show them so the picture is the same
/// on every machine.
fn measured() -> crate::gpu_tool::GpuRun {
    use valerian_gpu::bench::Row;
    let row = |name: &str, cpu: f64, gpu: f64, worst: f64, off: f64| Row {
        name: name.into(),
        pixels: 0,
        cpu: cpu / 1000.0,
        gpu: gpu / 1000.0,
        phases: (0.0, 0.0, 0.0),
        worst,
        coverage: 0.0,
        off,
    };
    crate::gpu_tool::GpuRun {
        adapter: "NVIDIA A100 80GB PCIe (Vulkan, DiscreteGpu)".into(),
        rows: vec![
            row(
                "calibrate (bias, dark, flat, pixel map)",
                39.2,
                91.3,
                4.2e-7,
                0.0,
            ),
            row("warp (bicubic)", 68.9, 101.3, 4.9e-5, 0.0),
            row("combine (sigma clip)", 355.7, 480.0, 3.2e-4, 1e-7),
            row("combine (winsorised)", 672.0, 534.5, 3.2e-4, 3e-7),
            row("combine (linear fit)", 478.6, 481.5, 5.7e-4, 2.4e-6),
            row("stack plane (warp + combine)", 1657.6, 576.0, 5.0e-2, 2e-5),
        ],
        size: (4096, 3072, 20),
    }
}

#[test]
fn gpu_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1200.0, 960.0);
    h.run();
    h.get_by_label("GPU").click();
    h.run_steps(3);
    h.snapshot("gpu-panel");
    h.state_mut().ws.gpu.result = Some(measured());
    h.run_steps(4);
    h.snapshot("gpu-result");
}

/// What the three folder steps leave behind, as if they had run.
fn mock_session(h: &mut Harness<'_, App>) {
    use crate::pipeline::{CalibrateRun, RegisterRun, StackRun};
    let ws = &mut h.state_mut().ws;
    ws.cal.result = Some(CalibrateRun {
        lights: 30,
        masters: vec!["master_dark.fits".into(), "master_flat.fits".into()],
        defects: Some((9925, 22, 0)),
        folder: "/data/ic5070/calibrated".into(),
        preview: Some(std::sync::Arc::new(picture())),
    });
    ws.reg.result = Some(RegisterRun {
        total: 30,
        written: 29,
        reference: 7,
        median_rms: 0.21,
        folder: "/data/ic5070/registered".into(),
        preview: Some(std::sync::Arc::new(picture())),
    });
    ws.stack.input = "/data/ic5070/registered".into();
    ws.stack.output = "/data/ic5070/stack.fits".into();
    ws.stack.result = Some(StackRun {
        total: 30,
        used: 29,
        reference: 7,
        rejection: "Sigma".into(),
        image: std::sync::Arc::new(picture()),
        path: "/data/ic5070/stack.fits".into(),
    });
}

#[test]
fn workflow_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 800.0);
    mock_session(&mut h);
    h.state_mut()
        .ws
        .load(Path::new("/data/ic5070/stack.fits"), picture());
    h.run_steps(4);
    h.snapshot("workflow-process");

    h.state_mut().ws.select(crate::rail::Step::Stack);
    let progress = crate::pipeline::Progress::new();
    progress.say("registering frame 18 of 30: 412 stars, 0.19 px rms");
    h.state_mut()
        .ws
        .fake_running("Registering and stacking the frames...", progress, 14);
    h.run_steps(4);
    h.snapshot("workflow-running");

    let ctx = h.ctx.clone();
    h.state_mut().set_night(&ctx, true);
    h.state_mut().ws.select(crate::rail::Step::Process);
    h.state_mut().ws.status = None;
    h.run_steps(4);
    h.snapshot("workflow-night");
    h.state_mut().set_night(&ctx, false);
}

// ---------------------------------------------------------------- workflow

pub(crate) fn wait_for(h: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..1600 {
        h.run_steps(2);
        if done(h.state()) {
            h.run_steps(3);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// A night's frames from one model sensor: lights of a dithered sky, darks, flats
/// and dark flats, each in its own folder under `dir`.
pub(crate) fn write_session(dir: &Path, lights: u64) {
    use valerian_core::fits::Format;
    use valerian_core::testutil::{Rng, Scene, Sensor};
    use valerian_core::transform::Transform;
    let sensor = Sensor::new(320, 240, false, 3);
    let mut rng = Rng::new(21);
    let scene = Scene::random(140, 320, 240, 8);
    let zero = sensor.dark_current_frame(0.0);
    let mut lamp = sensor.dark_current_frame(0.0);
    lamp.data.fill(20000.0);
    let put = |sub: &str, name: &str, k: u64, img: &Image| {
        let d = dir.join(sub);
        std::fs::create_dir_all(&d).expect("dir");
        valerian_core::fits::write(&d.join(format!("{name}_{k:02}.fits")), img, Format::Float32)
            .expect("write");
    };
    for k in 0..lights {
        let (dx, dy) = if k == 0 {
            (0.0, 0.0)
        } else {
            (rng.uniform() * 8.0 - 4.0, rng.uniform() * 8.0 - 4.0)
        };
        let view = Transform::similarity(1.0, 0.0, dx, dy);
        let sky = scene.render(&view, 40.0, 0.0, 1.0, 0.0, 70 + k);
        put("lights", "light", k, &sensor.expose(&sky, 60.0, &mut rng));
    }
    for k in 0..6 {
        put("darks", "dark", k, &sensor.expose(&zero, 60.0, &mut rng));
        put("flats", "flat", k, &sensor.expose(&lamp, 2.0, &mut rng));
        put(
            "darkflats",
            "darkflat",
            k,
            &sensor.expose(&zero, 2.0, &mut rng),
        );
    }
}

/// The button of a rail step or a top-bar chip: a step's name is also its panel's heading.
pub(crate) fn step_button<'a>(h: &'a Harness<'_, App>, name: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(eframe::egui::accesskit::Role::Button, name)
}

pub(crate) fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn the_rail_lists_five_steps_and_a_click_opens_one() {
    use crate::rail::Step;
    use crate::workspace::Tool;
    let mut h = headless(1200.0, 900.0);
    h.run();
    for name in ["Calibrate", "Register", "Stack", "Process", "Export"] {
        step_button(&h, name);
    }
    assert_eq!(h.state().ws.tool, Tool::Calibrate);
    step_button(&h, "Register").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.tool, Tool::Register);
    assert!(shown(&h, "Register frames"));
    step_button(&h, "Export").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.tool.step(), Some(Step::Export));
    step_button(&h, "Stack").click();
    h.run_steps(3);
    assert!(shown(&h, "Stack frames") && shown(&h, "Drizzle"));
}

#[test]
fn the_process_step_remembers_its_tool() {
    use crate::rail::Step;
    use crate::workspace::Tool;
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    h.get_by_label("Star removal").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.tool, Tool::Stars);
    h.state_mut().ws.select(Step::Export);
    h.state_mut().ws.select(Step::Process);
    assert_eq!(h.state().ws.tool, Tool::Stars);
}

#[test]
fn the_rail_reports_where_each_step_stands() {
    use crate::rail::{State, Step};
    let mut h = headless(1200.0, 900.0);
    h.run();
    let row = |h: &Harness<'_, App>, s: Step| {
        h.state()
            .ws
            .rail_rows()
            .into_iter()
            .find(|r| r.step == s)
            .expect("row")
    };
    assert_eq!(row(&h, Step::Calibrate).state, State::Idle);
    assert_eq!(row(&h, Step::Stack).state, State::Waiting);
    assert_eq!(row(&h, Step::Process).state, State::Waiting);
    open(&mut h);
    assert_eq!(row(&h, Step::Process).state, State::Ready);
    assert_eq!(row(&h, Step::Export).state, State::Ready);
    mock_session(&mut h);
    assert_eq!(row(&h, Step::Calibrate).state, State::Done);
    assert_eq!(row(&h, Step::Register).status, "29 of 30 aligned");
    assert_eq!(row(&h, Step::Stack).status, "29 of 30 frames");
}

#[test]
fn a_running_step_shows_a_notice_with_progress_and_no_dismiss() {
    let mut h = headless(1200.0, 900.0);
    h.run();
    let progress = crate::pipeline::Progress::new();
    progress.set_total(10);
    for _ in 0..4 {
        progress.tick();
    }
    progress.say("Calibrating light_03.fits (4 of 10)");
    h.state_mut()
        .ws
        .fake_running("Calibrating the lights...", progress, 5);
    h.run_steps(3);
    let n = h.state().ws.notice().expect("a notice");
    assert!(n.running && !n.error);
    assert_eq!(n.fraction, Some(0.4));
    assert!(n.detail.contains("4 of 10 frames"), "{}", n.detail);
    assert!(n.detail.contains("5 s"), "{}", n.detail);
    assert!(shown(&h, "Calibrating the lights"));
    assert!(!shown(&h, "Dismiss"));
}

#[test]
fn an_error_notice_stays_until_it_is_dismissed() {
    let mut h = headless(1200.0, 900.0);
    h.run();
    h.get_by_label("Calibrate lights").click();
    // The run reports the missing folder from its worker.
    wait_for(&mut h, "the error", |a| {
        a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let n = h.state().ws.notice().expect("notice");
    assert!(n.error && n.text.contains("Name a folder"), "{}", n.text);
    h.run_steps(5);
    assert!(shown(&h, "Dismiss"));
    h.get_by_label("Dismiss").click();
    h.run_steps(3);
    assert!(h.state().ws.status.is_none());
}

#[test]
fn the_folder_steps_chain_into_a_stack_that_opens_for_processing() {
    use crate::workspace::Tool;
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 8);
    let mut h = headless(1200.0, 1000.0);
    h.run();
    {
        let c = &mut h.state_mut().ws.cal;
        c.lights = path_text(&dir.path().join("lights"));
        c.darks = path_text(&dir.path().join("darks"));
        c.flats = path_text(&dir.path().join("flats"));
        c.dark_flats = path_text(&dir.path().join("darkflats"));
    }
    h.run_steps(2);
    h.get_by_label("Calibrate lights").click();
    wait_for(&mut h, "calibration", |a| a.ws.cal.result.is_some());
    let ws = &h.state().ws;
    let cal = ws.cal.result.clone().expect("result");
    assert_eq!(cal.lights, 8);
    assert!(cal.masters.contains(&"master_dark.fits".to_owned()));
    assert!(
        dir.path()
            .join("calibrated")
            .join("masters")
            .join("master_flat.fits")
            .exists()
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("calibrated"))
            .expect("dir")
            .filter(|e| e
                .as_ref()
                .is_ok_and(|e| e.file_name().to_string_lossy().ends_with("_cal.fits")))
            .count(),
        8
    );
    // The next two steps start from the folder just written.
    assert_eq!(ws.reg.input, cal.folder);
    assert_eq!(ws.stack.input, cal.folder);

    h.state_mut().ws.select(crate::rail::Step::Register);
    h.run_steps(3);
    h.get_by_label("Register frames").click();
    wait_for(&mut h, "registration", |a| a.ws.reg.result.is_some());
    let reg = h.state().ws.reg.result.clone().expect("result");
    assert!(reg.written >= 7, "aligned {} of {}", reg.written, reg.total);
    assert!(reg.median_rms < 1.0, "{}", reg.median_rms);
    assert_eq!(
        h.state().ws.stack.input,
        reg.folder,
        "Stack follows Register"
    );

    h.state_mut().ws.select(crate::rail::Step::Stack);
    h.run_steps(3);
    h.get_by_label("Stack frames").click();
    wait_for(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    let ws = &h.state().ws;
    let run = ws.stack.result.clone().expect("result");
    assert!(run.used >= 7, "{} of {}", run.used, run.total);
    assert!(run.path.exists());
    // The stack is the open image and the Process step is open.
    assert_eq!(ws.doc.as_ref().expect("opened").path, run.path);
    assert_eq!(ws.tool, Tool::Stretch);
    assert!(
        ws.notice()
            .is_some_and(|n| n.text.contains("opened the stack"))
    );
}

#[test]
fn stacking_without_frames_says_so() {
    let mut h = headless(1200.0, 900.0);
    h.run();
    h.state_mut().ws.select(crate::rail::Step::Stack);
    h.run_steps(2);
    h.get_by_label("Stack frames").click();
    wait_for(&mut h, "the error", |a| {
        a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let st = h.state().ws.status.clone().expect("status");
    assert!(st.1 && st.0.contains("Name a folder"), "{}", st.0);
    assert!(h.state().ws.stack.result.is_none());
}

#[test]
fn the_folder_dialogs_fill_their_boxes_and_suggest_an_output() {
    let dir = tempfile::tempdir().expect("tmp");
    let lights = dir.path().join("lights");
    std::fs::create_dir_all(&lights).expect("dir");
    let mut h = headless(1200.0, 1000.0);
    h.run();
    let (picker, seen) = fake_picker(Some(lights.clone()));
    h.state_mut().ws.chooser.picker = picker;
    // The first Choose... button is the lights folder.
    h.get_all_by_label("Choose...")
        .next()
        .expect("a button")
        .click();
    settle(&mut h);
    let ws = &h.state().ws;
    assert_eq!(ws.cal.lights, path_text(&lights));
    assert_eq!(ws.cal.output, path_text(&dir.path().join("calibrated")));
    let asked = seen.lock().expect("log");
    assert!(asked[0].folder && !asked[0].save);
}

#[test]
fn export_writes_fits_and_png() {
    use crate::rail::Step;
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1200.0, 1000.0);
    open(&mut h);
    h.state_mut().ws.select(Step::Export);
    h.run_steps(3);
    let fits_out = dir.path().join("out.fits");
    h.state_mut().ws.export_text = path_text(&fits_out);
    h.get_by_label("Export picture").click();
    wait_for(&mut h, "the FITS export", |a| a.ws.exported.is_some());
    let back = valerian_core::fits::read(&fits_out).expect("read back");
    assert_eq!((back.width(), back.height()), (720, 480));
    assert!(shown(&h, "Wrote "));

    h.get_by_label("PNG").click();
    h.run_steps(3);
    let text = h.state().ws.export_text.clone();
    assert_eq!(
        Path::new(&text).extension().and_then(|e| e.to_str()),
        Some("png"),
        "{text}"
    );
    let png_out = dir.path().join("out.png");
    h.state_mut().ws.export_text = path_text(&png_out);
    h.get_by_label("Export picture").click();
    wait_for(&mut h, "the PNG export", |a| {
        a.ws.exported
            .as_ref()
            .is_some_and(|e| e.ends_with("out.png"))
    });
    let bytes = std::fs::read(&png_out).expect("png");
    assert_eq!(&bytes[1..4], b"PNG");
}

#[test]
fn night_vision_switches_the_palette_and_is_remembered() {
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    assert!(!crate::theme::is_night());
    h.get_by_label("Night").click();
    h.run_steps(3);
    assert!(crate::theme::is_night());
    assert!(h.state().prefs.night);
    // The picture is red only: no green or blue in what is drawn.
    let hist = h.state().ws.hist.clone().expect("histogram");
    assert_eq!(hist.bins.len(), 1, "the histogram is of the plain picture");
    h.key_press(eframe::egui::Key::F2);
    h.run_steps(3);
    assert!(!crate::theme::is_night());
    assert!(!h.state().prefs.night);
}

#[test]
fn the_gpu_button_opens_the_benchmark_and_a_step_leaves_it() {
    use crate::workspace::Tool;
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    step_button(&h, "GPU").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.tool, Tool::Gpu);
    step_button(&h, "Process").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.tool, Tool::Stretch);
    step_button(&h, "GPU").click();
    h.run_steps(2);
    step_button(&h, "GPU").click();
    h.run_steps(2);
    assert_eq!(h.state().ws.tool, Tool::Stretch);
}

#[test]
fn the_wheel_zooms_and_the_fit_button_returns() {
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    assert!(h.state().ws.view.zoom.is_none());
    h.get_by_label_contains("Histogram");
    // Zoom in with the + button, then fit again with the percentage.
    h.get_by_label("+").click();
    h.run_steps(3);
    let z = h.state().ws.view.zoom.expect("zoomed");
    assert!(z > 0.8, "{z}");
    h.get_by_label_contains("%").click();
    h.run_steps(3);
    assert!(h.state().ws.view.zoom.is_none());
    h.get_by_label("-").click();
    h.run_steps(3);
    assert!(h.state().ws.view.zoom.expect("zoomed out") < 0.8);
}

#[test]
fn the_histogram_can_be_hidden() {
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    assert!(shown(&h, "Histogram"));
    assert!(h.state().ws.hist.is_some());
    h.get_by_label("Histogram").click();
    h.run_steps(3);
    assert!(h.state().ws.hide_hist);
    h.get_by_label("Histogram").click();
    h.run_steps(3);
    assert!(!h.state().ws.hide_hist);
}

#[test]
fn the_top_bar_names_the_open_file_and_keeps_the_about_page_one_click_away() {
    let mut h = headless(1400.0, 900.0);
    open(&mut h);
    assert!(shown(&h, "NGC_demo.fits"));
    h.get_by_label("About").click();
    h.run_steps(3);
    assert!(h.state().show_about);
    assert!(shown(&h, "Valerian"));
}

#[test]
fn narrow_window_snapshot() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    // Narrow window: the rail shows the numbers only.
    let mut h = window(820.0, 640.0);
    mock_session(&mut h);
    h.state_mut()
        .ws
        .load(Path::new("/data/ic5070/stack.fits"), picture());
    h.run_steps(4);
    h.snapshot("workflow-narrow");
}

#[test]
fn calibrate_snapshot() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 800.0);
    {
        let c = &mut h.state_mut().ws.cal;
        c.lights = "/data/ic5070/lights".into();
        c.darks = "/data/ic5070/darks".into();
        c.flats = "/data/ic5070/flats".into();
        c.dark_flats = "/data/ic5070/darkflats".into();
        c.output = "/data/ic5070/calibrated".into();
    }
    h.run_steps(4);
    h.snapshot("workflow-calibrate");
}

#[test]
fn export_snapshot() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 800.0);
    mock_session(&mut h);
    h.state_mut()
        .ws
        .load(Path::new("/data/ic5070/stack.fits"), picture());
    h.state_mut().ws.select(crate::rail::Step::Export);
    h.run_steps(4);
    h.snapshot("workflow-export");
}

#[test]
fn the_about_page_hides_the_rail_and_the_panel_but_not_the_top_bar() {
    let mut h = headless(1200.0, 900.0);
    open(&mut h);
    assert!(shown(&h, "Workflow") || shown(&h, "Calibrate"));
    h.get_by_label("About").click();
    h.run_steps(3);
    assert!(h.state().show_about);
    h.get_by_label_contains("Xaviar");
    assert!(!shown(&h, "Apply and save"));
    // Any step on the rail is not on screen; Escape returns to the workspace.
    h.key_press(eframe::egui::Key::Escape);
    h.run_steps(3);
    assert!(!h.state().show_about);
    assert!(shown(&h, "Apply and save"));
}

// ------------------------------------------------------------------ cancel

fn no_partials(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|d| {
        d.filter_map(Result::ok).all(|e| {
            !e.file_name()
                .to_string_lossy()
                .starts_with(".valerian-partial")
        })
    })
}

#[test]
fn a_cancelled_calibration_leaves_the_output_folder_as_it_was() {
    use crate::pipeline::{CANCELLED, CalibrateTool, Progress};
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 8);
    let tool = CalibrateTool {
        lights: path_text(&dir.path().join("lights")),
        darks: path_text(&dir.path().join("darks")),
        flats: path_text(&dir.path().join("flats")),
        dark_flats: path_text(&dir.path().join("darkflats")),
        output: path_text(&dir.path().join("fresh").join("calibrated")),
        ..CalibrateTool::default()
    };
    // Cancelled after three lights: a folder that did not exist is not left.
    let p = Progress::new();
    p.cancel_after(3);
    assert_eq!(tool.run(&p).expect_err("cancelled"), CANCELLED);
    assert_eq!(p.counts().map(|c| c.0), Some(3), "it stopped at the cancel");
    assert!(!dir.path().join("fresh").exists());
    // Cancelled before it starts, after two lights and while the masters are
    // combined, with the folder already holding an earlier run.
    let old = dir.path().join("old");
    std::fs::create_dir_all(&old).expect("dir");
    std::fs::write(old.join("light_00_cal.fits"), b"previous").expect("write");
    let again = CalibrateTool {
        output: path_text(&old),
        ..tool.clone()
    };
    let p = Progress::new();
    p.cancel();
    assert_eq!(again.run(&p).expect_err("cancelled"), CANCELLED);
    let p = Progress::new();
    p.cancel_after(2);
    assert_eq!(again.run(&p).expect_err("cancelled"), CANCELLED);
    let p = Progress::new();
    p.cancel_on_line("Combining 6 flat");
    assert_eq!(again.run(&p).expect_err("cancelled"), CANCELLED);
    assert_eq!(
        std::fs::read(old.join("light_00_cal.fits")).expect("read"),
        b"previous"
    );
    assert_eq!(std::fs::read_dir(&old).expect("dir").count(), 1);
    // Not cancelled, the same run finishes and replaces the old file.
    let done = again.run(&Progress::new()).expect("run");
    assert_eq!(done.lights, 8);
    assert!(no_partials(&old));
    assert_ne!(
        std::fs::read(old.join("light_00_cal.fits")).expect("read"),
        b"previous"
    );
}

#[test]
fn a_cancelled_registration_writes_no_frames() {
    use crate::pipeline::{CANCELLED, Progress, RegisterTool};
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 8);
    let out = dir.path().join("registered");
    let tool = RegisterTool {
        input: path_text(&dir.path().join("lights")),
        output: path_text(&out),
        ..RegisterTool::default()
    };
    // While the stars are found.
    let p = Progress::new();
    p.cancel_on_line("frame 2:");
    assert_eq!(tool.run(&p).expect_err("cancelled"), CANCELLED);
    assert!(!out.exists());
    // While the aligned frames are written.
    let p = Progress::new();
    p.cancel_after(2);
    assert_eq!(tool.run(&p).expect_err("cancelled"), CANCELLED);
    assert!(!out.exists(), "no half-written frames");
    let done = tool.run(&Progress::new()).expect("run");
    assert!(done.written >= 7 && out.is_dir());
    assert!(no_partials(&out));
}

#[test]
fn a_cancelled_stack_writes_no_file() {
    use crate::pipeline::{CANCELLED, Progress, StackTool};
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 8);
    let out = dir.path().join("out").join("stack.fits");
    let tool = StackTool {
        input: path_text(&dir.path().join("lights")),
        output: path_text(&out),
        ..StackTool::default()
    };
    for line in ["frame 3:", "combining"] {
        let p = Progress::new();
        p.cancel_on_line(line);
        assert_eq!(tool.run(&p).expect_err("cancelled"), CANCELLED, "{line}");
        assert!(!dir.path().join("out").exists(), "{line}");
    }
    let done = tool.run(&Progress::new()).expect("run");
    assert!(done.path.is_file() && no_partials(&dir.path().join("out")));
}

#[test]
fn a_cancel_in_the_core_reads_as_cancelled_in_the_app() {
    use valerian_core::starless::{self, remove_stars};
    let img = picture();
    let o = starless::Options::default();
    o.cancel.cancel();
    let e = remove_stars(&img, &o).expect_err("cancelled");
    assert_eq!(crate::pipeline::failure(&e), crate::pipeline::CANCELLED);
}

#[test]
fn the_cancel_button_asks_the_run_to_stop_and_the_notice_then_says_cancelled() {
    use crate::pipeline::{CANCELLED, Progress};
    use crate::rail::{State, Step};
    let mut h = headless(1200.0, 900.0);
    h.run();
    h.state_mut().ws.select(Step::Stack);
    let progress = Progress::new();
    progress.set_total(30);
    progress.say("registering frame 18 of 30");
    let end = h.state_mut().ws.fake_running_with_end(
        "Registering and stacking the frames...",
        std::sync::Arc::clone(&progress),
        14,
    );
    h.run_steps(3);
    let n = h.state().ws.notice().expect("notice");
    assert!(n.running && n.cancellable);
    assert!(h.query_by_label("Cancel").is_some() && !shown(&h, "Cancelling"));
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(progress.is_cancelled());
    let n = h.state().ws.notice().expect("notice");
    assert!(n.running && !n.cancellable && !n.error, "{n:?}");
    assert!(n.text.starts_with("Cancelling"), "{}", n.text);
    assert!(shown(&h, "Cancelling..."));
    // A second press does nothing more; the worker then reports the cancel.
    h.state_mut().ws.cancel();
    end.send(Err(CANCELLED.to_owned())).expect("send");
    h.run_steps(4);
    let ws = &h.state().ws;
    assert!(!ws.running() && ws.progress.is_none() && ws.busy_since.is_none());
    let n = ws.notice().expect("notice");
    assert!(!n.running && !n.error, "a cancel is not an error: {n:?}");
    assert!(
        n.text.starts_with("Cancelled: stacking stopped"),
        "{}",
        n.text
    );
    assert!(shown(&h, "Dismiss") && h.query_by_label("Cancel").is_none());
    let row = h
        .state()
        .ws
        .rail_rows()
        .into_iter()
        .find(|r| r.step == Step::Stack)
        .expect("row");
    assert_ne!(row.state, State::Running);
    h.get_by_label("Dismiss").click();
    h.run_steps(3);
    assert!(h.state().ws.status.is_none());
}

#[test]
fn escape_cancels_a_running_step_and_a_failure_still_shows_as_an_error() {
    use crate::pipeline::Progress;
    let mut h = headless(1200.0, 900.0);
    h.run();
    // With nothing running Escape does nothing.
    h.key_press(eframe::egui::Key::Escape);
    h.run_steps(2);
    let progress = Progress::new();
    let end = h.state_mut().ws.fake_running_with_end(
        "Calibrating the lights...",
        std::sync::Arc::clone(&progress),
        3,
    );
    h.run_steps(3);
    assert!(!progress.is_cancelled());
    h.key_press(eframe::egui::Key::Escape);
    h.run_steps(3);
    assert!(progress.is_cancelled());
    end.send(Err("Cannot read x.fits: gone".to_owned()))
        .expect("send");
    h.run_steps(4);
    let n = h.state().ws.notice().expect("notice");
    assert!(n.error && n.text.contains("Cannot read"), "{n:?}");
}

#[test]
fn cancel_snapshots() {
    use crate::pipeline::{CANCELLED, Progress};
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 800.0);
    mock_session(&mut h);
    h.state_mut()
        .ws
        .load(Path::new("/data/ic5070/stack.fits"), picture());
    h.state_mut().ws.select(crate::rail::Step::Stack);
    let progress = Progress::new();
    progress.set_total(60);
    for _ in 0..24 {
        progress.tick();
    }
    progress.say("registering frame 24 of 60: 388 stars, 0.17 px rms");
    let end = h.state_mut().ws.fake_running_with_end(
        "Registering and stacking the frames...",
        std::sync::Arc::clone(&progress),
        21,
    );
    h.run_steps(4);
    h.snapshot("cancel-running");
    h.state_mut().ws.cancel();
    h.run_steps(4);
    h.snapshot("cancel-cancelling");
    end.send(Err(CANCELLED.to_owned())).expect("send");
    h.run_steps(4);
    h.snapshot("cancel-cancelled");
}

/// Open the Science panel, which sits in the Stack step.
fn go_science(h: &mut Harness<'_, App>) {
    h.state_mut().ws.select(crate::rail::Step::Stack);
    h.run_steps(2);
    h.get_by_label("Science").click();
    h.run_steps(2);
}

#[test]
fn the_science_panel_measures_a_folder_and_feeds_the_stack() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 12);
    let out = dir.path().join("stack").join("s.fits");
    let mut h = headless(1000.0, 2400.0);
    h.run();
    go_science(&mut h);
    assert!(shown(&h, "Measure frames"));
    assert!(shown(&h, "No frames measured yet"));
    h.state_mut().ws.science.folder = dir.path().to_string_lossy().into_owned();
    h.get_by_label("Measure frames").click();
    wait_for(&mut h, "the measurements", |a| {
        a.ws.science.analysis.is_some()
    });
    let analysis = h.state().ws.science.analysis.clone().expect("analysis");
    assert_eq!(analysis.frames.len(), 12);
    assert!(analysis.frames.iter().all(|r| r.metrics.stars > 20));
    assert!(shown(&h, "Use for stacking") && shown(&h, "Save report"));
    // A verdict can be overridden by hand, and the stack then leaves that frame out.
    h.state_mut()
        .ws
        .science
        .analysis
        .as_mut()
        .expect("analysis")
        .frames[3]
        .approved = false;
    h.get_by_label("Use for stacking").click();
    h.run_steps(3);
    let left_out = h.state().ws.stack.exclude.clone();
    assert!(left_out.contains(&"sub_03.fits".to_owned()), "{left_out:?}");
    // The report is written as plots, CSV, JSON and a self-contained page.
    let report = dir.path().join("report");
    h.state_mut().ws.science.report = report.to_string_lossy().into_owned();
    h.get_by_label("Save report").click();
    h.run_steps(3);
    for f in [
        "subframes.csv",
        "subframes.json",
        "subframes_index.png",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
    let table = std::fs::read_to_string(report.join("subframes.csv")).expect("csv");
    assert!(
        table.contains("sub_03.fits") && table.contains(",no,"),
        "{table}"
    );
    // The stack uses the rest.
    h.state_mut().ws.select(crate::rail::Step::Stack);
    h.run_steps(2);
    h.get_by_label("Mean").click();
    h.state_mut().ws.stack.input = dir.path().to_string_lossy().into_owned();
    h.state_mut().ws.stack.output = out.to_string_lossy().into_owned();
    h.run_steps(2);
    assert!(shown(&h, "left out by the Science panel"));
    h.get_by_label("Stack frames").click();
    wait_for(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    let r = h.state().ws.stack.result.clone().expect("result");
    assert_eq!((r.total, r.used), (12, 12 - left_out.len()));
}

/// Four made-up Seestar-sized GRBG subs: a star field painted onto the mosaic.
fn write_seestar_subs(dir: &Path) {
    use valerian_core::fits::{Format, Value};
    use valerian_core::testutil::Scene;
    use valerian_core::transform::Transform;
    std::fs::create_dir_all(dir).expect("dir");
    let scene = Scene::random(250, 1080, 1920, 9);
    for k in 0..4u32 {
        let view = Transform::similarity(1.0, 0.0, f64::from(k), -f64::from(k));
        let mut img = Image::mono(
            scene.render(&view, 800.0, 4.0, 1.0, 0.0, 70 + u64::from(k)),
            Header::new(),
        );
        img.header.set("BAYERPAT", Value::Str("GRBG".into()), "");
        img.planes[0].bayer = Some(valerian_core::BayerPattern::Grbg);
        valerian_core::fits::write(
            &dir.join(format!("seestar_{k}.fits")),
            &img,
            Format::Float32,
        )
        .expect("write");
    }
}

#[test]
fn the_science_panel_grades_seestar_subs_and_rejects_by_score() {
    let dir = tempfile::tempdir().expect("tmp");
    write_seestar_subs(dir.path());
    let mut h = headless(1000.0, 2400.0);
    h.run();
    go_science(&mut h);
    assert!(shown(&h, "Grade with the Seestar model"));
    h.state_mut().ws.science.folder = dir.path().to_string_lossy().into_owned();
    h.state_mut().ws.science.grade = true;
    h.state_mut().ws.science.min_grade = 0.0;
    // The contract-test model is untrained: its scores mean nothing, the wiring is what is tested.
    h.state_mut().ws.science.grader_model = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../valerian-science/tests/data/untrained-seestar-subgrade.onnx"
    )
    .into();
    h.run_steps(2);
    assert!(shown(&h, "Reject below"));
    h.get_by_label("Measure frames").click();
    wait_for(&mut h, "the graded measurements", |a| {
        a.ws.science.analysis.is_some()
    });
    let a = h.state().ws.science.analysis.clone().expect("analysis");
    assert!(a.frames.iter().all(|r| r.grade.is_some()));
    // Raising the bar rejects every frame, with the reason, without measuring again.
    h.state_mut().ws.science.min_grade = 0.999;
    h.state_mut().ws.science.apply_limits();
    let a = h.state().ws.science.analysis.clone().expect("analysis");
    assert!(a.frames.iter().all(|r| !r.approved));
    assert!(a.frames[0].reasons.iter().any(|w| w.starts_with("grade ")));
    assert_eq!(h.state().ws.science.rejected().len(), 4);
}

#[test]
fn science_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    // A fixed name, because the folder shows in the picture.
    let dir = std::env::temp_dir().join("valerian-science-shot");
    let _ = std::fs::remove_dir_all(&dir);
    write_dithered_frames(&dir, 14);
    let mut h = window(1280.0, 980.0);
    go_science(&mut h);
    h.snapshot("science-empty");
    h.state_mut().ws.science.folder = dir.to_string_lossy().into_owned();
    h.get_by_label("Measure frames").click();
    wait_for(&mut h, "the measurements", |a| {
        a.ws.science.analysis.is_some()
    });
    // The picture shows the folder as it reads on Linux, whatever this machine's temp folder is.
    h.state_mut().ws.science.folder = "/tmp/valerian-science-shot".into();
    h.run_steps(2);
    h.snapshot("science-panel");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open the Optics tool, which sits in the Process step.
fn go_optics(h: &mut Harness<'_, App>) {
    h.state_mut().ws.select(crate::rail::Step::Process);
    h.run_steps(2);
    h.get_by_label("Optics").click();
    h.run_steps(2);
}

#[test]
fn the_optics_tool_measures_the_open_image_and_saves_a_report() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 1100.0);
    h.run();
    open(&mut h);
    go_optics(&mut h);
    assert!(shown(&h, "Measure the stars"));
    h.get_by_label("Measure the stars").click();
    wait_for(&mut h, "the optics", |a| {
        a.ws.optics.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let status = h.state().ws.status.clone();
    let run = h
        .state()
        .ws
        .optics
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("no result: {status:?}"));
    assert!(run.field.stars.len() >= 30);
    assert_eq!(run.tilt.zones.len(), 5);
    assert!(!run.tilt.hints.is_empty() && shown(&h, "Save report"));
    let report = dir.path().join("optics");
    h.state_mut().ws.optics.report = report.to_string_lossy().into_owned();
    h.get_by_label("Save report").click();
    h.run_steps(3);
    for f in [
        "aberration_inspector.png",
        "fwhm_map.png",
        "optics.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
}

#[test]
fn optics_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 980.0);
    open(&mut h);
    go_optics(&mut h);
    h.get_by_label("Measure the stars").click();
    wait_for(&mut h, "the optics", |a| a.ws.optics.result.is_some());
    h.snapshot("optics-panel");
}

#[test]
fn the_science_panel_stacks_for_the_maps() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 12);
    let mut h = headless(1000.0, 1100.0);
    h.run();
    go_science(&mut h);
    h.state_mut().ws.science.folder = dir.path().to_string_lossy().into_owned();
    h.get_by_label("Stack for the maps").click();
    wait_for(&mut h, "the stack for the maps", |a| {
        a.ws.science.integration.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let status = h.state().ws.status.clone();
    let run = h
        .state()
        .ws
        .science
        .integration
        .as_ref()
        .unwrap_or_else(|| panic!("no run: {status:?}"));
    assert_eq!(run.used, 12);
    assert!(run.noise.is_some());
    assert!(shown(&h, "Rejected high") && shown(&h, "Histogram") && shown(&h, "Noise"));
}

#[test]
fn science_maps_snapshot() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let dir = std::env::temp_dir().join("valerian-science-maps");
    let _ = std::fs::remove_dir_all(&dir);
    write_dithered_frames(&dir, 14);
    let mut h = window(1280.0, 980.0);
    go_science(&mut h);
    h.state_mut().ws.science.folder = dir.to_string_lossy().into_owned();
    h.get_by_label("Stack for the maps").click();
    wait_for(&mut h, "the stack for the maps", |a| {
        a.ws.science.integration.is_some()
    });
    h.state_mut().ws.science.folder = "/tmp/valerian-science-maps".into();
    h.run_steps(2);
    h.snapshot("science-maps");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open the Astrometry tool, which sits in the Process step.
fn go_astrometry(h: &mut Harness<'_, App>) {
    h.state_mut().ws.select(crate::rail::Step::Process);
    h.run_steps(2);
    h.get_by_label("Astrometry").click();
    h.run_steps(2);
}

#[test]
fn the_astrometry_tool_draws_the_solution_and_the_colour_fit() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut h = headless(1000.0, 1100.0);
    h.run();
    open_colour(&mut h);
    go_astrometry(&mut h);
    assert!(shown(&h, "Draw the solution"));
    h.get_by_label("Draw the solution").click();
    wait_for(&mut h, "the drawing", |a| {
        a.ws.astro.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let status = h.state().ws.status.clone();
    let run = h
        .state()
        .ws
        .astro
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("no result: {status:?}"));
    assert!(run.annotated.stars.len() > 20);
    let r = run.residuals.as_ref().expect("residuals");
    assert!(
        r.stars.len() > 20 && r.rms_px < 0.3,
        "{} {}",
        r.stars.len(),
        r.rms_px
    );
    let c = run.calibration.as_ref().expect("colour fit");
    assert!(c.used > 15);
    assert!(shown(&h, "Colour fit") && shown(&h, "Residuals"));
    let report = dir.path().join("astrometry");
    h.state_mut().ws.astro.report = report.to_string_lossy().into_owned();
    h.get_by_label("Save report").click();
    h.run_steps(3);
    for f in [
        "annotated.png",
        "wcs_residuals.png",
        "pcc.png",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f}");
    }
}

#[test]
fn an_unsolved_image_says_so() {
    let mut h = headless(1000.0, 1100.0);
    h.run();
    open(&mut h);
    go_astrometry(&mut h);
    assert!(shown(&h, "no plate solution yet"));
}

#[test]
fn the_science_panel_measures_a_star_across_the_frames() {
    let dir = tempfile::tempdir().expect("tmp");
    write_dithered_frames(dir.path(), 12);
    let mut h = headless(1000.0, 1200.0);
    h.run();
    go_science(&mut h);
    h.state_mut().ws.science.folder = dir.path().to_string_lossy().into_owned();
    h.get_by_label("Measure the star").click();
    wait_for(&mut h, "the light curve", |a| {
        a.ws.science.light.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    let status = h.state().ws.status.clone();
    let run = h
        .state()
        .ws
        .science
        .light
        .as_ref()
        .unwrap_or_else(|| panic!("no curve: {status:?}"));
    assert!(run.curve.rows.iter().filter(|r| r.ok).count() >= 10);
    assert!(run.curve.scatter_mag < 0.1, "{}", run.curve.scatter_mag);
    assert!(shown(&h, "Light curve"));
}

#[test]
fn astrometry_snapshots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = window(1280.0, 980.0);
    open_colour(&mut h);
    go_astrometry(&mut h);
    h.get_by_label("Draw the solution").click();
    wait_for(&mut h, "the drawing", |a| a.ws.astro.result.is_some());
    // The report folder is shown: "/data" is a Unix-style fake, so Windows would join it with a backslash.
    h.state_mut().ws.astro.report = "/data/astrometry-report".into();
    h.run_steps(2);
    // WARP (Windows' software rasteriser) draws the picture's first row white (647 px, the
    // picture's width). Cause, measured on the Windows runner (issue #111): the
    // picture's top edge sits on the first row of the scroll area's clip (scissor) rectangle,
    // y = 177, an odd row. WARP shades pixels in 2x2 quads; with the scissor starting on an odd
    // row the first row's quad is half outside the scissor and its texture fetch returns white.
    // Repeating the draw with the scissor top at 401 or 403 gave a white row, at 400 or 402
    // none; moving the picture off the top row, or the clip one row up, removes it. It does not
    // depend on the texture (a flat grey one does it too), the filter (linear or nearest), the
    // wrap mode, the UVs or the picture's sub-pixel position, and an untextured quad is
    // unaffected. Lavapipe renders it correctly. So it is a WARP quirk, one pixel high, and
    // only on a software adapter; no change to the texture path avoids it. This one snapshot
    // allows that row and nothing more.
    h.snapshot_options(
        "astrometry-panel",
        &options().max_failed_pixels(WARP_EDGE_ROW_PIXELS),
    );
}
