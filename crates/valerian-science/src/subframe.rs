// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Subframe analysis in the manner of PixInsight's SubframeSelector.
//!
//! Each frame is measured (stars, FWHM, eccentricity, sky level, noise,
//! registration residual), given an SNR weight, and approved or rejected
//! against the user's limits. The table of verdicts is exported as CSV and
//! JSON, drawn as per-frame plots against frame index and time, and read back
//! by `valerian stack --approve` so the decision feeds the stack.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use valerian_core::fits::Image;
use valerian_core::stack::{FrameReport, StackOptions, Status, register_frames};
use valerian_core::stats;

use crate::grader::{self, Grade};
use crate::plot::{self, Guide, Marker, Panel};
use crate::report::{Figure, ReportDir, Section, Table};
use crate::table::{csv_text, num, split_csv_line};
use crate::timeutil;
use crate::{Error, Result};

/// JSON `null` reads back as NaN, which is how a missing measurement is written.
fn null_nan<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

/// What was measured on one frame.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FrameMetrics {
    /// Position in the input list, from 0.
    pub index: usize,
    /// File name.
    pub name: String,
    /// `DATE-OBS`, if the header has it.
    pub time_utc: Option<String>,
    /// Seconds since the Unix epoch.
    pub epoch_s: Option<f64>,
    /// Stars detected.
    pub stars: usize,
    /// Median FWHM, pixels.
    #[serde(deserialize_with = "null_nan")]
    pub fwhm_px: f64,
    /// Median ellipticity, `1 - b/a`.
    #[serde(deserialize_with = "null_nan")]
    pub ellipticity: f64,
    /// Sky background median, ADU.
    #[serde(deserialize_with = "null_nan")]
    pub sky_adu: f64,
    /// Pixel noise, ADU.
    #[serde(deserialize_with = "null_nan")]
    pub noise_adu: f64,
    /// SNR weight, median 1: `(transparency / noise)^2` relative to the median;
    /// empty for a frame that did not register (its transparency is unknown).
    #[serde(deserialize_with = "null_nan")]
    pub snr_weight: f64,
    /// Registration residual to the reference, pixels (0 for the reference).
    #[serde(deserialize_with = "null_nan")]
    pub registration_rms_px: f64,
    /// Registered to the reference.
    pub registered: bool,
    /// Why registration was skipped, if it was.
    pub note: String,
}

/// The limits frames are judged against; `None` is no limit.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Limits {
    /// Reject frames with a larger FWHM, pixels.
    pub max_fwhm_px: Option<f64>,
    /// Reject frames with a larger ellipticity.
    pub max_ellipticity: Option<f64>,
    /// Reject frames with fewer stars.
    pub min_stars: Option<usize>,
    /// Reject frames with more noise, ADU.
    pub max_noise_adu: Option<f64>,
    /// Reject frames with a lower SNR weight.
    pub min_snr_weight: Option<f64>,
    /// Reject frames with a larger registration residual, pixels.
    pub max_registration_rms_px: Option<f64>,
    /// Reject frames further than this many robust sigmas from the median in
    /// the bad direction of FWHM, ellipticity, noise, SNR weight, star count
    /// and registration residual, or either way for the sky level.
    pub outlier_sigma: Option<f64>,
}

/// A frame and its verdict.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Row {
    /// The measurements.
    #[serde(flatten)]
    pub metrics: FrameMetrics,
    /// Whether the frame goes into the stack.
    pub approved: bool,
    /// Why not, one entry per failed test.
    pub reasons: Vec<String>,
    /// The Seestar grader's verdict, when `--grade` ran and the frame could be graded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
}

/// Median and robust sigma (1.4826 MAD) of each plotted quantity.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Centre {
    /// Median.
    pub median: f64,
    /// Robust standard deviation.
    pub sigma: f64,
}

