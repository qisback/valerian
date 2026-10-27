// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian annotate`, `valerian photometry` and the astrometry kinds of `valerian plot`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use valerian_core::catalogue::{self, Catalogue};
use valerian_core::fits::{self, Image};
use valerian_core::photocal::{self, Calibration};
use valerian_core::register::RegisterOptions;
use valerian_core::solve::{self, Hint, hint_from_header};
use valerian_core::stack::{Reference, StackOptions};
use valerian_core::stars::DetectOptions;
use valerian_core::wcs::Wcs;
use valerian_science::annotate::{self, AnnotateOptions};
use valerian_science::astro_report;
use valerian_science::pcc;
use valerian_science::photometry::{self, LightCurve, Options};
use valerian_science::plot::encode_png;
use valerian_science::report::ReportDir;
use valerian_science::residual_map;
use valerian_science::table::{signed, write_file};
use valerian_science::zeropoint;

use crate::files::{any_bayer, collect, demosaic_all, load};
use crate::{AnnotateArgs, PhotometryArgs, PlotArgs, PlotKind};

fn catalogue_of(path: Option<&Path>) -> Result<Catalogue> {
    Ok(match path {
        Some(p) => Catalogue::open(p)?,
        None => catalogue::open_default()?,
    })
}

/// The plate solution: `wcs_from`'s header, the image's own, or a quick solve
/// from the header's position and scale.
fn solution(img: &Image, cat: &Catalogue, wcs_from: Option<&Path>, what: &Path) -> Result<Wcs> {
    if let Some(p) = wcs_from {
        let h = fits::read_header(p).with_context(|| format!("cannot read {}", p.display()))?;
        return Wcs::from_header(&h)
            .with_context(|| format!("{} has no world coordinate system", p.display()));
    }
    if let Some(w) = Wcs::from_header(&img.header) {
        return Ok(w);
    }
    let hint = hint_from_header(&img.header, (img.width(), img.height()));
    let (Some((ra, dec)), Some(scale)) = (hint.position, hint.scale) else {
        bail!(
            "{} has no world coordinate system and the header lacks a position and scale to \
             solve one: run `valerian solve` first",
            what.display()
        );
    };
    eprintln!("no WCS in the image: solving it from the header first");
    let opts = solve::Options {
        hint: Some(Hint {
            ra,
            dec,
            radius: 1.0,
        }),
        scale: (scale * 0.85, scale * 1.15),
        ..solve::Options::default()
    };
    Ok(solve::solve(img, cat, &opts)?.wcs)
}

/// Parse `X,Y`.
fn parse_xy(s: &str) -> Result<(f64, f64)> {
    let (a, b) = s
        .split_once(',')
        .with_context(|| format!("`{s}` is not X,Y"))?;
    Ok((
        a.trim().parse().context("X is not a number")?,
        b.trim().parse().context("Y is not a number")?,
    ))
}

fn annotate_options(
    no_grid: bool,
    no_objects: bool,
    no_stars: bool,
    star_mag: f32,
    label_mag: f32,
    max_side: usize,
) -> AnnotateOptions {
    AnnotateOptions {
        grid: !no_grid,
        objects: !no_objects,
        stars: !no_stars,
        star_mag,
        label_mag,
        max_side: max_side.clamp(200, 6000),
    }
}

