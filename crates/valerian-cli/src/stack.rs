// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian stack`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use valerian_core::combine::Rejection;
use valerian_core::debayer::{Method, demosaic_image};
use valerian_core::error::Error as CoreError;
use valerian_core::fits::{Header, Image};
use valerian_core::register::RegisterOptions;
use valerian_core::stack::{FrameReport, Reference, StackOptions, Stacked, Status};
use valerian_core::stack_stream::{FrameSource, StreamOptions, stack_streaming};
use valerian_core::stars::DetectOptions;

use crate::StackArgs;
use crate::backend;
use crate::files::{any_bayer, collect, demosaic_all, load, load_quiet, write};

/// Frames read from files one at a time, demosaiced as `valerian stack` does.
struct Files {
    paths: Vec<PathBuf>,
    method: Option<Method>,
}

impl FrameSource for Files {
    fn len(&self) -> usize {
        self.paths.len()
    }

    fn load(&self, i: usize) -> valerian_core::error::Result<Image> {
        let p = &self.paths[i];
        let img = valerian_core::fits::read(p)
            .map_err(|e| CoreError::invalid(format!("cannot read {}: {e}", p.display())))?;
        match self.method {
            Some(m) if img.planes.len() == 1 && img.plane().bayer.is_some() => {
                demosaic_image(&img, m)
            }
            _ => Ok(img),
        }
    }
}

/// One line per frame.
#[must_use]
pub fn table(reports: &[FrameReport], names: &[String]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:>4}  {:<28} {:>6} {:>6} {:>6} {:>8} {:>8} {:>8} {:>6} {:>6} {:>7}  status",
        "#",
        "frame",
        "stars",
        "FWHM",
        "ellip",
        "sky",
        "noise",
        "flux x",
        "rms px",
        "weight",
        "reject"
    );
    for r in reports {
        let name = names.get(r.index).map_or("", String::as_str);
        let status = match &r.status {
            Status::Reference => "reference".to_owned(),
            Status::Stacked => "stacked".to_owned(),
            Status::Skipped(why) => format!("skipped: {why}"),
        };
        let used = matches!(r.status, Status::Reference | Status::Stacked);
        let _ = writeln!(
            s,
            "{:>4}  {:<28} {:>6} {:>6.2} {:>6.3} {:>8.1} {:>8.2} {:>8.4} {:>6.3} {:>6.2} {:>6.2}%  {status}",
            r.index,
            crate::files::elide(name, 28),
            r.stars,
            r.fwhm,
            r.ellipticity,
            r.sky,
            r.noise,
            r.flux_scale,
            r.registration_rms,
            r.weight,
            if used { 100.0 * r.rejected } else { 0.0 },
        );
    }
    s
}

/// Frames graded per read, so grading holds a few raw frames, not the stack.
const GRADE_CHUNK: usize = 64;

/// Grade the raw frames and drop the ones that fail, saying why. Returns the
/// survivors with their scores (1 for a frame that could not be graded).
fn grade_filter(
    a: &StackArgs,
    files: &[PathBuf],
    names: &[String],
) -> Result<(Vec<PathBuf>, Vec<String>, Vec<f32>)> {
    let policy = a.grade.policy();
    let (mut kept_files, mut kept_names, mut scores) = (Vec::new(), Vec::new(), Vec::new());
    for (fchunk, nchunk) in files.chunks(GRADE_CHUNK).zip(names.chunks(GRADE_CHUNK)) {
        let frames = load(fchunk)?;
        let grades = crate::grade::grade_frames(&a.grade, nchunk, &frames)?;
        drop(frames);
        for ((file, name), g) in fchunk.iter().zip(nchunk).zip(grades) {
            let why = g.map(|g| policy.reasons(&g)).unwrap_or_default();
            if why.is_empty() {
                kept_files.push(file.clone());
                kept_names.push(name.clone());
                scores.push(g.map_or(1.0, |g| g.score));
            } else {
                eprintln!("grade: {name} skipped: {}", why.join("; "));
            }
        }
    }
    if kept_files.len() < 2 {
        bail!("the grader leaves fewer than two frames to stack");
    }
    eprintln!("grade: {} frames kept", kept_files.len());
    Ok((kept_files, kept_names, scores))
}

