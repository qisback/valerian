// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The first three steps of the workflow: Calibrate, Register and Stack.
//!
//! Each step works on folders of FITS frames, like the `valerian calibrate`,
//! `register` and `stack` commands it mirrors (the same library code, the CPU
//! engine), and each folder defaults to the previous step's output so the three
//! chain. A run goes in a background thread and reports through a [`Progress`]
//! the notice above the viewer reads; Stack finishes by opening its picture as
//! the working image for the Process step.
//!
//! A run can be cancelled: the [`Progress`] holds a [`CancelToken`] that the
//! core loops check, the steps check it between files, and what a step writes
//! goes through a [`Staging`] folder that is moved into place only when the whole
//! run succeeded, so a cancelled or failed run leaves the output folder as it was.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{RichText, Ui};
use valerian_core::CancelToken;
use valerian_core::calibrate::{Calibrator, FlatNormalisation, PixelMap, flag};
use valerian_core::combine::Rejection;
use valerian_core::compute::{Compute, Cpu};
use valerian_core::debayer::{Method, demosaic_image};
use valerian_core::fits::{self, Format, Header, Image, Value};
use valerian_core::preview::downsample;
use valerian_core::register::RegisterOptions;
use valerian_core::stack::{
    Normalisation, Reference, StackOptions, Status, align_with, stack_with,
};
use valerian_core::streaming::{
    master_bias_cancellable, master_dark_cancellable, master_flat_cancellable,
};

use crate::dialogs::Target;
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};
use crate::workspace::PREVIEW_SIDE;

/// Memory for combining calibration frames, in bytes (they are read in bands).
const BUDGET: usize = 1024 << 20;

/// How a master frame is combined from its files.
type Build<'a> = dyn Fn(&[PathBuf], Rejection) -> Result<Image, String> + 'a;

/// The error a cancelled run returns: the interface shows it as a notice, not as a failure.
pub const CANCELLED: &str = "Cancelled";

/// A core error as the text a run returns: [`CANCELLED`] for a cancel.
#[must_use]
pub fn failure(e: &valerian_core::Error) -> String {
    match e {
        valerian_core::Error::Cancelled => CANCELLED.to_owned(),
        other => other.to_string(),
    }
}

/// What a running step reports, read by the notice above the viewer. It also
/// carries the step's cancel token.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicUsize,
    total: AtomicUsize,
    line: Mutex<String>,
    cancel: CancelToken,
    /// Tests: cancel when this many frames are done.
    #[cfg(test)]
    stop_after: Mutex<Option<usize>>,
    /// Tests: cancel when a line starts with this.
    #[cfg(test)]
    stop_on: Mutex<Option<String>>,
}

impl Progress {
    /// A new, empty progress.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Frames of work in all, when known.
    pub fn set_total(&self, n: usize) {
        self.total.store(n, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    /// One more frame done.
    pub fn tick(&self) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        #[cfg(test)]
        if self
            .stop_after
            .lock()
            .is_ok_and(|n| n.is_some_and(|n| done >= n))
        {
            self.cancel();
        }
        #[cfg(not(test))]
        let _ = done;
    }

    /// What it is doing now.
    pub fn say(&self, text: &str) {
        if let Ok(mut l) = self.line.lock() {
            text.clone_into(&mut l);
        }
        #[cfg(test)]
        if self
            .stop_on
            .lock()
            .is_ok_and(|p| p.as_ref().is_some_and(|p| text.starts_with(p.as_str())))
        {
            self.cancel();
        }
    }

    /// Tests: cancel once `n` frames are done.
    #[cfg(test)]
    pub fn cancel_after(&self, n: usize) {
        *self.stop_after.lock().expect("lock") = Some(n);
    }

    /// Tests: cancel when a line starts with `prefix`.
    #[cfg(test)]
    pub fn cancel_on_line(&self, prefix: &str) {
        *self.stop_on.lock().expect("lock") = Some(prefix.to_owned());
    }

    /// Done and total, when a total is known.
    #[must_use]
    pub fn counts(&self) -> Option<(usize, usize)> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed).min(total), total))
    }

    /// The latest line.
    #[must_use]
    pub fn line(&self) -> String {
        self.line.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// The token the core loops of this run check.
    #[must_use]
    pub fn token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Ask the run to stop at the next frame, band or plane.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Whether a cancel was asked for.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// `Ok` while the run goes on, [`CANCELLED`] once it was cancelled.
    ///
    /// # Errors
    /// [`CANCELLED`] after [`cancel`](Self::cancel).
    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err(CANCELLED.to_owned())
        } else {
            Ok(())
        }
    }
}