pub fn annotate(a: &AnnotateArgs) -> Result<()> {
    let img = fits::read(&a.image).with_context(|| format!("cannot read {}", a.image.display()))?;
    let cat = catalogue_of(a.catalogue.as_deref()).ok();
    let wcs = match &cat {
        Some(c) => solution(&img, c, a.wcs.as_deref(), &a.image)?,
        None => solution_without_catalogue(&img, a.wcs.as_deref(), &a.image)?,
    };
    let opts = annotate_options(
        a.no_grid,
        a.no_objects,
        a.no_stars,
        a.star_mag,
        a.label_mag,
        a.max_side,
    );
    let annotated = annotate::annotate(&img, &wcs, cat.as_ref(), &opts)?;
    println!(
        "{} objects and {} Gaia stars in the field; grid every {} arc minutes and {} seconds of time",
        annotated.objects.len(),
        annotated.stars.len(),
        annotated.dec_step_arcmin,
        annotated.ra_step_seconds
    );
    for p in annotated.objects.iter().take(12) {
        println!(
            "  {:<12} {:<22} {:<24} at ({:.0}, {:.0})",
            p.object.name,
            p.object.common,
            p.object.kind_word(),
            p.x,
            p.y
        );
    }
    let residuals = if let Some(c) = &cat {
        match annotate::residuals(&img, &wcs, c, 12.0, 3.0) {
            Ok(r) => {
                println!(
                    "residuals: {} stars, rms {:.3} px = {:.2} arcsec",
                    r.stars.len(),
                    r.rms_px,
                    r.rms_arcsec
                );
                Some(r)
            }
            Err(e) => {
                eprintln!("note: no residuals: {e}");
                None
            }
        }
    } else {
        eprintln!("note: no star catalogue found, so no Gaia stars and no residuals");
        None
    };
    let mut report = ReportDir::open(&a.report)?;
    for f in astro_report::write_astrometry(&mut report, &annotated, residuals.as_ref())? {
        println!("wrote {}", a.report.join(f).display());
    }
    if let Some(r) = &residuals {
        match residual_map::residual_map(r, a.map_grid) {
            Ok(m) => {
                print_map(&m);
                for f in astro_report::write_residual_map(&mut report, &m, r)? {
                    println!("wrote {}", a.report.join(f).display());
                }
            }
            Err(e) => eprintln!("note: no residual map: {e}"),
        }
    }
    println!("wrote {}", a.report.join("report.html").display());
    Ok(())
}

fn solution_without_catalogue(img: &Image, wcs_from: Option<&Path>, what: &Path) -> Result<Wcs> {
    if let Some(p) = wcs_from {
        let h = fits::read_header(p)?;
        return Wcs::from_header(&h)
            .with_context(|| format!("{} has no world coordinate system", p.display()));
    }
    Wcs::from_header(&img.header).with_context(|| {
        format!(
            "{} has no world coordinate system (run `valerian solve` first)",
            what.display()
        )
    })
}

fn stack_options(threshold: f32, model: crate::ModelArg, reference: Option<usize>) -> StackOptions {
    StackOptions {
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
    }
}

fn measure_light_curve(
    inputs: &[std::path::PathBuf],
    star: Option<&str>,
    aperture: f64,
    comparison: usize,
    opts: &StackOptions,
) -> Result<LightCurve> {
    let files = collect(inputs)?;
    let names = crate::science::names_of(&files);
    let frames = load(&files)?;
    let method = crate::DebayerArg::Auto.method(any_bayer(&frames));
    let frames = demosaic_all(frames, method)?;
    let target = star.map(parse_xy).transpose()?;
    let options = Options {
        aperture,
        comparison,
    };
    let curve = photometry::light_curve(&frames, &names, target, &options, opts, &mut |l| {
        eprintln!("{l}");
    })?;
    if target.is_none() {
        eprintln!(
            "star: the brightest unsaturated one in frame {} at ({:.1}, {:.1}); use --star X,Y to choose",
            curve.reference, curve.reference_xy.0, curve.reference_xy.1
        );
    }
    Ok(curve)
}

pub fn photometry(a: &PhotometryArgs) -> Result<()> {
    let opts = stack_options(a.threshold, a.model, a.reference);
    let curve = measure_light_curve(
        &a.frames,
        a.star.as_deref(),
        a.aperture,
        a.comparison,
        &opts,
    )?;
    println!(
        "{} frames measured; scatter {:.4} mag, check star {:.4} mag; aperture {:.1} px",
        curve.rows.iter().filter(|r| r.ok).count(),
        curve.scatter_mag,
        curve.check_scatter_mag,
        curve.aperture_px
    );
    println!(
        "{:>4}  {:<30} {:>10} {:>9} {:>8} {:>8}",
        "#", "frame", "flux", "mag", "error", "check"
    );
    for r in &curve.rows {
        println!(
            "{:>4}  {:<30} {:>10.0} {:>+9.4} {:>8.4} {:>+8.4}{}",
            r.frame,
            crate::files::elide(&r.name, 30),
            r.flux,
            r.mag_relative,
            r.mag_error,
            r.check_mag,
            if r.ok {
                String::new()
            } else {
                format!("  ({})", r.note)
            }
        );
    }
    let mut report = ReportDir::open(&a.report)?;
    for f in astro_report::write_photometry(&mut report, &curve)? {
        println!("wrote {}", a.report.join(f).display());
    }
    println!("wrote {}", a.report.join("report.html").display());
    Ok(())
}

