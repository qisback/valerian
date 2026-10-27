// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Science panel: SubframeSelector-style analysis of a folder of frames.
//!
//! The frames are registered and measured on a worker thread; the panel then
//! shows the per-frame plots in the viewer, the limits as editable boxes, and
//! the approve/reject table, where a verdict can be overridden by hand. The
//! approved frames feed the Stack step, and the whole analysis can be written
//! out as plots, CSV, JSON and a self-contained HTML report.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Checkbox, ColorImage, DragValue, RichText, ScrollArea, TextureHandle, TextureOptions, Ui,
};
use valerian_core::stack::StackOptions;
use valerian_core::stars::DetectOptions;
use valerian_science::grader::{Grade, Grader, Policy};
use valerian_science::histogram;
use valerian_science::integration::{self, MapKind, NoiseCurve};
use valerian_science::photometry::{self, LightCurve};
use valerian_science::report::ReportDir;
use valerian_science::subframe::{self, Analysis, FrameMetrics, Limits, SHEET, XAxis};

use crate::dialogs::Target;
use crate::pipeline::{Progress, demosaic_all, frames_in, read_all};
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Measure the frames.
    Run,
    /// Choose a folder with the dialog.
    Browse(Target),
    /// The limits changed: judge the frames again.
    Apply,
    /// A verdict was changed by hand: redraw the plots.
    Redraw,
    /// Write the plots, CSV, JSON and report.
    Save,
    /// Leave the rejected frames out of the next stack.
    UseForStacking,
    /// Stack on the CPU for the rejection, count and weight maps and the noise curve.
    Integrate,
    /// Measure one star across the frames.
    LightCurve,
}

/// The picture the viewer shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SciView {
    /// The per-frame plots.
    #[default]
    Subframes,
    /// Samples rejected low.
    RejectionLow,
    /// Samples rejected high.
    RejectionHigh,
    /// Samples used per pixel.
    Count,
    /// Summed weight per pixel.
    Weight,
    /// Noise against the number of frames.
    Noise,
    /// Histograms of the stack.
    Histogram,
    /// Light curve of one star.
    LightCurve,
}

