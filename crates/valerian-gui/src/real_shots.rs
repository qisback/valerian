// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Screenshots of the app working on real data, for the manual.
//!
//! The same application, with its real worker threads, is driven through the test
//! harness on the stacks and frames of two real sessions (a monochrome H-alpha
//! session with calibration frames and a one-shot colour Seestar session), and the
//! window is drawn by the software adapter. It does nothing unless both variables
//! are set, so the normal test run skips it:
//!
//! ```sh
//! VALERIAN_REAL_DATA=/data/valerian-real VALERIAN_REAL_SHOTS=/tmp/shots \
//!   VALERIAN_GPU_TESTS=software UPDATE_SNAPSHOTS=1 \
//!   cargo test --release -p valerian-gui real_data_shots -- --nocapture
//! ```
//!
//! The tests change the working directory to a folder of links to the data, so
//! the paths in the pictures are short and relative (`california/lights`); run
//! them on their own, as above, and not with the rest of the suite.
//!
//! `VALERIAN_REAL_DATA` holds `ha5/{lights,dark,flat,dflat}` and `ha5/stack.fits`
//! (five 300 s H-alpha lights and their calibration frames, and their stack),
//! `ss/subs/` and `ss/stack.fits` (60 raw Seestar subframes and their stack),
//! `m34/subs/` (184 raw Seestar subframes of M 34, for `real_data_shots_seestar_m34`) and
//! `gaia-dr3-g11-v2.vcat` (the star catalogue). The pictures are written to
//! `VALERIAN_REAL_SHOTS` with the prefix `real-`; copy them to
//! `docs/book/src/images/ui/`. Frames, stacks and catalogue stay out of the
//! repository.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{Pos2, pos2, vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, SnapshotOptions};

use crate::app::App;
use crate::rail::Step;
use crate::science_tool::SciView;
use crate::stretch_tool::CurveKind;

const WAIT: Duration = Duration::from_mins(30);

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Step the window until `done` holds.
fn drive(h: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            h.run_steps(3);
            return;
        }
        assert!(t0.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What a real-data test needs: the data, the folder the pictures go to and a
/// working folder, which is also the current directory.
struct Rig {
    data: PathBuf,
    shots: PathBuf,
    work: tempfile::TempDir,
}

/// One test at a time: they share the current directory.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn rig() -> Option<Rig> {
    let (Some(data), Some(shots)) = (var("VALERIAN_REAL_DATA"), var("VALERIAN_REAL_SHOTS")) else {
        eprintln!("skipping real-data shots: set VALERIAN_REAL_DATA and VALERIAN_REAL_SHOTS");
        return None;
    };
    if std::env::var("VALERIAN_GPU_TESTS").as_deref() != Ok("software") {
        eprintln!("skipping real-data shots: set VALERIAN_GPU_TESTS=software");
        return None;
    }
    std::fs::create_dir_all(&shots).expect("shots dir");
    let work = tempfile::tempdir().expect("work dir");
    std::env::set_current_dir(work.path()).expect("work dir");
    Some(Rig { data, shots, work })
}

impl Rig {
    /// Link `name` in the working folder to `what` in the data.
    fn link(&self, name: &str, what: &str) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(self.data.join(what), self.work.path().join(name))
            .expect("link the data");
        #[cfg(not(unix))]
        panic!("linking {name} to {what} needs a Unix machine");
    }

    fn harness(&self) -> Harness<'static, App> {
        harness_at(&self.shots)
    }
}

fn harness_at(shots: &Path) -> Harness<'static, App> {
    Harness::builder()
        .with_size(vec2(1360.0, 980.0))
        .with_options(
            SnapshotOptions::new()
                .threshold(0.6)
                .max_failed_pixels(usize::MAX / 2)
                .output_path(shots.to_path_buf()),
        )
        .wgpu()
        .build_eframe(|cc| App::new(cc, None))
}

fn shot(h: &mut Harness<'_, App>, name: &str) {
    h.run_steps(4);
    h.snapshot(format!("real-{name}"));
}