/// A folder beside the outputs that the run writes into, moved into place only by
/// [`commit`](Self::commit). Dropping it without a commit deletes it, and the
/// folders it made, so a cancelled or failed run leaves nothing behind and
/// files that were already there are never touched half-way.
#[derive(Debug)]
pub struct Staging {
    root: PathBuf,
    dir: PathBuf,
    /// Folders this run made, deepest first.
    made: Vec<PathBuf>,
    files: Vec<PathBuf>,
    committed: bool,
}

impl Staging {
    /// A staging folder inside `root`, which is made if it is missing.
    ///
    /// # Errors
    /// The folder cannot be made.
    pub fn new(root: &Path) -> Result<Self, String> {
        let mut made = Vec::new();
        let mut at = Some(root);
        while let Some(p) = at {
            if p.as_os_str().is_empty() || p.exists() {
                break;
            }
            made.push(p.to_path_buf());
            at = p.parent();
        }
        let dir = root.join(format!(".valerian-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
        Ok(Self {
            root: root.to_path_buf(),
            dir,
            made,
            files: Vec::new(),
            committed: false,
        })
    }

    /// Write `img` as `rel` (which may include folders) in the staging folder.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn write(&mut self, rel: &Path, img: &Image) -> Result<(), String> {
        let path = self.dir.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        fits::write(&path, img, Format::Float32)
            .map_err(|e| format!("Cannot write {}: {e}", self.root.join(rel).display()))?;
        self.files.push(rel.to_path_buf());
        Ok(())
    }

    /// Move everything written into the output folder, replacing what is there.
    ///
    /// # Errors
    /// A file cannot be moved.
    pub fn commit(mut self) -> Result<(), String> {
        for rel in &self.files {
            let to = self.root.join(rel);
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
            }
            std::fs::rename(self.dir.join(rel), &to)
                .map_err(|e| format!("Cannot write {}: {e}", to.display()))?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        if !self.committed {
            for d in &self.made {
                let _ = std::fs::remove_dir(d);
            }
        }
    }
}

/// What the user asked for in a step's panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Run the step.
    Run,
    /// Open the dialog for a box.
    Browse(Target),
}

/// The FITS files `path` names: the file itself, or those in the folder, sorted.
///
/// # Errors
/// The path is missing or empty, or a folder holds no FITS files.
pub fn frames_in(path: &str) -> Result<Vec<PathBuf>, String> {
    let p = Path::new(path.trim());
    if p.is_file() {
        return Ok(vec![p.to_path_buf()]);
    }
    if path.trim().is_empty() {
        return Err("Name a folder of FITS frames first.".into());
    }
    if !p.is_dir() {
        return Err(format!("{} does not exist.", p.display()));
    }
    crate::drizzle_tool::frame_files(p)
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map_or_else(|| "frame".into(), |s| s.to_string_lossy().into_owned())
}

pub(crate) fn read_all(files: &[PathBuf], progress: &Progress) -> Result<Vec<Image>, String> {
    progress.say(&format!("Reading {} frames...", files.len()));
    files
        .iter()
        .map(|p| {
            progress.check()?;
            fits::read(p).map_err(|e| format!("Cannot read {}: {e}", p.display()))
        })
        .collect()
}

