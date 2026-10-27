// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian subframes` and `valerian plot`: the science outputs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use valerian_core::Frame;
use valerian_core::drizzle::Drizzled;
use valerian_core::fits::{self, Header, Image};
use valerian_core::register::RegisterOptions;
use valerian_core::stack::{Reference, StackOptions, Stacked};
use valerian_core::stars::DetectOptions;
use valerian_science::histogram;
use valerian_science::integration::{self, MapKind, NoiseCurve};
use valerian_science::integration_report::{self, Input};
use valerian_science::report::ReportDir;
use valerian_science::subframe::{self, Analysis, Limits, XAxis};

use crate::files::{any_bayer, collect, demosaic_all, load};
use crate::{LimitArgs, PlotArgs, PlotKind, SubframesArgs, XAxisArg};

impl From<&LimitArgs> for Limits {
    fn from(a: &LimitArgs) -> Self {
        Self {
            max_fwhm_px: a.max_fwhm,
            max_ellipticity: a.max_ellipticity,
            min_stars: a.min_star_count,
            max_noise_adu: a.max_noise,
            min_snr_weight: a.min_snr_weight,
            max_registration_rms_px: a.max_residual,
            outlier_sigma: a.outlier_sigma,
        }
    }
}

/// File names, for the table.
pub fn names_of(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_owned()
        })
        .collect()
}

/// Print the verdicts, one line per frame.
fn print_table(a: &Analysis) {
    let graded = a.frames.iter().any(|r| r.grade.is_some());
    println!(
        "{:>4}  {:<30} {:>6} {:>6} {:>6} {:>8} {:>8} {:>7} {:>7}{}  verdict",
        "#",
        "frame",
        "stars",
        "FWHM",
        "ecc",
        "sky",
        "noise",
        "SNR wt",
        "reg px",
        if graded { "   grade" } else { "" }
    );
    for r in &a.frames {
        let m = &r.metrics;
        let grade = match (graded, r.grade) {
            (false, _) => String::new(),
            (true, Some(g)) => format!(" {:>7.2}", g.score),
            (true, None) => format!(" {:>7}", "-"),
        };
        println!(
            "{:>4}  {:<30} {:>6} {:>6.2} {:>6.3} {:>8.1} {:>8.2} {:>7.3} {:>7.3}{grade}  {}",
            m.index,
            crate::files::elide(&m.name, 30),
            m.stars,
            m.fwhm_px,
            m.ellipticity,
            m.sky_adu,
            m.noise_adu,
            m.snr_weight,
            m.registration_rms_px,
            if r.approved {
                "approved".to_owned()
            } else {
                format!("REJECTED: {}", r.reasons.join("; "))
            }
        );
    }
    println!("{} of {} frames approved", a.approved(), a.frames.len());
}

/// Register and measure frames already loaded.
fn measure(
    files: &[PathBuf],
    frames: &[Image],
    threshold: f32,
    model: crate::ModelArg,
    reference: Option<usize>,
) -> Result<Vec<subframe::FrameMetrics>> {
    let opts = StackOptions {
        detect: DetectOptions {
            threshold,
            ..DetectOptions::default()
        },
        register: RegisterOptions {
            model: model.into(),
            ..RegisterOptions::default()
        },
        reference: reference.map_or(Reference::Best, Reference::Index),
        ..StackOptions::default()
    };
    subframe::measure(frames, &names_of(files), &opts, &mut |l| eprintln!("{l}"))
        .context("measuring the frames failed")
}

/// Write the whole subframe report of an analysis into `dir`.
pub fn write_subframe_report(dir: &Path, a: &Analysis) -> Result<()> {
    let mut report = ReportDir::open(dir)?;
    let out = subframe::write_report(&mut report, a)?;
    for f in &out.files {
        println!("wrote {}", dir.join(f).display());
    }
    println!("wrote {}", dir.join("report.html").display());
    Ok(())
}

pub fn subframes(a: &SubframesArgs) -> Result<()> {
    let files = collect(&a.frames)?;
    let frames = load(&files)?;
    let grades = if a.grade.grade {
        Some(crate::grade::grade_frames(
            &a.grade,
            &names_of(&files),
            &frames,
        )?)
    } else {
        None
    };
    let method = a.debayer.method(any_bayer(&frames));
    let frames = demosaic_all(frames, method)?;
    let metrics = measure(&files, &frames, a.threshold, a.model, a.reference)?;
    let mut analysis = subframe::evaluate(&metrics, &Limits::from(&a.limits));
    if let Some(g) = &grades {
        subframe::apply_grades(&mut analysis, g, &a.grade.policy());
    }
    print_table(&analysis);
    write_subframe_report(&a.report, &analysis)
}

