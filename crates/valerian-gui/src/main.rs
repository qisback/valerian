// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian-gui`: the desktop app.
//!
//! A window with the workspace (tool panel and viewer) and the About page.
//! `valerian-gui [FILE.fits]` opens a file at start, which is how a file manager's
//! "Open with" works. The window size is remembered between runs (see [`prefs`]).
//! `-V` prints the name and version, `--version` adds the
//! copyright and licence notices; both exit without opening a window, which is
//! how the release workflow checks a packaged binary starts.

// A double-clicked valerian-gui.exe must not open a console window. The test harness
// is a console program, so it keeps the default subsystem.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#![allow(
    clippy::too_many_lines,
    reason = "immediate-mode drawing reads best top to bottom"
)]

mod about;
mod app;
mod astro_tool;
mod colour_tool;
mod dialogs;
mod drizzle_tool;
mod gpu_tool;
mod optics_tool;
mod pipeline;
mod rail;
mod readout;
// Real-data shots link the data with symlinks and run by hand on Unix only.
#[cfg(all(test, unix))]
mod real_shots;
mod review;
mod science_tool;
mod shortcuts;
#[cfg(test)]
mod snapshots;
mod solve_tool;
mod sorter;
mod star_tool;
mod start;
mod stretch_tool;
mod theme;
mod tiles;
#[cfg(test)]
mod usability_tests;
mod view;
mod widgets;
mod workspace;

// Packaging: window icon, console attach for --version on Windows, remembered settings.
mod branding;
mod console;
mod prefs;

use eframe::egui;

fn main() -> eframe::Result {
    let mut open = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-V" => {
                console::attach();
                println!("valerian-gui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--version" => {
                console::attach();
                println!(
                    "{}",
                    valerian_core::about::long_version(
                        "valerian-gui",
                        env!("CARGO_PKG_VERSION"),
                        valerian_gpu::WGPU_VERSION,
                    )
                );
                return Ok(());
            }
            path if !path.starts_with('-') && open.is_none() => {
                open = Some(std::path::PathBuf::from(path));
            }
            _ => {}
        }
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(prefs::Prefs::load_default().window_or_default())
        .with_min_inner_size([640.0, 480.0])
        .with_title(format!("Valerian {}", env!("CARGO_PKG_VERSION")))
        .with_app_id("valerian");
    if let Some(icon) = branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "Valerian",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, open)))),
    )
}