/// The whole analysis.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Analysis {
    /// The limits used.
    pub limits: Limits,
    /// One row per frame.
    pub frames: Vec<Row>,
    /// FWHM centre, over registered frames.
    pub fwhm: Centre,
    /// Ellipticity centre.
    pub ellipticity: Centre,
    /// Star-count centre.
    pub stars: Centre,
    /// Sky-level centre.
    pub sky: Centre,
    /// Noise centre.
    pub noise: Centre,
    /// Registration-residual centre.
    pub registration_rms: Centre,
}

impl Analysis {
    /// Frames approved.
    #[must_use]
    pub fn approved(&self) -> usize {
        self.frames.iter().filter(|r| r.approved).count()
    }
}

/// Which quantity the horizontal axis shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XAxis {
    /// Frame number.
    Index,
    /// Hours since the first frame.
    Time,
}

/// The time stamp of a frame from its header.
#[must_use]
pub fn epoch_of(img: &Image) -> Option<f64> {
    img.header
        .get_str("DATE-OBS")
        .or_else(|| img.header.get_str("DATE_OBS"))
        .and_then(timeutil::parse_date_obs)
}

/// Pixel noise of a frame, ADU: the standard deviation of the differences of
/// pixels two columns apart (which cancels smooth sky and nebulosity), clipped
/// iteratively at 3 sigma so stars do not count, over the square root of 2 and
/// corrected for the clipping. Unlike a median absolute deviation it is not
/// quantised on data that is a coarse multiple of the ADU (a 12-bit camera
/// scaled to 16 bits), so frames can be compared. For a colour image the
/// middle plane is measured. `NaN` when the frame is too small.
#[must_use]
pub fn noise_of(img: &Image) -> f64 {
    let plane = &img.planes[img.planes.len() / 2];
    let (w, h) = (plane.width, plane.height);
    let rows = (h / 1024).max(1);
    let mut d: Vec<f32> = Vec::with_capacity(w * h / rows);
    for y in (0..h).step_by(rows) {
        let row = plane.row(y);
        for x in 0..w.saturating_sub(2) {
            let (a, b) = (row[x], row[x + 2]);
            if a.is_finite() && b.is_finite() {
                d.push(b - a);
            }
        }
    }
    if d.len() < 1000 {
        return f64::NAN;
    }
    let mut mid = f64::from(stats::median_in_place(&mut d.clone()));
    let mut dev: Vec<f32> = d
        .iter()
        .map(|v| (f64::from(*v) - mid).abs() as f32)
        .collect();
    let mut sigma = 1.4826 * f64::from(stats::median_in_place(&mut dev));
    for _ in 0..6 {
        let kept: Vec<f64> = d
            .iter()
            .map(|v| f64::from(*v))
            .filter(|v| (v - mid).abs() <= 3.0 * sigma)
            .collect();
        if kept.len() < 100 {
            break;
        }
        let n = kept.len() as f64;
        mid = kept.iter().sum::<f64>() / n;
        sigma = (kept.iter().map(|v| (v - mid).powi(2)).sum::<f64>() / n).sqrt();
    }
    // A 3 sigma truncated Gaussian has 0.9866 of the standard deviation.
    sigma / 0.9866 / std::f64::consts::SQRT_2
}

/// Median and robust sigma of the finite values.
#[must_use]
pub fn centre(values: impl IntoIterator<Item = f64>) -> Centre {
    let mut v: Vec<f32> = values
        .into_iter()
        .filter(|x| x.is_finite())
        .map(|x| x as f32)
        .collect();
    if v.is_empty() {
        return Centre {
            median: f64::NAN,
            sigma: f64::NAN,
        };
    }
    let median = stats::median_in_place(&mut v);
    let mut dev: Vec<f32> = v.iter().map(|x| (x - median).abs()).collect();
    let mad = stats::median_in_place(&mut dev);
    Centre {
        median: f64::from(median),
        sigma: 1.4826 * f64::from(mad),
    }
}

