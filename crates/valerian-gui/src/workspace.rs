// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The workspace: the steps of the workflow, their panels and the viewer.
//!
//! The workflow has five steps (see `rail.rs`). Calibrate, Register and Stack
//! work on folders of frames (`pipeline.rs`); Stack opens its picture, and the
//! Process step works on the open image: a tool edits its settings in the
//! panel, the viewer shows the tool's preview of a reduced copy, and Apply runs
//! the real operation on the full frame in a background thread and writes a new
//! FITS file next to the original, which is never changed. Process holds
//! Stretch, Star removal, Plate solve and Colour calibration; Drizzle is an
//! alternative way to do Stack; Export writes the picture out. The GPU tool is
//! a benchmark reached from the top bar.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Instant;

use eframe::egui::{
    self, Align, ColorImage, Context, Frame, Layout, Margin, RichText, ScrollArea, Stroke,
    TextEdit, TextureHandle, TextureOptions, Ui, vec2,
};
use valerian_core::fits::{self, Format, Image};
use valerian_core::photocal::{self, Calibration};
use valerian_core::preview::{downsample, rgba8};
use valerian_core::solve::{self, Solution};
use valerian_core::starless::remove_stars;
use valerian_core::wcs::Wcs;
use valerian_science::subframe::Analysis;

use crate::astro_tool::{Ask as AstroAsk, AstroRun, AstroTool};
use crate::colour_tool::{Ask as ColourAsk, ColourTool, View as ColourView};
use crate::dialogs::{Chooser, Target};
use crate::drizzle_tool::{Ask as DrizzleAsk, DrizzleRun, DrizzleTool};
use crate::gpu_tool::{self, Ask as GpuAsk, GpuRun, GpuTool};
use crate::optics_tool::{Ask as OpticsAsk, OpticsRun, OpticsTool};
use crate::pipeline::{
    Ask as PipeAsk, CANCELLED, CalibrateRun, CalibrateTool, Progress, RegisterRun, RegisterTool,
    StackRun, StackTool, failure,
};
use crate::prefs::{FolderSet, Prefs};
use crate::rail::{Row, State, Step};
use crate::readout::{self, Reading};
use crate::review::{self, Act};
use crate::science_tool::{Ask as ScienceAsk, IntegrationRun, LightRun, ScienceRun, ScienceTool};
use crate::solve_tool::{Ask as SolveAsk, SolveTool};
use crate::sorter::{self, Folder, Kind as FrameKind, Review, Scan};
use crate::star_tool::{Ask, StarResult, StarTool};
use crate::start::{self, Pick};
use crate::stretch_tool::{Measured, StretchTool};
use crate::theme::{RADIUS_LARGE, mono, ok_colour, palette, semibold};
use crate::tiles::{self, Scene, Tiles};
use crate::view::{self, Histogram, View};
use crate::widgets::{self, Kind};

/// Longest side of the reduced copy the previews are made from.
pub const PREVIEW_SIDE: usize = 1600;

/// The open image.
pub struct Doc {
    /// Where it was read from.
    pub path: PathBuf,
    /// The full frame, shared with the apply thread.
    pub full: Arc<Image>,
    /// The reduced copy previews are made from.
    pub small: Image,
    /// The sky system of the file, when it is plate solved.
    pub wcs: Option<Wcs>,
}

/// The tool whose panel is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Calibrate: darks, flats and bias out of the lights.
    #[default]
    Calibrate,
    /// Register: align the frames.
    Register,
    /// Stack: combine the frames.
    Stack,
    /// Export: write the picture out.
    Export,
    /// Stretch.
    Stretch,
    /// Star removal.
    Stars,
    /// Plate solving.
    Solve,
    /// Colour calibration against Gaia.
    Colour,
    /// Drizzle of a folder of frames.
    Drizzle,
    /// The GPU kernels: adapter and benchmark.
    Gpu,
    /// Science: measure and plot a folder of frames.
    Science,
    /// Optics: what the stars of the open image say about the telescope.
    Optics,
    /// Astrometry: the plate solution drawn on the open image.
    Astrometry,
}

impl Tool {
    /// The workflow step the tool belongs to; the GPU benchmark belongs to none.
    #[must_use]
    pub fn step(self) -> Option<Step> {
        match self {
            Self::Calibrate => Some(Step::Calibrate),
            Self::Register => Some(Step::Register),
            Self::Stack | Self::Drizzle | Self::Science => Some(Step::Stack),
            Self::Stretch
            | Self::Stars
            | Self::Solve
            | Self::Colour
            | Self::Optics
            | Self::Astrometry => Some(Step::Process),
            Self::Export => Some(Step::Export),
            Self::Gpu => None,
        }
    }
}

/// What Export writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportFormat {
    /// 32-bit float FITS of the open image as it is.
    #[default]
    Fits,
    /// 8-bit PNG of the open image with the current stretch.
    Png,
}

/// Workspace state.
#[derive(Default)]
pub struct Workspace {
    /// The open image.
    pub doc: Option<Doc>,
    /// The path box.
    pub path_text: String,
    /// The native file dialogs.
    pub chooser: Chooser,
    /// The open tool.
    pub tool: Tool,
    /// The Stretch tool.
    pub stretch: StretchTool,
    /// The Star removal tool.
    pub stars: StarTool,
    /// Where the starless image is written (the star layer goes beside it).
    pub star_output_text: String,
    /// The Plate solve tool.
    pub solve: SolveTool,
    /// Where the solved copy is written.
    pub solve_output_text: String,
    /// The Colour calibration tool.
    pub colour: ColourTool,
    /// Where the calibrated image is written.
    pub colour_output_text: String,
    /// The Drizzle tool.
    pub drizzle: DrizzleTool,
    /// Where the drizzled image is written.
    pub drizzle_output_text: String,
    /// The GPU tool.
    pub gpu: GpuTool,
    /// The Science panel.
    pub science: ScienceTool,
    /// The Optics tool.
    pub optics: OpticsTool,
    /// The Astrometry tool.
    pub astro: AstroTool,
    /// The Calibrate step.
    pub cal: CalibrateTool,
    /// The Register step.
    pub reg: RegisterTool,
    /// The Stack step.
    pub stack: StackTool,
    /// The Process tool last used, which the Process step returns to.
    pub process_tool: Option<Tool>,
    /// The tool the GPU benchmark replaced.
    before_gpu: Option<Tool>,
    /// What Export writes.
    pub export_format: ExportFormat,
    /// Where Export writes.
    pub export_text: String,
    /// What the last export wrote.
    pub exported: Option<String>,
    /// Zoom and pan of the viewer.
    pub view: View,
    /// Histogram of what the viewer shows.
    pub hist: Option<Histogram>,
    /// The pyramid of the open image and the full-resolution tiles drawn from it.
    pub tiles: Tiles,
    /// The pyramid of a newly opened image is yet to be started.
    tiles_pending: bool,
    /// What the pointer is over, when it is over the picture.
    pub reading: Option<Reading>,
    /// Size of the picture area on the last frame, for the zoom readout.
    pub view_avail: egui::Vec2,
    /// The histogram strip is hidden.
    pub hide_hist: bool,
    /// Levels measured on the full frame.
    pub measured: Option<Measured>,
    /// The output path box.
    pub output_text: String,
    /// The last message, and whether it is an error.
    pub status: Option<(String, bool)>,
    texture: Option<TextureHandle>,
    dirty: bool,
    job: Option<Receiver<Result<String, String>>>,
    star_run: Option<Receiver<Result<StarResult, String>>>,
    solve_run: Option<Receiver<Result<Solution, String>>>,
    colour_run: Option<Receiver<Result<Calibration, String>>>,
    drizzle_run: Option<Receiver<Result<DrizzleRun, String>>>,
    gpu_run: Option<Receiver<Result<GpuRun, String>>>,
    science_run: Option<Receiver<Result<ScienceRun, String>>>,
    science_int_run: Option<Receiver<Result<IntegrationRun, String>>>,
    optics_run: Option<Receiver<Result<OpticsRun, String>>>,
    astro_run: Option<Receiver<Result<AstroRun, String>>>,
    light_run: Option<Receiver<Result<LightRun, String>>>,
    cal_run: Option<Receiver<Result<CalibrateRun, String>>>,
    reg_run: Option<Receiver<Result<RegisterRun, String>>>,
    stack_run: Option<Receiver<Result<StackRun, String>>>,
    /// The folder last chosen to sort.
    pub sort_text: String,
    /// A folder chosen with the dialog, to be sorted once a frame has a context to run in.
    pending_sort: Option<PathBuf>,
    /// Dropped frames, sorted, waiting for the user to accept.
    pub review: Option<Review>,
    sort_run: Option<Receiver<Result<Sorted, String>>>,
    stage_run: Option<Receiver<Result<Gathered, String>>>,
    /// A short note on an action that does not start work (a shortcut), and when it was made.
    pub(crate) flash: Option<(String, Instant)>,
    /// The folders of the step just started, for the app to put in the session list.
    pub(crate) pending_session: Option<FolderSet>,
    /// What the running step reports, if it does.
    pub(crate) progress: Option<Arc<Progress>>,
    /// When the work now running started.
    pub(crate) busy_since: Option<Instant>,
}

/// What the notice adds when a run was cancelled.
const KEPT: &str = "Nothing from the run was kept.";

/// A finished scan: what was dropped, and what was found.
type Sorted = (Vec<PathBuf>, Scan);
/// The folders after gathering, and whether any frames were gathered as links.
type Gathered = (FolderSet, bool);

/// What the strip above the viewer says.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// The headline.
    pub text: String,
    /// What is going on in detail: the latest line, the count, the time taken.
    pub detail: String,
    /// How far along, when that is known.
    pub fraction: Option<f32>,
    /// Work is running (a spinner, no dismiss button).
    pub running: bool,
    /// The running work can be cancelled (a Cancel button).
    pub cancellable: bool,
    /// Something went wrong.
    pub error: bool,
}

/// The hint drawn over the viewer while files are dragged above the window.
fn drop_hint(ui: &mut Ui) {
    let p = palette();
    let rect = ui.max_rect().shrink(10.0);
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, p.ground.gamma_multiply(0.85));
    painter.rect_stroke(
        rect,
        12.0,
        Stroke::new(2.0, p.accent_hover),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop a folder of frames to sort it, or a FITS file to open it",
        semibold(18.0),
        p.text,
    );
}

