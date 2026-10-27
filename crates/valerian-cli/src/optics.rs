// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian optics` and the optics kinds of `valerian plot`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use valerian_core::debayer::{Method, demosaic_image};
use valerian_core::fits::{self, Image};
use valerian_science::optics::{self, Field, Quantity};
use valerian_science::optics_report::{self, Params};
use valerian_science::report::ReportDir;
use valerian_science::table::write_file;

use crate::{OpticsArgs, PlotArgs, PlotKind};

/// Read the image; a raw Bayer mosaic is demosaiced so the stars have their true width.
fn read(path: &Path) -> Result<Image> {
    let img = fits::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if img.planes.len() == 1 && img.plane().bayer.is_some() {
        return demosaic_image(&img, Method::Mhc).context("cannot demosaic the raw frame");
    }
    Ok(img)
}

fn field_of(img: &Image, threshold: f32, min_snr: f64) -> Result<Field> {
    let field = optics::measure_field(img, threshold, min_snr)?;
    eprintln!(
        "{} usable stars: median FWHM {:.2} px, eccentricity {:.3}",
        field.stars.len(),
        field.median_fwhm(),
        field.median_ellipticity()
    );
    Ok(field)
}

fn check(order: usize) -> Result<()> {
    if !(1..=4).contains(&order) {
        bail!("--order must be 1 to 4");
    }
    Ok(())
}

pub fn run(a: &OpticsArgs) -> Result<()> {
    check(a.order)?;
    let img = read(&a.image)?;
    let field = field_of(&img, a.threshold, a.min_snr)?;
    let params = Params {
        crop: a.crop,
        zoom: a.zoom,
        order: a.order,
        psf_stars: a.psf_stars,
    };
    let mut report = ReportDir::open(&a.report)?;
    let files = optics_report::write_report(&mut report, &img, &field, &params)?;
    let tilt = optics::tilt(&field);
    println!(
        "{:<14} {:>5} {:>8} {:>6}",
        "zone", "stars", "FWHM px", "ecc"
    );
    for z in &tilt.zones {
        println!(
            "{:<14} {:>5} {:>8} {:>6}",
            z.name,
            z.stars,
            z.fwhm.map_or("-".into(), |v| format!("{v:.2}")),
            z.ellipticity.map_or("-".into(), |v| format!("{v:.3}"))
        );
    }
    for h in &tilt.hints {
        println!("* {h}");
    }
    for f in &files {
        println!("wrote {}", a.report.join(f).display());
    }
    println!("wrote {}", a.report.join("report.html").display());
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    write_file(path, bytes)?;
    println!("wrote {}", path.display());
    Ok(())
}

pub fn plot(a: &PlotArgs) -> Result<()> {
    check(a.order)?;
    let [input] = a.input.as_slice() else {
        bail!("this plot takes one FITS image");
    };
    let img = read(input)?;
    let field = field_of(&img, a.threshold, a.min_snr)?;
    let out = &a.output;
    match a.kind {
        PlotKind::Aberration => {
            let i = optics::inspector(&img, Some(&field), a.crop, a.zoom)?;
            write(out, &optics::inspector_png(&i)?)?;
            let json = serde_json::to_string_pretty(&i.panels)?;
            write(&out.with_extension("json"), json.as_bytes())?;
        }
        PlotKind::FwhmMap | PlotKind::EccentricityMap => {
            let q = if a.kind == PlotKind::FwhmMap {
                Quantity::Fwhm
            } else {
                Quantity::Ellipticity
            };
            write(out, &optics::map_png(&field, q, a.order)?)?;
            if let Some(csv) = optics::map_csv(&field, q, a.order) {
                write(&out.with_extension("csv"), csv.as_bytes())?;
            }
            write(
                &out.with_extension("stars.csv"),
                optics::stars_csv(&field).as_bytes(),
            )?;
        }
        PlotKind::Tilt => {
            let t = optics::tilt(&field);
            let png = valerian_science::plot::encode_png(
                &optics::tilt_rgb(&t)?,
                optics::TILT_SIZE.0,
                optics::TILT_SIZE.1,
            )?;
            write(out, &png)?;
            write(
                &out.with_extension("json"),
                serde_json::to_string_pretty(&t)?.as_bytes(),
            )?;
            for h in &t.hints {
                println!("* {h}");
            }
        }
        PlotKind::Psf => {
            let p = optics::psf(&img, &field, a.psf_stars)?;
            let png = valerian_science::plot::encode_png(
                &optics::psf_rgb(&p)?,
                optics::PSF_SIZE.0,
                optics::PSF_SIZE.1,
            )?;
            write(out, &png)?;
            write(&out.with_extension("csv"), optics::psf_csv(&p).as_bytes())?;
            write(
                &out.with_extension("json"),
                serde_json::to_string_pretty(&p.fit)?.as_bytes(),
            )?;
            println!(
                "Moffat FWHM {:.2} px, beta {:.2}; Gaussian FWHM {:.2} px",
                p.fit.fwhm_moffat, p.fit.beta, p.fit.fwhm_gauss
            );
        }
        _ => unreachable!("handled by the caller"),
    }
    Ok(())
}