/// Measurements from the per-frame reports of a registration or a stack.
/// `names`, `epochs` and `noises` (from [`noise_of`]; a missing or `NaN` one
/// falls back to the report's) are indexed like `reports`.
#[must_use]
pub fn metrics_from_reports(
    reports: &[FrameReport],
    names: &[String],
    epochs: &[Option<f64>],
    noises: &[f64],
) -> Vec<FrameMetrics> {
    let mut out: Vec<FrameMetrics> = reports
        .iter()
        .map(|r| {
            let registered = matches!(r.status, Status::Reference | Status::Stacked);
            let note = match &r.status {
                Status::Skipped(why) if why.is_empty() => "not registered".to_owned(),
                Status::Skipped(why) => why.clone(),
                _ => String::new(),
            };
            let epoch = epochs.get(r.index).copied().flatten();
            let noise = noises
                .get(r.index)
                .copied()
                .filter(|n| n.is_finite())
                .unwrap_or_else(|| f64::from(r.noise));
            let t = noise * r.flux_scale;
            FrameMetrics {
                index: r.index,
                name: names.get(r.index).cloned().unwrap_or_default(),
                time_utc: epoch.map(timeutil::format_utc),
                epoch_s: epoch,
                stars: r.stars,
                fwhm_px: f64::from(r.fwhm),
                ellipticity: f64::from(r.ellipticity),
                sky_adu: f64::from(r.sky),
                noise_adu: noise,
                snr_weight: if registered && t > 0.0 {
                    1.0 / (t * t)
                } else {
                    f64::NAN
                },
                registration_rms_px: r.registration_rms,
                registered,
                note,
            }
        })
        .collect();
    // Relative to the median of the registered frames, as SubframeSelector does.
    let m = centre(out.iter().filter(|f| f.registered).map(|f| f.snr_weight)).median;
    if m.is_finite() && m > 0.0 {
        for f in &mut out {
            f.snr_weight /= m;
        }
    }
    out
}

