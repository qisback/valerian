// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Tests of the usability features: the start screen with recent sessions, dropping
//! folders to sort them, and the keyboard shortcuts. Snapshots go to
//! `docs/book/src/images/ui/` like the others (`VALERIAN_GPU_TESTS=software`).

use std::path::Path;

use egui_kittest::kittest::Queryable;

use crate::prefs::FolderSet;
use crate::snapshots::{headless, path_text, shown, wait_for, window, write_session};

fn set_in(dir: &Path) -> FolderSet {
    FolderSet {
        lights: path_text(&dir.join("lights")),
        darks: path_text(&dir.join("darks")),
        flats: path_text(&dir.join("flats")),
        dark_flats: path_text(&dir.join("darkflats")),
        bias: String::new(),
    }
}

#[test]
fn the_start_screen_lists_recent_sessions_and_a_click_fills_the_folders() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 3);
    let mut h = headless(1200.0, 900.0);
    h.state_mut().prefs.remember_session(&set_in(dir.path()));
    h.run_steps(3);
    assert!(shown(&h, "RECENT SESSIONS"));
    h.get_by_label("Open session").click();
    h.run_steps(3);
    let cal = &h.state().ws.cal;
    assert_eq!(cal.lights, path_text(&dir.path().join("lights")));
    assert_eq!(cal.flats, path_text(&dir.path().join("flats")));
    assert!(cal.output.ends_with("calibrated"), "{}", cal.output);
    let reg = &h.state().ws.reg;
    assert_eq!(reg.input, cal.output);
    assert!(h.state().ws.stack.output.ends_with("stack.fits"));
    let n = h.state().ws.notice().expect("a notice");
    assert!(n.text.contains("Filled the Calibrate"), "{}", n.text);
    assert!(!n.error);
}

#[test]
fn a_session_with_a_missing_folder_is_flagged_and_restored_without_it() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 2);
    std::fs::remove_dir_all(dir.path().join("flats")).expect("rm");
    let mut h = headless(1200.0, 900.0);
    h.state_mut().prefs.remember_session(&set_in(dir.path()));
    h.run_steps(3);
    assert!(shown(&h, "Not found: flats"));
    h.get_by_label("Open session").click();
    h.run_steps(3);
    assert_eq!(h.state().ws.cal.flats, "");
    assert_ne!(h.state().ws.cal.darks, "");
    let n = h.state().ws.notice().expect("a notice");
    assert!(n.text.contains("left out: flats"), "{}", n.text);
}

#[test]
fn a_session_whose_lights_are_gone_can_only_be_forgotten() {
    let dir = tempfile::tempdir().expect("tmp");
    let gone = set_in(&dir.path().join("nowhere"));
    let mut h = headless(1200.0, 900.0);
    h.state_mut().prefs.remember_session(&gone);
    h.run_steps(3);
    assert!(shown(&h, "Not found: lights"));
    h.get_by_label("Open session").click();
    h.run_steps(3);
    assert!(
        h.state().ws.cal.lights.is_empty(),
        "a disabled button did nothing"
    );
    h.get_by_label("Forget").click();
    h.run_steps(3);
    assert_eq!(h.state().prefs.sessions.len(), 0);
    assert!(!shown(&h, "RECENT SESSIONS"));
    assert!(
        h.state()
            .ws
            .notice()
            .expect("notice")
            .text
            .contains("Forgot")
    );
}

#[test]
fn clearing_forgets_sessions_and_images() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 1);
    let mut h = headless(1200.0, 900.0);
    h.state_mut().prefs.remember_session(&set_in(dir.path()));
    h.state_mut()
        .prefs
        .remember_recent(&dir.path().join("old.fits"));
    h.run_steps(3);
    assert!(shown(&h, "RECENT IMAGES"));
    assert!(
        shown(&h, "Not found"),
        "a recent image that is gone is flagged"
    );
    h.get_by_label("Clear recent sessions and images").click();
    h.run_steps(3);
    assert!(h.state().prefs.sessions.is_empty() && h.state().prefs.recent.is_empty());
    assert!(!shown(&h, "RECENT SESSIONS"));
}

