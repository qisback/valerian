// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The astrometry and photometry sections of the session report: the
//! annotated image with the residuals of the plate solution, the colour
//! calibration scatter, and the light curve of a star.

use serde::Serialize;
use valerian_core::photocal::Calibration;

use crate::annotate::{self, Annotated, Residuals};
use crate::pcc;
use crate::photometry::{self, LightCurve};
use crate::plot::encode_png;
use crate::report::{Figure, ReportDir, Section, Table};
use crate::residual_map::{self, ResidualMap};
use crate::table::{num, signed};
use crate::zeropoint::{self, Photometry};
use crate::{Error, Result};

#[derive(Serialize)]
struct AstrometrySummary<'a> {
    objects: usize,
    stars: usize,
    dec_step_arcmin: f64,
    ra_step_seconds: f64,
    residuals: Option<&'a Residuals>,
}

/// Add the annotated image, its object and star lists and the residuals.
///
/// # Errors
/// If a file cannot be written or a plot cannot be drawn.
pub fn write_astrometry(
    report: &mut ReportDir,
    a: &Annotated,
    r: Option<&Residuals>,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut figures = Vec::new();
    let mut text = vec![format!(
        "{} catalogue objects and {} Gaia stars in the field; grid lines every {} in declination and every {} in right ascension.",
        a.objects.len(),
        a.stars.len(),
        if a.dec_step_arcmin > 0.0 {
            format!("{} arc minutes", a.dec_step_arcmin)
        } else {
            "-".into()
        },
        if a.ra_step_seconds > 0.0 {
            format!("{} seconds of time", a.ra_step_seconds)
        } else {
            "-".into()
        },
    )];
    report.save("annotated.png", &annotate::annotated_png(a)?)?;
    report.save("annotated_objects.csv", annotate::objects_csv(a).as_bytes())?;
    report.save("annotated_stars.csv", annotate::stars_csv(a).as_bytes())?;
    files.extend(
        [
            "annotated.png",
            "annotated_objects.csv",
            "annotated_stars.csv",
        ]
        .map(String::from),
    );
    figures.push(Figure {
        file: "annotated.png".into(),
        caption:
            "The plate solution drawn on the image: RA/Dec grid, catalogue objects and Gaia stars"
                .into(),
    });
    if let Some(r) = r {
        let (w, h, rgb) = annotate::residuals_rgb(r)?;
        report.save("wcs_residuals.png", &encode_png(&rgb, w, h)?)?;
        report.save("wcs_residuals.csv", annotate::residuals_csv(r).as_bytes())?;
        files.extend(["wcs_residuals.png", "wcs_residuals.csv"].map(String::from));
        figures.push(Figure {
            file: "wcs_residuals.png".into(),
            caption: "Residuals of the plate solution: detected star minus catalogue position, and against magnitude".into(),
        });
        text.push(format!(
            "{} stars matched to Gaia: residual rms {:.3} px = {:.2} arcsec, median {:.3} px, mean shift ({:+.3}, {:+.3}) px.",
            r.stars.len(),
            r.rms_px,
            r.rms_arcsec,
            r.median_px,
            r.mean_dx,
            r.mean_dy
        ));
    }
    let json = serde_json::to_string_pretty(&AstrometrySummary {
        objects: a.objects.len(),
        stars: a.stars.len(),
        dec_step_arcmin: a.dec_step_arcmin,
        ra_step_seconds: a.ra_step_seconds,
        residuals: r,
    })
    .map_err(|e| Error::Parse(e.to_string()))?;
    report.save("astrometry.json", json.as_bytes())?;
    files.push("astrometry.json".into());
    let rows = a
        .objects
        .iter()
        .take(60)
        .map(|p| {
            vec![
                p.object.name.clone(),
                p.object.common.clone(),
                p.object.kind_word().to_owned(),
                p.object.major.map_or(String::new(), |v| num(v, 1)),
                p.object.mag.map_or(String::new(), |v| num(v, 1)),
            ]
        })
        .collect();
    report.put(Section {
        id: "astrometry".into(),
        title: "Astrometry".into(),
        text,
        figures,
        files: files.clone(),
        table: Some(Table {
            header: ["object", "name", "kind", "size (arcmin)", "mag"]
                .map(String::from)
                .to_vec(),
            rows,
            flag_column: None,
        }),
    });
    report.write()?;
    Ok(files)
}

