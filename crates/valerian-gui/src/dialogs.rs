// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Native file choosers for opening and saving.
//!
//! The dialog is the operating system's own (`rfd`: the Windows common dialog,
//! the desktop portal on Linux), shown from a worker thread so the window keeps
//! drawing while it is open. What the dialog is for is a [`Target`]; the picked
//! path goes to that target's box when the dialog closes. The program that shows
//! the dialog is a [`Picker`], which tests replace so no window opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::Context;

/// What a dialog chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The image to open.
    Image,
    /// Where the stretched image is written.
    StretchOut,
    /// Where the starless image is written.
    StarsOut,
    /// Where the solved copy is written.
    SolveOut,
    /// Where the colour-calibrated image is written.
    ColourOut,
    /// The star catalogue to solve against.
    Catalogue,
    /// The folder of frames to drizzle.
    DrizzleFolder,
    /// Where the drizzled image is written.
    DrizzleOut,
    /// A folder of frames to sort into lights, darks, flats and bias.
    SortFolder,
    /// The folder of light frames to calibrate.
    CalLights,
    /// The folder of dark frames.
    CalDarks,
    /// The folder of flat frames.
    CalFlats,
    /// The folder of dark flats.
    CalDarkFlats,
    /// The folder of bias frames.
    CalBias,
    /// The folder the calibrated frames go to.
    CalOut,
    /// The folder of frames to register.
    RegIn,
    /// The folder the registered frames go to.
    RegOut,
    /// The folder of frames to stack.
    StackIn,
    /// Where the stack is written.
    StackOut,
    /// Where the finished picture is exported.
    ExportOut,
    /// The folder of frames the Science panel measures.
    ScienceIn,
}

impl Target {
    /// Whether the dialog chooses a folder.
    #[must_use]
    pub fn folder(self) -> bool {
        matches!(
            self,
            Self::DrizzleFolder
                | Self::SortFolder
                | Self::CalLights
                | Self::CalDarks
                | Self::CalFlats
                | Self::CalDarkFlats
                | Self::CalBias
                | Self::CalOut
                | Self::RegIn
                | Self::RegOut
                | Self::StackIn
                | Self::ScienceIn
        )
    }

    /// Whether the dialog chooses a file to write.
    #[must_use]
    pub fn saves(self) -> bool {
        !matches!(self, Self::Image | Self::Catalogue) && !self.folder()
    }
}

/// What to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The window title.
    pub title: String,
    /// Choose a file to write, not to read.
    pub save: bool,
    /// Choose a folder, not a file.
    pub folder: bool,
    /// The one filter offered: its name and extensions.
    pub filter: (&'static str, &'static [&'static str]),
    /// Where the dialog starts, when that folder exists.
    pub dir: Option<PathBuf>,
    /// The file name offered when saving.
    pub file_name: Option<String>,
}

/// The FITS file extensions.
pub const FITS: (&str, &[&str]) = ("FITS images", &["fits", "fit", "fts"]);
/// What an export can be written as.
pub const EXPORT: (&str, &[&str]) = ("FITS or PNG images", &["fits", "png"]);
/// The catalogue file extension.
pub const CATALOGUE: (&str, &[&str]) = ("Star catalogues", &["vcat"]);

impl Request {
    /// The dialog for `target`, starting from what is already in its box.
    #[must_use]
    pub fn for_target(target: Target, current: &str) -> Self {
        let path = Path::new(current.trim());
        // A folder is chosen from inside itself; a file from beside it.
        let start = if target.folder() && path.is_dir() {
            Some(path)
        } else {
            path.parent()
        };
        let dir = start
            .filter(|d| !d.as_os_str().is_empty() && d.is_dir())
            .map(Path::to_path_buf);
        let (title, filter) = match target {
            Target::Image => ("Open a FITS image", FITS),
            Target::StretchOut => ("Save the stretched image", FITS),
            Target::StarsOut => ("Save the starless image", FITS),
            Target::SolveOut => ("Save the image with its WCS", FITS),
            Target::ColourOut => ("Save the calibrated image", FITS),
            Target::Catalogue => ("Choose a star catalogue", CATALOGUE),
            Target::DrizzleFolder => ("Choose the folder of calibrated frames", FITS),
            Target::DrizzleOut => ("Save the drizzled image", FITS),
            Target::SortFolder => ("Choose a folder of frames to sort", FITS),
            Target::CalLights => ("Choose the folder of light frames", FITS),
            Target::CalDarks => ("Choose the folder of dark frames", FITS),
            Target::CalFlats => ("Choose the folder of flat frames", FITS),
            Target::CalDarkFlats => ("Choose the folder of dark flats", FITS),
            Target::CalBias => ("Choose the folder of bias frames", FITS),
            Target::CalOut => ("Choose the folder for the calibrated frames", FITS),
            Target::RegIn => ("Choose the folder of frames to register", FITS),
            Target::RegOut => ("Choose the folder for the registered frames", FITS),
            Target::StackIn => ("Choose the folder of frames to stack", FITS),
            Target::StackOut => ("Save the stack", FITS),
            Target::ExportOut => ("Export the picture", EXPORT),
            Target::ScienceIn => ("Choose the folder of frames to measure", FITS),
        };
        Self {
            title: title.to_owned(),
            save: target.saves(),
            folder: target.folder(),
            filter,
            dir,
            file_name: target
                .saves()
                .then(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
                .flatten(),
        }
    }
}

/// Something that shows a dialog and returns the path chosen, or `None` when it
/// was cancelled. It runs on a worker thread.
pub type Picker = Arc<dyn Fn(&Request) -> Option<PathBuf> + Send + Sync>;

/// The operating system's file dialog.
#[must_use]
pub fn native() -> Picker {
    Arc::new(|r: &Request| {
        let mut d = rfd::FileDialog::new().set_title(&r.title);
        if !r.folder {
            d = d.add_filter(r.filter.0, r.filter.1);
        }
        if let Some(dir) = &r.dir {
            d = d.set_directory(dir);
        }
        if r.folder {
            return d.pick_folder();
        }
        if r.save {
            if let Some(n) = &r.file_name {
                d = d.set_file_name(n);
            }
            d.save_file()
        } else {
            d.pick_file()
        }
    })
}

/// The dialog in progress, if any, and the program that shows it.
pub struct Chooser {
    /// Shows the dialog; tests replace it.
    pub picker: Picker,
    pending: Option<(Target, Receiver<Option<PathBuf>>)>,
}

impl Default for Chooser {
    fn default() -> Self {
        Self {
            picker: native(),
            pending: None,
        }
    }
}

impl Chooser {
    /// Whether a dialog is open.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Show the dialog for `target` on a worker thread, starting from `current`.
    /// Does nothing while another is open.
    pub fn start(&mut self, ctx: &Context, target: Target, current: &str) {
        if self.pending.is_some() {
            return;
        }
        let request = Request::for_target(target, current);
        let picker = Arc::clone(&self.picker);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let chosen =
                picker(&request).map(|p| with_extension(p, request.save, request.filter.1));
            let _ = tx.send(chosen);
            ctx.request_repaint();
        });
        self.pending = Some((target, rx));
    }

    /// The path chosen since the last call, with what it was chosen for.
    /// A cancelled dialog gives nothing.
    pub fn poll(&mut self) -> Option<(Target, PathBuf)> {
        let (target, rx) = self.pending.as_ref()?;
        match rx.try_recv() {
            Ok(picked) => {
                let target = *target;
                self.pending = None;
                picked.map(|p| (target, p))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                None
            }
        }
    }
}