fn click(h: &mut Harness<'_, App>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
}

/// Open the panel of `step` and let it draw.
fn go(h: &mut Harness<'_, App>, step: Step) {
    h.state_mut().ws.select(step);
    h.run_steps(3);
}

/// A monochrome session: five 300 s H-alpha lights of the California Nebula with
/// darks, flats and dark flats, calibrated, registered and processed.
#[test]
fn real_data_shots_mono() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("california", "ha5");
    let mut h = rig.harness();
    {
        let c = &mut h.state_mut().ws.cal;
        c.lights = "california/lights".into();
        c.darks = "california/dark".into();
        c.flats = "california/flat".into();
        c.dark_flats = "california/dflat".into();
        c.output = "california-calibrated".into();
    }
    h.run_steps(3);
    shot(&mut h, "calibrate-ready");
    click(&mut h, "Calibrate lights");
    drive(&mut h, "calibration", |a| a.ws.cal.result.is_some());
    shot(&mut h, "calibrate-done");

    go(&mut h, Step::Register);
    click(&mut h, "Register frames");
    // While it runs: the notice carries the step's own lines.
    drive(&mut h, "the first line from the registration", |a| {
        a.ws.notice().is_some_and(|n| n.detail.contains("stars"))
    });
    shot(&mut h, "register-running");
    drive(&mut h, "registration", |a| a.ws.reg.result.is_some());
    shot(&mut h, "register-done");

    // The staged stack of the same five lights.
    h.state_mut().ws.open(Path::new("california/stack.fits"));
    drive(&mut h, "the stack to open", |a| a.ws.measured.is_some());
    shot(&mut h, "mono-stretch");
    {
        let t = &mut h.state_mut().ws.stretch;
        t.kind = CurveKind::Ghs;
        t.ghs.stretch = 3.0;
        t.ghs.b = 2.0;
        t.ghs.sp = 0.04;
        t.ghs.lp = 0.0;
        t.ghs.hp = 0.9;
    }
    h.state_mut().ws.mark_dirty();
    shot(&mut h, "mono-ghs");
    h.state_mut().ws.stretch.kind = CurveKind::Auto;
    h.state_mut().ws.mark_dirty();
    click(&mut h, "Star removal");
    click(&mut h, "Remove stars");
    drive(&mut h, "star removal", |a| a.ws.stars.result.is_some());
    // The nebula is faint: draw the starless layer with a brighter sky.
    h.state_mut().ws.stretch.target = 0.2;
    h.state_mut().ws.mark_dirty();
    shot(&mut h, "mono-starless");
    go(&mut h, Step::Export);
    shot(&mut h, "mono-export");
}