/// Register `frames` to the sharpest and measure every one.
///
/// # Errors
/// If the frames are inconsistent or fewer than two register.
pub fn measure(
    frames: &[Image],
    names: &[String],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Vec<FrameMetrics>> {
    let reg = register_frames(frames, opts, log).map_err(|e| Error::Empty(e.to_string()))?;
    let epochs: Vec<Option<f64>> = frames.iter().map(epoch_of).collect();
    let noises: Vec<f64> = frames.iter().map(noise_of).collect();
    Ok(metrics_from_reports(&reg.reports, names, &epochs, &noises))
}

/// Judge every frame against `limits`.
#[must_use]
pub fn evaluate(frames: &[FrameMetrics], limits: &Limits) -> Analysis {
    let reg = || frames.iter().filter(|f| f.registered);
    let fwhm = centre(reg().map(|f| f.fwhm_px));
    let ellipticity = centre(reg().map(|f| f.ellipticity));
    let stars = centre(reg().map(|f| f.stars as f64));
    let sky = centre(reg().map(|f| f.sky_adu));
    let noise = centre(reg().map(|f| f.noise_adu));
    let weight = centre(reg().map(|f| f.snr_weight));
    let registration_rms = centre(reg().map(|f| f.registration_rms_px));
    let k = limits.outlier_sigma;
    // Robust z-score of `v` against `c`; 0 when the spread is unusable.
    let z = |v: f64, c: &Centre| {
        if c.sigma > 0.0 && v.is_finite() {
            (v - c.median) / c.sigma
        } else {
            0.0
        }
    };
    let rows = frames
        .iter()
        .map(|f| {
            let mut why = Vec::new();
            if f.registered {
                if let Some(m) = limits.max_fwhm_px
                    && f.fwhm_px > m
                {
                    why.push(format!("FWHM {:.2} px above {m}", f.fwhm_px));
                }
                if let Some(m) = limits.max_ellipticity
                    && f.ellipticity > m
                {
                    why.push(format!("ellipticity {:.3} above {m}", f.ellipticity));
                }
                if let Some(m) = limits.min_stars
                    && f.stars < m
                {
                    why.push(format!("{} stars below {m}", f.stars));
                }
                if let Some(m) = limits.max_noise_adu
                    && f.noise_adu > m
                {
                    why.push(format!("noise {:.2} above {m}", f.noise_adu));
                }
                if let Some(m) = limits.min_snr_weight
                    && f.snr_weight < m
                {
                    why.push(format!("SNR weight {:.3} below {m}", f.snr_weight));
                }
                if let Some(m) = limits.max_registration_rms_px
                    && f.registration_rms_px > m
                {
                    why.push(format!(
                        "registration residual {:.3} px above {m}",
                        f.registration_rms_px
                    ));
                }
                if let Some(k) = k {
                    let tests = [
                        ("FWHM", z(f.fwhm_px, &fwhm), false),
                        ("ellipticity", z(f.ellipticity, &ellipticity), false),
                        ("noise", z(f.noise_adu, &noise), false),
                        (
                            "registration residual",
                            z(f.registration_rms_px, &registration_rms),
                            false,
                        ),
                        ("star count", -z(f.stars as f64, &stars), false),
                        ("SNR weight", -z(f.snr_weight, &weight), false),
                        ("sky level", z(f.sky_adu, &sky), true),
                    ];
                    for (what, s, both) in tests {
                        if (both && s.abs() > k) || (!both && s > k) {
                            why.push(format!("{what} {:.1} sigma from the median", s.abs()));
                        }
                    }
                }
            } else {
                why.push(f.note.clone());
            }
            Row {
                metrics: f.clone(),
                approved: why.is_empty(),
                reasons: why,
                grade: None,
            }
        })
        .collect();
    Analysis {
        limits: limits.clone(),
        frames: rows,
        fwhm,
        ellipticity,
        stars,
        sky,
        noise,
        registration_rms,
    }
}

/// Attach the grader's verdicts, one per row (`None` for a frame it could not
/// grade), and reject the frames `policy` fails, adding the reasons.
pub fn apply_grades(a: &mut Analysis, grades: &[Option<Grade>], policy: &grader::Policy) {
    for (row, g) in a.frames.iter_mut().zip(grades) {
        row.grade = *g;
        if let Some(g) = g {
            let why = policy.reasons(g);
            row.approved &= why.is_empty();
            row.reasons.extend(why);
        }
    }
}

/// Whether any row carries a grade.
fn graded(a: &Analysis) -> bool {
    a.frames.iter().any(|r| r.grade.is_some())
}

/// Grade cells for one row: the score then the five defects, empty when ungraded.
fn grade_cells(g: Option<Grade>) -> Vec<String> {
    let v = g.map_or_else(
        || vec![None; 6],
        |g| {
            let mut v = vec![Some(g.score)];
            v.extend(g.defects.as_array().map(Some));
            v
        },
    );
    v.into_iter()
        .map(|x| x.map_or_else(String::new, |x| format!("{x:.3}")))
        .collect()
}

/// The table as CSV; the `approved` column is what `valerian stack --approve` reads.
#[must_use]
pub fn to_csv(a: &Analysis) -> String {
    let with_grades = graded(a);
    let rows: Vec<Vec<String>> = a
        .frames
        .iter()
        .map(|r| {
            let m = &r.metrics;
            let mut v = vec![
                m.index.to_string(),
                m.name.clone(),
                m.time_utc.clone().unwrap_or_default(),
                m.stars.to_string(),
                num(m.fwhm_px, 3),
                num(m.ellipticity, 4),
                num(m.sky_adu, 2),
                num(m.noise_adu, 3),
                num(m.snr_weight, 4),
                num(m.registration_rms_px, 4),
            ];
            if with_grades {
                v.extend(grade_cells(r.grade));
            }
            v.push(if r.approved { "yes" } else { "no" }.to_owned());
            v.push(r.reasons.join("; "));
            v
        })
        .collect();
    let mut header = vec![
        "index",
        "name",
        "time_utc",
        "stars",
        "fwhm_px",
        "ellipticity",
        "sky_adu",
        "noise_adu",
        "snr_weight",
        "registration_rms_px",
    ];
    if with_grades {
        header.extend(["grade_score"]);
        header.extend(grader::CLASSES);
    }
    header.extend(["approved", "reasons"]);
    csv_text(&header, &rows)
}

/// The analysis as JSON.
///
/// # Errors
/// If serialisation fails.
pub fn to_json(a: &Analysis) -> Result<String> {
    serde_json::to_string_pretty(a).map_err(|e| Error::Parse(e.to_string()))
}

/// Read an analysis back from its JSON.
///
/// # Errors
/// If the text is not a subframe analysis.
pub fn from_json(text: &str) -> Result<Analysis> {
    serde_json::from_str(text).map_err(|e| Error::Parse(format!("not a subframe table: {e}")))
}

/// Read the approvals of a table, by file name, from the CSV or JSON this
/// module writes. In the CSV the `approved` column may hold yes/no, true/false
/// or 1/0, so a table can be edited by hand.
///
/// # Errors
/// If the file cannot be read or has no `name` and `approved` columns.
pub fn read_approvals(path: &Path) -> Result<HashMap<String, bool>> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        return Ok(from_json(&text)?
            .frames
            .into_iter()
            .map(|r| (r.metrics.name, r.approved))
            .collect());
    }
    let mut lines = text.lines();
    let header = split_csv_line(lines.next().unwrap_or_default());
    let col = |n: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(n));
    let (Some(ni), Some(ai)) = (col("name"), col("approved")) else {
        return Err(Error::Parse(format!(
            "{}: the table needs `name` and `approved` columns",
            path.display()
        )));
    };
    let mut out = HashMap::new();
    for l in lines.filter(|l| !l.trim().is_empty()) {
        let f = split_csv_line(l);
        let (Some(n), Some(a)) = (f.get(ni), f.get(ai)) else {
            continue;
        };
        let yes = matches!(
            a.trim().to_ascii_lowercase().as_str(),
            "yes" | "y" | "true" | "1" | "approved"
        );
        out.insert(n.clone(), yes);
    }
    Ok(out)
}