/// `name` beside the folder `path` names (or beside the file it names).
fn beside(path: &str, name: &str) -> String {
    let p = Path::new(path.trim());
    let owned: String = if p.is_file() {
        p.parent()
            .map_or_else(String::new, |d| d.to_string_lossy().into_owned())
    } else {
        path.trim().to_owned()
    };
    let trimmed = owned.trim_end_matches(std::path::is_separator);
    match trimmed.rfind(std::path::is_separator) {
        Some(i) => format!("{}{name}", &trimmed[..=i]),
        None => name.to_owned(),
    }
}

/// Take a finished result off `rx`, or note that the worker died.
fn take<T>(
    rx: &mut Option<Receiver<Result<T, String>>>,
    stopped: &str,
) -> Option<Result<T, String>> {
    let r = rx.as_ref()?;
    match r.try_recv() {
        Ok(v) => {
            *rx = None;
            Some(v)
        }
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            *rx = None;
            Some(Err(stopped.to_owned()))
        }
    }
}

/// The default output name beside `input`: `<stem>_<suffix>.fits`. The folder part is kept as it was
/// written, separators included: `Path::with_file_name` would join a `/data/x.fits` given on Windows
/// with a backslash.
fn default_output(input: &Path, suffix: &str) -> String {
    let stem = input
        .file_stem()
        .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
    let name = format!("{stem}_{suffix}.fits");
    let full = input.to_string_lossy();
    match full.rfind(std::path::is_separator) {
        Some(i) => format!("{}{name}", &full[..=i]),
        None => name,
    }
}

impl Workspace {
    /// Read `path` and make it the open image.
    pub fn open(&mut self, path: &Path) {
        match fits::read(path) {
            Ok(img) => self.load(path, img),
            Err(e) => self.status = Some((format!("Cannot open {}: {e}", path.display()), true)),
        }
    }

    /// Make `image` (read from `path`) the open image.
    pub fn load(&mut self, path: &Path, image: Image) {
        let small = downsample(&image, PREVIEW_SIDE);
        self.solve
            .prefill(&image.header, (image.width(), image.height()));
        self.solve_output_text = default_output(path, "solved");
        self.solve_run = None;
        self.colour.result = None;
        self.colour_run = None;
        self.colour_output_text = default_output(path, "calibrated");
        self.path_text = path.to_string_lossy().into_owned();
        self.output_text = default_output(path, "stretched");
        self.star_output_text = default_output(path, "starless");
        self.stars.result = None;
        self.star_run = None;
        self.doc = Some(Doc {
            path: path.to_path_buf(),
            wcs: Wcs::from_header(&image.header),
            full: Arc::new(image),
            small,
        });
        self.tiles.clear();
        self.tiles_pending = true;
        self.reading = None;
        self.measured = None;
        self.status = None;
        self.dirty = true;
        self.view.fit();
        self.export_text = default_output(path, "export");
        self.exported = None;
        // An image opened from a folder step is for processing.
        if matches!(
            self.tool,
            Tool::Calibrate | Tool::Register | Tool::Stack | Tool::Drizzle | Tool::Science
        ) {
            self.tool = Tool::Stretch;
        }
    }