pub fn plot(a: &PlotArgs) -> Result<()> {
    match a.kind {
        PlotKind::Subframes => plot_subframes(a),
        PlotKind::Histogram => plot_histogram(a),
        PlotKind::Annotate
        | PlotKind::Residuals
        | PlotKind::Pcc
        | PlotKind::Lightcurve
        | PlotKind::Zeropoint
        | PlotKind::LimitingMag
        | PlotKind::ResidualMap => crate::astrometry::plot(a),
        PlotKind::RejectionLow
        | PlotKind::RejectionHigh
        | PlotKind::WeightMap
        | PlotKind::CountMap
        | PlotKind::Noise => plot_integration(a),
        _ => crate::optics::plot(a),
    }
}

fn plot_subframes(a: &PlotArgs) -> Result<()> {
    let analysis = match a.input.as_slice() {
        [one]
            if one
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json")) =>
        {
            let text = std::fs::read_to_string(one)
                .with_context(|| format!("cannot read {}", one.display()))?;
            subframe::from_json(&text)?
        }
        inputs => {
            let files = collect(inputs)?;
            let frames = load(&files)?;
            let method = crate::DebayerArg::Auto.method(any_bayer(&frames));
            let frames = demosaic_all(frames, method)?;
            let metrics = measure(
                &files,
                &frames,
                a.threshold,
                crate::ModelArg::default(),
                None,
            )?;
            subframe::evaluate(&metrics, &Limits::from(&a.limits))
        }
    };
    let x = match a.x {
        XAxisArg::Index => XAxis::Index,
        XAxisArg::Time if subframe::has_times(&analysis) => XAxis::Time,
        XAxisArg::Time => bail!("--x time needs DATE-OBS in every frame, and at least two frames"),
    };
    let png = subframe::plot_png(&analysis, x)?;
    write(&a.output, &png)?;
    write(
        &a.output.with_extension("csv"),
        subframe::to_csv(&analysis).as_bytes(),
    )?;
    write(
        &a.output.with_extension("json"),
        subframe::to_json(&analysis)?.as_bytes(),
    )?;
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    valerian_science::table::write_file(path, bytes)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Drop the frames a table rejects; frames it does not name stay.
pub fn apply_approvals(files: Vec<PathBuf>, table: &Path) -> Result<Vec<PathBuf>> {
    let approvals = subframe::read_approvals(table)?;
    let before = files.len();
    let mut unnamed = 0;
    let kept: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if let Some(ok) = approvals.get(name) {
                *ok
            } else {
                unnamed += 1;
                true
            }
        })
        .collect();
    eprintln!(
        "approve table {}: {} of {before} frames kept ({} rejected{})",
        table.display(),
        kept.len(),
        before - kept.len(),
        if unnamed > 0 {
            format!(", {unnamed} not in the table and kept")
        } else {
            String::new()
        }
    );
    if kept.len() < 2 {
        bail!("the approve table leaves fewer than two frames to stack");
    }
    Ok(kept)
}

/// Stack on the CPU with the maps on and the noise curve; the log goes to stderr.
pub fn stack_for_science(
    frames: Vec<Image>,
    opts: &StackOptions,
) -> valerian_core::Result<(Stacked, Option<NoiseCurve>)> {
    integration::stack_with_maps(frames, opts, &mut |l| eprintln!("{l}"))
}

/// Add the integration section of a stack to the report folder.
pub fn write_integration_report(
    dir: &Path,
    stacked: &Stacked,
    noise: Option<&NoiseCurve>,
) -> Result<()> {
    let mut report = ReportDir::open(dir)?;
    let maps = stacked.maps.as_ref();
    let weight = maps.map_or(&stacked.coverage, |m| &m.weight);
    let input = Input {
        drizzle: false,
        image: &stacked.image,
        low: maps.map(|m| &m.low),
        high: maps.map(|m| &m.high),
        rejection: None,
        count: &stacked.coverage,
        weight,
        noise,
        frames: stacked.used(),
    };
    for f in integration_report::write_report(&mut report, &input)? {
        println!("wrote {}", dir.join(f).display());
    }
    println!("wrote {}", dir.join("report.html").display());
    Ok(())
}