/// What one panel of the plot shows.
struct Spec<'a> {
    title: &'a str,
    unit: &'a str,
    value: fn(&FrameMetrics) -> f64,
    /// Limit line and its name.
    limit: Option<(f64, &'a str)>,
    median: f64,
    /// Only registered frames have a value (the residual).
    registered_only: bool,
}

fn panel_for(a: &Analysis, x: XAxis, s: &Spec<'_>) -> Panel {
    let t0 = a
        .frames
        .iter()
        .filter_map(|r| r.metrics.epoch_s)
        .fold(f64::INFINITY, f64::min);
    Panel {
        title: s.title.to_owned(),
        x_label: match x {
            XAxis::Index => "frame".into(),
            XAxis::Time => "hours since the first frame".into(),
        },
        y_label: s.unit.to_owned(),
        points: a
            .frames
            .iter()
            .map(|r| Marker {
                x: match x {
                    XAxis::Index => r.metrics.index as f64,
                    XAxis::Time => (r.metrics.epoch_s.unwrap_or(t0) - t0) / 3600.0,
                },
                y: if r.metrics.registered || !s.registered_only {
                    (s.value)(&r.metrics)
                } else {
                    f64::NAN
                },
                flagged: !r.approved,
            })
            .collect(),
        guides: s
            .limit
            .map(|(y, name)| Guide {
                y,
                label: format!("{name} limit"),
            })
            .into_iter()
            .collect(),
        median: s.median.is_finite().then_some(s.median),
    }
}

/// Whether every frame has a time stamp, so the time axis can be drawn.
#[must_use]
pub fn has_times(a: &Analysis) -> bool {
    a.frames.len() > 1 && a.frames.iter().all(|r| r.metrics.epoch_s.is_some())
}

/// Size of the per-frame plot sheet, pixels.
pub const SHEET: (u32, u32) = (1400, 1480);