/// A one-shot colour session: sixty raw Seestar subframes of IC 5070, stacked,
/// plate solved, colour calibrated against Gaia, star-removed and drizzled.
#[test]
fn real_data_shots_colour() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("ic5070", "ss");
    rig.link("gaia-dr3-g11-v2.vcat", "gaia-dr3-g11-v2.vcat");
    let mut h = rig.harness();

    go(&mut h, Step::Stack);
    {
        let s = &mut h.state_mut().ws.stack;
        s.input = "ic5070/subs".into();
        s.output = "ic5070-stack.fits".into();
    }
    h.run_steps(3);
    shot(&mut h, "stack-ready");
    click(&mut h, "Stack frames");
    drive(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    shot(&mut h, "stack-done");

    // The staged stack of the same frames.
    h.state_mut().ws.open(Path::new("ic5070/stack.fits"));
    drive(&mut h, "the stack to open", |a| a.ws.measured.is_some());
    h.state_mut().ws.solve.catalogue_path = "gaia-dr3-g11-v2.vcat".into();
    shot(&mut h, "colour-stretch");
    {
        let t = &mut h.state_mut().ws.stretch;
        t.kind = CurveKind::Ghs;
        t.ghs.stretch = 2.5;
        t.ghs.b = 1.5;
        t.ghs.sp = 0.05;
        t.ghs.lp = 0.0;
        t.ghs.hp = 0.9;
    }
    h.state_mut().ws.mark_dirty();
    shot(&mut h, "colour-ghs");
    h.state_mut().ws.stretch.kind = CurveKind::Auto;
    h.state_mut().ws.mark_dirty();
    click(&mut h, "Plate solve");
    click(&mut h, "Solve");
    drive(&mut h, "the plate solve", |a| {
        a.ws.solve.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.solve.result.is_some(),
        "the plate solve failed: {:?}",
        h.state().ws.status
    );
    shot(&mut h, "colour-solve");
    click(&mut h, "Colour");
    click(&mut h, "Calibrate colour");
    drive(&mut h, "the colour calibration", |a| {
        a.ws.colour.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.colour.result.is_some(),
        "the colour calibration failed: {:?}",
        h.state().ws.status
    );
    shot(&mut h, "colour-photocal");
    click(&mut h, "Star removal");
    click(&mut h, "Remove stars");
    drive(&mut h, "star removal", |a| a.ws.stars.result.is_some());
    shot(&mut h, "colour-starless");

    // The night-vision palette on the same stack.
    let ctx = h.ctx.clone();
    h.state_mut().set_night(&ctx, true);
    click(&mut h, "Stretch");
    shot(&mut h, "colour-night");
    h.state_mut().set_night(&ctx, false);

    // Drizzle of the same subframes, last because it takes the longest.
    go(&mut h, Step::Stack);
    click(&mut h, "Drizzle");
    h.state_mut().ws.drizzle.folder = "ic5070/subs".into();
    h.state_mut().ws.drizzle_output_text = "ic5070-drizzled.fits".into();
    h.run_steps(3);
    click(&mut h, "Drizzle frames");
    drive(&mut h, "the drizzle", |a| {
        a.ws.drizzle.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.drizzle.result.is_some(),
        "the drizzle failed: {:?}",
        h.state().ws.status
    );
    shot(&mut h, "colour-drizzle");
}

/// Cancelling real runs: a Calibrate of the H-alpha session and a Stack of the
/// Seestar subframes, each stopped part way through, with the notice while it
/// runs (the Cancel button) and after the cancel. The output folders must not
/// exist afterwards.
#[test]
fn real_data_shots_cancel() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("california", "ha5");
    rig.link("ic5070", "ss");
    let mut h = rig.harness();
    let cancelled = |a: &App| {
        !a.ws.running()
            && a.ws
                .notice()
                .is_some_and(|n| n.text.starts_with("Cancelled") && !n.error)
    };

    {
        let c = &mut h.state_mut().ws.cal;
        c.lights = "california/lights".into();
        c.darks = "california/dark".into();
        c.flats = "california/flat".into();
        c.dark_flats = "california/dflat".into();
        c.output = "california-calibrated".into();
    }
    h.run_steps(3);
    click(&mut h, "Calibrate lights");
    drive(&mut h, "the first calibrated light", |a| {
        a.ws.notice()
            .is_some_and(|n| n.detail.contains("Calibrating"))
    });
    shot(&mut h, "cancel-calibrate-running");
    click(&mut h, "Cancel");
    shot(&mut h, "cancel-calibrate-cancelling");
    drive(&mut h, "the calibration to stop", cancelled);
    shot(&mut h, "cancel-calibrate-cancelled");
    assert!(
        !Path::new("california-calibrated").exists(),
        "a cancelled calibration leaves no output folder"
    );
    assert!(h.state().ws.cal.result.is_none());

    go(&mut h, Step::Stack);
    {
        let s = &mut h.state_mut().ws.stack;
        s.input = "ic5070/subs".into();
        s.output = "ic5070-stack.fits".into();
    }
    h.run_steps(3);
    click(&mut h, "Stack frames");
    drive(&mut h, "the registration lines", |a| {
        a.ws.notice().is_some_and(|n| n.detail.contains("stars"))
    });
    shot(&mut h, "cancel-stack-running");
    click(&mut h, "Cancel");
    drive(&mut h, "the stack to stop", cancelled);
    shot(&mut h, "cancel-stack-cancelled");
    assert!(
        !Path::new("ic5070-stack.fits").exists(),
        "a cancelled stack writes no file"
    );
    assert!(h.state().ws.stack.result.is_none());

    // The same step runs again to the end after a cancel.
    click(&mut h, "Stack frames");
    drive(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    assert!(Path::new("ic5070-stack.fits").is_file());
}

/// The Science panel on the sixty IC 5070 subframes: measure, the plots in the viewer, the
/// limits and the approve/reject table, then Use for stacking and a stack that leaves the
/// rejected frames out. `VALERIAN_REAL_DATA` holds `ss/subs/`.
#[test]
fn real_data_shots_science() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("ic5070", "ss");
    let mut h = rig.harness();
    go(&mut h, Step::Stack);
    h.state_mut().ws.tool = crate::workspace::Tool::Science;
    h.state_mut().ws.science.folder = "ic5070/subs".into();
    h.run_steps(3);
    click(&mut h, "Measure frames");
    drive(&mut h, "the measurements", |a| {
        a.ws.science.analysis.is_some()
    });
    shot(&mut h, "science-subframes");
    let (approved, total) = {
        let a = h.state().ws.science.analysis.as_ref().expect("analysis");
        (a.approved(), a.frames.len())
    };
    assert_eq!(total, 60);
    assert!(approved < total && approved > 30, "{approved} of {total}");
    // The button is below the fold of this window; its effect is the same.
    let rejected = h.state().ws.science.rejected();
    h.state_mut().ws.stack.exclude = rejected;
    assert_eq!(h.state().ws.stack.exclude.len(), total - approved);
    {
        let s = &mut h.state_mut().ws.stack;
        s.input = "ic5070/subs".into();
        s.output = "ic5070-approved.fits".into();
    }
    h.state_mut().ws.tool = crate::workspace::Tool::Stack;
    h.run_steps(3);
    click(&mut h, "Stack frames");
    drive(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    let r = h.state().ws.stack.result.as_ref().expect("stack");
    assert_eq!((r.total, r.used), (total, approved));
    shot(&mut h, "science-stacked");
}

/// The Astrometry tool on the solved IC 5070 stack and the Science panel's light curve of the
/// brightest unsaturated star across the sixty subframes. `VALERIAN_REAL_DATA` holds
/// `ss/solved.fits` (the stack after `valerian solve`), `ss/subs/` and the catalogue.
#[test]
fn real_data_shots_astrometry() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("ic5070", "ss");
    rig.link("gaia-dr3-g11-v2.vcat", "gaia-dr3-g11-v2.vcat");
    let mut h = rig.harness();
    h.state_mut().ws.open(Path::new("ic5070/solved.fits"));
    h.state_mut().ws.solve.catalogue_path = "gaia-dr3-g11-v2.vcat".into();
    h.run_steps(4);
    go(&mut h, Step::Process);
    click(&mut h, "Astrometry");
    click(&mut h, "Draw the solution");
    drive(&mut h, "the drawing", |a| a.ws.astro.result.is_some());
    for (view, name) in [
        (crate::astro_tool::View::Annotated, "astrometry-annotated"),
        (crate::astro_tool::View::Residuals, "astrometry-residuals"),
        (
            crate::astro_tool::View::ResidualMap,
            "astrometry-residual-map",
        ),
        (crate::astro_tool::View::Colour, "astrometry-colour"),
        (crate::astro_tool::View::Zeropoint, "astrometry-zeropoint"),
        (
            crate::astro_tool::View::Limit,
            "astrometry-limiting-magnitude",
        ),
    ] {
        h.state_mut().ws.astro.view = view;
        shot(&mut h, name);
    }
    let run = h.state().ws.astro.result.as_ref().expect("run");
    assert!(run.residuals.is_some() && run.calibration.is_some());
    assert!(run.residual_map.is_some() && run.photometry.is_some());
    go(&mut h, Step::Stack);
    h.state_mut().ws.tool = crate::workspace::Tool::Science;
    h.state_mut().ws.science.folder = "ic5070/subs".into();
    h.run_steps(3);
    click(&mut h, "Measure the star");
    drive(&mut h, "the light curve", |a| a.ws.science.light.is_some());
    shot(&mut h, "photometry-lightcurve");
    let l = h.state().ws.science.light.as_ref().expect("curve");
    assert!(l.curve.rows.iter().filter(|r| r.ok).count() > 50);
}

/// The Science panel's integration views on the sixty IC 5070 subframes: stack for the maps, then the
/// rejection, count and weight maps, the noise against the square root and the histograms.
/// `VALERIAN_REAL_DATA` holds `ss/subs/`.
#[test]
fn real_data_shots_integration() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("ic5070", "ss");
    let mut h = rig.harness();
    go(&mut h, Step::Stack);
    h.state_mut().ws.tool = crate::workspace::Tool::Science;
    h.state_mut().ws.science.folder = "ic5070/subs".into();
    h.run_steps(3);
    click(&mut h, "Stack for the maps");
    drive(&mut h, "the stack for the maps", |a| {
        a.ws.science.integration.is_some()
    });
    for (view, name) in [
        (SciView::RejectionHigh, "integration-rejected-high"),
        (SciView::Count, "integration-count"),
        (SciView::Noise, "integration-noise"),
        (SciView::Histogram, "integration-histogram"),
    ] {
        h.state_mut().ws.science.view = view;
        shot(&mut h, name);
    }
    let run = h.state().ws.science.integration.as_ref().expect("run");
    assert_eq!(run.used, 60);
    assert!(run.noise.as_ref().is_some_and(|c| c.reduction() > 3.0));
}

/// The Optics tool on the 60-frame Seestar stack of IC 5070: the aberration inspector, both
/// maps, the corner FWHM and the star profile. `VALERIAN_REAL_DATA` holds `ss/stack.fits`.
#[test]
fn real_data_shots_optics() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("ic5070", "ss");
    let mut h = rig.harness();
    h.state_mut().ws.open(Path::new("ic5070/stack.fits"));
    h.run_steps(4);
    go(&mut h, Step::Process);
    click(&mut h, "Optics");
    click(&mut h, "Measure the stars");
    drive(&mut h, "the measurements", |a| a.ws.optics.result.is_some());
    for (view, name) in [
        (crate::optics_tool::View::Inspector, "optics-inspector"),
        (crate::optics_tool::View::Fwhm, "optics-fwhm-map"),
        (crate::optics_tool::View::Ellipticity, "optics-ecc-map"),
        (crate::optics_tool::View::Tilt, "optics-corners"),
        (crate::optics_tool::View::Psf, "optics-profile"),
    ] {
        h.state_mut().ws.optics.view = view;
        shot(&mut h, name);
    }
    let run = h.state().ws.optics.result.as_ref().expect("result");
    assert!(run.field.stars.len() > 200 && !run.tilt.hints.is_empty());
}