/// Raw colour frames are demosaiced so they register and stack as colour.
pub(crate) fn demosaic_all(frames: Vec<Image>, progress: &Progress) -> Result<Vec<Image>, String> {
    frames
        .into_iter()
        .map(|img| {
            progress.check()?;
            if img.planes.len() == 1 && img.plane().bayer.is_some() {
                demosaic_image(&img, Method::Mhc).map_err(|e| failure(&e))
            } else {
                Ok(img)
            }
        })
        .collect()
}

// ---------------------------------------------------------------- Calibrate

/// The Calibrate step: lights, calibration frames and where the result goes.
#[derive(Clone, Debug, Default)]
pub struct CalibrateTool {
    /// Folder (or file) of light frames.
    pub lights: String,
    /// Dark frames, or a master dark.
    pub darks: String,
    /// Flat frames, or a master flat.
    pub flats: String,
    /// Dark flats, which match the flats' exposure.
    pub dark_flats: String,
    /// Bias frames, or a master bias.
    pub bias: String,
    /// Where the calibrated lights go.
    pub output: String,
    /// The last run.
    pub result: Option<CalibrateRun>,
}

/// What a Calibrate run did.
#[derive(Clone, Debug)]
pub struct CalibrateRun {
    /// Lights written.
    pub lights: usize,
    /// The master frames made, by name.
    pub masters: Vec<String>,
    /// Hot, cold and dead pixels in the pixel map.
    pub defects: Option<(usize, usize, usize)>,
    /// The folder written.
    pub folder: String,
    /// The first calibrated light, reduced, for the viewer.
    pub preview: Option<Arc<Image>>,
}