/// The seven per-frame plots and a summary, as one PNG.
///
/// # Errors
/// If drawing fails.
pub fn plot_png(a: &Analysis, x: XAxis) -> Result<Vec<u8>> {
    plot::encode_png(&plot_rgb(a, x)?, SHEET.0, SHEET.1)
}

/// The per-frame plot sheet as RGB bytes of [`SHEET`] size.
///
/// # Errors
/// If drawing fails.
pub fn plot_rgb(a: &Analysis, x: XAxis) -> Result<Vec<u8>> {
    let l = &a.limits;
    let spec = |title, unit, value, limit, median, registered_only| Spec {
        title,
        unit,
        value,
        limit,
        median,
        registered_only,
    };
    let panels = [
        spec(
            "FWHM",
            "pixels",
            |m| m.fwhm_px,
            l.max_fwhm_px.map(|v| (v, "FWHM")),
            a.fwhm.median,
            false,
        ),
        spec(
            "Eccentricity",
            "1 - b/a",
            |m| m.ellipticity,
            l.max_ellipticity.map(|v| (v, "ellipticity")),
            a.ellipticity.median,
            false,
        ),
        spec(
            "Star count",
            "stars",
            |m| m.stars as f64,
            l.min_stars.map(|v| (v as f64, "star count")),
            a.stars.median,
            false,
        ),
        spec(
            "Background median",
            "ADU",
            |m| m.sky_adu,
            None,
            a.sky.median,
            false,
        ),
        spec(
            "Noise estimate",
            "ADU",
            |m| m.noise_adu,
            l.max_noise_adu.map(|v| (v, "noise")),
            a.noise.median,
            false,
        ),
        spec(
            "SNR weight",
            "relative to the median",
            |m| m.snr_weight,
            l.min_snr_weight.map(|v| (v, "weight")),
            1.0,
            false,
        ),
        spec(
            "Registration residual",
            "pixels rms",
            |m| m.registration_rms_px,
            l.max_registration_rms_px.map(|v| (v, "residual")),
            a.registration_rms.median,
            true,
        ),
    ]
    .map(|s| panel_for(a, x, &s));
    let approved = a.approved();
    let mut summary = vec![
        format!(
            "{} frames, {} approved, {} rejected",
            a.frames.len(),
            approved,
            a.frames.len() - approved
        ),
        format!(
            "median FWHM {:.2} px, ellipticity {:.3}",
            a.fwhm.median, a.ellipticity.median
        ),
        format!(
            "median noise {:.2} ADU, sky {:.1} ADU",
            a.noise.median, a.sky.median
        ),
        "blue dot: approved   red cross: rejected".to_owned(),
        "dashed grey: median   dashed red: limit".to_owned(),
    ];
    if let Some(k) = l.outlier_sigma {
        summary.push(format!("outliers: beyond {k} robust sigma"));
    }
    plot::render_rgb(SHEET.0, SHEET.1, |root| {
        let cells = root.split_evenly((4, 2));
        for (cell, p) in cells.iter().zip(&panels) {
            plot::draw_panel(cell, p)?;
        }
        plot::draw_text_block(&cells[7], "Summary", &summary)
    })
}

/// File names written by [`write_report`].
#[derive(Clone, Debug, Default)]
pub struct Outputs {
    /// The files written, relative to the report folder.
    pub files: Vec<String>,
}