/// Sorting dropped folders, the recent sessions and the help list, on two real captures:
/// the ASI1600MM H-alpha session (lights, darks, flats and dark flats in separate folders,
/// typed `LIGHT`, `DARK`, `FLAT` and `DARKFLAT` in the header) and the Seestar IC 5070 subs
/// (lights only). `VALERIAN_SORT_DATA` holds `ha1600/` and `seestar-ic5070/`. The folders
/// are linked under a fixed working folder so the paths in the pictures are short and stable.
#[test]
fn real_data_shots_sorting() {
    let _one = exclusive();
    let (Some(data), Some(shots)) = (var("VALERIAN_SORT_DATA"), var("VALERIAN_REAL_SHOTS")) else {
        eprintln!("skipping sorting shots: set VALERIAN_SORT_DATA and VALERIAN_REAL_SHOTS");
        return;
    };
    if std::env::var("VALERIAN_GPU_TESTS").as_deref() != Ok("software") {
        eprintln!("skipping sorting shots: set VALERIAN_GPU_TESTS=software");
        return;
    }
    std::fs::create_dir_all(&shots).expect("shots dir");
    let work = std::env::temp_dir().join("valerian-astro");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    std::env::set_current_dir(&work).expect("work dir");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(data.join("ha1600"), work.join("ha1600")).expect("link");
        std::os::unix::fs::symlink(data.join("seestar-ic5070"), work.join("ic5070")).expect("link");
    }
    #[cfg(not(unix))]
    panic!("linking the data needs a Unix machine");
    let mut h = harness_at(&shots);
    let sorted = |h: &mut Harness<'_, App>, dir: &str| {
        crate::usability_tests::drop_on(h, &[Path::new(dir)]);
        drive(h, "the sort", |a| a.ws.review.is_some());
    };

    sorted(&mut h, "ha1600");
    shot(&mut h, "sort-review");
    click(&mut h, "Accept sorting");
    drive(&mut h, "the folders", |a| a.ws.review.is_none());
    shot(&mut h, "sort-accepted");
    h.key_press(eframe::egui::Key::F1);
    shot(&mut h, "help-overlay");
    h.key_press(eframe::egui::Key::F1);
    h.run_steps(3);

    sorted(&mut h, "ic5070");
    shot(&mut h, "sort-review-lights-only");
    click(&mut h, "Accept sorting");
    drive(&mut h, "the folders", |a| a.ws.review.is_none());
    // Back to the start screen with the two sessions.
    h.state_mut().ws.status = None;
    shot(&mut h, "start-recents");
}