#[test]
fn starting_a_step_remembers_its_folders_as_a_session() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 3);
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    {
        let c = &mut h.state_mut().ws.cal;
        let s = set_in(dir.path());
        c.lights = s.lights.clone();
        c.darks = s.darks;
        c.output = path_text(&dir.path().join("cal"));
    }
    h.get_by_label("Calibrate lights").click();
    wait_for(&mut h, "calibration", |a| a.ws.cal.result.is_some());
    let s = &h.state().prefs.sessions;
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].lights, path_text(&dir.path().join("lights")));
    assert_eq!(s[0].flats, "");
}

#[test]
fn start_screen_snapshot() {
    if !crate::snapshots::enabled() {
        return;
    }
    let _serial = crate::snapshots::serial();
    // The picture shows these paths, so they are the same text on every platform: on Windows
    // "/tmp/..." names the temp folder on the current drive, and is made there.
    let root = std::path::PathBuf::from("/tmp/valerian-start-shot");
    let _ = std::fs::remove_dir_all(&root);
    for d in [
        "ic5070/lights",
        "ic5070/darks",
        "ic5070/flats",
        "m31/lights",
        "m31/darks",
    ] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let mut h = window(1280.0, 800.0);
    {
        // Pushed straight in: remember_session and remember_recent want drive-qualified paths on Windows.
        let p = &mut h.state_mut().prefs;
        let set = |name: &str| FolderSet {
            lights: format!("{}/{name}/lights", root.display()),
            darks: format!("{}/{name}/darks", root.display()),
            flats: format!("{}/{name}/flats", root.display()),
            ..FolderSet::default()
        };
        p.sessions = vec![set("ic5070"), set("m31")];
        p.recent = vec![root.join("ic5070/stack.fits")];
    }
    h.run_steps(4);
    h.snapshot("start-recents");
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------ dropping folders

/// A file dropped on the window, as the harness injects it.
#[derive(Debug)]
pub(crate) struct Drop(std::path::PathBuf);

impl eframe::egui::DroppedFile for Drop {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

pub(crate) fn drop_on(h: &mut egui_kittest::Harness<'_, crate::app::App>, paths: &[&Path]) {
    for p in paths {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Drop(p.to_path_buf())));
    }
    h.run_steps(2);
}

fn sorted(h: &mut egui_kittest::Harness<'_, crate::app::App>, dir: &Path) {
    drop_on(h, &[dir]);
    wait_for(h, "the sort", |a| a.ws.review.is_some());
}

#[test]
fn dropping_a_folder_sorts_it_and_accepting_fills_the_folders() {
    use crate::sorter::Kind;
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 4);
    let mut h = headless(1400.0, 1000.0);
    sorted(&mut h, dir.path());
    let r = h.state().ws.review.as_ref().expect("review");
    assert_eq!(r.count(Kind::Light), (4, 4));
    assert_eq!(r.count(Kind::Dark), (6, 6));
    assert_eq!(r.count(Kind::Flat), (6, 6));
    assert_eq!(r.count(Kind::DarkFlat), (6, 6));
    assert!(shown(&h, "Sort the frames"));
    assert!(shown(&h, "Accept sorting"));
    let n = h.state().ws.notice().expect("notice");
    assert!(n.text.contains("Sorted 22 frames"), "{}", n.text);
    // Nothing is filled until it is accepted.
    assert_eq!(h.state().ws.cal.lights, "");
    h.get_by_label("Accept sorting").click();
    wait_for(&mut h, "the folders", |a| a.ws.review.is_none());
    let cal = &h.state().ws.cal;
    assert_eq!(cal.lights, path_text(&dir.path().join("lights")));
    assert_eq!(cal.darks, path_text(&dir.path().join("darks")));
    assert_eq!(cal.flats, path_text(&dir.path().join("flats")));
    assert_eq!(cal.dark_flats, path_text(&dir.path().join("darkflats")));
    assert_eq!(cal.bias, "");
    assert!(cal.output.ends_with("calibrated"));
    assert_eq!(h.state().ws.reg.input, cal.output);
    assert!(h.state().ws.stack.output.ends_with("stack.fits"));
    assert!(
        !dir.path().join("valerian-sorted").exists(),
        "whole folders are used as they are"
    );
    assert_eq!(
        h.state().prefs.sessions.len(),
        1,
        "an accepted sorting is a session"
    );
}