/// Add the astrometric residual map.
///
/// # Errors
/// If a file cannot be written or the plot cannot be drawn.
pub fn write_residual_map(
    report: &mut ReportDir,
    m: &ResidualMap,
    r: &Residuals,
) -> Result<Vec<String>> {
    report.save("residual_map.png", &residual_map::plot_png(m, r)?)?;
    report.save("residual_map.csv", residual_map::to_csv(m).as_bytes())?;
    report.save("residual_map.json", residual_map::to_json(m)?.as_bytes())?;
    let files: Vec<String> = ["residual_map.png", "residual_map.csv", "residual_map.json"]
        .map(String::from)
        .to_vec();
    let l = &m.linear;
    report.put(Section {
        id: "residual-map".into(),
        title: "Astrometric residual map".into(),
        text: vec![format!(
            "{} stars in a {} by {} grid. Linear terms of the residual field: scale {} ppm, rotation {} arcsec at the corner, anisotropy {} ppm, shear {} ppm. Residual rms {:.3} px before and {:.3} px after removing them.",
            m.stars,
            m.grid,
            m.grid,
            signed(l.scale_ppm, 0),
            signed(l.rotation_arcsec, 2),
            signed(l.anisotropy_ppm, 0),
            signed(l.shear_ppm, 0),
            l.rms_before_px,
            l.rms_after_px
        )],
        figures: vec![Figure {
            file: "residual_map.png".into(),
            caption: "Mean residual per cell, and the radial and tangential residual against distance from the centre".into(),
        }],
        files: files.clone(),
        table: None,
    });
    report.write()?;
    Ok(files)
}

/// Add the zero points, colour terms, limiting magnitude and calibrated catalogue.
///
/// # Errors
/// If a file cannot be written or a plot cannot be drawn.
pub fn write_photometry_calibration(report: &mut ReportDir, p: &Photometry) -> Result<Vec<String>> {
    report.save("zeropoint.png", &zeropoint::zeropoint_png(p)?)?;
    report.save("zeropoint.csv", zeropoint::zeropoint_csv(p).as_bytes())?;
    report.save("zeropoint.json", zeropoint::to_json(p)?.as_bytes())?;
    report.save("limiting_magnitude.png", &zeropoint::limit_png(p)?)?;
    report.save("calibrated_stars.csv", zeropoint::stars_csv(p).as_bytes())?;
    let files: Vec<String> = [
        "zeropoint.png",
        "zeropoint.csv",
        "zeropoint.json",
        "limiting_magnitude.png",
        "calibrated_stars.csv",
    ]
    .map(String::from)
    .to_vec();
    let c = &p.channels;
    let mut text = vec![format!(
        "Zero point at BP-RP {:.2}, aperture {:.1} px (magnitudes for the pixel values of the image): red {:.3} +/- {:.3}, green {:.3} +/- {:.3}, blue {:.3} +/- {:.3}. Colour terms {:+.3}, {:+.3}, {:+.3} mag per BP-RP; scatter {:.3}, {:.3}, {:.3} mag.",
        p.reference_colour,
        p.aperture_px,
        c[0].zero,
        c[0].zero_error,
        c[1].zero,
        c[1].zero_error,
        c[2].zero,
        c[2].zero_error,
        c[0].colour_term,
        c[1].colour_term,
        c[2].colour_term,
        c[0].scatter,
        c[1].scatter,
        c[2].scatter,
    )];
    text.push(format!(
        "{} stars in the calibrated catalogue. Limiting magnitude at {:.0} sigma (green): {:.2} from the aperture noise{}{}.",
        p.stars.len(),
        p.limit.snr,
        p.limit.from_noise[1],
        p.limit.faintest_measured.map_or(String::new(), |v| format!(
            ", faintest star measured at that level {v:.2}"
        )),
        p.limit
            .counts_peak
            .map_or(String::new(), |v| format!(", star counts peak at {v:.2}")),
    ));
    text.extend(p.warnings.iter().cloned());
    let rows = p
        .stars
        .iter()
        .take(40)
        .map(|s| {
            vec![
                s.id.to_string(),
                num(s.x, 1),
                num(s.y, 1),
                num(s.mag[1], 2),
                num(s.mag_error[1], 3),
                num(s.bp_rp, 2),
                s.flags.clone(),
            ]
        })
        .collect();
    report.put(Section {
        id: "photometric-calibration".into(),
        title: "Photometric zero point and limiting magnitude".into(),
        text,
        figures: vec![
            Figure {
                file: "zeropoint.png".into(),
                caption: "Zero point against G and BP-RP for each colour plane, and its flatness across the field".into(),
            },
            Figure {
                file: "limiting_magnitude.png".into(),
                caption: "Signal-to-noise and star counts against magnitude, with the limiting magnitude".into(),
            },
        ],
        files: files.clone(),
        table: Some(Table {
            header: ["id", "x", "y", "mag G", "error", "BP-RP", "flags"]
                .map(String::from)
                .to_vec(),
            rows,
            flag_column: None,
        }),
    });
    report.write()?;
    Ok(files)
}