/// One line each for the linear terms of a residual map.
fn print_map(m: &residual_map::ResidualMap) {
    let l = &m.linear;
    println!(
        "residual map: scale {} ppm, rotation {} arcsec at the corner, rms {:.3} px before and {:.3} px after the linear terms",
        signed(l.scale_ppm, 0),
        signed(l.rotation_arcsec, 2),
        l.rms_before_px,
        l.rms_after_px
    );
}

/// Print the zero points and the limiting magnitude.
pub fn print_calibration(p: &zeropoint::Photometry) {
    for c in &p.channels {
        println!(
            "{:>5} zero point {:.3} +/- {:.3} (after the gain {:.3}), colour term {:+.3} +/- {:.3} mag per BP-RP, scatter {:.3} mag, {} stars",
            c.channel,
            c.zero,
            c.zero_error,
            c.zero_after_gain,
            c.colour_term,
            c.colour_term_error,
            c.scatter,
            c.stars
        );
    }
    println!(
        "{} stars in the calibrated catalogue; {:.0}-sigma limiting magnitude {:.2} (red {:.2}, blue {:.2}){}{}",
        p.stars.len(),
        p.limit.snr,
        p.limit.from_noise[1],
        p.limit.from_noise[0],
        p.limit.from_noise[2],
        p.limit.faintest_measured.map_or(String::new(), |v| format!(
            "; faintest star at that level {v:.2}"
        )),
        p.limit
            .counts_peak
            .map_or(String::new(), |v| format!("; counts peak {v:.2}")),
    );
    for w in &p.warnings {
        eprintln!("warning: {w}");
    }
}

/// Add the photometric calibration of `p` to a report folder.
pub fn write_calibration_report(dir: &Path, p: &zeropoint::Photometry) -> Result<()> {
    let mut report = ReportDir::open(dir)?;
    for f in astro_report::write_photometry_calibration(&mut report, p)? {
        println!("wrote {}", dir.join(f).display());
    }
    println!("wrote {}", dir.join("report.html").display());
    Ok(())
}