#[test]
fn leaving_frames_out_gathers_the_rest_as_links() {
    use crate::sorter::Kind;
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 4);
    let mut h = headless(1400.0, 1000.0);
    sorted(&mut h, dir.path());
    {
        let r = h.state_mut().ws.review.as_mut().expect("review");
        let first = r
            .frames
            .iter()
            .position(|f| f.kind == Kind::Light)
            .expect("a light");
        r.frames[first].include = false;
        for _ in 0..2 {
            let dark = r
                .frames
                .iter()
                .position(|f| f.kind == Kind::Dark)
                .expect("a dark");
            r.assign(dark, Kind::Bias);
        }
    }
    h.run_steps(3);
    h.get_by_label("Accept sorting").click();
    wait_for(&mut h, "the folders", |a| a.ws.review.is_none());
    let cal = &h.state().ws.cal;
    let lights = std::path::PathBuf::from(&cal.lights);
    assert!(lights.ends_with("valerian-sorted/lights"), "{}", cal.lights);
    assert_eq!(std::fs::read_dir(&lights).expect("lights").count(), 3);
    assert!(
        Path::new(&cal.bias).ends_with("valerian-sorted/bias"),
        "{}",
        cal.bias
    );
    assert!(std::path::Path::new(&cal.darks).ends_with("valerian-sorted/darks"));
    let n = h.state().ws.notice().expect("notice");
    assert!(n.text.contains("gathered as links"), "{}", n.text);
    // The calibration runs on what was gathered.
    h.state_mut().ws.cal.output = path_text(&dir.path().join("cal"));
    h.run_steps(2);
    h.get_by_label("Calibrate lights").click();
    wait_for(&mut h, "calibration", |a| a.ws.cal.result.is_some());
    assert_eq!(h.state().ws.cal.result.as_ref().map(|r| r.lights), Some(3));
}

#[test]
fn cancelling_the_review_changes_nothing() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 2);
    let mut h = headless(1400.0, 1000.0);
    sorted(&mut h, dir.path());
    h.get_by_label("Cancel sorting").click();
    h.run_steps(3);
    assert!(h.state().ws.review.is_none());
    assert_eq!(h.state().ws.cal.lights, "");
    let n = h.state().ws.notice().expect("notice");
    assert!(n.text.contains("Dropped the sorting"), "{}", n.text);
}

#[test]
fn a_review_without_lights_cannot_be_accepted() {
    use crate::sorter::Kind;
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 2);
    let mut h = headless(1400.0, 1000.0);
    sorted(&mut h, dir.path());
    h.state_mut()
        .ws
        .review
        .as_mut()
        .expect("review")
        .assign_all(Kind::Light, Kind::Other);
    h.run_steps(3);
    assert!(shown(&h, "No frames are set as lights"));
    let ctx = h.ctx.clone();
    h.state_mut().ws.accept_review(&ctx);
    h.run_steps(2);
    assert!(h.state().ws.review.is_some());
    assert!(h.state().ws.notice().expect("notice").error);
}