impl CalibrateTool {
    /// Run the step. Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read or written, or nothing is given to
    /// calibrate with.
    pub fn run(&self, progress: &Progress) -> Result<CalibrateRun, String> {
        let lights = frames_in(&self.lights)?;
        let out = PathBuf::from(self.output.trim());
        if out.as_os_str().is_empty() {
            return Err("Name the folder for the calibrated frames.".into());
        }
        let optional = |s: &str| -> Result<Vec<PathBuf>, String> {
            if s.trim().is_empty() {
                Ok(Vec::new())
            } else {
                frames_in(s)
            }
        };
        let (bias, dark, flat, dark_flat) = (
            optional(&self.bias)?,
            optional(&self.darks)?,
            optional(&self.flats)?,
            optional(&self.dark_flats)?,
        );
        if bias.is_empty() && dark.is_empty() && flat.is_empty() {
            return Err("Nothing to calibrate with: name darks, flats or bias frames.".into());
        }
        progress.set_total(lights.len());
        let compute: &dyn Compute = &Cpu;
        let cancel = progress.token();
        let mut staging = Staging::new(&out)?;
        let group =
            |label: &str, files: &[PathBuf], build: &Build| -> Result<Option<Image>, String> {
                progress.check()?;
                if files.is_empty() {
                    return Ok(None);
                }
                progress.say(&format!("Combining {} {label} frames...", files.len()));
                if files.len() == 1 {
                    let img = fits::read(&files[0])
                        .map_err(|e| format!("Cannot read {}: {e}", files[0].display()))?;
                    if img
                        .header
                        .get_str("IMAGETYP")
                        .is_some_and(|t| t.to_ascii_lowercase().contains("master"))
                    {
                        return Ok(Some(img));
                    }
                }
                build(files, Rejection::auto(files.len())).map(Some)
            };
        let bias_m = group("bias", &bias, &|f, r| {
            master_bias_cancellable(f, r, BUDGET, compute, &cancel).map_err(|e| failure(&e))
        })?;
        let dark_m = group("dark", &dark, &|f, r| {
            master_dark_cancellable(f, bias_m.as_ref(), r, BUDGET, compute, &cancel)
                .map_err(|e| failure(&e))
        })?;
        let dark_flat_m = group("dark flat", &dark_flat, &|f, r| {
            master_dark_cancellable(f, None, r, BUDGET, compute, &cancel).map_err(|e| failure(&e))
        })?;
        let flat_m = group("flat", &flat, &|f, r| {
            let sub = dark_flat_m.as_ref().or(bias_m.as_ref());
            master_flat_cancellable(
                f,
                sub,
                r,
                FlatNormalisation::Global,
                BUDGET,
                compute,
                &cancel,
            )
            .map_err(|e| failure(&e))
        })?;
        let mut calibrator = Calibrator::new(bias_m.clone(), dark_m.clone(), flat_m.clone());
        let mut map = dark_m
            .as_ref()
            .map(|d| PixelMap::from_dark(d.plane(), 40.0, 12.0));
        if let (Some(m), Some(f)) = (map.as_mut(), flat_m.as_ref()) {
            m.add_dead_from_flat(f.plane(), 0.2);
        } else if let (None, Some(f)) = (map.as_ref(), flat_m.as_ref()) {
            let mut m = PixelMap::empty(f.width(), f.height(), f.plane().bayer.is_some());
            m.add_dead_from_flat(f.plane(), 0.2);
            map = Some(m);
        }
        let defects = map.as_ref().map(|m| {
            (
                m.count_flag(flag::HOT),
                m.count_flag(flag::COLD),
                m.count_flag(flag::DEAD),
            )
        });
        calibrator.pixel_map = map;
        let mut masters = Vec::new();
        for (name, m) in [
            ("master_bias.fits", &bias_m),
            ("master_dark.fits", &dark_m),
            ("master_dark_flat.fits", &dark_flat_m),
            ("master_flat.fits", &flat_m),
        ] {
            if let Some(img) = m {
                progress.check()?;
                staging.write(&Path::new("masters").join(name), img)?;
                masters.push(name.to_owned());
            }
        }
        let mut preview = None;
        for (i, path) in lights.iter().enumerate() {
            progress.check()?;
            progress.say(&format!(
                "Calibrating {} ({} of {})",
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                i + 1,
                lights.len()
            ));
            let light =
                fits::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            let (cal, _) = compute
                .calibrate(&calibrator, &light)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            staging.write(Path::new(&format!("{}_cal.fits", stem(path))), &cal)?;
            if i == 0 {
                preview = Some(Arc::new(downsample(&cal, PREVIEW_SIDE)));
            }
            progress.tick();
        }
        progress.check()?;
        progress.say("Moving the calibrated frames into place...");
        staging.commit()?;
        Ok(CalibrateRun {
            lights: lights.len(),
            masters,
            defects,
            folder: out.to_string_lossy().into_owned(),
            preview,
        })
    }

    /// The panel. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let mut ask = Ask::Nothing;
        hint(
            ui,
            "Frames the camera took with the lens capped (darks, bias) or the sensor evenly lit \
             (flats) are combined into masters and taken out of every light. Leave a box empty \
             to skip that kind of frame; give one master file instead of a folder to reuse it.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            for (label, text, hint, target) in [
                (
                    "Lights",
                    &mut self.lights,
                    "Folder of light frames",
                    Target::CalLights,
                ),
                (
                    "Darks",
                    &mut self.darks,
                    "Optional: folder of darks, or a master dark",
                    Target::CalDarks,
                ),
                (
                    "Flats",
                    &mut self.flats,
                    "Optional: folder of flats, or a master flat",
                    Target::CalFlats,
                ),
                (
                    "Dark flats",
                    &mut self.dark_flats,
                    "Optional: darks that match the flats",
                    Target::CalDarkFlats,
                ),
                (
                    "Bias",
                    &mut self.bias,
                    "Optional: folder of bias frames",
                    Target::CalBias,
                ),
                (
                    "Calibrated frames go to",
                    &mut self.output,
                    "Folder to write",
                    Target::CalOut,
                ),
            ] {
                if let Some(t) = widgets::folder_row(ui, label, text, hint, target) {
                    ask = Ask::Browse(t);
                }
            }
            ui.add_space(4.0);
            if widgets::button(
                ui,
                if busy {
                    "Calibrating..."
                } else {
                    "Calibrate lights"
                },
                Kind::Primary,
                0.0,
            )
            .on_hover_text("Combine the calibration frames and calibrate every light")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            widgets::overline(ui, "Result");
            let p = palette();
            for l in r.lines() {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
        }
        ask
    }
}

