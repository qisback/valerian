// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The integration section of the session report: the rejection, count and
//! weight maps, the noise against the square root of the number of frames, and
//! the histograms of the result.

use serde::Serialize;
use valerian_core::Frame;
use valerian_core::fits::{self, Format, Header, Image};

use crate::histogram::{self, Histograms};
use crate::integration::{self, MapKind, MapStats, NoiseCurve};
use crate::report::{Figure, ReportDir, Section, Table};
use crate::table::{num, write_file};
use crate::{Error, Result};

/// What an integration produced.
pub struct Input<'a> {
    /// `true` for a drizzle, `false` for a stack.
    pub drizzle: bool,
    /// The result.
    pub image: &'a Image,
    /// Rejections below the centre, per pixel (stack).
    pub low: Option<&'a Frame>,
    /// Rejections above the centre, per pixel (stack).
    pub high: Option<&'a Frame>,
    /// Rejections as outliers, per pixel (drizzle).
    pub rejection: Option<&'a Frame>,
    /// Samples that went into each pixel.
    pub count: &'a Frame,
    /// Summed weight at each pixel.
    pub weight: &'a Frame,
    /// Noise against frames, when it could be measured.
    pub noise: Option<&'a NoiseCurve>,
    /// Frames integrated.
    pub frames: usize,
}

#[derive(Serialize)]
struct Summary<'a> {
    frames: usize,
    low: Option<MapStats>,
    high: Option<MapStats>,
    rejection: Option<MapStats>,
    count: MapStats,
    weight: MapStats,
    noise: Option<&'a NoiseCurve>,
    histograms: &'a Histograms,
}

fn save_fits(report: &ReportDir, name: &str, f: &Frame) -> Result<()> {
    let path = report.dir().join(name);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    fits::write(
        &path,
        &Image::mono(f.clone(), Header::new()),
        Format::Float32,
    )
    .map_err(|e| Error::Parse(format!("{}: {e}", path.display())))
}