/// Add the colour-calibration scatter of `cal` to a report folder.
pub fn write_pcc_report(dir: &Path, cal: &Calibration) -> Result<()> {
    let mut report = ReportDir::open(dir)?;
    for f in astro_report::write_pcc(&mut report, cal)? {
        println!("wrote {}", dir.join(f).display());
    }
    println!("wrote {}", dir.join("report.html").display());
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    write_file(path, bytes)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn plot_residual_map(a: &PlotArgs, img: &Image, input: &Path, cat: &Catalogue) -> Result<()> {
    let wcs = solution(img, cat, a.wcs.as_deref(), input)?;
    let r = annotate::residuals(img, &wcs, cat, 12.0, 3.0)?;
    let m = residual_map::residual_map(&r, a.grid)?;
    write(&a.output, &residual_map::plot_png(&m, &r)?)?;
    write(
        &a.output.with_extension("csv"),
        residual_map::to_csv(&m).as_bytes(),
    )?;
    write(
        &a.output.with_extension("json"),
        residual_map::to_json(&m)?.as_bytes(),
    )?;
    print_map(&m);
    Ok(())
}

fn plot_calibration(a: &PlotArgs, img: &Image, input: &Path, cat: &Catalogue) -> Result<()> {
    let wcs = solution(img, cat, a.wcs.as_deref(), input)?;
    let cal = photocal::calibrate(img, &wcs, cat, &photocal::Options::default())?;
    let opts = zeropoint::Options {
        limit_snr: a.limit_snr,
        ..zeropoint::Options::default()
    };
    let p = zeropoint::calibrate_field(img, &wcs, &cal, &opts)?;
    let (png, csv) = if a.kind == PlotKind::Zeropoint {
        (zeropoint::zeropoint_png(&p)?, zeropoint::zeropoint_csv(&p))
    } else {
        (zeropoint::limit_png(&p)?, zeropoint::stars_csv(&p))
    };
    write(&a.output, &png)?;
    write(&a.output.with_extension("csv"), csv.as_bytes())?;
    write(
        &a.output.with_extension("json"),
        zeropoint::to_json(&p)?.as_bytes(),
    )?;
    print_calibration(&p);
    Ok(())
}

pub fn plot(a: &PlotArgs) -> Result<()> {
    if a.kind == PlotKind::Lightcurve {
        let opts = stack_options(a.threshold, crate::ModelArg::default(), None);
        let curve =
            measure_light_curve(&a.input, a.star.as_deref(), a.aperture, a.comparison, &opts)?;
        write(&a.output, &photometry::plot_png(&curve)?)?;
        write(
            &a.output.with_extension("csv"),
            photometry::to_csv(&curve).as_bytes(),
        )?;
        write(
            &a.output.with_extension("json"),
            photometry::to_json(&curve)?.as_bytes(),
        )?;
        println!(
            "scatter {:.4} mag, check star {:.4} mag",
            curve.scatter_mag, curve.check_scatter_mag
        );
        return Ok(());
    }
    let [input] = a.input.as_slice() else {
        bail!("this plot takes one solved FITS image");
    };
    let img = fits::read(input).with_context(|| format!("cannot read {}", input.display()))?;
    let cat = catalogue_of(a.catalogue.as_deref());
    match a.kind {
        PlotKind::Annotate => {
            let cat = cat.ok();
            let wcs = match &cat {
                Some(c) => solution(&img, c, a.wcs.as_deref(), input)?,
                None => solution_without_catalogue(&img, a.wcs.as_deref(), input)?,
            };
            let opts = AnnotateOptions {
                star_mag: a.star_mag,
                ..AnnotateOptions::default()
            };
            let annotated = annotate::annotate(&img, &wcs, cat.as_ref(), &opts)?;
            write(&a.output, &annotate::annotated_png(&annotated)?)?;
            write(
                &a.output.with_extension("objects.csv"),
                annotate::objects_csv(&annotated).as_bytes(),
            )?;
            write(
                &a.output.with_extension("stars.csv"),
                annotate::stars_csv(&annotated).as_bytes(),
            )?;
        }
        PlotKind::Residuals => {
            let cat = cat?;
            let wcs = solution(&img, &cat, a.wcs.as_deref(), input)?;
            let r = annotate::residuals(&img, &wcs, &cat, 12.0, 3.0)?;
            let (w, h, rgb) = annotate::residuals_rgb(&r)?;
            write(&a.output, &encode_png(&rgb, w, h)?)?;
            write(
                &a.output.with_extension("csv"),
                annotate::residuals_csv(&r).as_bytes(),
            )?;
            write(
                &a.output.with_extension("json"),
                serde_json::to_string_pretty(&r)?.as_bytes(),
            )?;
            println!(
                "{} stars, rms {:.3} px = {:.2} arcsec",
                r.stars.len(),
                r.rms_px,
                r.rms_arcsec
            );
        }
        PlotKind::Pcc => {
            let cat = cat?;
            let wcs = solution(&img, &cat, a.wcs.as_deref(), input)?;
            let cal = photocal::calibrate(&img, &wcs, &cat, &photocal::Options::default())?;
            write(&a.output, &pcc::plot_png(&cal)?)?;
            write(
                &a.output.with_extension("csv"),
                pcc::to_csv(&cal).as_bytes(),
            )?;
            write(
                &a.output.with_extension("json"),
                pcc::to_json(&cal)?.as_bytes(),
            )?;
            println!(
                "{} stars used; red slope {:+.3}, blue slope {:+.3}; gains {:.4} {:.4}",
                cal.used, cal.lines[0].slope, cal.lines[1].slope, cal.gains[0], cal.gains[2]
            );
        }
        PlotKind::ResidualMap => plot_residual_map(a, &img, input, &cat?)?,
        PlotKind::Zeropoint | PlotKind::LimitingMag => plot_calibration(a, &img, input, &cat?)?,
        _ => unreachable!("handled by the caller"),
    }
    Ok(())
}