impl SciView {
    const ALL: [Self; 8] = [
        Self::Subframes,
        Self::RejectionLow,
        Self::RejectionHigh,
        Self::Count,
        Self::Weight,
        Self::Noise,
        Self::Histogram,
        Self::LightCurve,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Subframes => "Subframes",
            Self::RejectionLow => "Rejected low",
            Self::RejectionHigh => "Rejected high",
            Self::Count => "Pixel count",
            Self::Weight => "Weight",
            Self::Noise => "Noise",
            Self::Histogram => "Histogram",
            Self::LightCurve => "Light curve",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// A drawn plot.
#[derive(Clone)]
pub struct Picture {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

/// A measured light curve and its picture.
#[derive(Clone)]
pub struct LightRun {
    /// The curve.
    pub curve: LightCurve,
    picture: Picture,
}

/// What the integration run drew.
#[derive(Clone)]
pub struct IntegrationRun {
    /// Frames in the stack.
    pub used: usize,
    /// Noise against frames.
    pub noise: Option<NoiseCurve>,
    pictures: Vec<Option<Picture>>,
}

/// A limit that can be switched off.
#[derive(Clone, Copy, Debug)]
pub struct Opt {
    /// Whether the limit applies.
    pub on: bool,
    /// Its value.
    pub value: f64,
}

impl Opt {
    fn get(self) -> Option<f64> {
        self.on.then_some(self.value)
    }
}

/// The limits as the panel edits them.
#[derive(Clone, Debug)]
pub struct LimitEdit {
    /// Largest FWHM, pixels.
    pub fwhm: Opt,
    /// Largest eccentricity.
    pub ellipticity: Opt,
    /// Fewest stars.
    pub stars: Opt,
    /// Most noise, ADU.
    pub noise: Opt,
    /// Lowest SNR weight.
    pub weight: Opt,
    /// Largest registration residual, pixels.
    pub residual: Opt,
    /// Robust sigmas from the median.
    pub sigma: Opt,
}

impl Default for LimitEdit {
    fn default() -> Self {
        let off = |value| Opt { on: false, value };
        Self {
            fwhm: off(4.0),
            ellipticity: off(0.5),
            stars: off(50.0),
            noise: off(100.0),
            weight: off(0.5),
            residual: off(1.0),
            sigma: Opt {
                on: true,
                value: 3.0,
            },
        }
    }
}

impl LimitEdit {
    /// The limits to judge by.
    #[must_use]
    pub fn limits(&self) -> Limits {
        Limits {
            max_fwhm_px: self.fwhm.get(),
            max_ellipticity: self.ellipticity.get(),
            min_stars: self.stars.get().map(|v| v.max(0.0) as usize),
            max_noise_adu: self.noise.get(),
            min_snr_weight: self.weight.get(),
            max_registration_rms_px: self.residual.get(),
            outlier_sigma: self.sigma.get(),
        }
    }
}

/// What a measuring run found.
#[derive(Clone, Debug)]
pub struct ScienceRun {
    /// The folder measured.
    pub folder: String,
    /// The measurements, which verdicts are judged from.
    pub metrics: Vec<FrameMetrics>,
    /// The grader's verdict per frame; empty when grading was off.
    pub grades: Vec<Option<Grade>>,
}

/// The Science panel.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches of one panel"
)]
pub struct ScienceTool {
    /// Folder of frames.
    pub folder: String,
    /// Where the report is written.
    pub report: String,
    /// The limits.
    pub limits: LimitEdit,
    /// Hours on the horizontal axis instead of frame number.
    pub time_axis: bool,
    /// Grade raw Seestar subs with the neural-network model on the next run.
    pub grade: bool,
    /// Reject frames graded below this, 0 to 1.
    pub min_grade: f64,
    /// An ONNX model file; empty for the built-in one.
    pub grader_model: String,
    /// The last run.
    pub result: Option<ScienceRun>,
    /// The verdicts, which a click can override.
    pub analysis: Option<Analysis>,
    /// The last integration run.
    pub integration: Option<IntegrationRun>,
    /// The last light curve.
    pub light: Option<LightRun>,
    /// The star to measure, pixels in the reference frame.
    pub star: (f64, f64),
    /// Measure the brightest unsaturated star instead of `star`.
    pub auto_star: bool,
    /// Aperture radius in units of the FWHM.
    pub aperture: f64,
    /// The picture shown.
    pub view: SciView,
    texture: Option<TextureHandle>,
    stale: bool,
    textures: Vec<Option<TextureHandle>>,
}

/// The last part of a name, which is where frames differ.
fn tail(name: &str, n: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= n {
        name.to_owned()
    } else {
        format!("...{}", chars[chars.len() - n..].iter().collect::<String>())
    }
}

impl Default for ScienceTool {
    fn default() -> Self {
        Self {
            folder: String::new(),
            report: String::new(),
            limits: LimitEdit::default(),
            time_axis: false,
            grade: false,
            min_grade: 0.5,
            grader_model: String::new(),
            result: None,
            analysis: None,
            integration: None,
            light: None,
            star: (0.0, 0.0),
            auto_star: true,
            aperture: 2.0,
            view: SciView::default(),
            texture: None,
            stale: false,
            textures: Vec::new(),
        }
    }
}

impl ScienceTool {
    /// A tool that holds only the folder, to move into the worker thread.
    #[must_use]
    pub fn for_run(folder: &str) -> Self {
        Self {
            folder: folder.to_owned(),
            ..Self::default()
        }
    }

    /// A copy of the folder and the grading settings, for the measuring thread.
    #[must_use]
    pub fn for_measure(&self) -> Self {
        Self {
            folder: self.folder.clone(),
            grade: self.grade,
            grader_model: self.grader_model.clone(),
            ..Self::default()
        }
    }

    /// A copy of the folder and the photometry settings, for the worker thread.
    #[must_use]
    pub fn for_photometry(&self) -> Self {
        Self {
            folder: self.folder.clone(),
            star: self.star,
            auto_star: self.auto_star,
            aperture: self.aperture,
            ..Self::default()
        }
    }