/// A written file gets `.fits` when the name has no extension.
#[must_use]
pub fn with_extension(mut path: PathBuf, save: bool, filter: &[&str]) -> PathBuf {
    if save
        && path.extension().is_none()
        && let Some(e) = filter.first()
    {
        path.set_extension(e);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_starts_where_the_box_points_and_offers_its_file_name() {
        let dir = std::env::temp_dir();
        let text = dir
            .join("m31_stretched.fits")
            .to_string_lossy()
            .into_owned();
        let r = Request::for_target(Target::StretchOut, &text);
        assert!(r.save);
        assert_eq!(r.dir.as_deref(), Some(dir.as_path()));
        assert_eq!(r.file_name.as_deref(), Some("m31_stretched.fits"));
        assert_eq!(r.filter.1, FITS.1);
    }

    #[test]
    fn opening_offers_no_file_name_and_a_missing_folder_is_ignored() {
        let r = Request::for_target(Target::Image, "/no/such/folder/m31.fits");
        assert!(!r.save && r.file_name.is_none() && r.dir.is_none());
        let r = Request::for_target(Target::Image, "");
        assert!(r.dir.is_none());
    }

    #[test]
    fn a_folder_dialog_starts_inside_the_folder_and_offers_no_name() {
        let dir = std::env::temp_dir();
        let r = Request::for_target(Target::DrizzleFolder, &dir.to_string_lossy());
        assert!(r.folder && !r.save && r.file_name.is_none());
        assert_eq!(r.dir.as_deref(), Some(dir.as_path()));
        let r = Request::for_target(Target::DrizzleOut, "/no/such/out.fits");
        assert!(r.save && !r.folder);
    }

    #[test]
    fn the_catalogue_dialog_filters_catalogues() {
        let r = Request::for_target(Target::Catalogue, "");
        assert_eq!(r.filter.1, ["vcat"]);
        assert!(!r.save);
    }

    #[test]
    fn a_saved_name_without_an_extension_gets_one() {
        let p = with_extension(PathBuf::from("/x/out"), true, FITS.1);
        assert_eq!(p, PathBuf::from("/x/out.fits"));
        let p = with_extension(PathBuf::from("/x/out.fit"), true, FITS.1);
        assert_eq!(p, PathBuf::from("/x/out.fit"));
        let p = with_extension(PathBuf::from("/x/in"), false, FITS.1);
        assert_eq!(p, PathBuf::from("/x/in"));
    }
}