    /// Redraw the preview on the next frame.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn show(&mut self, ctx: &Context, img: &Image) {
        let size = [img.width(), img.height()];
        let mut rgba = rgba8(img);
        self.hist = Some(Histogram::of(&ColorImage::from_rgba_unmultiplied(
            size, &rgba,
        )));
        if crate::theme::is_night() {
            tiles::night_tint(&mut rgba);
        }
        let tex = ColorImage::from_rgba_unmultiplied(size, &rgba);
        self.texture = Some(ctx.load_texture("view", tex, TextureOptions::LINEAR));
    }

    /// The picture a folder step made, with what to call it, when the viewer
    /// should show it: the drizzle result, the first calibrated frame, the
    /// reference frame as aligned. Register and Stack fall back to the earlier
    /// step's picture, so the view is not empty while they run; Stack shows the
    /// open image first, which is usually the last stack.
    fn step_preview(&self) -> Option<(&Image, &'static str)> {
        let calibrated = || {
            self.cal
                .result
                .as_ref()
                .and_then(|r| r.preview.as_deref())
                .map(|i| (i, "first calibrated frame"))
        };
        let registered = || {
            self.reg
                .result
                .as_ref()
                .and_then(|r| r.preview.as_deref())
                .map(|i| (i, "reference frame"))
        };
        match self.tool {
            Tool::Drizzle => self
                .drizzle
                .result
                .as_ref()
                .map(|r| (&r.small, "drizzle result")),
            Tool::Calibrate => calibrated(),
            Tool::Register => registered().or_else(calibrated),
            Tool::Stack if self.doc.is_none() => registered().or_else(calibrated),
            _ => None,
        }
    }

    /// Whether the viewer shows the open image itself, stretched, so that
    /// full-resolution tiles of the pyramid apply to it.
    fn tiled(&self) -> bool {
        self.doc.is_some()
            && self.step_preview().is_none()
            && !matches!(self.tool, Tool::Gpu | Tool::Drizzle | Tool::Stars)
            && !(self.tool == Tool::Colour
                && self.colour.view == ColourView::Calibrated
                && self.colour.result.is_some())
    }

    fn refresh(&mut self, ctx: &Context) {
        self.tiles.invalidate();
        if matches!(
            self.tool,
            Tool::Gpu | Tool::Science | Tool::Optics | Tool::Astrometry
        ) {
            return;
        }
        if let Some((small, _)) = self.step_preview() {
            // A step's own picture, stretched like any other linear picture.
            let shown = self
                .stretch
                .measure(small, None)
                .and_then(|m| self.stretch.render(small, &m));
            match shown {
                Ok(img) => self.show(ctx, &img),
                Err(e) => self.status = Some((e, true)),
            }
            return;
        }
        if self.tool == Tool::Drizzle {
            self.texture = None;
            return;
        }
        let Some(doc) = &self.doc else {
            self.texture = None;
            return;
        };
        if self.tool == Tool::Stars {
            match self.stars.layer(&doc.small, self.stretch.target) {
                Ok(img) => self.show(ctx, &img),
                Err(e) => self.status = Some((e, true)),
            }
            return;
        }
        if self.tool == Tool::Colour
            && self.colour.view == ColourView::Calibrated
            && let Some(cal) = &self.colour.result
        {
            let shown = cal
                .apply(&doc.small)
                .map_err(|e| e.to_string())
                .and_then(|img| {
                    let m = self.stretch.measure(&img, None)?;
                    self.stretch.render(&img, &m)
                });
            match shown {
                Ok(img) => self.show(ctx, &img),
                Err(e) => self.status = Some((e, true)),
            }
            return;
        }
        let m = match self.stretch.measure(&doc.full, self.measured.as_ref()) {
            Ok(m) => m,
            Err(e) => {
                self.status = Some((e, true));
                return;
            }
        };
        match self.stretch.render(&doc.small, &m) {
            Ok(img) => {
                self.show(ctx, &img);
                if self.status.as_ref().is_some_and(|s| s.1) {
                    self.status = None;
                }
            }
            Err(e) => self.status = Some((e, true)),
        }
        self.measured = Some(m);
    }

    fn start_apply(&mut self, ctx: &Context) {
        let (Some(doc), Some(m)) = (&self.doc, &self.measured) else {
            return;
        };
        let full = Arc::clone(&doc.full);
        let (tool, m) = (self.stretch.clone(), m.clone());
        let out = PathBuf::from(self.output_text.trim());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = tool.apply(&full, &m).and_then(|img| {
                fits::write(&out, &img, Format::Float32)
                    .map(|()| format!("Wrote {}", out.display()))
                    .map_err(|e| format!("Cannot write {}: {e}", out.display()))
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.status = Some(("Stretching the full image...".into(), false));
    }

    fn start_stars(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else { return };
        let full = Arc::clone(&doc.full);
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        let mut opts = self.stars.run_options();
        opts.cancel = progress.token();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = remove_stars(&full, &opts)
                .map(|r| StarResult::new(r, PREVIEW_SIDE))
                .map_err(|e| failure(&e));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.star_run = Some(rx);
        self.status = Some(("Finding and removing the stars...".into(), false));
    }

    fn save_stars(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else { return };
        let (Some(starless), Some(stars)) =
            (self.stars.starless_full(&doc.full), self.stars.stars_full())
        else {
            return;
        };
        let out = PathBuf::from(self.star_output_text.trim());
        let stem = out
            .file_stem()
            .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
        let stars_path = out.with_file_name(format!(
            "{}_stars.fits",
            stem.strip_suffix("_starless").unwrap_or(&stem)
        ));
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = fits::write(&out, &starless, Format::Float32)
                .map_err(|e| format!("Cannot write {}: {e}", out.display()))
                .and_then(|()| {
                    fits::write(&stars_path, &stars, Format::Float32)
                        .map_err(|e| format!("Cannot write {}: {e}", stars_path.display()))
                })
                .map(|()| format!("Wrote {} and {}", out.display(), stars_path.display()));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.status = Some((
            "Writing the starless image and the star layer...".into(),
            false,
        ));
    }

    fn start_solve(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else { return };
        let mut opts = match self.solve.options() {
            Ok(o) => o,
            Err(e) => {
                self.status = Some((e, true));
                return;
            }
        };
        let cat = match self.solve.load_catalogue() {
            Ok(c) => c,
            Err(e) => {
                self.status = Some((e, true));
                return;
            }
        };
        self.solve.catalogue = Some(Arc::clone(&cat));
        let full = Arc::clone(&doc.full);
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        opts.cancel = progress.token();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = solve::solve(&full, &cat, &opts).map_err(|e| failure(&e));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.solve_run = Some(rx);
        self.status = Some((
            "Detecting stars and matching them to the catalogue...".into(),
            false,
        ));
    }

    fn save_solve(&mut self, ctx: &Context) {
        let (Some(doc), Some(sol)) = (&self.doc, &self.solve.result) else {
            return;
        };
        let (full, sol) = (Arc::clone(&doc.full), sol.clone());
        let out = PathBuf::from(self.solve_output_text.trim());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut img = (*full).clone();
            solve::record(&mut img, &sol);
            let r = fits::write(&out, &img, Format::Float32)
                .map(|()| format!("Wrote {}", out.display()))
                .map_err(|e| format!("Cannot write {}: {e}", out.display()));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.status = Some((
            "Writing the image with its world coordinates...".into(),
            false,
        ));
    }

    fn start_colour(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else { return };
        let wcs = Wcs::from_header(&doc.full.header)
            .or_else(|| self.solve.result.as_ref().map(|s| s.wcs.clone()));
        let Some(wcs) = wcs else {
            self.status = Some((
                "The image has no plate solution: run Plate solve first.".into(),
                true,
            ));
            return;
        };
        let cat = match self.solve.load_catalogue() {
            Ok(c) => c,
            Err(e) => {
                self.status = Some((e, true));
                return;
            }
        };
        self.solve.catalogue = Some(Arc::clone(&cat));
        let full = Arc::clone(&doc.full);
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        let mut opts = self.colour.opts.clone();
        opts.cancel = progress.token();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = photocal::calibrate(&full, &wcs, &cat, &opts).map_err(|e| failure(&e));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.colour_run = Some(rx);
        self.status = Some(("Measuring the Gaia stars in each colour...".into(), false));
    }

    fn save_colour(&mut self, ctx: &Context) {
        let (Some(doc), Some(cal)) = (&self.doc, &self.colour.result) else {
            return;
        };
        let (full, cal) = (Arc::clone(&doc.full), cal.clone());
        let out = PathBuf::from(self.colour_output_text.trim());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = cal
                .apply(&full)
                .map_err(|e| e.to_string())
                .and_then(|mut img| {
                    cal.record(&mut img);
                    fits::write(&out, &img, Format::Float32)
                        .map(|()| format!("Wrote {}", out.display()))
                        .map_err(|e| format!("Cannot write {}: {e}", out.display()))
                });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.status = Some(("Writing the calibrated image...".into(), false));
    }

    /// The text box that holds the path a dialog for `target` chooses.
    fn path_box(&mut self, target: Target) -> &mut String {
        match target {
            Target::Image => &mut self.path_text,
            Target::StretchOut => &mut self.output_text,
            Target::StarsOut => &mut self.star_output_text,
            Target::SolveOut => &mut self.solve_output_text,
            Target::ColourOut => &mut self.colour_output_text,
            Target::Catalogue => &mut self.solve.catalogue_path,
            Target::DrizzleFolder => &mut self.drizzle.folder,
            Target::DrizzleOut => &mut self.drizzle_output_text,
            Target::SortFolder => &mut self.sort_text,
            Target::CalLights => &mut self.cal.lights,
            Target::CalDarks => &mut self.cal.darks,
            Target::CalFlats => &mut self.cal.flats,
            Target::CalDarkFlats => &mut self.cal.dark_flats,
            Target::CalBias => &mut self.cal.bias,
            Target::CalOut => &mut self.cal.output,
            Target::RegIn => &mut self.reg.input,
            Target::RegOut => &mut self.reg.output,
            Target::StackIn => &mut self.stack.input,
            Target::StackOut => &mut self.stack.output,
            Target::ExportOut => &mut self.export_text,
            Target::ScienceIn => &mut self.science.folder,
        }
    }

    /// Open the native dialog for `target`, starting from what its box holds.
    pub fn browse(&mut self, ctx: &Context, target: Target) {
        let current = self.path_box(target).clone();
        self.chooser.start(ctx, target, &current);
    }

    /// A path box with a button that chooses the file to write.
    fn save_row(&mut self, ui: &mut Ui, target: Target) {
        let mut ask = false;
        let idle = !self.chooser.busy();
        let row = vec2(ui.available_width(), 44.0);
        ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
            ui.add_enabled_ui(idle, |ui| {
                ask = widgets::button(ui, "Save as...", Kind::Outline, 0.0)
                    .on_hover_text("Choose where to save with the file dialog")
                    .clicked();
            });
            ui.add(TextEdit::singleline(self.path_box(target)).desired_width(f32::INFINITY));
        });
        if ask {
            let ctx = ui.ctx().clone();
            self.browse(&ctx, target);
        }
    }

    fn start_drizzle(&mut self, ctx: &Context) {
        let folder = self.drizzle.folder.trim().to_owned();
        if folder.is_empty() {
            self.status = Some(("Enter the folder of calibrated frames.".into(), true));
            return;
        }
        if self.drizzle_output_text.trim().is_empty() {
            self.drizzle_output_text = Path::new(&folder)
                .join("drizzled.fits")
                .to_string_lossy()
                .into_owned();
        }
        let opts = self.drizzle.options();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = DrizzleTool::run(&folder, &opts, PREVIEW_SIDE, &progress);
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.drizzle_run = Some(rx);
        self.status = Some(("Registering and drizzling the frames...".into(), false));
    }

    fn save_drizzle(&mut self, ctx: &Context) {
        let Some(r) = &self.drizzle.result else {
            return;
        };
        let d = Arc::clone(&r.drizzled);
        let out = PathBuf::from(self.drizzle_output_text.trim());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = fits::write(&out, &d.image, Format::Float32)
                .map(|()| format!("Wrote {}", out.display()))
                .map_err(|e| format!("Cannot write {}: {e}", out.display()));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.status = Some(("Writing the drizzled image...".into(), false));
    }

    fn open_drizzle(&mut self) {
        let Some(r) = &self.drizzle.result else {
            return;
        };
        let image = r.drizzled.image.clone();
        let path = PathBuf::from(self.drizzle_output_text.trim());
        self.tool = Tool::Stretch;
        self.load(&path, image);
    }

    fn start_gpu(&mut self, ctx: &Context) {
        let tool = &self.gpu;
        let (adapter, width, height, frames) = (tool.adapter, tool.width, tool.height, tool.frames);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = GpuTool::run(adapter, width, height, frames);
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.gpu_run = Some(rx);
        self.status = Some(("Running each stage on the CPU and the GPU...".into(), false));
    }

    fn poll(&mut self) {
        if let Some(s) = self.tiles.poll() {
            self.status = Some((
                format!(
                    "Full-resolution zoom ready: {} levels in {}, {:.0} MB more memory",
                    s.levels,
                    if s.secs < 1.0 {
                        format!("{:.0} ms", s.secs * 1000.0)
                    } else {
                        format!("{:.1} s", s.secs)
                    },
                    s.extra_bytes as f32 / 1e6
                ),
                false,
            ));
        }
        if let Some(rx) = &self.gpu_run {
            match rx.try_recv() {
                Ok(Ok(run)) => {
                    self.status = Some((format!("Benchmarked on {}", run.adapter), false));
                    self.gpu.result = Some(run);
                    self.gpu_run = None;
                }
                Ok(Err(e)) => {
                    self.status = Some((e, true));
                    self.gpu.result = None;
                    self.gpu_run = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("The benchmark stopped unexpectedly.".into(), true));
                    self.gpu_run = None;
                }
            }
        }
        if let Some(rx) = &self.drizzle_run {
            match rx.try_recv() {
                Ok(Ok(run)) => {
                    self.status = Some((
                        format!(
                            "Drizzled {} of {} frames to {} x {}",
                            run.drizzled.used(),
                            run.found,
                            run.drizzled.image.width(),
                            run.drizzled.image.height()
                        ),
                        false,
                    ));
                    self.drizzle.result = Some(run);
                    self.drizzle_run = None;
                    self.dirty = true;
                }
                Ok(Err(e)) => {
                    if !self.cancelled_or_failed(e, "drizzling") {
                        self.drizzle.result = None;
                    }
                    self.drizzle_run = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("Drizzle stopped unexpectedly.".into(), true));
                    self.drizzle_run = None;
                }
            }
        }
        if let Some((target, path)) = self.chooser.poll() {
            if target == Target::Image {
                self.open(&path);
            } else if target == Target::SortFolder {
                self.pending_sort = Some(path);
            } else {
                if target == Target::Catalogue {
                    self.solve.catalogue = None;
                }
                *self.path_box(target) = path.to_string_lossy().into_owned();
                self.suggest(target);
            }
        }
        if let Some(rx) = &self.colour_run {
            match rx.try_recv() {
                Ok(Ok(cal)) => {
                    self.status = Some((
                        format!(
                            "Colour calibrated from {} stars: red x {:.3}, blue x {:.3}",
                            cal.used, cal.gains[0], cal.gains[2]
                        ),
                        false,
                    ));
                    self.colour.result = Some(cal);
                    self.colour_run = None;
                    self.dirty = true;
                }
                Ok(Err(e)) => {
                    if !self.cancelled_or_failed(e, "colour calibration") {
                        self.colour.result = None;
                    }
                    self.colour_run = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("Colour calibration stopped unexpectedly.".into(), true));
                    self.colour_run = None;
                }
            }
        }
        if let Some(rx) = &self.solve_run {
            match rx.try_recv() {
                Ok(Ok(sol)) => {
                    self.status = Some((
                        format!(
                            "Solved: {} stars matched, rms {:.2} arcsec",
                            sol.matched, sol.rms_arcsec
                        ),
                        false,
                    ));
                    // The pixel readout reads sky positions from the new solution.
                    if let Some(d) = &mut self.doc {
                        d.wcs = Some(sol.wcs.clone());
                    }
                    self.solve.result = Some(sol);
                    self.solve_run = None;
                }
                Ok(Err(e)) => {
                    if !self.cancelled_or_failed(e, "plate solving") {
                        self.solve.result = None;
                    }
                    self.solve_run = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("Plate solving stopped unexpectedly.".into(), true));
                    self.solve_run = None;
                }
            }
        }
        if let Some(rx) = &self.star_run {
            match rx.try_recv() {
                Ok(Ok(r)) => {
                    self.status = Some((
                        format!(
                            "{} stars removed, {:.1} % of the pixels filled",
                            r.removal.found,
                            100.0 * r.removal.masked_fraction
                        ),
                        false,
                    ));
                    self.stars.result = Some(r);
                    self.star_run = None;
                    self.dirty = true;
                }
                Ok(Err(e)) => {
                    self.cancelled_or_failed(e, "star removal");
                    self.star_run = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("Star removal stopped unexpectedly.".into(), true));
                    self.star_run = None;
                }
            }
        }
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(Ok(msg)) => {
                    self.status = Some((msg, false));
                    self.job = None;
                }
                Ok(Err(e)) => {
                    self.status = Some((e, true));
                    self.job = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = Some(("The write stopped unexpectedly.".into(), true));
                    self.job = None;
                }
            }
        }
    }

    /// Files dropped on the window: one FITS file opens; folders, or several files, are
    /// scanned and sorted into lights, darks, flats and bias for review.
    pub fn handle_drops(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        match dropped.as_slice() {
            [] => {}
            [one] if !one.is_dir() => self.open(one),
            _ => self.start_sort(ctx, dropped),
        }
    }

    /// Read the headers under `paths` on a worker and sort what is found.
    pub fn start_sort(&mut self, ctx: &Context, paths: Vec<PathBuf>) {
        if self.running() {
            self.status = Some((
                "Wait for the running step to finish, or cancel it (Esc), before dropping frames."
                    .into(),
                true,
            ));
            return;
        }
        self.review = None;
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.sort_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            sorter::scan(&paths, p).map(|s| (paths, s))
        }));
        self.status = Some(("Reading the headers to sort the frames...".into(), false));
    }

    /// Accept the review: gather any selections of frames, then fill the folder boxes.
    pub fn accept_review(&mut self, ctx: &Context) {
        let Some(r) = &self.review else { return };
        if r.count(FrameKind::Light).1 == 0 {
            self.status = Some((
                "Assign at least one frame to Lights before accepting.".into(),
                true,
            ));
            return;
        }
        let (resolved, root) = (r.resolve(), r.root.clone());
        let gathered: usize = resolved
            .iter()
            .map(|(_, f)| {
                if let Folder::Gather(v) = f {
                    v.len()
                } else {
                    0
                }
            })
            .sum();
        let progress = Progress::new();
        progress.set_total(gathered);
        self.progress = Some(Arc::clone(&progress));
        self.stage_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            let mut set = FolderSet::default();
            let mut staged = false;
            for (kind, folder) in resolved {
                let path = match folder {
                    Folder::Whole(d) | Folder::File(d) => d,
                    Folder::Gather(files) => {
                        p.say(&format!("Gathering the {}...", kind.label().to_lowercase()));
                        staged = true;
                        sorter::stage(&root, kind, &files, p)?
                    }
                };
                let text = path.to_string_lossy().into_owned();
                match kind {
                    FrameKind::Light => set.lights = text,
                    FrameKind::Dark => set.darks = text,
                    FrameKind::DarkFlat => set.dark_flats = text,
                    FrameKind::Flat => set.flats = text,
                    FrameKind::Bias => set.bias = text,
                    FrameKind::Other => {}
                }
            }
            Ok((set, staged))
        }));
        self.status = Some((
            "Filling the folders from the sorted frames...".into(),
            false,
        ));
    }

    /// The folder chosen to sort, if the dialog has just answered.
    pub fn take_pending_sort(&mut self) -> Option<PathBuf> {
        self.pending_sort.take()
    }

    /// Run the open step or tool, as its own button does, or say why not.
    pub fn run_current(&mut self, ctx: &Context) {
        if self.running() {
            self.notify("Something is already running: Esc cancels it.");
            return;
        }
        if self.review.is_some() {
            self.accept_review(ctx);
            return;
        }
        let needs_image = |ws: &mut Self| {
            ws.status = Some(("Open an image first (Ctrl+O).".into(), true));
        };
        match self.tool {
            Tool::Calibrate => self.start_calibrate(ctx),
            Tool::Register => self.start_register(ctx),
            Tool::Stack => self.start_stack(ctx),
            Tool::Drizzle => self.start_drizzle(ctx),
            Tool::Gpu => self.start_gpu(ctx),
            Tool::Science => self.start_science(ctx),
            Tool::Optics => self.start_optics(ctx),
            Tool::Astrometry => self.start_astro(ctx),
            Tool::Export if self.doc.is_none() || self.measured.is_none() => {
                self.status = Some(("Open or stack an image to export it.".into(), true));
            }
            Tool::Export => self.start_export(ctx),
            _ if self.doc.is_none() => needs_image(self),
            Tool::Stretch if self.output_text.trim().is_empty() => {
                self.status = Some((
                    "Name the file for the stretched image first, then run.".into(),
                    true,
                ));
            }
            Tool::Stretch => self.start_apply(ctx),
            Tool::Stars => self.start_stars(ctx),
            Tool::Solve => self.start_solve(ctx),
            Tool::Colour => self.start_colour(ctx),
        }
    }

    /// Move to the step `delta` places on (negative: back), or say it is the end.
    pub fn step_by(&mut self, delta: i32) {
        let Some(current) = self.tool.step() else {
            self.notify("Click a step on the left to leave the GPU benchmark.");
            return;
        };
        let last = Step::ALL.len() - 1;
        let to = i64::try_from(current.index()).unwrap_or(0) + i64::from(delta);
        if to < 0 {
            self.notify("Already at the first step: Calibrate.");
        } else if to > i64::try_from(last).unwrap_or(0) {
            self.notify("Already at the last step: Export.");
        } else {
            let step = Step::ALL[usize::try_from(to).unwrap_or(0)];
            self.select(step);
            self.notify(&format!(
                "Step {} of {}: {}",
                step.index() + 1,
                Step::ALL.len(),
                step.label()
            ));
        }
    }

    /// Zoom the picture by `factor` about its centre, or say there is none.
    pub fn zoom_key(&mut self, factor: f32) {
        let Some(t) = self.texture.as_ref() else {
            self.notify("Open an image to zoom it.");
            return;
        };
        let size = self.logical_size(t.size_vec2());
        self.view
            .zoom_by(factor, egui::Vec2::ZERO, size, self.view_avail);
        let pct = self.view.scale(size, self.view_avail) * 100.0;
        self.notify(&format!("Zoom {pct:.0} percent"));
    }

    /// Fit the picture to the window, or show it at 100 percent.
    pub fn zoom_to(&mut self, fit: bool) {
        if self.texture.is_none() {
            self.notify("Open an image to zoom it.");
        } else if fit {
            self.view.fit();
            self.notify("Fitted the picture to the window");
        } else {
            self.view.zoom = Some(1.0);
            self.view.pan = egui::Vec2::ZERO;
            self.notify("Zoom 100 percent");
        }
    }

    /// Throw the review away.
    pub fn cancel_review(&mut self) {
        self.review = None;
        self.status = Some((
            "Dropped the sorting; the folders are as they were.".into(),
            false,
        ));
    }

    /// Say something short about an action that starts no work. While work runs the
    /// words go beside its progress and the run's own message stays.
    pub fn notify(&mut self, text: &str) {
        if self.running() {
            self.flash = Some((text.to_owned(), Instant::now()));
        } else {
            self.status = Some((text.to_owned(), false));
        }
    }

    /// Fill the output box that goes with a folder just chosen, when it is empty.
    fn suggest(&mut self, target: Target) {
        match target {
            Target::CalLights if self.cal.output.trim().is_empty() => {
                self.cal.output = beside(&self.cal.lights, "calibrated");
            }
            Target::RegIn if self.reg.output.trim().is_empty() => {
                self.reg.output = beside(&self.reg.input, "registered");
            }
            Target::StackIn if self.stack.output.trim().is_empty() => {
                self.stack.output = beside(&self.stack.input, "stack.fits");
            }
            _ => {}
        }
    }

    /// Whether any background work is running.
    #[must_use]
    pub fn running(&self) -> bool {
        self.job.is_some()
            || self.star_run.is_some()
            || self.solve_run.is_some()
            || self.colour_run.is_some()
            || self.drizzle_run.is_some()
            || self.gpu_run.is_some()
            || self.science_run.is_some()
            || self.science_int_run.is_some()
            || self.optics_run.is_some()
            || self.astro_run.is_some()
            || self.light_run.is_some()
            || self.cal_run.is_some()
            || self.reg_run.is_some()
            || self.stack_run.is_some()
            || self.sort_run.is_some()
            || self.stage_run.is_some()
            || self.tiles.building()
    }

    /// What the strip above the viewer should say, if anything.
    #[must_use]
    pub fn notice(&self) -> Option<Notice> {
        let (text, error) = self.status.clone()?;
        if !self.running() {
            return Some(Notice {
                text,
                detail: String::new(),
                fraction: None,
                running: false,
                cancellable: false,
                error,
            });
        }
        let mut detail = Vec::new();
        let mut fraction = None;
        let mut line = String::new();
        if let Some(p) = &self.progress {
            if let Some((done, total)) = p.counts() {
                detail.push(format!("{done} of {total} frames"));
                fraction = Some(done as f32 / total as f32);
            }
            line = p.line();
        }
        if let Some((said, when)) = &self.flash
            && when.elapsed() < std::time::Duration::from_secs(4)
        {
            detail.push(said.clone());
        }
        if let Some(t) = self.busy_since {
            detail.push(format!("{} s", t.elapsed().as_secs()));
        }
        if !line.is_empty() {
            detail.push(line);
        }
        Some(Notice {
            text,
            detail: detail.join("  -  "),
            fraction,
            running: true,
            cancellable: self.progress.as_ref().is_some_and(|p| !p.is_cancelled()),
            error: false,
        })
    }

    /// Whether a running step can be cancelled now.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.running() && self.progress.as_ref().is_some_and(|p| !p.is_cancelled())
    }

    /// Ask the running step to stop. It does so at the next frame, band or plane,
    /// and the notice says `Cancelled` when it has.
    pub fn cancel(&mut self) {
        let Some(p) = &self.progress else { return };
        if !self.running() || p.is_cancelled() {
            return;
        }
        p.cancel();
        self.status = Some((
            "Cancelling: finishing the frame in hand, nothing will be kept...".into(),
            false,
        ));
    }

    /// A failure from a run: a cancel becomes a plain notice that `what` stopped,
    /// anything else an error. True when it was a cancel.
    fn cancelled_or_failed(&mut self, e: String, what: &str) -> bool {
        if e == CANCELLED {
            self.status = Some((format!("Cancelled: {what} stopped. {KEPT}"), false));
            true
        } else {
            self.status = Some((e, true));
            false
        }
    }

    /// Pretend `step` is running with `progress`, for the screenshots.
    #[cfg(test)]
    pub fn fake_running(&mut self, text: &str, progress: Arc<Progress>, secs: u64) {
        let (tx, rx) = channel::<Result<StackRun, String>>();
        // Keep the sender alive so the receiver never reports a dead worker.
        std::mem::forget(tx);
        self.stack_run = Some(rx);
        self.progress = Some(progress);
        self.busy_since =
            Instant::now().checked_sub(std::time::Duration::from_millis(secs * 1000 + 400));
        self.status = Some((text.to_owned(), false));
    }

    /// As [`Self::fake_running`], keeping the way to finish the pretend run.
    #[cfg(test)]
    pub fn fake_running_with_end(
        &mut self,
        text: &str,
        progress: Arc<Progress>,
        secs: u64,
    ) -> std::sync::mpsc::Sender<Result<StackRun, String>> {
        let (tx, rx) = channel::<Result<StackRun, String>>();
        self.stack_run = Some(rx);
        self.progress = Some(progress);
        self.busy_since =
            Instant::now().checked_sub(std::time::Duration::from_millis(secs * 1000 + 400));
        self.status = Some((text.to_owned(), false));
        tx
    }

    /// Dismiss the message in the notice strip.
    pub fn dismiss_notice(&mut self) {
        if !self.running() {
            self.status = None;
        }
    }

    /// Open the GPU benchmark, or go back to the step it replaced.
    pub fn toggle_gpu(&mut self) {
        if self.tool == Tool::Gpu {
            self.tool = self.before_gpu.take().unwrap_or(Tool::Calibrate);
        } else {
            self.before_gpu = Some(self.tool);
            self.tool = Tool::Gpu;
        }
        self.dirty = true;
    }

    /// Open the panel of workflow step `step`.
    pub fn select(&mut self, step: Step) {
        if self.tool.step() == Some(step) {
            return;
        }
        if self.tool.step() == Some(Step::Process) {
            self.process_tool = Some(self.tool);
        }
        self.tool = match step {
            Step::Calibrate => Tool::Calibrate,
            Step::Register => Tool::Register,
            Step::Stack => Tool::Stack,
            Step::Process => self.process_tool.unwrap_or(Tool::Stretch),
            Step::Export => Tool::Export,
        };
        self.dirty = true;
    }

    /// The rail: one row per step, with where it stands.
    #[must_use]
    pub fn rail_rows(&self) -> Vec<Row> {
        let has_image = self.doc.is_some();
        let name = self
            .doc
            .as_ref()
            .and_then(|d| d.path.file_name().map(|n| n.to_string_lossy().into_owned()));
        Step::ALL
            .iter()
            .map(|&step| {
                let (state, status) = match step {
                    Step::Calibrate => {
                        if self.cal_run.is_some() {
                            (State::Running, "Calibrating".to_owned())
                        } else if let Some(r) = &self.cal.result {
                            (State::Done, format!("{} lights", r.lights))
                        } else {
                            (State::Idle, "Optional".to_owned())
                        }
                    }
                    Step::Register => {
                        if self.reg_run.is_some() {
                            (State::Running, "Registering".to_owned())
                        } else if let Some(r) = &self.reg.result {
                            (State::Done, format!("{} of {} aligned", r.written, r.total))
                        } else {
                            (State::Idle, "Optional".to_owned())
                        }
                    }
                    Step::Stack => {
                        if self.stack_run.is_some() {
                            (State::Running, "Stacking".to_owned())
                        } else if self.drizzle_run.is_some() {
                            (State::Running, "Drizzling".to_owned())
                        } else if let Some(r) = &self.stack.result {
                            (State::Done, format!("{} of {} frames", r.used, r.total))
                        } else if let Some(r) = &self.drizzle.result {
                            (
                                State::Done,
                                format!("{} of {} drizzled", r.drizzled.used(), r.found),
                            )
                        } else if self.stack.input.trim().is_empty() {
                            (State::Waiting, "No frames yet".to_owned())
                        } else {
                            (State::Ready, "Ready".to_owned())
                        }
                    }
                    Step::Process => {
                        if self.star_run.is_some()
                            || self.solve_run.is_some()
                            || self.colour_run.is_some()
                        {
                            (State::Running, "Working".to_owned())
                        } else if !has_image {
                            (State::Waiting, "No image".to_owned())
                        } else {
                            let mut done = Vec::new();
                            if self.stars.result.is_some() {
                                done.push("starless");
                            }
                            if self.solve.result.is_some() {
                                done.push("solved");
                            }
                            if self.colour.result.is_some() {
                                done.push("colour");
                            }
                            if done.is_empty() {
                                (State::Ready, name.clone().unwrap_or_default())
                            } else {
                                (State::Done, done.join(", "))
                            }
                        }
                    }
                    Step::Export => {
                        if self.job.is_some() && self.tool == Tool::Export {
                            (State::Running, "Writing".to_owned())
                        } else if self.exported.is_some() {
                            (State::Done, "Written".to_owned())
                        } else if has_image {
                            (State::Ready, "Ready".to_owned())
                        } else {
                            (State::Waiting, "No image".to_owned())
                        }
                    }
                };
                Row {
                    step,
                    state,
                    status,
                    hint: step.hint().to_owned(),
                }
            })
            .collect()
    }

    /// Start `run` on a worker thread with a fresh [`Progress`], keeping its receiver in `slot`.
    fn spawn_step<T: Send + 'static>(
        ctx: &Context,
        progress: &Arc<Progress>,
        run: impl FnOnce(&Progress) -> Result<T, String> + Send + 'static,
    ) -> Receiver<Result<T, String>> {
        let (tx, rx) = channel();
        let (ctx, progress) = (ctx.clone(), Arc::clone(progress));
        std::thread::spawn(move || {
            let _ = tx.send(run(&progress));
            ctx.request_repaint();
        });
        rx
    }

    /// Note the folders in the step panels as the session now being worked on.
    fn note_session(&mut self) {
        let set = if self.cal.lights.trim().is_empty() {
            let input = if self.reg.input.trim().is_empty() {
                &self.stack.input
            } else {
                &self.reg.input
            };
            FolderSet {
                lights: input.trim().to_owned(),
                ..FolderSet::default()
            }
        } else {
            FolderSet {
                lights: self.cal.lights.trim().to_owned(),
                darks: self.cal.darks.trim().to_owned(),
                flats: self.cal.flats.trim().to_owned(),
                dark_flats: self.cal.dark_flats.trim().to_owned(),
                bias: self.cal.bias.trim().to_owned(),
            }
        };
        if !set.lights.is_empty() {
            self.pending_session = Some(set);
        }
    }

    /// Fill the Calibrate, Register and Stack folder boxes from `set` and open the first step
    /// that has work to do: Calibrate when there are calibration frames, else Stack.
    ///
    /// Folders that no longer exist are left out and named in the message.
    pub fn apply_folders(&mut self, set: &FolderSet) {
        let gone = set.missing();
        let keep = |kind: &str, v: &str| {
            if gone.contains(&kind) {
                String::new()
            } else {
                v.to_owned()
            }
        };
        let set = FolderSet {
            lights: set.lights.clone(),
            darks: keep("darks", &set.darks),
            flats: keep("flats", &set.flats),
            dark_flats: keep("dark flats", &set.dark_flats),
            bias: keep("bias", &set.bias),
        };
        let calibrating = set.has_calibration();
        let first = if calibrating {
            self.cal.lights.clone_from(&set.lights);
            self.cal.darks.clone_from(&set.darks);
            self.cal.flats.clone_from(&set.flats);
            self.cal.dark_flats.clone_from(&set.dark_flats);
            self.cal.bias.clone_from(&set.bias);
            self.cal.output = beside(&set.lights, "calibrated");
            self.reg.input.clone_from(&self.cal.output);
            self.reg.output = beside(&self.cal.output, "registered");
            self.stack.input.clone_from(&self.cal.output);
            self.cal.output.clone()
        } else {
            self.cal.lights.clear();
            self.cal.darks.clear();
            self.cal.flats.clear();
            self.cal.dark_flats.clear();
            self.cal.bias.clear();
            self.cal.output.clear();
            self.reg.input.clone_from(&set.lights);
            self.reg.output = beside(&set.lights, "registered");
            self.stack.input.clone_from(&set.lights);
            set.lights.clone()
        };
        self.stack.output = beside(&first, "stack.fits");
        self.select(if calibrating {
            Step::Calibrate
        } else {
            Step::Stack
        });
        let mut said = format!(
            "Filled the Calibrate, Register and Stack folders from {}",
            set.title()
        );
        if !gone.is_empty() {
            let _ = write!(said, " (not found, left out: {})", gone.join(", "));
        }
        self.status = Some((said, false));
    }

    fn start_calibrate(&mut self, ctx: &Context) {
        if self.cal.output.trim().is_empty() && !self.cal.lights.trim().is_empty() {
            self.cal.output = beside(&self.cal.lights, "calibrated");
        }
        self.note_session();
        let tool = self.cal.clone();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.cal_run = Some(Self::spawn_step(ctx, &progress, move |p| tool.run(p)));
        self.status = Some(("Calibrating the lights...".into(), false));
    }

    fn start_register(&mut self, ctx: &Context) {
        if self.reg.input.trim().is_empty() {
            self.reg.input = self
                .cal
                .result
                .as_ref()
                .map_or_else(String::new, |r| r.folder.clone());
        }
        if self.reg.output.trim().is_empty() && !self.reg.input.trim().is_empty() {
            self.reg.output = beside(&self.reg.input, "registered");
        }
        self.note_session();
        let tool = self.reg.clone();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.reg_run = Some(Self::spawn_step(ctx, &progress, move |p| tool.run(p)));
        self.status = Some(("Finding the stars and aligning the frames...".into(), false));
    }

    fn start_stack(&mut self, ctx: &Context) {
        if self.stack.input.trim().is_empty() {
            self.stack.input = self
                .reg
                .result
                .as_ref()
                .map(|r| r.folder.clone())
                .or_else(|| self.cal.result.as_ref().map(|r| r.folder.clone()))
                .unwrap_or_default();
        }
        if self.stack.output.trim().is_empty() && !self.stack.input.trim().is_empty() {
            self.stack.output = beside(&self.stack.input, "stack.fits");
        }
        self.note_session();
        let tool = self.stack.clone();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.stack_run = Some(Self::spawn_step(ctx, &progress, move |p| tool.run(p)));
        self.status = Some(("Registering and stacking the frames...".into(), false));
    }

    fn start_science(&mut self, ctx: &Context) {
        if self.science.folder.trim().is_empty() {
            self.science.folder = self
                .reg
                .result
                .as_ref()
                .map(|r| r.folder.clone())
                .or_else(|| self.cal.result.as_ref().map(|r| r.folder.clone()))
                .unwrap_or_else(|| self.stack.input.clone());
        }
        let tool = self.science.for_measure();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.science_run = Some(Self::spawn_step(ctx, &progress, move |p| tool.run(p)));
        self.status = Some(("Registering and measuring the frames...".into(), false));
    }

    fn start_optics(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else {
            self.status = Some(("Open an image first (Ctrl+O).".into(), true));
            return;
        };
        let full = Arc::clone(&doc.full);
        let tool = self.optics.for_run();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.optics_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            tool.run(&full, p)
        }));
        self.status = Some(("Measuring the stars...".into(), false));
    }

    /// The plate solution to draw: the image's own, or the last solve's.
    fn astro_wcs(&self) -> Option<Wcs> {
        let doc = self.doc.as_ref()?;
        Wcs::from_header(&doc.full.header)
            .or_else(|| self.solve.result.as_ref().map(|s| s.wcs.clone()))
    }

    fn start_astro(&mut self, ctx: &Context) {
        let Some(doc) = &self.doc else {
            self.status = Some(("Open an image first (Ctrl+O).".into(), true));
            return;
        };
        let Some(wcs) = self.astro_wcs() else {
            self.status = Some(("Plate solve the image first.".into(), true));
            return;
        };
        let cat = match self.solve.load_catalogue() {
            Ok(c) => c,
            Err(e) => {
                self.status = Some((e, true));
                return;
            }
        };
        let full = Arc::clone(&doc.full);
        let tool = self.astro.for_run();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.astro_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            tool.run(&full, &wcs, &cat, p)
        }));
        self.status = Some(("Drawing the plate solution...".into(), false));
    }

    fn astro_panel(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let have = self.astro_wcs().is_some();
        match self.astro.controls(ui, self.astro_run.is_some(), have) {
            AstroAsk::Nothing => {}
            AstroAsk::Run => self.start_astro(&ctx),
            AstroAsk::Save => {
                self.status = Some(match self.astro.save() {
                    Ok(p) => (format!("Wrote the report to {}", p.display()), false),
                    Err(e) => (e, true),
                });
            }
        }
    }

    fn start_light_curve(&mut self, ctx: &Context) {
        if self.science.folder.trim().is_empty() {
            self.science.folder = self.stack.input.clone();
        }
        let tool = self.science.for_photometry();
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.light_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            tool.run_light_curve(p)
        }));
        self.status = Some(("Measuring the star in every frame...".into(), false));
    }

    fn optics_panel(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        match self.optics.controls(ui, self.optics_run.is_some()) {
            OpticsAsk::Nothing => {}
            OpticsAsk::Run => self.start_optics(&ctx),
            OpticsAsk::Save => {
                self.status = Some(match self.optics.save() {
                    Ok(p) => (format!("Wrote the report to {}", p.display()), false),
                    Err(e) => (e, true),
                });
            }
        }
    }

    fn start_integration(&mut self, ctx: &Context) {
        if self.science.folder.trim().is_empty() {
            self.science.folder = self.stack.input.clone();
        }
        let tool = ScienceTool::for_run(&self.science.folder);
        let progress = Progress::new();
        self.progress = Some(Arc::clone(&progress));
        self.science_int_run = Some(Self::spawn_step(ctx, &progress, move |p| {
            tool.run_integration(p)
        }));
        self.status = Some((
            "Stacking on the CPU for the rejection and weight maps...".into(),
            false,
        ));
    }

    fn science_panel(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        match self.science.controls(ui, self.science_run.is_some()) {
            ScienceAsk::Nothing => {}
            ScienceAsk::Run => self.start_science(&ctx),
            ScienceAsk::Integrate => self.start_integration(&ctx),
            ScienceAsk::LightCurve => self.start_light_curve(&ctx),
            ScienceAsk::Browse(t) => self.browse(&ctx, t),
            ScienceAsk::Apply => self.science.apply_limits(),
            ScienceAsk::Redraw => self.science.redraw(),
            ScienceAsk::Save => {
                self.status = Some(match self.science.save() {
                    Ok(p) => (format!("Wrote the report to {}", p.display()), false),
                    Err(e) => (e, true),
                });
            }
            ScienceAsk::UseForStacking => {
                let out = self.science.rejected();
                self.status = Some((
                    format!(
                        "The next stack leaves out {} frame{}.",
                        out.len(),
                        if out.len() == 1 { "" } else { "s" }
                    ),
                    false,
                ));
                self.stack.exclude = out;
            }
        }
    }

    fn start_export(&mut self, ctx: &Context) {
        let (Some(doc), Some(m)) = (&self.doc, &self.measured) else {
            return;
        };
        let full = Arc::clone(&doc.full);
        let out = PathBuf::from(self.export_text.trim());
        let format = self.export_format;
        let (tool, m) = (self.stretch.clone(), m.clone());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = match format {
                ExportFormat::Fits => fits::write(&out, &full, Format::Float32)
                    .map_err(|e| format!("Cannot write {}: {e}", out.display())),
                ExportFormat::Png => tool.apply(&full, &m).and_then(|img| {
                    valerian_core::preview::write_png(&out, &img)
                        .map_err(|e| format!("Cannot write {}: {e}", out.display()))
                }),
            };
            let _ = tx.send(r.map(|()| format!("Wrote {}", out.display())));
            ctx.request_repaint();
        });
        self.job = Some(rx);
        self.exported = None;
        self.status = Some(("Writing the picture...".into(), false));
    }

    /// Collect finished background work and keep the running clock. Called
    /// every frame, whatever is on screen.
    pub fn tick(&mut self) {
        self.poll();
        self.poll_pipeline();
    }

    fn poll_pipeline(&mut self) {
        if let Some(r) = take(&mut self.cal_run, "Calibration stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    self.status = Some((
                        format!("Calibrated {} lights into {}", run.lights, run.folder),
                        false,
                    ));
                    if self.reg.input.trim().is_empty() {
                        self.reg.input.clone_from(&run.folder);
                        self.reg.output = beside(&run.folder, "registered");
                    }
                    if self.stack.input.trim().is_empty() {
                        self.stack.input.clone_from(&run.folder);
                        self.stack.output = beside(&run.folder, "stack.fits");
                    }
                    self.cal.result = Some(run);
                    self.dirty = true;
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "calibration");
                }
            }
        }
        if let Some(r) = take(&mut self.reg_run, "Registration stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    self.status = Some((
                        format!(
                            "Aligned {} of {} frames to frame {}",
                            run.written, run.total, run.reference
                        ),
                        false,
                    ));
                    let was_calibrated = self
                        .cal
                        .result
                        .as_ref()
                        .is_some_and(|c| c.folder == self.stack.input);
                    if self.stack.input.trim().is_empty() || was_calibrated {
                        self.stack.input.clone_from(&run.folder);
                    }
                    self.reg.result = Some(run);
                    self.dirty = true;
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "registration");
                }
            }
        }
        if let Some(r) = take(&mut self.stack_run, "Stacking stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    let said = format!(
                        "Stacked {} of {} frames ({}) and opened the stack",
                        run.used, run.total, run.rejection
                    );
                    let (path, image) = (run.path.clone(), (*run.image).clone());
                    self.stack.result = Some(run);
                    self.load(&path, image);
                    self.status = Some((said, false));
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "stacking");
                }
            }
        }
        if let Some(r) = take(&mut self.astro_run, "The drawing stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    let said = format!(
                        "Drew {} objects and {} Gaia stars{}",
                        run.annotated.objects.len(),
                        run.annotated.stars.len(),
                        run.residuals.as_ref().map_or(String::new(), |r| format!(
                            "; residual rms {:.2} arcsec",
                            r.rms_arcsec
                        ))
                    );
                    let path = self
                        .doc
                        .as_ref()
                        .map(|d| d.path.clone())
                        .unwrap_or_default();
                    self.astro.accept(run, &path);
                    self.status = Some((said, false));
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "the drawing");
                }
            }
        }
        if let Some(r) = take(&mut self.light_run, "The photometry stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    self.status = Some((
                        format!(
                            "Measured the star in {} frames: scatter {:.3} mag",
                            run.curve.rows.iter().filter(|r| r.ok).count(),
                            run.curve.scatter_mag
                        ),
                        false,
                    ));
                    self.science.accept_light(run);
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "the photometry");
                }
            }
        }
        if let Some(r) = take(
            &mut self.optics_run,
            "The measurement stopped unexpectedly.",
        ) {
            match r {
                Ok(run) => {
                    let said = format!(
                        "Measured {} stars: median FWHM {:.2} px",
                        run.field.stars.len(),
                        run.field.median_fwhm()
                    );
                    let path = self
                        .doc
                        .as_ref()
                        .map(|d| d.path.clone())
                        .unwrap_or_default();
                    self.optics.accept(run, &path);
                    self.status = Some((said, false));
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "the measurement");
                }
            }
        }
        if let Some(r) = take(
            &mut self.science_int_run,
            "The stack for the maps stopped unexpectedly.",
        ) {
            match r {
                Ok(run) => {
                    self.status =
                        Some((format!("Stacked {} frames for the maps", run.used), false));
                    self.science.accept_integration(run);
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "the stack for the maps");
                }
            }
        }
        if let Some(r) = take(&mut self.science_run, "The analysis stopped unexpectedly.") {
            match r {
                Ok(run) => {
                    let (n, folder) = (run.metrics.len(), run.folder.clone());
                    self.science.folder = folder;
                    self.science.accept(run);
                    let approved = self.science.analysis.as_ref().map_or(0, Analysis::approved);
                    self.status =
                        Some((format!("Measured {n} frames: {approved} approved"), false));
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "the analysis");
                }
            }
        }
        if let Some(r) = take(&mut self.sort_run, "Sorting stopped unexpectedly.") {
            match r {
                Ok((paths, scan)) => {
                    let review = Review::new(&paths, scan);
                    let parts: Vec<String> = FrameKind::ALL
                        .iter()
                        .filter(|k| review.count(**k).0 > 0)
                        .map(|k| format!("{} {}", review.count(*k).0, k.label().to_lowercase()))
                        .collect();
                    self.status = Some((
                        format!(
                            "Sorted {} frames: {}. Check the groups, then accept.",
                            review.frames.len(),
                            parts.join(", ")
                        ),
                        false,
                    ));
                    self.review = Some(review);
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "sorting");
                }
            }
        }
        if let Some(r) = take(
            &mut self.stage_run,
            "Gathering the frames stopped unexpectedly.",
        ) {
            match r {
                Ok((set, staged)) => {
                    self.review = None;
                    self.apply_folders(&set);
                    if staged && let Some((said, _)) = &mut self.status {
                        let _ = write!(
                            said,
                            ". Selections were gathered as links in {}",
                            sorter::STAGE_DIR
                        );
                    }
                    self.pending_session = Some(set);
                }
                Err(e) => {
                    self.cancelled_or_failed(e, "gathering the frames");
                }
            }
        }
        if self.tool == Tool::Export
            && self.job.is_none()
            && self.exported.is_none()
            && let Some((m, false)) = &self.status
            && m.starts_with("Wrote ")
        {
            self.exported = Some(m.clone());
        }
        if self.running() {
            self.busy_since.get_or_insert_with(Instant::now);
        } else {
            self.busy_since = None;
            self.progress = None;
        }
    }

    /// Chips that switch between the tools of a step.
    fn tool_chips(&mut self, ui: &mut Ui, tools: &[(Tool, &str)]) {
        ui.horizontal_wrapped(|ui| {
            for &(t, label) in tools {
                if widgets::chip(ui, self.tool == t, label).clicked() && self.tool != t {
                    self.tool = t;
                    self.dirty = true;
                }
            }
        });
    }

    /// The path box and Browse and Open buttons for the working image.
    fn open_block(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Image");
        ui.add(
            TextEdit::singleline(&mut self.path_text)
                .hint_text("Path to a FITS file")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.chooser.busy(), |ui| {
                if widgets::button(ui, "Browse...", Kind::Outline, 0.0)
                    .on_hover_text("Choose a FITS file with the file dialog (Ctrl+O)")
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.browse(&ctx, Target::Image);
                }
            });
            if widgets::button(ui, "Open", Kind::Outline, 0.0)
                .on_hover_text("Open the file named above, or drop one on the window")
                .clicked()
            {
                let path = PathBuf::from(self.path_text.trim());
                self.open(&path);
            }
        });
        if let Some(doc) = &self.doc {
            ui.label(
                RichText::new(format!(
                    "{}: {} x {} px, {} plane{}",
                    doc.path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    doc.full.width(),
                    doc.full.height(),
                    doc.full.planes.len(),
                    if doc.full.planes.len() == 1 { "" } else { "s" }
                ))
                .font(mono(12.5))
                .color(p.faint),
            );
        }
    }

    /// The inspector: the open step's name and what it is for, then its panel.
    pub fn tool_panel(&mut self, ui: &mut Ui) {
        let p = palette();
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                if let Some(step) = self.tool.step() {
                    widgets::overline(
                        ui,
                        &format!("Step {} of {}", step.index() + 1, Step::ALL.len()),
                    );
                    ui.label(
                        RichText::new(step.label())
                            .font(semibold(22.0))
                            .color(p.text),
                    );
                    ui.label(RichText::new(step.summary()).small().color(p.muted));
                } else {
                    widgets::overline(ui, "Tool");
                    ui.label(RichText::new("GPU").font(semibold(22.0)).color(p.text));
                }
                ui.add_space(2.0);
                let ctx = ui.ctx().clone();
                match self.tool {
                    Tool::Calibrate => {
                        let ask = self.cal.controls(ui, self.cal_run.is_some());
                        match ask {
                            PipeAsk::Run => self.start_calibrate(&ctx),
                            PipeAsk::Browse(t) => self.browse(&ctx, t),
                            PipeAsk::Nothing => {}
                        }
                    }
                    Tool::Register => {
                        let ask = self.reg.controls(ui, self.reg_run.is_some());
                        match ask {
                            PipeAsk::Run => self.start_register(&ctx),
                            PipeAsk::Browse(t) => self.browse(&ctx, t),
                            PipeAsk::Nothing => {}
                        }
                    }
                    Tool::Stack | Tool::Drizzle | Tool::Science => {
                        self.tool_chips(
                            ui,
                            &[
                                (Tool::Stack, "Mean"),
                                (Tool::Drizzle, "Drizzle"),
                                (Tool::Science, "Science"),
                            ],
                        );
                        if self.tool == Tool::Science {
                            self.science_panel(ui);
                        } else if self.tool == Tool::Stack {
                            let ask = self.stack.controls(ui, self.stack_run.is_some());
                            match ask {
                                PipeAsk::Run => self.start_stack(&ctx),
                                PipeAsk::Browse(t) => self.browse(&ctx, t),
                                PipeAsk::Nothing => {}
                            }
                        } else {
                            self.drizzle_panel(ui);
                        }
                    }
                    Tool::Export => self.export_panel(ui),
                    Tool::Gpu => {
                        let running = self.gpu_run.is_some();
                        if self.gpu.controls(ui, running) == GpuAsk::Run {
                            self.start_gpu(&ctx);
                        }
                    }
                    Tool::Stretch
                    | Tool::Stars
                    | Tool::Solve
                    | Tool::Colour
                    | Tool::Optics
                    | Tool::Astrometry => {
                        self.process_panel(ui);
                    }
                }
            });
    }

    fn drizzle_panel(&mut self, ui: &mut Ui) {
        let running = self.drizzle_run.is_some();
        let ask = self.drizzle.controls(ui, running);
        if self.drizzle.result.is_some() {
            widgets::overline(ui, "Write");
            self.save_row(ui, Target::DrizzleOut);
        }
        let ctx = ui.ctx().clone();
        match ask {
            DrizzleAsk::Run => self.start_drizzle(&ctx),
            DrizzleAsk::Save => self.save_drizzle(&ctx),
            DrizzleAsk::Open => self.open_drizzle(),
            DrizzleAsk::Browse => self.browse(&ctx, Target::DrizzleFolder),
            DrizzleAsk::Nothing => {}
        }
    }

    fn export_panel(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.doc.is_none() {
            ui.label(RichText::new("Open or stack an image to export it.").color(p.muted));
            self.open_block(ui);
            return;
        }
        widgets::overline(ui, "Format");
        let before = self.export_format;
        ui.horizontal_wrapped(|ui| {
            for (f, label) in [(ExportFormat::Fits, "FITS"), (ExportFormat::Png, "PNG")] {
                if widgets::chip(ui, self.export_format == f, label).clicked() {
                    self.export_format = f;
                }
            }
        });
        if before != self.export_format {
            let mut path = PathBuf::from(self.export_text.trim());
            path.set_extension(match self.export_format {
                ExportFormat::Fits => "fits",
                ExportFormat::Png => "png",
            });
            self.export_text = path.to_string_lossy().into_owned();
        }
        ui.label(
            RichText::new(match self.export_format {
                ExportFormat::Fits => {
                    "The open image as 32-bit float FITS, with its header, unstretched."
                }
                ExportFormat::Png => {
                    "An 8-bit picture drawn with the stretch chosen in the Process step, for sharing."
                }
            })
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Write");
        self.save_row(ui, Target::ExportOut);
        ui.add_enabled_ui(self.job.is_none(), |ui| {
            if widgets::button(ui, "Export picture", Kind::Primary, 0.0)
                .on_hover_text("Write the picture to the file above")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_export(&ctx);
            }
        });
        if let Some(done) = &self.exported {
            ui.label(RichText::new(done).font(mono(12.5)).color(ok_colour()));
        }
    }

    fn process_panel(&mut self, ui: &mut Ui) {
        let p = palette();
        self.open_block(ui);
        ui.separator();
        self.tool_chips(
            ui,
            &[
                (Tool::Stretch, "Stretch"),
                (Tool::Stars, "Star removal"),
                (Tool::Solve, "Plate solve"),
                (Tool::Colour, "Colour"),
                (Tool::Optics, "Optics"),
                (Tool::Astrometry, "Astrometry"),
            ],
        );
        if self.doc.is_none() {
            ui.label(RichText::new("Open a FITS image to work on it.").color(p.muted));
            return;
        }
        let ctx = ui.ctx().clone();
        match self.tool {
            Tool::Stretch => {
                if self.stretch.controls(ui, self.measured.as_ref()) {
                    self.dirty = true;
                }
                ui.separator();
                widgets::overline(ui, "Write");
                self.save_row(ui, Target::StretchOut);
                ui.add_enabled_ui(self.job.is_none(), |ui| {
                    if widgets::button(ui, "Apply and save", Kind::Outline, 0.0)
                        .on_hover_text("Stretch the full image and write a new FITS file")
                        .clicked()
                    {
                        self.start_apply(&ctx);
                    }
                });
            }
            Tool::Solve => {
                let running = self.solve_run.is_some();
                let ask = self.solve.controls(ui, running);
                if self.solve.result.is_some() {
                    widgets::overline(ui, "Write");
                    self.save_row(ui, Target::SolveOut);
                }
                match ask {
                    SolveAsk::Run => self.start_solve(&ctx),
                    SolveAsk::Save => self.save_solve(&ctx),
                    SolveAsk::Catalogue => self.browse(&ctx, Target::Catalogue),
                    SolveAsk::Nothing => {}
                }
            }
            Tool::Optics => self.optics_panel(ui),
            Tool::Astrometry => self.astro_panel(ui),
            Tool::Colour => {
                let running = self.colour_run.is_some();
                let three = self.doc.as_ref().is_some_and(|d| d.full.planes.len() == 3);
                let ask = self.colour.controls(ui, three, running);
                if self.colour.result.is_some() {
                    widgets::overline(ui, "Write");
                    self.save_row(ui, Target::ColourOut);
                }
                match ask {
                    ColourAsk::Run => self.start_colour(&ctx),
                    ColourAsk::Redraw => self.dirty = true,
                    ColourAsk::Save => self.save_colour(&ctx),
                    ColourAsk::Nothing => {}
                }
            }
            _ => {
                let running = self.star_run.is_some();
                let ask = self.stars.controls(ui, true, running);
                if self.stars.result.is_some() {
                    widgets::overline(ui, "Write");
                    self.save_row(ui, Target::StarsOut);
                }
                match ask {
                    Ask::Run => self.start_stars(&ctx),
                    Ask::Redraw => self.dirty = true,
                    Ask::Save => self.save_stars(&ctx),
                    Ask::Nothing => {}
                }
            }
        }
    }

    /// The row above the picture: what is shown, the zoom and the histogram switch.
    fn viewer_toolbar(&mut self, ui: &mut Ui, picture: bool) {
        let p = palette();
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 34.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                if picture && let Some(t) = self.texture.clone() {
                    let size = t.size_vec2();
                    let logical = self.logical_size(size);
                    view::toolbar_buttons(ui, &mut self.view, logical, self.view_avail);
                    if widgets::chip(ui, !self.hide_hist, "Histogram")
                        .on_hover_text("Show or hide the histogram of what is drawn")
                        .clicked()
                    {
                        self.hide_hist = !self.hide_hist;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let stepped = self.step_preview().map(|(_, what)| what);
                        let what = match (&self.doc, stepped) {
                            (_, Some(what)) => what.to_owned(),
                            (Some(d), None) => d
                                .path
                                .file_name()
                                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                            (None, None) => String::new(),
                        };
                        let full = self
                            .doc
                            .as_ref()
                            .filter(|_| stepped.is_none())
                            .map_or_else(String::new, |d| {
                                format!("{} x {} px  -  ", d.full.width(), d.full.height())
                            });
                        ui.label(RichText::new(what).font(semibold(13.0)).color(p.text));
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{full}preview {} x {}",
                                    size.x as usize, size.y as usize
                                ))
                                .font(mono(12.0))
                                .color(p.faint),
                            )
                            .truncate(),
                        );
                    });
                }
            },
        );
    }

    /// The viewer: the toolbar, the picture with zoom and pan, and the histogram.
    pub fn viewer(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let p = palette();
        let ctx = ui.ctx().clone();
        self.handle_drops(&ctx);
        if self.tiles_pending {
            self.tiles_pending = false;
            if let Some(doc) = &self.doc {
                self.tiles.start(&ctx, &doc.full, PREVIEW_SIDE);
                if self.tiles.building() {
                    self.status = Some((
                        "Building the zoom levels for full-resolution viewing...".into(),
                        false,
                    ));
                }
            }
        }
        if self.dirty {
            self.dirty = false;
            self.refresh(&ctx);
        }
        let picture = !matches!(
            self.tool,
            Tool::Gpu | Tool::Science | Tool::Optics | Tool::Astrometry
        ) && self.texture.is_some()
            && self.review.is_none();
        self.viewer_toolbar(ui, picture);
        let mut act = Act::Nothing;
        let hist_h = if picture && !self.hide_hist && self.hist.is_some() {
            128.0
        } else {
            0.0
        };
        let frame_h =
            (ui.available_height() - hist_h - if hist_h > 0.0 { 8.0 } else { 0.0 }).max(80.0);
        ui.allocate_ui(vec2(ui.available_width(), frame_h), |ui| {
            Frame::new()
                .fill(p.ground)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(RADIUS_LARGE)
                .inner_margin(Margin::same(1))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    if let Some(r) = &mut self.review {
                        act = review::show(ui, r);
                    } else if self.tool == Tool::Gpu {
                        self.gpu_view(ui);
                    } else if self.tool == Tool::Science {
                        self.science.view(ui);
                    } else if self.tool == Tool::Optics && self.doc.is_some() {
                        self.optics.show(ui);
                    } else if self.tool == Tool::Astrometry && self.doc.is_some() {
                        self.astro.show(ui);
                    } else if picture && let Some(tex) = self.texture.clone() {
                        self.picture(ui, &tex);
                    } else {
                        let pick = start::show(ui, prefs, self.tool == Tool::Export);
                        self.carry_out(pick, prefs);
                    }
                    if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
                        drop_hint(ui);
                    }
                });
        });
        match act {
            Act::Nothing => {}
            Act::Accept => self.accept_review(&ctx),
            Act::Cancel => self.cancel_review(),
        }
        if hist_h > 0.0
            && let Some(h) = &self.hist
        {
            ui.add_space(8.0);
            Frame::new()
                .fill(p.panel)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(RADIUS_LARGE)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_min_size(vec2(ui.available_width(), hist_h - 18.0));
                    ui.horizontal(|ui| {
                        widgets::overline(ui, "Histogram, as drawn");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let c = h.bins.len().min(3);
                            let med = (0..c).map(|k| h.percentile(k, 0.5)).sum::<f32>() / c as f32;
                            ui.label(
                                RichText::new(format!("median level {:.0} percent", med * 100.0))
                                    .font(mono(12.0))
                                    .color(p.faint),
                            );
                        });
                    });
                    let rect = ui.available_rect_before_wrap();
                    ui.allocate_rect(rect, egui::Sense::hover());
                    view::draw_histogram(ui, rect, h);
                });
        }
    }

    /// The size the zoom is counted in: the open image's own pixels when the
    /// viewer shows it (so 100 % is one screen pixel per image pixel), else
    /// the size of the picture drawn.
    fn logical_size(&self, tex: egui::Vec2) -> egui::Vec2 {
        match &self.doc {
            Some(d) if self.step_preview().is_none() && self.tool != Tool::Drizzle => {
                vec2(d.full.width() as f32, d.full.height() as f32)
            }
            _ => tex,
        }
    }

    fn picture(&mut self, ui: &mut Ui, tex: &TextureHandle) {
        let p = palette();
        let logical = self.logical_size(tex.size_vec2());
        let (rect, clip) = view::show(ui, &mut self.view, tex.id(), logical);
        self.view_avail = clip.size();
        let Some(doc) = &self.doc else { return };
        let k = rect.width() / doc.full.width() as f32;
        let painter = ui.painter_at(clip);
        if self.tiled()
            && let Some(m) = &self.measured
        {
            let scene = Scene {
                rect,
                clip,
                full: (doc.full.width(), doc.full.height()),
                preview_ratio: tex.size_vec2().x / logical.x,
                tool: &self.stretch,
                measured: m,
                night: crate::theme::is_night(),
            };
            let missing = self.tiles.draw(ui.ctx(), &painter, &scene);
            if missing {
                ui.ctx().request_repaint();
            }
        }
        self.reading = None;
        self.pixel_readout(ui, rect, clip, k);
        if let Some(cal) = &self.colour.result
            && self.tool == Tool::Colour
        {
            for s in cal.stars.iter().filter(|s| s.used) {
                let c = rect.min + vec2(s.x as f32 * k, s.y as f32 * k);
                painter.circle_stroke(c, 9.0, Stroke::new(1.5, p.accent_hover));
            }
        }
        if let Some(sol) = &self.solve.result
            && self.tool == Tool::Solve
        {
            // The matched stars, in image pixels, onto the drawn picture.
            for &(x, y) in &sol.matched_pixels {
                let c = rect.min + vec2(x as f32 * k, y as f32 * k);
                painter.circle_stroke(c, 9.0, Stroke::new(1.5, p.accent_hover));
            }
        }
    }

    /// The box with the pixel under the pointer, and an outline round the pixel
    /// once the zoom is high enough to see it.
    fn pixel_readout(&mut self, ui: &mut Ui, rect: egui::Rect, clip: egui::Rect, k: f32) {
        let p = palette();
        let Some(doc) = &self.doc else { return };
        let Some(at) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !clip.contains(at) || !rect.contains(at) {
            return;
        }
        let local = (at - rect.min) / k;
        let Some((x, y)) = readout::pixel_at(&doc.full, local.x, local.y) else {
            return;
        };
        let mut reading = readout::read(&doc.full, doc.wcs.as_ref(), x, y);
        if self.tiled()
            && let Some(m) = &self.measured
        {
            // The stretch of this one pixel, the same maths the tiles use.
            let one = valerian_core::pyramid::crop(
                &doc.full,
                valerian_core::pyramid::PixelRect { x, y, w: 1, h: 1 },
            );
            reading.shown = self
                .stretch
                .render(&one, m)
                .ok()
                .map(|s| s.planes.iter().map(|pl| pl.data[0]).collect());
        }
        let painter = ui.painter_at(clip);
        if k >= 4.0 {
            let min = rect.min + vec2(x as f32, y as f32) * k;
            painter.rect_stroke(
                egui::Rect::from_min_size(min, vec2(k, k)),
                0.0,
                Stroke::new(1.5, p.accent_hover),
                egui::StrokeKind::Outside,
            );
        }
        let text = readout::lines(&reading).join("\n");
        let galley = painter.layout(text, mono(12.0), p.text, 420.0);
        let pad = vec2(10.0, 6.0);
        let boxr = egui::Rect::from_min_size(
            clip.left_bottom() + vec2(10.0, -10.0 - galley.size().y - 2.0 * pad.y),
            galley.size() + 2.0 * pad,
        );
        painter.rect(
            boxr,
            6.0,
            p.panel.gamma_multiply(0.92),
            Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
        painter.galley(boxr.min + pad, galley, p.text);
        self.reading = Some(reading);
    }

    fn gpu_view(&self, ui: &mut Ui) {
        let p = palette();
        match &self.gpu.result {
            Some(run) => gpu_tool::chart(ui, run),
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.4);
                    ui.label(
                        RichText::new("No benchmark yet")
                            .font(semibold(18.0))
                            .color(p.text),
                    );
                    ui.label(
                        RichText::new(
                            "Choose an adapter and press Run benchmark to time the kernels.",
                        )
                        .color(p.muted),
                    );
                });
            }
        }
    }

    /// Do what the start screen was asked for.
    fn carry_out(&mut self, pick: Pick, prefs: &mut Prefs) {
        match pick {
            Pick::Nothing => {}
            Pick::Session(set) => self.apply_folders(&set),
            Pick::Image(f) => self.open(&f),
            Pick::ForgetSession(lights) => {
                prefs.forget_session(&lights);
                self.status = Some(("Forgot the session".into(), false));
            }
            Pick::ForgetImage(f) => {
                prefs.forget_recent(&f);
                self.status = Some((format!("Forgot {}", f.display()), false));
            }
            Pick::ClearAll => {
                prefs.clear_recents();
                self.status = Some(("Cleared the recent sessions and images".into(), false));
            }
        }
    }
}

#[cfg(test)]
mod default_output_tests {
    use super::*;

    #[test]
    fn a_folder_gets_a_sibling_output_as_written() {
        assert_eq!(
            beside("/data/m31/lights", "calibrated"),
            "/data/m31/calibrated"
        );
        assert_eq!(
            beside("/data/m31/lights/", "stack.fits"),
            "/data/m31/stack.fits"
        );
        assert_eq!(beside("lights", "calibrated"), "calibrated");
        assert_eq!(beside("a/lights", "registered"), "a/registered");
    }

    #[test]
    fn the_default_output_keeps_the_folder_as_written() {
        assert_eq!(
            default_output(Path::new("/data/NGC_demo.fits"), "stretched"),
            "/data/NGC_demo_stretched.fits"
        );
        assert_eq!(
            default_output(Path::new("m31.fit"), "solved"),
            "m31_solved.fits"
        );
        assert_eq!(
            default_output(Path::new("a/b/c.fits"), "starless"),
            "a/b/c_starless.fits"
        );
        assert_eq!(default_output(Path::new(""), "x"), "image_x.fits");
    }
}