#[allow(clippy::too_many_lines)]
pub fn run(a: &StackArgs) -> Result<()> {
    let mut files = collect(&a.frames)?;
    if let Some(table) = &a.approve {
        files = crate::science::apply_approvals(files, table)?;
    }
    let names = crate::science::names_of(&files);
    let (files, names, scores) = if a.grade.grade {
        grade_filter(a, &files, &names)?
    } else {
        (files, names, Vec::new())
    };
    let science_run = a.science_report.is_some();
    // The method is decided from the first frame; the rest are read later, one at a time.
    let probe = load_quiet(&files[..1])?;
    let method = a.debayer.method(any_bayer(&probe));
    drop(probe);
    let n = files.len();
    let opts = StackOptions {
        detect: DetectOptions {
            threshold: a.threshold,
            ..DetectOptions::default()
        },
        register: RegisterOptions {
            model: a.model.into(),
            ..RegisterOptions::default()
        },
        interpolation: a.interpolation.into(),
        rejection: match a.rejection {
            // Limits given: apply them to the scheme auto would pick.
            crate::RejectionArg::Auto if a.low.is_none() && a.high.is_none() => None,
            other => Some(other.resolve(a.low, a.high, n)),
        },
        weights: a.weights.into(),
        normalisation: a.normalisation.into(),
        reference: a.reference.map_or(Reference::Best, Reference::Index),
        min_stars: a.min_stars,
        max_fwhm_ratio: a.max_fwhm_ratio,
        frame_scores: if a.grade_weights { scores } else { Vec::new() },
        mosaic: a.mosaic.then_some(valerian_core::mosaic::MosaicOptions {
            feather: a.mosaic_feather.max(0.0),
            match_levels: !a.no_mosaic_level_match,
        }),
        ..StackOptions::default()
    };
    if (a.low.is_some() || a.high.is_some())
        && !matches!(
            opts.rejection,
            None | Some(
                Rejection::Sigma { .. }
                    | Rejection::Winsorised { .. }
                    | Rejection::LinearFit { .. }
            )
        )
    {
        eprintln!("note: --low and --high have no effect on this rejection scheme");
    }
    let limits = valerian_science::subframe::Limits::from(&a.limits);
    let limited = limits != valerian_science::subframe::Limits::default();
    let t = Instant::now();
    // Per-frame epochs and noise, for the science report and for judging the limits.
    let measured = (science_run || limited)
        .then(|| measure(&files, method))
        .transpose()?;
    let (mut result, mut noise) = stack_once(a, &files, method, &opts, science_run)?;
    let mut names = names;
    let mut analysis = None;
    if let (true, Some((epochs, noises))) = (limited, &measured) {
        // The limits decide which frames are combined, not only what the report
        // says: judge every frame from the first pass, then stack again without
        // the rejected ones (the reference may change).
        let metrics = valerian_science::subframe::metrics_from_reports(
            &result.frames,
            &names,
            epochs,
            noises,
        );
        let judged = valerian_science::subframe::evaluate(&metrics, &limits);
        let keep: Vec<usize> = judged
            .frames
            .iter()
            .filter(|r| r.approved)
            .map(|r| r.metrics.index)
            .collect();
        let dropped = judged
            .frames
            .iter()
            .filter(|r| !r.approved && r.metrics.registered)
            .count();
        if dropped > 0 {
            if keep.len() < 2 {
                bail!(
                    "the limits leave {} of {n} frames to stack; need at least two",
                    keep.len()
                );
            }
            eprintln!(
                "limits: {dropped} of {n} frames rejected, stacking the other {}",
                keep.len()
            );
            for r in judged.frames.iter().filter(|r| !r.approved) {
                eprintln!("  {}: {}", r.metrics.name, r.reasons.join("; "));
            }
            let kept_files: Vec<PathBuf> = keep.iter().map(|&i| files[i].clone()).collect();
            let mut again = opts.clone();
            if !again.frame_scores.is_empty() {
                again.frame_scores = keep.iter().map(|&i| again.frame_scores[i]).collect();
            }
            if let Reference::Index(r) = again.reference {
                again.reference = keep
                    .iter()
                    .position(|&i| i == r)
                    .map_or(Reference::Best, Reference::Index);
            }
            names = keep.iter().map(|&i| names[i].clone()).collect();
            (result, noise) = stack_once(a, &kept_files, method, &again, science_run)?;
        }
        analysis = Some(judged);
    }
    if a.mosaic && a.mosaic_min_coverage > 1 {
        let (cols, rows) =
            valerian_core::mosaic::crop_to_coverage(&mut result, a.mosaic_min_coverage)?;
        eprintln!(
            "mosaic: cut to {cols} x {rows} pixels where at least {} frames reach",
            a.mosaic_min_coverage
        );
    }
    let text = table(&result.frames, &names);
    print!("{text}");
    println!(
        "{} of {n} frames stacked with {:?}, reference frame {} ({}), {:.1?}",
        result.used(),
        result.rejection,
        result.reference,
        names[result.reference],
        t.elapsed()
    );
    if !a.mosaic {
        let failed = result
            .frames
            .iter()
            .filter(|f| matches!(&f.status, Status::Skipped(why) if why.contains("match")))
            .count();
        if failed >= 5 && failed * 10 >= n {
            eprintln!(
                "note: {failed} of {n} frames did not register to the reference. If they are other panes of a mosaic, stack with --mosaic to build one canvas."
            );
        }
    }
    write(&a.output, &result.image)?;
    println!("wrote {}", a.output.display());
    write_extras(a, &result, &text)?;
    write_science(
        a,
        &result,
        &names,
        noise.as_ref(),
        measured.as_ref(),
        analysis,
    )?;
    Ok(())
}