/// Write the CSV, JSON and PNG plots into the report folder and add the
/// section to its HTML report.
///
/// # Errors
/// If a file cannot be written or a plot cannot be drawn.
pub fn write_report(report: &mut ReportDir, a: &Analysis) -> Result<Outputs> {
    if a.frames.is_empty() {
        return Err(Error::Empty("no frames to report".into()));
    }
    let mut files = vec!["subframes.csv".to_owned(), "subframes.json".to_owned()];
    report.save("subframes.csv", to_csv(a).as_bytes())?;
    report.save("subframes.json", to_json(a)?.as_bytes())?;
    let mut figures = Vec::new();
    report.save("subframes_index.png", &plot_png(a, XAxis::Index)?)?;
    figures.push(Figure {
        file: "subframes_index.png".into(),
        caption: "Per-frame measurements against frame number".into(),
    });
    files.push("subframes_index.png".into());
    if has_times(a) {
        report.save("subframes_time.png", &plot_png(a, XAxis::Time)?)?;
        figures.push(Figure {
            file: "subframes_time.png".into(),
            caption: "The same measurements against time".into(),
        });
        files.push("subframes_time.png".into());
    }
    let with_grades = graded(a);
    let rows = a
        .frames
        .iter()
        .map(|r| {
            let m = &r.metrics;
            let mut v = vec![
                m.index.to_string(),
                m.name.clone(),
                m.stars.to_string(),
                num(m.fwhm_px, 2),
                num(m.ellipticity, 3),
                num(m.sky_adu, 1),
                num(m.noise_adu, 2),
                num(m.snr_weight, 3),
                num(m.registration_rms_px, 3),
            ];
            if with_grades {
                v.push(grade_cells(r.grade).swap_remove(0));
            }
            v.push(if r.approved { "approved" } else { "rejected" }.to_owned());
            v.push(r.reasons.join("; "));
            v
        })
        .collect();
    report.put(Section {
        id: "subframes".into(),
        title: "Subframe analysis".into(),
        text: vec![format!(
            "{} of {} frames approved. Rejected frames are marked with red crosses; the reasons are in the last column.",
            a.approved(),
            a.frames.len()
        )],
        figures,
        files: files.clone(),
        table: Some(Table {
            header: {
                let mut h = vec![
                    "#", "frame", "stars", "FWHM px", "ecc", "sky ADU", "noise ADU", "SNR wt",
                    "reg rms px",
                ];
                if with_grades {
                    h.push("grade");
                }
                h.extend(["verdict", "reasons"]);
                h.into_iter().map(String::from).collect()
            },
            rows,
            flag_column: Some(if with_grades { 10 } else { 9 }),
        }),
    });
    report.write()?;
    Ok(Outputs { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(i: usize, fwhm: f64, stars: usize) -> FrameMetrics {
        FrameMetrics {
            index: i,
            name: format!("f{i}.fits"),
            time_utc: Some(timeutil::format_utc(1.6e9 + 300.0 * i as f64)),
            epoch_s: Some(1.6e9 + 300.0 * i as f64),
            stars,
            fwhm_px: fwhm,
            ellipticity: 0.1,
            sky_adu: 500.0,
            noise_adu: 5.0,
            snr_weight: 1.0,
            registration_rms_px: 0.1,
            registered: true,
            note: String::new(),
        }
    }

    #[test]
    fn grades_reject_add_columns_and_round_trip() {
        let mut a = evaluate(
            &[
                metric(0, 2.0, 100),
                metric(1, 2.0, 100),
                metric(2, 2.0, 100),
            ],
            &Limits::default(),
        );
        assert!(
            !to_csv(&a).contains("grade_score"),
            "no grade columns without a grade"
        );
        let good = Grade {
            score: 0.9,
            ..Grade::default()
        };
        let poor = Grade {
            score: 0.2,
            ..Grade::default()
        };
        let policy = grader::Policy::default();
        apply_grades(&mut a, &[Some(good), Some(poor), None], &policy);
        assert_eq!(
            a.frames.iter().map(|r| r.approved).collect::<Vec<_>>(),
            [true, false, true],
            "an ungraded frame is left to the other limits"
        );
        assert!(a.frames[1].reasons[0].contains("grade 0.20 below 0.5"));
        let csv = to_csv(&a);
        let head = csv.lines().next().expect("header");
        assert!(head.contains("grade_score,cloud_haze,trail,blur,dew,star_poor,approved"));
        assert_eq!(
            csv.lines().nth(3).expect("row").split(',').nth(10),
            Some("")
        );
        let back = from_json(&to_json(&a).expect("json")).expect("reads back");
        assert_eq!(back.frames[0].grade, Some(good));
        assert_eq!(back.frames[2].grade, None);
    }

    fn set() -> Vec<FrameMetrics> {
        let mut v: Vec<FrameMetrics> = (0..12)
            .map(|i| metric(i, 3.0 + 0.05 * (i % 3) as f64, 200 + 3 * (i % 4)))
            .collect();
        v[5].fwhm_px = 6.0;
        v[8].stars = 40;
        v[10].registered = false;
        v[10].note = "only 4 stars".into();
        v
    }

    #[test]
    fn limits_and_outliers_mark_the_right_frames() {
        let a = evaluate(
            &set(),
            &Limits {
                max_fwhm_px: Some(4.0),
                outlier_sigma: Some(4.0),
                ..Limits::default()
            },
        );
        let bad: Vec<usize> = a
            .frames
            .iter()
            .filter(|r| !r.approved)
            .map(|r| r.metrics.index)
            .collect();
        assert_eq!(bad, vec![5, 8, 10], "{:?}", a.frames[5].reasons);
        assert!(a.frames[5].reasons[0].starts_with("FWHM 6.00"));
        assert_eq!(a.frames[10].reasons, vec!["only 4 stars"]);
    }

    #[test]
    fn the_noise_estimate_recovers_sigma_through_stars_and_quantisation() {
        use valerian_core::testutil::Rng;
        let mut rng = Rng::new(7);
        let (w, h) = (600, 500);
        // Noise of 11.3 ADU on a 16-step grid (12-bit data scaled to 16 bits).
        let mut data: Vec<f32> = (0..w * h)
            .map(|_| ((500.0_f64 + rng.gauss(0.0, 11.3)) / 16.0).round() as f32 * 16.0)
            .collect();
        for k in 0..200 {
            data[(37 * k) % (w * h)] += 4000.0;
        }
        let img = Image::mono(
            valerian_core::Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            },
            valerian_core::fits::Header::new(),
        );
        // Rounding to 16 adds 16/sqrt(12) = 4.6 in quadrature.
        let want = (11.3_f64.powi(2) + 16.0 * 16.0 / 12.0).sqrt();
        let got = noise_of(&img);
        assert!((got - want).abs() / want < 0.04, "{got} vs {want}");
    }

    #[test]
    fn no_limits_approve_every_registered_frame() {
        let a = evaluate(&set(), &Limits::default());
        assert_eq!(a.approved(), 11);
    }

    #[test]
    fn csv_and_json_round_trip_the_verdicts() {
        let a = evaluate(
            &set(),
            &Limits {
                max_fwhm_px: Some(4.0),
                ..Limits::default()
            },
        );
        let dir = tempfile::tempdir().expect("tmp");
        let (c, j) = (dir.path().join("t.csv"), dir.path().join("t.json"));
        std::fs::write(&c, to_csv(&a)).expect("csv");
        std::fs::write(&j, to_json(&a).expect("json")).expect("json file");
        let from_c = read_approvals(&c).expect("csv read");
        let from_j = read_approvals(&j).expect("json read");
        assert_eq!(from_c, from_j);
        assert!(!from_c["f5.fits"]);
        assert!(from_c["f0.fits"]);
        assert_eq!(from_json(&to_json(&a).expect("json")).expect("parse"), a);
    }

    #[test]
    fn nan_survives_json() {
        let mut v = set();
        v[0].fwhm_px = f64::NAN;
        let a = evaluate(&v, &Limits::default());
        let back = from_json(&to_json(&a).expect("json")).expect("parse");
        assert!(back.frames[0].metrics.fwhm_px.is_nan());
    }

    #[test]
    fn the_report_holds_plots_csv_and_json() {
        let a = evaluate(&set(), &Limits::default());
        let dir = tempfile::tempdir().expect("tmp");
        let mut r = ReportDir::open(dir.path()).expect("open");
        let out = write_report(&mut r, &a).expect("report");
        for f in &out.files {
            assert!(dir.path().join(f).is_file(), "{f}");
        }
        assert!(out.files.contains(&"subframes_time.png".to_owned()));
        assert!(dir.path().join("report.html").is_file());
    }
}