#[test]
fn a_dropped_fits_file_still_opens_and_an_empty_folder_says_so() {
    let dir = tempfile::tempdir().expect("tmp");
    let f = dir.path().join("one.fits");
    valerian_core::fits::write(
        &f,
        &crate::snapshots::picture(),
        valerian_core::fits::Format::Float32,
    )
    .expect("write");
    let mut h = headless(1200.0, 900.0);
    drop_on(&mut h, &[&f]);
    wait_for(&mut h, "the image", |a| a.ws.doc.is_some());
    let empty = tempfile::tempdir().expect("tmp");
    drop_on(&mut h, &[empty.path()]);
    wait_for(&mut h, "the error", |a| {
        a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(h.state().ws.review.is_none());
    assert!(
        h.state()
            .ws
            .notice()
            .expect("notice")
            .text
            .contains("No FITS files")
    );
}

// ------------------------------------------------------------------ keyboard

use eframe::egui::{Key, Modifiers};

type Win<'a> = egui_kittest::Harness<'a, crate::app::App>;

fn notice_text(h: &Win<'_>) -> String {
    h.state().ws.notice().map(|n| n.text).unwrap_or_default()
}

#[test]
fn f1_and_the_question_mark_open_the_help_and_escape_closes_it() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press(Key::F1);
    h.run_steps(3);
    assert!(h.state().show_help);
    assert!(shown(&h, "Keyboard shortcuts"));
    assert!(shown(&h, "Ctrl+Enter"));
    assert!(
        notice_text(&h).contains("Showing the keyboard shortcuts"),
        "{}",
        notice_text(&h)
    );
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().show_help);
    assert!(notice_text(&h).contains("Closed the keyboard shortcuts"));
    h.key_press(Key::Questionmark);
    h.run_steps(3);
    assert!(h.state().show_help);
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert!(!h.state().show_help);
    // Help leads to the About page.
    h.key_press(Key::F1);
    h.run_steps(3);
    h.get_all_by_label("About Valerian")
        .last()
        .expect("the help's About button")
        .click();
    h.run_steps(3);
    assert!(h.state().show_about && !h.state().show_help);
}

#[test]
fn control_f1_toggles_the_about_page() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::F1);
    h.run_steps(3);
    assert!(h.state().show_about);
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().show_about);
}

#[test]
fn control_enter_runs_the_open_step_and_says_so() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    wait_for(&mut h, "the error", |a| {
        a.ws.status.as_ref().is_some_and(|s| s.1)
    });
    assert!(
        notice_text(&h).contains("Name a folder"),
        "{}",
        notice_text(&h)
    );
    // The same from F5, on another step.
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(3);
    assert_eq!(h.state().ws.tool, crate::workspace::Tool::Register);
    h.key_press(Key::F5);
    wait_for(&mut h, "the error", |a| {
        a.ws.status
            .as_ref()
            .is_some_and(|s| s.1 && s.0.contains("Name a folder"))
    });
}

#[test]
fn escape_cancels_a_running_step() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 6);
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    {
        let c = &mut h.state_mut().ws.cal;
        let s = set_in(dir.path());
        c.lights = s.lights;
        c.darks = s.darks;
        c.output = path_text(&dir.path().join("cal"));
    }
    h.key_press(Key::F5);
    h.run_steps(2);
    assert!(h.state().ws.running());
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(notice_text(&h).contains("Cancelling") || notice_text(&h).starts_with("Cancelled"));
    wait_for(&mut h, "the end of the run", |a| !a.ws.running());
    let n = h.state().ws.notice().expect("notice");
    assert!(n.text.starts_with("Cancelled"), "{}", n.text);
    assert!(!n.error);
    assert!(h.state().ws.cal.result.is_none());
}

#[test]
fn escape_with_nothing_to_cancel_is_left_alone() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().ws.notice().is_none());
}

#[test]
fn alt_arrows_move_between_steps_and_stop_at_the_ends() {
    use crate::workspace::Tool;
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert!(notice_text(&h).contains("Already at the first step"));
    for (want, said) in [
        (Tool::Register, "Step 2 of 5: Register"),
        (Tool::Stack, "Step 3 of 5: Stack"),
        (Tool::Stretch, "Step 4 of 5: Process"),
        (Tool::Export, "Step 5 of 5: Export"),
    ] {
        h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
        h.run_steps(2);
        assert_eq!(h.state().ws.tool, want);
        assert_eq!(notice_text(&h), said);
    }
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(2);
    assert!(notice_text(&h).contains("Already at the last step"));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(h.state().ws.tool, Tool::Stretch);
}