/// Write the `--science-report` files, when asked for. `analysis` is the verdict already
/// made for the frame limits, if any; otherwise the frames are judged here.
fn write_science(
    a: &StackArgs,
    result: &Stacked,
    names: &[String],
    noise: Option<&valerian_science::integration::NoiseCurve>,
    measured: Option<&(Vec<Option<f64>>, Vec<f64>)>,
    analysis: Option<valerian_science::subframe::Analysis>,
) -> Result<()> {
    let (Some(dir), Some((epochs, noises))) = (&a.science_report, measured) else {
        return Ok(());
    };
    let analysis = analysis.unwrap_or_else(|| {
        let metrics =
            valerian_science::subframe::metrics_from_reports(&result.frames, names, epochs, noises);
        valerian_science::subframe::evaluate(&metrics, &(&a.limits).into())
    });
    crate::science::write_subframe_report(dir, &analysis)?;
    crate::science::write_integration_report(dir, result, noise)?;
    Ok(())
}

/// Frames measured per read, so measuring holds a few frames, not the stack.
const MEASURE_CHUNK: usize = 64;

/// Epoch and noise of every frame (after demosaicing, as the stack sees them).
fn measure(files: &[PathBuf], method: Option<Method>) -> Result<(Vec<Option<f64>>, Vec<f64>)> {
    let (mut epochs, mut noises) = (Vec::new(), Vec::new());
    for chunk in files.chunks(MEASURE_CHUNK) {
        let frames = demosaic_all(load(chunk)?, method)?;
        epochs.extend(frames.iter().map(valerian_science::subframe::epoch_of));
        noises.extend(frames.iter().map(valerian_science::subframe::noise_of));
    }
    Ok((epochs, noises))
}

/// Register and combine `files` once: on the CPU with the maps when a science report is
/// wanted (it holds every frame), streamed otherwise.
fn stack_once(
    a: &StackArgs,
    files: &[PathBuf],
    method: Option<Method>,
    opts: &StackOptions,
    science: bool,
) -> Result<(Stacked, Option<valerian_science::integration::NoiseCurve>)> {
    if science {
        let frames = demosaic_all(load(files)?, method)?;
        eprintln!("compute: CPU (the rejection maps and the noise curve need the CPU combiner)");
        crate::science::stack_for_science(frames, opts).context("stacking failed")
    } else {
        let engine = backend::open(a.backend, true)?;
        let source = Files {
            paths: files.to_vec(),
            method,
        };
        let stream = StreamOptions {
            budget: a.memory.saturating_mul(1 << 20),
            scratch: a.scratch_dir.clone(),
        };
        let stacked = stack_streaming(&source, opts, &stream, &*engine.compute, &mut |line| {
            eprintln!("{line}");
        })
        .context("stacking failed")?;
        backend::report(&engine);
        Ok((stacked, None))
    }
}

fn write_extras(a: &StackArgs, result: &Stacked, text: &str) -> Result<()> {
    if let Some(p) = &a.coverage {
        let img = Image::mono(result.coverage.clone(), Header::new());
        write(p, &img)?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = &a.report {
        std::fs::write(p, text).with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