/// Add the integration section of a drizzle to the report folder.
pub fn write_drizzle_report(dir: &Path, d: &Drizzled) -> Result<()> {
    let mut report = ReportDir::open(dir)?;
    let input = Input {
        drizzle: true,
        image: &d.image,
        low: None,
        high: None,
        rejection: Some(&d.rejection),
        count: &d.count,
        weight: &d.weight,
        noise: None,
        frames: d.used(),
    };
    for f in integration_report::write_report(&mut report, &input)? {
        println!("wrote {}", dir.join(f).display());
    }
    println!("wrote {}", dir.join("report.html").display());
    Ok(())
}

fn plot_histogram(a: &PlotArgs) -> Result<()> {
    let [input] = a.input.as_slice() else {
        bail!("this plot takes one FITS image");
    };
    let img = fits::read(input).with_context(|| format!("cannot read {}", input.display()))?;
    let h = histogram::histograms(&img, 256)?;
    write(&a.output, &histogram::plot_png(&h)?)?;
    write(
        &a.output.with_extension("csv"),
        histogram::to_csv(&h).as_bytes(),
    )?;
    write(
        &a.output.with_extension("json"),
        serde_json::to_string_pretty(&h)?.as_bytes(),
    )?;
    for c in &h.channels {
        println!(
            "{:<6} median {:.1}  sigma {:.2}  black-clipped {:.3} %  white-clipped {:.3} %",
            c.stats.name,
            c.stats.median,
            c.stats.sigma,
            100.0 * c.stats.clipped_black,
            100.0 * c.stats.clipped_white
        );
    }
    Ok(())
}

fn plot_integration(a: &PlotArgs) -> Result<()> {
    let files = collect(&a.input)?;
    let frames = load(&files)?;
    let method = crate::DebayerArg::Auto.method(any_bayer(&frames));
    let frames = demosaic_all(frames, method)?;
    let opts = StackOptions {
        detect: DetectOptions {
            threshold: a.threshold,
            ..DetectOptions::default()
        },
        ..StackOptions::default()
    };
    eprintln!("compute: CPU (the maps and the noise curve need the CPU combiner)");
    let (stacked, curve) = stack_for_science(frames, &opts).context("stacking failed")?;
    let maps = stacked.maps.as_ref().context("the combiner kept no maps")?;
    let (kind, frame): (MapKind, &Frame) = match a.kind {
        PlotKind::RejectionLow => (MapKind::RejectionLow, &maps.low),
        PlotKind::RejectionHigh => (MapKind::RejectionHigh, &maps.high),
        PlotKind::WeightMap => (MapKind::Weight, &maps.weight),
        PlotKind::CountMap => (MapKind::Count, &stacked.coverage),
        _ => {
            let c = curve.context("the noise curve needs at least 4 registered frames")?;
            let png = valerian_science::plot::encode_png(
                &integration::noise_rgb(&c)?,
                integration::NOISE_SIZE.0,
                integration::NOISE_SIZE.1,
            )?;
            write(&a.output, &png)?;
            write(
                &a.output.with_extension("csv"),
                integration::noise_csv(&c).as_bytes(),
            )?;
            write(
                &a.output.with_extension("json"),
                serde_json::to_string_pretty(&c)?.as_bytes(),
            )?;
            println!(
                "one frame {:.2} ADU; {} frames per half {:.2} ADU: reduction {:.2} against {:.2} for the square root",
                c.sigma1,
                c.points.last().map_or(0, |p| p.frames),
                c.points.last().map_or(f64::NAN, |p| p.noise),
                c.reduction(),
                c.theoretical()
            );
            return Ok(());
        }
    };
    write(&a.output, &integration::map_png(frame, kind)?)?;
    crate::files::write(
        &a.output.with_extension("fits"),
        &Image::mono(frame.clone(), Header::new()),
    )?;
    if kind != MapKind::Weight {
        write(
            &a.output.with_extension("csv"),
            integration::value_counts_csv(frame).as_bytes(),
        )?;
    }
    let s = integration::map_stats(frame);
    println!(
        "min {} median {} mean {:.3} max {}",
        s.min, s.median, s.mean, s.max
    );
    Ok(())
}