/// Add the colour-calibration scatter.
///
/// # Errors
/// If a file cannot be written or the plot cannot be drawn.
pub fn write_pcc(report: &mut ReportDir, c: &Calibration) -> Result<Vec<String>> {
    report.save("pcc.png", &pcc::plot_png(c)?)?;
    report.save("pcc.csv", pcc::to_csv(c).as_bytes())?;
    report.save("pcc.json", pcc::to_json(c)?.as_bytes())?;
    let files: Vec<String> = ["pcc.png", "pcc.csv", "pcc.json"]
        .map(String::from)
        .to_vec();
    let mut text = vec![format!(
        "{} Gaia stars measured, {} used. Red against green: slope {:+.3} mag per BP-RP, scatter {:.3} mag, gain {:.4}. Blue against green: slope {:+.3}, scatter {:.3}, gain {:.4}.",
        c.stars.len(),
        c.used,
        c.lines[0].slope,
        c.lines[0].rms,
        c.gains[0],
        c.lines[1].slope,
        c.lines[1].rms,
        c.gains[2]
    )];
    text.extend(c.warnings.iter().cloned());
    report.put(Section {
        id: "photometric-colour".into(),
        title: "Photometric colour calibration".into(),
        text,
        figures: vec![Figure {
            file: "pcc.png".into(),
            caption:
                "Instrumental colour against Gaia BP-RP, with the fitted lines that set the gains"
                    .into(),
        }],
        files: files.clone(),
        table: None,
    });
    report.write()?;
    Ok(files)
}

/// Add the light curve.
///
/// # Errors
/// If a file cannot be written or the plot cannot be drawn.
pub fn write_photometry(report: &mut ReportDir, c: &LightCurve) -> Result<Vec<String>> {
    report.save("lightcurve.png", &photometry::plot_png(c)?)?;
    report.save("lightcurve.csv", photometry::to_csv(c).as_bytes())?;
    report.save("lightcurve.json", photometry::to_json(c)?.as_bytes())?;
    let files: Vec<String> = ["lightcurve.png", "lightcurve.csv", "lightcurve.json"]
        .map(String::from)
        .to_vec();
    let rows = c
        .rows
        .iter()
        .map(|r| {
            vec![
                r.frame.to_string(),
                r.name.clone(),
                r.time_utc.clone().unwrap_or_default(),
                num(r.flux, 0),
                num(r.mag_relative, 4),
                num(r.mag_error, 4),
                num(r.check_mag, 4),
                r.note.clone(),
            ]
        })
        .collect();
    report.put(Section {
        id: "photometry".into(),
        title: "Aperture photometry".into(),
        text: vec![format!(
            "Star at ({:.1}, {:.1}) in frame {}: aperture radius {:.1} px, sky annulus {:.1} to {:.1} px, {} comparison stars; scatter {:.4} mag, check-star scatter {:.4} mag.",
            c.reference_xy.0,
            c.reference_xy.1,
            c.reference,
            c.aperture_px,
            c.annulus_px.0,
            c.annulus_px.1,
            c.comparison.len(),
            c.scatter_mag,
            c.check_scatter_mag
        )],
        figures: vec![Figure {
            file: "lightcurve.png".into(),
            caption: "Magnitude of the star against the comparison ensemble, and the check star".into(),
        }],
        files: files.clone(),
        table: Some(Table {
            header: ["frame", "name", "time (UTC)", "flux ADU", "mag", "error", "check", "note"]
                .map(String::from)
                .to_vec(),
            rows,
            flag_column: None,
        }),
    });
    report.write()?;
    Ok(files)
}