/// A one-shot colour session on a Seestar S50 open cluster: the 184 raw subframes of M 34 stacked, plate
/// solved, colour calibrated against Gaia and measured. `VALERIAN_REAL_DATA` holds `m34/subs/` and the
/// catalogue. The deconvolution and background tools have no panel in the app, so the book
/// shows those two steps on the command line only.
#[test]
fn real_data_shots_seestar_m34() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("m34", "m34");
    rig.link("gaia-dr3-g11-v2.vcat", "gaia-dr3-g11-v2.vcat");
    let mut h = rig.harness();

    go(&mut h, Step::Stack);
    {
        let s = &mut h.state_mut().ws.stack;
        s.input = "m34/subs".into();
        s.output = "m34-stack.fits".into();
    }
    h.run_steps(3);
    shot(&mut h, "m34-stack-ready");
    click(&mut h, "Stack frames");
    drive(&mut h, "the stack", |a| a.ws.stack.result.is_some());
    shot(&mut h, "m34-stack-done");

    h.state_mut().ws.open(Path::new("m34-stack.fits"));
    drive(&mut h, "the stack to open", |a| a.ws.measured.is_some());
    h.state_mut().ws.solve.catalogue_path = "gaia-dr3-g11-v2.vcat".into();
    shot(&mut h, "m34-stretch");
    click(&mut h, "Plate solve");
    click(&mut h, "Solve");
    drive(&mut h, "the plate solve", |a| {
        a.ws.solve.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.solve.result.is_some(),
        "the plate solve failed: {:?}",
        h.state().ws.status
    );
    shot(&mut h, "m34-solve");
    click(&mut h, "Colour");
    click(&mut h, "Calibrate colour");
    drive(&mut h, "the colour calibration", |a| {
        a.ws.colour.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.colour.result.is_some(),
        "the colour calibration failed: {:?}",
        h.state().ws.status
    );
    shot(&mut h, "m34-photocal");
    go(&mut h, Step::Process);
    click(&mut h, "Astrometry");
    click(&mut h, "Draw the solution");
    drive(&mut h, "the astrometry drawing", |a| {
        a.ws.astro.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    for (view, name) in [
        (
            crate::astro_tool::View::ResidualMap,
            "m34-astrometry-residual-map",
        ),
        (
            crate::astro_tool::View::Zeropoint,
            "m34-astrometry-zeropoint",
        ),
        (
            crate::astro_tool::View::Limit,
            "m34-astrometry-limiting-magnitude",
        ),
    ] {
        h.state_mut().ws.astro.view = view;
        shot(&mut h, name);
    }
    let run = h.state().ws.astro.result.as_ref().expect("astrometry run");
    assert!(run.residual_map.is_some() && run.photometry.is_some());
    go(&mut h, Step::Process);
    click(&mut h, "Optics");
    click(&mut h, "Measure the stars");
    drive(&mut h, "the measurements", |a| a.ws.optics.result.is_some());
    shot(&mut h, "m34-optics");
}

/// Wait for the pyramid of the open image and for the tiles of the window to be
/// drawn: no more are made over a few frames.
fn settle_tiles(h: &mut Harness<'_, App>) {
    drive(h, "the zoom levels", |a| !a.ws.tiles.building());
    let mut last = usize::MAX;
    for _ in 0..200 {
        h.run_steps(2);
        let made = h.state().ws.tiles.made;
        if made == last {
            return;
        }
        last = made;
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// A bright pixel on the flank of a star in the middle half of the open image:
/// the brightest one below a quarter of the peak, so the readout shows values
/// that are neither noise nor saturated.
fn brightest(a: &App) -> (usize, usize) {
    let img = &a.ws.doc.as_ref().expect("an image is open").full;
    let (w, h) = (img.width(), img.height());
    let value = |x: usize, y: usize| -> f32 { img.planes.iter().map(|p| p.data[y * w + x]).sum() };
    let region = || (h / 4..3 * h / 4).flat_map(move |y| (w / 4..3 * w / 4).map(move |x| (x, y)));
    let peak = region()
        .map(|(x, y)| value(x, y))
        .filter(|v| v.is_finite())
        .fold(f32::MIN, f32::max);
    region()
        .filter(|&(x, y)| value(x, y) < 0.25 * peak)
        .max_by(|&(ax, ay), &(bx, by)| value(ax, ay).total_cmp(&value(bx, by)))
        .unwrap_or((w / 2, h / 2))
}

/// Zoom to `zoom` with image pixel `(x, y)` at the middle of the view, put the
/// pointer on that pixel and let the tiles fill in.
fn look_at(h: &mut Harness<'_, App>, (x, y): (usize, usize), zoom: f32) {
    let size = {
        let d = h.state().ws.doc.as_ref().expect("an image is open");
        vec2(d.full.width() as f32, d.full.height() as f32)
    };
    h.state_mut()
        .ws
        .view
        .centre_on(zoom, x as f32 + 0.5, y as f32 + 0.5, size);
    h.run_steps(4);
    // Find where the pixel is on screen from a probe, then aim at its middle.
    let mut at: Pos2 = pos2(820.0, 460.0);
    for _ in 0..3 {
        h.hover_at(at);
        h.run_steps(3);
        let r = h
            .state()
            .ws
            .reading
            .clone()
            .expect("the pointer is over the picture");
        let (dx, dy) = (x as f32 - r.x as f32, y as f32 - r.y as f32);
        if dx == 0.0 && dy == 0.0 {
            break;
        }
        at += vec2(dx, dy) * zoom;
    }
    settle_tiles(h);
}

/// 1:1 and 4:1 on a real stack, with the pixel readout under the pointer: the
/// H-alpha stack (monochrome, 4656 x 3520) and the Seestar stack, plate solved so
/// that the readout shows the sky position.
#[test]
fn real_data_shots_zoom() {
    let _one = exclusive();
    let Some(rig) = rig() else { return };
    rig.link("california", "ha5");
    rig.link("ic5070", "ss");
    rig.link("gaia-dr3-g11-v2.vcat", "gaia-dr3-g11-v2.vcat");
    let mut h = rig.harness();

    h.state_mut().ws.open(Path::new("california/stack.fits"));
    drive(&mut h, "the stack to open", |a| a.ws.measured.is_some());
    drive(&mut h, "the zoom levels", |a| !a.ws.tiles.building());
    shot(&mut h, "zoom-fit-mono");
    let star = brightest(h.state());
    look_at(&mut h, star, 1.0);
    shot(&mut h, "zoom-1to1-mono");
    look_at(&mut h, star, 4.0);
    shot(&mut h, "zoom-4to1-mono");
    // The pyramid is bounded: the cache never passes its budget.
    assert!(h.state().ws.tiles.cached_bytes() <= crate::tiles::CACHE_BYTES);

    h.state_mut().ws.open(Path::new("ic5070/stack.fits"));
    drive(&mut h, "the stack to open", |a| a.ws.measured.is_some());
    h.state_mut().ws.solve.catalogue_path = "gaia-dr3-g11-v2.vcat".into();
    click(&mut h, "Plate solve");
    click(&mut h, "Solve");
    drive(&mut h, "the plate solve", |a| {
        a.ws.solve.result.is_some() || a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        h.state().ws.solve.result.is_some(),
        "the plate solve failed: {:?}",
        h.state().ws.status
    );
    click(&mut h, "Stretch");
    drive(&mut h, "the zoom levels", |a| !a.ws.tiles.building());
    let star = brightest(h.state());
    look_at(&mut h, star, 1.0);
    shot(&mut h, "zoom-1to1-colour");
    look_at(&mut h, star, 4.0);
    shot(&mut h, "zoom-4to1-colour");
    let ctx = h.ctx.clone();
    h.state_mut().set_night(&ctx, true);
    settle_tiles(&mut h);
    shot(&mut h, "zoom-4to1-night");
    h.state_mut().set_night(&ctx, false);
}

/// Build time, memory and tile speed of the pyramid on the real stacks, for the
/// changelog. Prints one line per figure; needs only `VALERIAN_REAL_DATA`.
#[test]
fn real_data_pyramid_figures() {
    use valerian_core::fits;
    use valerian_core::preview::rgba8;
    use valerian_core::pyramid::{Pyramid, TileKey, tile_grid};
    let Some(data) = var("VALERIAN_REAL_DATA") else {
        eprintln!("skipping the pyramid figures: set VALERIAN_REAL_DATA");
        return;
    };
    let rss = || -> f64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|k| k.parse::<f64>().ok())
            })
            .map_or(0.0, |kb| kb / 1024.0)
    };
    for (name, file) in [
        ("mono 4656x3520", "ha5/stack.fits"),
        ("colour 1080x1920", "ss/stack.fits"),
    ] {
        let t0 = Instant::now();
        let img = std::sync::Arc::new(fits::read(&data.join(file)).expect("read the stack"));
        let read = t0.elapsed().as_secs_f64();
        let before = rss();
        let t1 = Instant::now();
        let pyr = Pyramid::build(std::sync::Arc::clone(&img));
        let build = t1.elapsed().as_secs_f64();
        let full_mb = img.planes.iter().map(|p| p.data.len() * 4).sum::<usize>() as f64 / 1e6;
        let tool = crate::stretch_tool::StretchTool::default();
        let m = tool.measure(&img, None).expect("levels");
        let (cols, rows) = tile_grid(img.width(), img.height());
        let t2 = Instant::now();
        let mut n = 0;
        for ty in 0..rows {
            for tx in 0..cols {
                let tile = pyr.tile(TileKey { level: 0, tx, ty });
                let shown = tool.render(&tile, &m).expect("stretch");
                std::hint::black_box(rgba8(&shown));
                n += 1;
            }
        }
        let per_tile = t2.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        eprintln!(
            "PYRAMID-FIGURES {name}: read {read:.2} s; pyramid {} levels in {:.0} ms;              extra memory {:.1} MB on {:.1} MB of pixels ({:.0} percent);              stretch+convert {per_tile:.2} ms per 512 px tile ({n} tiles);              peak RSS {:.0} MB (was {before:.0} MB before the build)",
            pyr.count(),
            build * 1000.0,
            pyr.extra_bytes() as f64 / 1e6,
            full_mb,
            100.0 * pyr.extra_bytes() as f64 / 1e6 / full_mb,
            rss(),
        );
    }
}
