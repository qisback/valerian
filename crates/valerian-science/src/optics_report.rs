// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The optics section of the session report: every plot, its data as CSV and
//! JSON, and the tilt and backfocus hints in words.

use serde::Serialize;
use valerian_core::fits::Image;

use crate::optics::{self, Field, Quantity, Tilt};
use crate::report::{Figure, ReportDir, Section, Table};
use crate::table::num;
use crate::{Error, Result};

/// What to draw.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Inspector crop, pixels.
    pub crop: usize,
    /// Inspector zoom.
    pub zoom: usize,
    /// Degree of the polynomial surface of the maps.
    pub order: usize,
    /// Stars stacked in the PSF profile.
    pub psf_stars: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            crop: 128,
            zoom: 2,
            order: 3,
            psf_stars: 50,
        }
    }
}

#[derive(Serialize)]
struct Summary<'a> {
    stars: usize,
    median_fwhm_px: f64,
    median_ellipticity: f64,
    tilt: &'a Tilt,
    inspector: &'a [optics::InspectorPanel],
    psf: Option<&'a optics::PsfFit>,
}

/// Write every optics output into the report folder and add its section.
///
/// # Errors
/// If a file cannot be written or a plot cannot be drawn. A profile that
/// cannot be fitted (too few isolated stars) is left out, not an error.
pub fn write_report(
    report: &mut ReportDir,
    img: &Image,
    field: &Field,
    p: &Params,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut figures = Vec::new();
    let mut save = |report: &ReportDir, name: &str, bytes: &[u8]| -> Result<()> {
        report.save(name, bytes)?;
        files.push(name.to_owned());
        Ok(())
    };
    let insp = optics::inspector(img, Some(field), p.crop, p.zoom)?;
    save(
        report,
        "aberration_inspector.png",
        &optics::inspector_png(&insp)?,
    )?;
    figures.push(Figure {
        file: "aberration_inspector.png".into(),
        caption: "Aberration inspector: corners, edges and centre, stretched alike".into(),
    });
    for (q, stem, caption) in [
        (Quantity::Fwhm, "fwhm_map", "FWHM across the field"),
        (
            Quantity::Ellipticity,
            "eccentricity_map",
            "Eccentricity across the field, with the elongation of the stars as whiskers",
        ),
    ] {
        save(
            report,
            &format!("{stem}.png"),
            &optics::map_png(field, q, p.order)?,
        )?;
        figures.push(Figure {
            file: format!("{stem}.png"),
            caption: caption.into(),
        });
        if let Some(csv) = optics::map_csv(field, q, p.order) {
            save(report, &format!("{stem}.csv"), csv.as_bytes())?;
        }
    }
    save(
        report,
        "field_stars.csv",
        optics::stars_csv(field).as_bytes(),
    )?;
    let tilt = optics::tilt(field);
    save(
        report,
        "tilt.png",
        &plot_png_of(&optics::tilt_rgb(&tilt)?, optics::TILT_SIZE)?,
    )?;
    figures.push(Figure {
        file: "tilt.png".into(),
        caption: "Median FWHM of the centre and the four corners".into(),
    });
    let psf = optics::psf(img, field, p.psf_stars).ok();
    if let Some(psf) = &psf {
        save(
            report,
            "psf.png",
            &plot_png_of(&optics::psf_rgb(psf)?, optics::PSF_SIZE)?,
        )?;
        save(report, "psf.csv", optics::psf_csv(psf).as_bytes())?;
        figures.push(Figure {
            file: "psf.png".into(),
            caption:
                "Radial profile of the brightest isolated stars with a Moffat and a Gaussian fit"
                    .into(),
        });
    }
    let summary = Summary {
        stars: field.stars.len(),
        median_fwhm_px: field.median_fwhm(),
        median_ellipticity: field.median_ellipticity(),
        tilt: &tilt,
        inspector: &insp.panels,
        psf: psf.as_ref().map(|p| &p.fit),
    };
    let json = serde_json::to_string_pretty(&summary).map_err(|e| Error::Parse(e.to_string()))?;
    save(report, "optics.json", json.as_bytes())?;
    let mut text = vec![format!(
        "{} stars measured: median FWHM {:.2} px, median eccentricity {:.3}.",
        field.stars.len(),
        field.median_fwhm(),
        field.median_ellipticity()
    )];
    text.extend(tilt.hints.iter().cloned());
    if let Some(psf) = &psf {
        text.push(format!(
            "Moffat fit of {} stars: FWHM {:.2} px, beta {:.2} (a Gaussian would be beta = infinity; \
             seeing-limited stars are usually 2.5 to 5). The Gaussian fit gives {:.2} px.",
            psf.fit.stars,
            psf.fit.fwhm_moffat,
            if psf.fit.beta > 50.0 { "above 50".to_owned() } else { format!("{:.2}", psf.fit.beta) },
            psf.fit.fwhm_gauss
        ));
    }
    let rows = tilt
        .zones
        .iter()
        .map(|z| {
            vec![
                z.name.clone(),
                z.stars.to_string(),
                z.fwhm.map_or(String::new(), |v| num(v, 2)),
                z.ellipticity.map_or(String::new(), |v| num(v, 3)),
            ]
        })
        .collect();
    report.put(Section {
        id: "optics".into(),
        title: "Optics".into(),
        text,
        figures,
        files: files.clone(),
        table: Some(Table {
            header: ["zone", "stars", "FWHM px", "eccentricity"]
                .map(String::from)
                .to_vec(),
            rows,
            flag_column: None,
        }),
    });
    report.write()?;
    Ok(files)
}

fn plot_png_of(rgb: &[u8], size: (u32, u32)) -> Result<Vec<u8>> {
    crate::plot::encode_png(rgb, size.0, size.1)
}