impl CalibrateRun {
    /// The result, as lines for the panel.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!("{} lights calibrated", self.lights)];
        if !self.masters.is_empty() {
            v.push(format!("masters: {}", self.masters.join(", ")));
        }
        if let Some((h, c, d)) = self.defects {
            v.push(format!("pixel map: {h} hot, {c} cold, {d} dead"));
        }
        v.push(format!("written to {}", self.folder));
        v
    }
}

// ----------------------------------------------------------------- Register

/// The Register step: align every frame to a reference and write them.
#[derive(Clone, Debug, Default)]
pub struct RegisterTool {
    /// Folder of frames to align.
    pub input: String,
    /// Where the aligned frames go.
    pub output: String,
    /// The last run.
    pub result: Option<RegisterRun>,
}

/// What a Register run did.
#[derive(Clone, Debug)]
pub struct RegisterRun {
    /// Frames in.
    pub total: usize,
    /// Frames aligned and written.
    pub written: usize,
    /// Index of the reference frame.
    pub reference: usize,
    /// Median registration residual of the aligned frames, pixels.
    pub median_rms: f64,
    /// The folder written.
    pub folder: String,
    /// The reference frame, reduced, for the viewer.
    pub preview: Option<Arc<Image>>,
}

impl RegisterTool {
    /// Run the step. Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read or written, or too few frames register.
    pub fn run(&self, progress: &Progress) -> Result<RegisterRun, String> {
        let files = frames_in(&self.input)?;
        let out = PathBuf::from(self.output.trim());
        if out.as_os_str().is_empty() {
            return Err("Name the folder for the aligned frames.".into());
        }
        progress.set_total(files.len());
        let frames = demosaic_all(read_all(&files, progress)?, progress)?;
        let opts = StackOptions {
            cancel: progress.token(),
            register: RegisterOptions::default(),
            // Registered frames are saved as they are: no level matching.
            normalisation: Normalisation::None,
            reference: Reference::Best,
            ..StackOptions::default()
        };
        let aligned = align_with(frames, &opts, &Cpu, &mut |line| progress.say(line)).map_err(
            |e| match e {
                valerian_core::Error::Cancelled => CANCELLED.to_owned(),
                e => format!("Registration failed: {e}"),
            },
        )?;
        let mut staging = Staging::new(&out)?;
        let mut rms = Vec::new();
        for &i in &aligned.keep {
            progress.check()?;
            let mut img = aligned.frames[i].clone();
            img.header.set(
                "REGREF",
                Value::Int(i64::try_from(aligned.reference).unwrap_or(i64::MAX)),
                "index of the registration reference",
            );
            if let Status::Stacked = aligned.reports[i].status {
                rms.push(aligned.reports[i].registration_rms);
            }
            let mut h: Header = img.header;
            h.add_history("registered by Valerian");
            img.header = h;
            progress.say(&format!(
                "Writing {} ({} of {})",
                files[i]
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                progress.counts().map_or(0, |c| c.0) + 1,
                aligned.keep.len()
            ));
            staging.write(Path::new(&format!("{}_reg.fits", stem(&files[i]))), &img)?;
            progress.tick();
        }
        progress.check()?;
        progress.say("Moving the aligned frames into place...");
        staging.commit()?;
        rms.sort_by(f64::total_cmp);
        let preview = aligned
            .frames
            .get(aligned.reference)
            .map(|f| Arc::new(downsample(f, PREVIEW_SIDE)));
        Ok(RegisterRun {
            total: files.len(),
            written: aligned.keep.len(),
            reference: aligned.reference,
            median_rms: rms.get(rms.len() / 2).copied().unwrap_or(0.0),
            folder: out.to_string_lossy().into_owned(),
            preview,
        })
    }