    /// Measure one star across the frames. Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read, too few frames register, or the star cannot be measured.
    pub fn run_light_curve(&self, progress: &Progress) -> Result<LightRun, String> {
        let files = frames_in(&self.folder)?;
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            })
            .collect();
        let frames = demosaic_all(read_all(&files, progress)?, progress)?;
        let opts = StackOptions {
            cancel: progress.token(),
            ..StackOptions::default()
        };
        progress.say("Registering the frames and measuring the star...");
        let options = photometry::Options {
            aperture: self.aperture,
            comparison: 10,
        };
        let target = (!self.auto_star).then_some(self.star);
        let curve = photometry::light_curve(&frames, &names, target, &options, &opts, &mut |l| {
            progress.say(l);
        })
        .map_err(|e| e.to_string())?;
        progress.check()?;
        let rgb = photometry::plot_rgb(&curve).map_err(|e| e.to_string())?;
        Ok(LightRun {
            curve,
            picture: Picture {
                width: photometry::SIZE.0 as usize,
                height: photometry::SIZE.1 as usize,
                rgb,
            },
        })
    }

    /// Take a finished light curve.
    pub fn accept_light(&mut self, run: LightRun) {
        self.textures = vec![None; SciView::ALL.len()];
        self.star = run.curve.reference_xy;
        self.light = Some(run);
        self.view = SciView::LightCurve;
    }

    /// Register and measure the frames. Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read, or too few frames register.
    pub fn run(&self, progress: &Progress) -> Result<ScienceRun, String> {
        let files = frames_in(&self.folder)?;
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            })
            .collect();
        let raw = read_all(&files, progress)?;
        let grades = if self.grade {
            self.grade_frames(&raw, &names, progress)?
        } else {
            Vec::new()
        };
        let frames = demosaic_all(raw, progress)?;
        let opts = StackOptions {
            cancel: progress.token(),
            detect: DetectOptions::default(),
            ..StackOptions::default()
        };
        progress.say("Registering and measuring the frames...");
        let metrics = subframe::measure(&frames, &names, &opts, &mut |l| {
            progress.say(l);
        })
        .map_err(|e| e.to_string())?;
        progress.check()?;
        Ok(ScienceRun {
            folder: self.folder.clone(),
            metrics,
            grades,
        })
    }

    /// Grade the raw frames; a frame the model cannot judge gets no grade.
    fn grade_frames(
        &self,
        raw: &[valerian_core::fits::Image],
        names: &[String],
        progress: &Progress,
    ) -> Result<Vec<Option<Grade>>, String> {
        progress.say("Grading the frames with the Seestar model...");
        let path = self.grader_model.trim();
        let model = if path.is_empty() {
            Grader::built_in()
        } else {
            Grader::from_path(Path::new(path))
        }
        .map_err(|e| format!("Cannot grade: {e}"))?;
        let graded: Vec<Option<Grade>> = model
            .grade_all(raw)
            .into_iter()
            .zip(names)
            .map(|(r, name)| match r {
                Ok(g) => Some(g),
                Err(why) => {
                    progress.say(&format!("{name} not graded: {why}"));
                    None
                }
            })
            .collect();
        progress.check()?;
        Ok(graded)
    }

    /// Judge the measurements against the limits and, if frames were graded, the grade limit.
    fn judge(&self, run: &ScienceRun) -> Analysis {
        let mut a = subframe::evaluate(&run.metrics, &self.limits.limits());
        if !run.grades.is_empty() {
            let policy = Policy {
                min_score: Some(self.min_grade as f32),
                max_defect: Vec::new(),
            };
            subframe::apply_grades(&mut a, &run.grades, &policy);
        }
        a
    }

    /// Stack the folder on the CPU and draw the maps, the noise curve and the histograms.
    /// Runs on the worker thread.
    ///
    /// # Errors
    /// A folder or frame cannot be read, or too few frames register.
    pub fn run_integration(&self, progress: &Progress) -> Result<IntegrationRun, String> {
        let files = frames_in(&self.folder)?;
        let frames = demosaic_all(read_all(&files, progress)?, progress)?;
        let opts = StackOptions {
            cancel: progress.token(),
            ..StackOptions::default()
        };
        progress.say("Registering and stacking on the CPU for the maps...");
        let (stacked, noise) = integration::stack_with_maps(frames, &opts, &mut |l| {
            progress.say(l);
        })
        .map_err(|e| match e {
            valerian_core::Error::Cancelled => crate::pipeline::CANCELLED.to_owned(),
            e => format!("Stacking failed: {e}"),
        })?;
        progress.check()?;
        progress.say("Drawing the maps...");
        let maps = stacked.maps.as_ref().ok_or("The combiner kept no maps.")?;
        let rgb = |r: Result<(u32, u32, Vec<u8>), valerian_science::Error>| {
            r.ok().map(|(w, h, rgb)| Picture {
                width: w as usize,
                height: h as usize,
                rgb,
            })
        };
        let mut pictures = vec![None; SciView::ALL.len()];
        pictures[SciView::RejectionLow.index()] =
            rgb(integration::map_rgb(&maps.low, MapKind::RejectionLow));
        pictures[SciView::RejectionHigh.index()] =
            rgb(integration::map_rgb(&maps.high, MapKind::RejectionHigh));
        pictures[SciView::Count.index()] =
            rgb(integration::map_rgb(&stacked.coverage, MapKind::Count));
        pictures[SciView::Weight.index()] =
            rgb(integration::map_rgb(&maps.weight, MapKind::Weight));
        if let Some(c) = &noise {
            pictures[SciView::Noise.index()] = integration::noise_rgb(c).ok().map(|rgb| Picture {
                width: integration::NOISE_SIZE.0 as usize,
                height: integration::NOISE_SIZE.1 as usize,
                rgb,
            });
        }
        if let Ok(h) = histogram::histograms(&stacked.image, 256) {
            pictures[SciView::Histogram.index()] =
                histogram::plot_rgb(&h).ok().map(|rgb| Picture {
                    width: histogram::SIZE.0 as usize,
                    height: histogram::SIZE.1 as usize,
                    rgb,
                });
        }
        Ok(IntegrationRun {
            used: stacked.used(),
            noise,
            pictures,
        })
    }

    /// Take a finished integration run.
    pub fn accept_integration(&mut self, run: IntegrationRun) {
        self.textures = vec![None; SciView::ALL.len()];
        self.integration = Some(run);
        self.view = SciView::RejectionHigh;
    }

    /// Take a finished run: judge it and mark the plots for drawing.
    pub fn accept(&mut self, run: ScienceRun) {
        if self.report.trim().is_empty() {
            self.report = Path::new(self.folder.trim())
                .parent()
                .map_or_else(
                    || PathBuf::from("science-report"),
                    |p| p.join("science-report"),
                )
                .to_string_lossy()
                .into_owned();
        }
        self.analysis = Some(self.judge(&run));
        self.result = Some(run);
        self.stale = true;
    }

    /// Judge the measured frames again against the limits (hand overrides go).
    pub fn apply_limits(&mut self) {
        if let Some(r) = &self.result {
            self.analysis = Some(self.judge(r));
            self.stale = true;
        }
    }

    /// Mark the plots as needing a redraw.
    pub fn redraw(&mut self) {
        self.stale = true;
    }

    /// Frames that are rejected now.
    #[must_use]
    pub fn rejected(&self) -> Vec<String> {
        self.analysis.as_ref().map_or_else(Vec::new, |a| {
            a.frames
                .iter()
                .filter(|r| !r.approved)
                .map(|r| r.metrics.name.clone())
                .collect()
        })
    }

    /// Write the report folder: plots, CSV, JSON and report.html.
    ///
    /// # Errors
    /// The folder cannot be written or a plot cannot be drawn.
    pub fn save(&self) -> Result<PathBuf, String> {
        let a = self.analysis.as_ref().ok_or("Measure the frames first.")?;
        let dir = self.report.trim();
        if dir.is_empty() {
            return Err("Name a folder for the report.".into());
        }
        let mut report = ReportDir::open(Path::new(dir)).map_err(|e| e.to_string())?;
        subframe::write_report(&mut report, a).map_err(|e| e.to_string())?;
        Ok(Path::new(dir).join("report.html"))
    }

    fn limit_row(ui: &mut Ui, label: &str, o: &mut Opt, speed: f64, max_decimals: usize) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui.add(Checkbox::new(&mut o.on, label)).changed();
            ui.add_enabled_ui(o.on, |ui| {
                changed |= ui
                    .add(
                        DragValue::new(&mut o.value)
                            .speed(speed)
                            .max_decimals(max_decimals),
                    )
                    .changed();
            });
        });
        changed
    }

    /// The controls. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        crate::pipeline::hint(
            ui,
            "Registers the frames and measures each one: FWHM, eccentricity, star count, \
             background, noise, SNR weight and registration residual, plotted against frame \
             number and time. Frames beyond your limits are marked and can be left out of the \
             stack.",
        );
        ui.add_enabled_ui(!busy, |ui| {
            if let Some(t) = widgets::folder_row(
                ui,
                "Frames",
                &mut self.folder,
                "Folder of frames",
                Target::ScienceIn,
            ) {
                ask = Ask::Browse(t);
            }
            ui.add(Checkbox::new(
                &mut self.grade,
                "Grade with the Seestar model",
            ))
            .on_hover_text(
                "Optional, off by default. Score each raw Seestar S50 sub from 0 to 1                      with a small neural network and reject the poor ones. It did not beat                      the classical limits on stacks; the dew output is unreliable. Other                      frames are not graded.",
            );
            if self.grade {
                ui.horizontal(|ui| {
                    ui.label("Reject below");
                    if ui
                        .add(
                            DragValue::new(&mut self.min_grade)
                                .range(0.0..=1.0)
                                .speed(0.01)
                                .max_decimals(2),
                        )
                        .on_hover_text("Frames that score less than this are rejected")
                        .changed()
                        && self.analysis.is_some()
                    {
                        ask = Ask::Apply;
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.grader_model)
                        .hint_text("Model file (.onnx); empty for the built-in one")
                        .desired_width(f32::INFINITY),
                );
            }
            if widgets::button(
                ui,
                if busy {
                    "Measuring..."
                } else {
                    "Measure frames"
                },
                Kind::Primary,
                0.0,
            )
            .on_hover_text("Register the frames to the sharpest and measure every one")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        ui.add_enabled_ui(!busy, |ui| {
            if widgets::button(ui, "Stack for the maps", Kind::Outline, 0.0)
                .on_hover_text(
                    "Stack the frames on the CPU and show where samples were rejected, how many \
                     frames and how much weight each pixel got, the noise against the square \
                     root of the number of frames, and the histograms",
                )
                .clicked()
            {
                ask = Ask::Integrate;
            }
        });
        if let Some(r) = &self.integration {
            ui.label(
                RichText::new(format!("{} frames stacked.", r.used))
                    .font(mono(12.0))
                    .color(p.text),
            );
            if let Some(c) = &r.noise {
                ui.label(
                    RichText::new(format!(
                        "Noise: one frame {:.1} ADU; {} frames per half {:.1}; reduction {:.2} of the {:.2} the square root promises.",
                        c.sigma1,
                        c.points.last().map_or(0, |q| q.frames),
                        c.points.last().map_or(f64::NAN, |q| q.noise),
                        c.reduction(),
                        c.theoretical()
                    ))
                    .small()
                    .color(p.muted),
                );
            }
        }
        widgets::overline(ui, "Light curve of a star");
        ui.add_enabled_ui(!busy, |ui| {
            ui.add(Checkbox::new(
                &mut self.auto_star,
                "The brightest unsaturated star",
            ));
            ui.add_enabled_ui(!self.auto_star, |ui| {
                ui.horizontal(|ui| {
                    ui.label("x");
                    ui.add(DragValue::new(&mut self.star.0).speed(1.0).max_decimals(1));
                    ui.label("y");
                    ui.add(DragValue::new(&mut self.star.1).speed(1.0).max_decimals(1));
                    ui.label("(reference frame)");
                });
            });
            ui.horizontal(|ui| {
                ui.label("Aperture (FWHM)");
                ui.add(
                    DragValue::new(&mut self.aperture)
                        .range(1.0..=5.0)
                        .speed(0.05),
                );
            });
            if widgets::button(ui, "Measure the star", Kind::Outline, 0.0)
                .on_hover_text(
                    "Aperture photometry of the star in every frame against comparison stars",
                )
                .clicked()
            {
                ask = Ask::LightCurve;
            }
        });
        if let Some(l) = &self.light {
            ui.label(
                RichText::new(format!(
                    "star at ({:.0}, {:.0}): scatter {:.3} mag, check star {:.3} mag, {} comparison stars",
                    l.curve.reference_xy.0,
                    l.curve.reference_xy.1,
                    l.curve.scatter_mag,
                    l.curve.check_scatter_mag,
                    l.curve.comparison.len()
                ))
                .small()
                .color(p.muted),
            );
        }
        if self.analysis.is_none() {
            return ask;
        }
        widgets::overline(ui, "Limits");
        let l = &mut self.limits;
        let mut changed = false;
        changed |= Self::limit_row(ui, "FWHM above (px)", &mut l.fwhm, 0.05, 2);
        changed |= Self::limit_row(ui, "Eccentricity above", &mut l.ellipticity, 0.01, 3);
        changed |= Self::limit_row(ui, "Stars below", &mut l.stars, 1.0, 0);
        changed |= Self::limit_row(ui, "Noise above (ADU)", &mut l.noise, 0.5, 1);
        changed |= Self::limit_row(ui, "SNR weight below", &mut l.weight, 0.01, 2);
        changed |= Self::limit_row(ui, "Residual above (px)", &mut l.residual, 0.01, 3);
        changed |= Self::limit_row(ui, "Outliers beyond (sigma)", &mut l.sigma, 0.1, 1);
        if changed {
            ask = Ask::Apply;
        }
        if let Some(a) = &mut self.analysis {
            widgets::overline(ui, "Frames");
            ui.label(
                RichText::new(format!(
                    "{} of {} approved. Untick a frame to leave it out, tick to keep it.",
                    a.approved(),
                    a.frames.len()
                ))
                .small()
                .color(p.muted),
            );
            let mut flipped = false;
            ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for r in &mut a.frames {
                    ui.horizontal(|ui| {
                        let label = format!("{:>3} {}", r.metrics.index, tail(&r.metrics.name, 16));
                        let resp = ui.add(Checkbox::new(&mut r.approved, label));
                        if resp.changed() {
                            flipped = true;
                        }
                        let tip = if r.reasons.is_empty() {
                            format!(
                                "FWHM {:.2} px, {} stars",
                                r.metrics.fwhm_px, r.metrics.stars
                            )
                        } else {
                            r.reasons.join("\n")
                        };
                        resp.on_hover_text(tip);
                        ui.label(
                            RichText::new(format!("{:.2}", r.metrics.fwhm_px))
                                .font(mono(12.0))
                                .color(if r.approved { p.faint } else { p.accent }),
                        );
                    });
                }
            });
            if flipped {
                ask = Ask::Redraw;
            }
        }
        ui.horizontal(|ui| {
            if widgets::button(ui, "Use for stacking", Kind::Outline, 0.0)
                .on_hover_text("Leave the rejected frames out of the next stack")
                .clicked()
            {
                ask = Ask::UseForStacking;
            }
        });
        widgets::overline(ui, "Report");
        ui.add(
            egui::TextEdit::singleline(&mut self.report)
                .hint_text("Folder for the plots, CSV, JSON and report.html")
                .desired_width(f32::INFINITY),
        );
        if widgets::button(ui, "Save report", Kind::Outline, 0.0)
            .on_hover_text("Write the plots, the approve/reject table and report.html")
            .clicked()
        {
            ask = Ask::Save;
        }
        ask
    }

    /// Draw the chosen picture into the viewer.
    pub fn view(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.analysis.is_none() && self.integration.is_none() && self.light.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(
                    RichText::new("No frames measured yet")
                        .size(18.0)
                        .color(p.text),
                );
                ui.label(
                    RichText::new("Choose a folder of frames and press Measure frames.")
                        .color(p.muted),
                );
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for v in SciView::ALL {
                let have = match v {
                    SciView::Subframes => self.analysis.is_some(),
                    SciView::LightCurve => self.light.is_some(),
                    _ => self
                        .integration
                        .as_ref()
                        .is_some_and(|r| r.pictures[v.index()].is_some()),
                };
                if have && widgets::chip(ui, self.view == v, v.label()).clicked() {
                    self.view = v;
                }
            }
        });
        if self.view == SciView::Subframes && self.analysis.is_some() {
            self.show_sheet(ui);
        } else {
            self.show_picture(ui);
        }
    }

    fn show_picture(&mut self, ui: &mut Ui) {
        let k = self.view.index();
        let pic = if self.view == SciView::LightCurve {
            self.light.as_ref().map(|l| &l.picture)
        } else {
            self.integration
                .as_ref()
                .and_then(|r| r.pictures.get(k))
                .and_then(Option::as_ref)
        };
        let Some(pic) = pic else {
            return;
        };
        if self.textures.len() != SciView::ALL.len() {
            self.textures = vec![None; SciView::ALL.len()];
        }
        if self.textures[k].is_none() {
            let img = ColorImage::from_rgb([pic.width, pic.height], &pic.rgb);
            self.textures[k] = Some(ui.ctx().load_texture(
                format!("science-{k}"),
                img,
                TextureOptions::LINEAR,
            ));
        }
        if let Some(tex) = &self.textures[k] {
            ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_size();
                    let scale = (avail.x / pic.width as f32)
                        .min(avail.y / pic.height as f32)
                        .max(0.1);
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(
                        pic.width as f32 * scale,
                        pic.height as f32 * scale,
                    )));
                });
        }
    }

    fn show_sheet(&mut self, ui: &mut Ui) {
        let Some(a) = &self.analysis else { return };
        let has_time = subframe::has_times(a);
        if !has_time {
            self.time_axis = false;
        }
        ui.horizontal(|ui| {
            if widgets::chip(ui, !self.time_axis, "By frame").clicked() && self.time_axis {
                self.time_axis = false;
                self.stale = true;
            }
            ui.add_enabled_ui(has_time, |ui| {
                if widgets::chip(ui, self.time_axis, "By time")
                    .on_hover_text("Needs DATE-OBS in every frame")
                    .clicked()
                    && !self.time_axis
                {
                    self.time_axis = true;
                    self.stale = true;
                }
            });
        });
        if self.stale || self.texture.is_none() {
            self.stale = false;
            let x = if self.time_axis {
                XAxis::Time
            } else {
                XAxis::Index
            };
            if let Ok(rgb) = subframe::plot_rgb(a, x) {
                let img = ColorImage::from_rgb([SHEET.0 as usize, SHEET.1 as usize], &rgb);
                self.texture = Some(ui.ctx().load_texture(
                    "science-sheet",
                    img,
                    TextureOptions::LINEAR,
                ));
            }
        }
        if let Some(tex) = &self.texture {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let w = ui.available_width();
                    let h = w * SHEET.1 as f32 / SHEET.0 as f32;
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(w, h)));
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_follow_the_switches() {
        let mut e = LimitEdit::default();
        let l = e.limits();
        assert_eq!(l.max_fwhm_px, None);
        assert_eq!(l.outlier_sigma, Some(3.0));
        e.fwhm.on = true;
        e.stars.on = true;
        e.stars.value = 80.0;
        let l = e.limits();
        assert_eq!((l.max_fwhm_px, l.min_stars), (Some(4.0), Some(80)));
    }

    #[test]
    fn a_name_keeps_its_distinguishing_tail() {
        assert_eq!(tail("short.fit", 16), "short.fit");
        assert_eq!(
            tail("Light_IC 5070_10.0s_LP_20260728-222713.fit", 16),
            "...60728-222713.fit"
        );
    }
}