/// Write every output into the report folder and add the section.
///
/// # Errors
/// If a file cannot be written or a plot cannot be drawn.
pub fn write_report(report: &mut ReportDir, inp: &Input<'_>) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut figures = Vec::new();
    let mut text = Vec::new();
    let maps: Vec<(MapKind, &Frame)> = [
        inp.low.map(|f| (MapKind::RejectionLow, f)),
        inp.high.map(|f| (MapKind::RejectionHigh, f)),
        inp.rejection.map(|f| (MapKind::Rejected, f)),
        Some((MapKind::Count, inp.count)),
        Some((MapKind::Weight, inp.weight)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let prefix = if inp.drizzle { "drizzle_" } else { "" };
    for (kind, f) in &maps {
        let stem = format!("{prefix}{}", kind.stem());
        report.save(&format!("{stem}.png"), &integration::map_png(f, *kind)?)?;
        files.push(format!("{stem}.png"));
        save_fits(report, &format!("{stem}.fits"), f)?;
        files.push(format!("{stem}.fits"));
        if !matches!(kind, MapKind::Weight) {
            report.save(
                &format!("{stem}.csv"),
                integration::value_counts_csv(f).as_bytes(),
            )?;
            files.push(format!("{stem}.csv"));
        }
        figures.push(Figure {
            file: format!("{stem}.png"),
            caption: match (kind, inp.drizzle) {
                (MapKind::Rejected, _) => {
                    "Input pixels rejected as outliers by the second pass, at the output pixel of their centre".to_owned()
                }
                (MapKind::Count, true) => "Input pixels (drops) that reached each output pixel".to_owned(),
                (MapKind::Weight, true) => "Summed drop area times frame weight at each output pixel".to_owned(),
                (MapKind::RejectionLow, _) => "Samples rejected below the centre, per pixel".to_owned(),
                (MapKind::RejectionHigh, _) => "Samples rejected above the centre, per pixel".to_owned(),
                (MapKind::Count, _) => "Samples used, per pixel".to_owned(),
                (MapKind::Weight, _) => "Summed weight of the samples used, per pixel".to_owned(),
            },
        });
    }
    let stats = |f: Option<&Frame>| f.map(integration::map_stats);
    let hist = histogram::histograms(inp.image, 256)?;
    let hname = format!("{prefix}histogram");
    report.save(&format!("{hname}.png"), &histogram::plot_png(&hist)?)?;
    report.save(&format!("{hname}.csv"), histogram::to_csv(&hist).as_bytes())?;
    files.push(format!("{hname}.png"));
    files.push(format!("{hname}.csv"));
    let mut rows = Vec::new();
    if let Some(c) = inp.noise {
        let nname = format!("{prefix}noise");
        report.save(
            &format!("{nname}.png"),
            &crate::plot::encode_png(
                &integration::noise_rgb(c)?,
                integration::NOISE_SIZE.0,
                integration::NOISE_SIZE.1,
            )?,
        )?;
        report.save(
            &format!("{nname}.csv"),
            integration::noise_csv(c).as_bytes(),
        )?;
        files.push(format!("{nname}.png"));
        files.push(format!("{nname}.csv"));
        figures.push(Figure {
            file: format!("{nname}.png"),
            caption: "Noise of a stack against the number of frames, with the square-root law"
                .into(),
        });
        for p in &c.points {
            rows.push(vec![
                p.frames.to_string(),
                num(p.noise, 2),
                num(p.expected, 2),
                num(p.noise / p.expected, 3),
                num(c.sigma1 / p.noise, 2),
                num((p.frames as f64).sqrt(), 2),
            ]);
        }
        text.push(format!(
            "One frame has {:.2} ADU of noise. A stack of {} frames has {:.2} ADU, a reduction of {:.2} \
             against the {:.2} the square root of the number of frames promises ({:.0} per cent of it).",
            c.sigma1,
            c.points.last().map_or(0, |p| p.frames),
            c.points.last().map_or(f64::NAN, |p| p.noise),
            c.reduction(),
            c.theoretical(),
            100.0 * c.reduction() / c.theoretical()
        ));
    }
    figures.push(Figure {
        file: format!("{hname}.png"),
        caption: "Histogram of each channel of the result, linear and stretched".into(),
    });
    let (cs, ws) = (
        integration::map_stats(inp.count),
        integration::map_stats(inp.weight),
    );
    text.push(format!(
        "{} frames integrated. Samples used per pixel: median {}, from {} to {}. Weight per pixel: median {:.2}.",
        inp.frames,
        num(cs.median, 0),
        num(cs.min, 0),
        num(cs.max, 0),
        ws.median
    ));
    for (what, f) in [
        ("low", inp.low),
        ("high", inp.high),
        ("as outliers", inp.rejection),
    ] {
        if let Some(f) = f {
            let s = integration::map_stats(f);
            text.push(format!(
                "Rejected {what}: {:.3} samples per pixel on average, up to {}; {:.1} per cent of the pixels lost none.",
                s.mean,
                num(s.max, 0),
                100.0 * s.zero_fraction
            ));
        }
    }
    let summary = Summary {
        frames: inp.frames,
        low: stats(inp.low),
        high: stats(inp.high),
        rejection: stats(inp.rejection),
        count: cs,
        weight: ws,
        noise: inp.noise,
        histograms: &hist,
    };
    let json = serde_json::to_string_pretty(&summary).map_err(|e| Error::Parse(e.to_string()))?;
    let jname = format!("{prefix}integration.json");
    write_file(&report.dir().join(&jname), json.as_bytes())?;
    files.push(jname);
    report.put(Section {
        id: if inp.drizzle {
            "integration-drizzle"
        } else {
            "integration"
        }
        .into(),
        title: if inp.drizzle {
            "Integration (drizzle)"
        } else {
            "Integration (stack)"
        }
        .into(),
        text,
        figures,
        files: files.clone(),
        table: (!rows.is_empty()).then(|| Table {
            header: [
                "frames per half",
                "noise ADU",
                "1/sqrt(N) line",
                "measured / line",
                "reduction",
                "sqrt(N)",
            ]
            .map(String::from)
            .to_vec(),
            rows,
            flag_column: None,
        }),
    });
    report.write()?;
    Ok(files)
}