    /// The panel. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let mut ask = Ask::Nothing;
        hint(
            ui,
            "Finds the stars in every frame, picks the sharpest as the reference and shifts and \
             turns the others onto it. Stack does this itself, so this step is for when you want \
             the aligned frames as files, to check them or to use another stacker. Raw colour \
             frames are demosaiced first.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            for (label, text, hint, target) in [
                (
                    "Frames",
                    &mut self.input,
                    "Folder of calibrated frames",
                    Target::RegIn,
                ),
                (
                    "Aligned frames go to",
                    &mut self.output,
                    "Folder to write",
                    Target::RegOut,
                ),
            ] {
                if let Some(t) = widgets::folder_row(ui, label, text, hint, target) {
                    ask = Ask::Browse(t);
                }
            }
            ui.add_space(4.0);
            if widgets::button(
                ui,
                if busy {
                    "Registering..."
                } else {
                    "Register frames"
                },
                Kind::Primary,
                0.0,
            )
            .on_hover_text("Align every frame to the best one and write them")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            widgets::overline(ui, "Result");
            let p = palette();
            for l in [
                format!("{} of {} frames aligned", r.written, r.total),
                format!(
                    "reference frame {}, median residual {:.3} px",
                    r.reference, r.median_rms
                ),
                format!("written to {}", r.folder),
            ] {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
        }
        ask
    }
}

// -------------------------------------------------------------------- Stack

/// How the stack rejects outliers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RejectionChoice {
    /// Chosen from the number of frames.
    #[default]
    Auto,
    /// Sigma clipping, 3 sigma.
    Sigma,
    /// Winsorised sigma clipping, 3 sigma.
    Winsorised,
    /// No rejection: a plain mean.
    None,
}

impl RejectionChoice {
    /// The rejection to use for `n` frames, `None` to let the stacker choose.
    #[must_use]
    pub fn resolve(self) -> Option<Rejection> {
        match self {
            Self::Auto => None,
            Self::Sigma => Some(Rejection::Sigma {
                low: 3.0,
                high: 3.0,
            }),
            Self::Winsorised => Some(Rejection::Winsorised {
                low: 3.0,
                high: 3.0,
            }),
            Self::None => Some(Rejection::None),
        }
    }
}

/// The Stack step: register and combine a folder of frames into one picture.
#[derive(Clone, Debug, Default)]
pub struct StackTool {
    /// Folder of frames.
    pub input: String,
    /// Where the stack is written.
    pub output: String,
    /// Outlier rejection.
    pub rejection: RejectionChoice,
    /// File names the Science panel rejected; the stack leaves them out.
    pub exclude: Vec<String>,
    /// The last run.
    pub result: Option<StackRun>,
}

/// What a Stack run did.
#[derive(Clone, Debug)]
pub struct StackRun {
    /// Frames in.
    pub total: usize,
    /// Frames in the stack.
    pub used: usize,
    /// Index of the reference frame.
    pub reference: usize,
    /// The rejection that was used.
    pub rejection: String,
    /// The stack.
    pub image: Arc<Image>,
    /// Where it was written.
    pub path: PathBuf,
}