#[test]
fn plus_minus_zero_and_one_zoom_the_picture() {
    let mut h = headless(1200.0, 900.0);
    h.state_mut().ws.load(
        Path::new("/data/NGC_demo.fits"),
        crate::snapshots::picture(),
    );
    h.run_steps(4);
    assert!(h.state().ws.view.zoom.is_none());
    h.key_press(Key::Plus);
    h.run_steps(3);
    let z = h.state().ws.view.zoom.expect("zoomed in");
    assert!(notice_text(&h).starts_with("Zoom "), "{}", notice_text(&h));
    h.key_press(Key::Minus);
    h.run_steps(3);
    assert!(h.state().ws.view.zoom.expect("zoomed out") < z);
    h.key_press(Key::Num1);
    h.run_steps(3);
    assert_eq!(h.state().ws.view.zoom, Some(1.0));
    assert_eq!(notice_text(&h), "Zoom 100 percent");
    h.key_press(Key::Num0);
    h.run_steps(3);
    assert!(h.state().ws.view.zoom.is_none());
    assert!(notice_text(&h).contains("Fitted"));
}

#[test]
fn zooming_with_no_image_says_so() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    h.key_press(Key::Plus);
    h.run_steps(2);
    assert_eq!(notice_text(&h), "Open an image to zoom it.");
}

#[test]
fn n_and_f2_toggle_night_vision_with_a_notice() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    let was = crate::theme::is_night();
    h.key_press(Key::N);
    h.run_steps(3);
    assert_eq!(crate::theme::is_night(), !was);
    assert!(
        notice_text(&h).starts_with("Night vision"),
        "{}",
        notice_text(&h)
    );
    h.key_press(Key::F2);
    h.run_steps(3);
    assert_eq!(crate::theme::is_night(), was);
}

#[test]
fn plain_keys_go_to_a_text_box_that_has_the_keyboard() {
    let mut h = headless(1200.0, 900.0);
    h.run_steps(2);
    let was = crate::theme::is_night();
    h.get_all_by_role(eframe::egui::accesskit::Role::TextInput)
        .next()
        .expect("a text box")
        .focus();
    h.run_steps(3);
    h.key_press(Key::N);
    h.key_press(Key::Questionmark);
    h.run_steps(3);
    assert_eq!(crate::theme::is_night(), was, "N went to the box");
    assert!(!h.state().show_help);
}

#[test]
fn control_shift_o_chooses_a_folder_to_sort_and_enter_accepts() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 2);
    let (picker, seen) = crate::snapshots::fake_picker(Some(dir.path().to_path_buf()));
    let mut h = headless(1400.0, 1000.0);
    h.state_mut().ws.chooser.picker = picker;
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    crate::snapshots::settle(&mut h);
    {
        let asked = seen.lock().expect("log");
        assert_eq!(asked.len(), 1);
        assert!(asked[0].folder, "the folder dialog");
    }
    wait_for(&mut h, "the review", |a| a.ws.review.is_some());
    assert!(notice_text(&h).contains("Sorted"), "{}", notice_text(&h));
    h.key_press(Key::Enter);
    wait_for(&mut h, "accepting", |a| a.ws.review.is_none());
    assert_eq!(
        h.state().ws.cal.lights,
        path_text(&dir.path().join("lights"))
    );
}

#[test]
fn escape_drops_the_review() {
    let dir = tempfile::tempdir().expect("tmp");
    write_session(dir.path(), 2);
    let mut h = headless(1400.0, 1000.0);
    sorted(&mut h, dir.path());
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().ws.review.is_none());
    assert_eq!(h.state().ws.cal.lights, "");
}

#[test]
fn help_overlay_snapshot() {
    if !crate::snapshots::enabled() {
        return;
    }
    let _serial = crate::snapshots::serial();
    let mut h = window(1280.0, 800.0);
    h.run_steps(2);
    h.key_press(Key::F1);
    h.run_steps(4);
    h.snapshot("help-overlay");
}