impl StackTool {
    /// Run the step. Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read, too few frames register, or the stack
    /// cannot be written.
    pub fn run(&self, progress: &Progress) -> Result<StackRun, String> {
        let mut files = frames_in(&self.input)?;
        let found = files.len();
        files.retain(|f| {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned());
            !name.is_some_and(|n| self.exclude.contains(&n))
        });
        if files.len() < 2 && !self.exclude.is_empty() {
            return Err("The Science panel left fewer than two frames to stack.".into());
        }
        let out = PathBuf::from(self.output.trim());
        if out.as_os_str().is_empty() {
            return Err("Name the file for the stack.".into());
        }
        let frames = demosaic_all(read_all(&files, progress)?, progress)?;
        let opts = StackOptions {
            cancel: progress.token(),
            rejection: self.rejection.resolve(),
            ..StackOptions::default()
        };
        let stacked = stack_with(&frames, &opts, &Cpu, &mut |line| progress.say(line)).map_err(
            |e| match e {
                valerian_core::Error::Cancelled => CANCELLED.to_owned(),
                e => format!("Stacking failed: {e}"),
            },
        )?;
        progress.check()?;
        progress.say("Writing the stack...");
        let root = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let name = out
            .file_name()
            .ok_or_else(|| "Name the file for the stack.".to_owned())?;
        let mut staging = Staging::new(&root)?;
        staging.write(Path::new(name), &stacked.image)?;
        staging.commit()?;
        Ok(StackRun {
            total: found,
            used: stacked.used(),
            reference: stacked.reference,
            rejection: format!("{:?}", stacked.rejection),
            image: Arc::new(stacked.image),
            path: out,
        })
    }

    /// The panel. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let mut ask = Ask::Nothing;
        hint(
            ui,
            "Registers the frames and averages them, throwing out what is not in every frame \
             (satellite trails, hot pixels, cosmic rays). The stack is written as one FITS file \
             and opened for the Process step.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            for (label, text, hint, target) in [
                (
                    "Frames",
                    &mut self.input,
                    "Folder of calibrated frames",
                    Target::StackIn,
                ),
                (
                    "Stack goes to",
                    &mut self.output,
                    "File to write",
                    Target::StackOut,
                ),
            ] {
                if let Some(t) = widgets::folder_row(ui, label, text, hint, target) {
                    ask = Ask::Browse(t);
                }
            }
            if !self.exclude.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    hint(
                        ui,
                        &format!(
                            "{} frames left out by the Science panel.",
                            self.exclude.len()
                        ),
                    );
                    if ui.small_button("Use all").clicked() {
                        self.exclude.clear();
                    }
                });
            }
            widgets::overline(ui, "Rejection");
            ui.horizontal_wrapped(|ui| {
                for (c, label) in [
                    (RejectionChoice::Auto, "Automatic"),
                    (RejectionChoice::Winsorised, "Winsorised"),
                    (RejectionChoice::Sigma, "Sigma"),
                    (RejectionChoice::None, "None"),
                ] {
                    ui.selectable_value(&mut self.rejection, c, label);
                }
            });
            ui.add_space(4.0);
            if widgets::button(
                ui,
                if busy { "Stacking..." } else { "Stack frames" },
                Kind::Primary,
                0.0,
            )
            .on_hover_text("Register and combine the frames, then open the stack")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            widgets::overline(ui, "Result");
            let p = palette();
            for l in [
                format!("{} of {} frames stacked", r.used, r.total),
                format!("reference frame {}, {}", r.reference, r.rejection),
                format!("{} x {} px", r.image.width(), r.image.height()),
            ] {
                ui.label(RichText::new(l).font(mono(12.5)).color(p.text));
            }
        }
        ask
    }
}

/// A small muted paragraph at the top of a panel.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(palette().muted));
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::Header;

    #[test]
    fn progress_counts_and_says() {
        let p = Progress::new();
        assert_eq!(p.counts(), None);
        p.set_total(4);
        p.tick();
        p.tick();
        p.say("Registering frame 2");
        assert_eq!(p.counts(), Some((2, 4)));
        assert_eq!(p.line(), "Registering frame 2");
        for _ in 0..9 {
            p.tick();
        }
        assert_eq!(p.counts(), Some((4, 4)));
    }

    fn tiny() -> Image {
        Image::mono(
            valerian_core::Frame::new(4, 3, vec![1.0; 12]).expect("frame"),
            Header::new(),
        )
    }

    #[test]
    fn progress_cancel_is_seen_by_its_token_and_check() {
        let p = Progress::new();
        let t = p.token();
        assert!(p.check().is_ok() && !t.is_cancelled());
        p.cancel();
        assert!(t.is_cancelled() && p.is_cancelled());
        assert_eq!(p.check(), Err(CANCELLED.to_owned()));
        assert_eq!(
            failure(&valerian_core::Error::Cancelled),
            CANCELLED.to_owned()
        );
        assert_eq!(
            failure(&valerian_core::Error::invalid("bad")),
            "bad".to_owned()
        );
    }

    #[test]
    fn staged_files_appear_only_on_commit_and_replace_what_was_there() {
        let dir = tempfile::tempdir().expect("tmp");
        let out = dir.path().join("deep").join("out");
        let mut s = Staging::new(&out).expect("staging");
        s.write(Path::new("masters/a.fits"), &tiny())
            .expect("write");
        s.write(Path::new("b_cal.fits"), &tiny()).expect("write");
        assert!(!out.join("b_cal.fits").exists(), "nothing in place yet");
        s.commit().expect("commit");
        assert!(out.join("masters/a.fits").is_file() && out.join("b_cal.fits").is_file());
        let left: Vec<_> = std::fs::read_dir(&out)
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            left.iter().all(|n| !n.starts_with(".valerian-partial")),
            "{left:?}"
        );
    }

    #[test]
    fn a_dropped_staging_leaves_the_folder_as_it_was() {
        let dir = tempfile::tempdir().expect("tmp");
        // A folder that did not exist is not left behind, empty or otherwise.
        let fresh = dir.path().join("a").join("b");
        {
            let mut s = Staging::new(&fresh).expect("staging");
            s.write(Path::new("x.fits"), &tiny()).expect("write");
        }
        assert!(!dir.path().join("a").exists());
        // A folder that did exist keeps its files, byte for byte.
        let old = dir.path().join("old");
        std::fs::create_dir_all(&old).expect("dir");
        std::fs::write(old.join("x_cal.fits"), b"previous").expect("write");
        {
            let mut s = Staging::new(&old).expect("staging");
            s.write(Path::new("x_cal.fits"), &tiny()).expect("write");
        }
        assert_eq!(
            std::fs::read(old.join("x_cal.fits")).expect("read"),
            b"previous"
        );
        assert_eq!(std::fs::read_dir(&old).expect("dir").count(), 1);
    }

    #[test]
    fn frames_are_found_in_a_folder_or_named_directly() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(frames_in("").is_err());
        assert!(frames_in(&dir.path().to_string_lossy()).is_err());
        let a = dir.path().join("b.fits");
        let b = dir.path().join("a.fit");
        std::fs::write(&a, b"x").expect("write");
        std::fs::write(&b, b"x").expect("write");
        std::fs::write(dir.path().join("notes.txt"), b"x").expect("write");
        let found = frames_in(&dir.path().to_string_lossy()).expect("frames");
        assert_eq!(found, vec![b, a.clone()]);
        assert_eq!(frames_in(&a.to_string_lossy()).expect("file"), vec![a]);
    }

    #[test]
    fn calibrating_with_nothing_to_calibrate_with_says_so() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("l.fits"), b"x").expect("write");
        let t = CalibrateTool {
            lights: dir.path().to_string_lossy().into_owned(),
            output: dir.path().join("out").to_string_lossy().into_owned(),
            ..CalibrateTool::default()
        };
        let e = t.run(&Progress::default()).expect_err("no masters");
        assert!(e.contains("Nothing to calibrate with"), "{e}");
    }

    #[test]
    fn the_rejection_choices_resolve() {
        assert!(RejectionChoice::Auto.resolve().is_none());
        assert_eq!(RejectionChoice::None.resolve(), Some(Rejection::None));
        assert!(matches!(
            RejectionChoice::Sigma.resolve(),
            Some(Rejection::Sigma { .. })
        ));
    }
}
