// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Pure-Rust plotting onto an RGB buffer, encoded as PNG.
//!
//! Colours are the Okabe-Ito set, which stays readable with colour-vision
//! deficiency, on a white ground so the plots print and sit in documents.

use std::sync::Once;

use plotters::coord::Shift;
use plotters::prelude::*;
use plotters::style::{FontStyle, register_font};

use crate::{Error, Result};

/// Drawing surface used by every plot.
pub type Area<'a> = DrawingArea<BitMapBackend<'a>, Shift>;

/// Blue of the Okabe-Ito set: approved or normal data.
pub const BLUE: RGBColor = RGBColor(0, 114, 178);
/// Vermillion: rejected data and limits.
pub const RED: RGBColor = RGBColor(213, 94, 0);
/// Bluish green: fits and references.
pub const GREEN: RGBColor = RGBColor(0, 158, 115);
/// Orange.
pub const ORANGE: RGBColor = RGBColor(230, 159, 0);
/// Mid grey for guides.
pub const GREY: RGBColor = RGBColor(110, 110, 110);

const FAMILY: &str = "sans-serif";
const PLEX: &[u8] = include_bytes!("../../valerian-gui/assets/fonts/IBMPlexSans-Regular.ttf");
const PLEX_BOLD: &[u8] = include_bytes!("../../valerian-gui/assets/fonts/IBMPlexSans-SemiBold.ttf");

fn ensure_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Both register under the family plotters asks for by default.
        let _ = register_font(FAMILY, FontStyle::Normal, PLEX);
        let _ = register_font(FAMILY, FontStyle::Bold, PLEX_BOLD);
    });
}

/// Draw on a white `width` x `height` canvas and return the RGB bytes.
///
/// # Errors
/// The drawing closure's error.
pub fn render_rgb<F>(width: u32, height: u32, draw: F) -> Result<Vec<u8>>
where
    F: FnOnce(&Area<'_>) -> Result<()>,
{
    ensure_fonts();
    let mut buf = vec![0u8; width as usize * height as usize * 3];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (width, height)).into_drawing_area();
        root.fill(&WHITE).map_err(Error::plot)?;
        draw(&root)?;
        root.present().map_err(Error::plot)?;
    }
    Ok(buf)
}

/// Draw on top of an existing RGB buffer of `width` x `height` pixels and
/// return it.
///
/// # Errors
/// The drawing closure's error.
pub fn render_rgb_over<F>(width: u32, height: u32, mut base: Vec<u8>, draw: F) -> Result<Vec<u8>>
where
    F: FnOnce(&Area<'_>) -> Result<()>,
{
    ensure_fonts();
    {
        let root = BitMapBackend::with_buffer(&mut base, (width, height)).into_drawing_area();
        draw(&root)?;
        root.present().map_err(Error::plot)?;
    }
    Ok(base)
}

/// Draw on a white `width` x `height` canvas and return the PNG.
///
/// # Errors
/// The drawing closure's error, or a PNG encoding failure.
pub fn render<F>(width: u32, height: u32, draw: F) -> Result<Vec<u8>>
where
    F: FnOnce(&Area<'_>) -> Result<()>,
{
    encode_png(&render_rgb(width, height, draw)?, width, height)
}

/// Encode an RGB buffer as PNG.
///
/// # Errors
/// If the encoder fails.
pub fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(Error::plot)?;
        w.write_image_data(rgb).map_err(Error::plot)?;
    }
    Ok(out)
}

/// One plotted measurement.
#[derive(Clone, Copy, Debug)]
pub struct Marker {
    /// Horizontal value.
    pub x: f64,
    /// Vertical value; non-finite values are not drawn.
    pub y: f64,
    /// Drawn as a red cross instead of a blue dot.
    pub flagged: bool,
}

/// A horizontal guide line (a threshold).
#[derive(Clone, Debug)]
pub struct Guide {
    /// Height of the line.
    pub y: f64,
    /// What it is.
    pub label: String,
}

/// A scatter-with-line panel of one quantity.
#[derive(Clone, Debug, Default)]
pub struct Panel {
    /// Title above the panel.
    pub title: String,
    /// Horizontal axis caption.
    pub x_label: String,
    /// Vertical axis caption.
    pub y_label: String,
    /// The data, in order.
    pub points: Vec<Marker>,
    /// Threshold lines drawn dashed in red.
    pub guides: Vec<Guide>,
    /// A reference line (the median) drawn dashed in grey.
    pub median: Option<f64>,
}

/// A tick label with sensible digits for its size.
#[must_use]
pub fn tick(v: f64) -> String {
    let a = v.abs();
    if a < 1e-9 {
        "0".into()
    } else if v.fract() == 0.0 && a < 1e5 {
        format!("{v:.0}")
    } else if !(1e-3..1e5).contains(&a) {
        format!("{v:.1e}")
    } else if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

/// Padded `(lo, hi)` of the finite values; a flat series gets a visible span.
#[must_use]
pub fn padded_range(values: impl IntoIterator<Item = f64>) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in values.into_iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    let span = hi - lo;
    let pad = if span > 0.0 {
        span * 0.08
    } else {
        lo.abs().max(1.0) * 0.05
    };
    (lo - pad, hi + pad)
}

/// Draw one panel into `area`.
///
/// # Errors
/// If plotters fails to draw.
pub fn draw_panel(area: &Area<'_>, panel: &Panel) -> Result<()> {
    let (x0, x1) = padded_range(panel.points.iter().map(|p| p.x));
    let ys = panel
        .points
        .iter()
        .map(|p| p.y)
        .chain(panel.guides.iter().map(|g| g.y))
        .chain(panel.median);
    let (y0, y1) = padded_range(ys);
    let mut chart = ChartBuilder::on(area)
        .caption(
            &panel.title,
            (FAMILY, 15).into_font().style(FontStyle::Bold),
        )
        .margin(6)
        .x_label_area_size(30)
        .y_label_area_size(56)
        .build_cartesian_2d(x0..x1, y0..y1)
        .map_err(Error::plot)?;
    chart
        .configure_mesh()
        .x_labels(8)
        .y_labels(6)
        .light_line_style(WHITE)
        .bold_line_style(RGBColor(215, 215, 215))
        .x_desc(&panel.x_label)
        .y_desc(&panel.y_label)
        .label_style((FAMILY, 11))
        .axis_desc_style((FAMILY, 12))
        .x_label_formatter(&|v| tick(*v))
        .y_label_formatter(&|v| tick(*v))
        .draw()
        .map_err(Error::plot)?;
    let dash = |c: RGBColor| ShapeStyle::from(&c).stroke_width(1);
    if let Some(m) = panel.median {
        chart
            .draw_series(DashedLineSeries::new([(x0, m), (x1, m)], 5, 4, dash(GREY)))
            .map_err(Error::plot)?;
    }
    for g in &panel.guides {
        chart
            .draw_series(DashedLineSeries::new(
                [(x0, g.y), (x1, g.y)],
                6,
                3,
                dash(RED),
            ))
            .map_err(Error::plot)?;
    }
    let finite: Vec<&Marker> = panel.points.iter().filter(|p| p.y.is_finite()).collect();
    chart
        .draw_series(LineSeries::new(
            finite.iter().map(|p| (p.x, p.y)),
            RGBColor(170, 190, 210).stroke_width(1),
        ))
        .map_err(Error::plot)?;
    chart
        .draw_series(
            finite
                .iter()
                .filter(|p| !p.flagged)
                .map(|p| Circle::new((p.x, p.y), 3, BLUE.filled())),
        )
        .map_err(Error::plot)?;
    chart
        .draw_series(
            finite
                .iter()
                .filter(|p| p.flagged)
                .map(|p| Cross::new((p.x, p.y), 5, RED.stroke_width(2))),
        )
        .map_err(Error::plot)?;
    Ok(())
}

/// Write the lines of `text` in a small block (a legend or a summary).
///
/// # Errors
/// If plotters fails to draw.
pub fn draw_text_block(area: &Area<'_>, title: &str, lines: &[String]) -> Result<()> {
    area.draw(&Text::new(
        title.to_owned(),
        (14, 16),
        (FAMILY, 15).into_font().style(FontStyle::Bold),
    ))
    .map_err(Error::plot)?;
    for (i, l) in lines.iter().enumerate() {
        area.draw(&Text::new(
            l.clone(),
            (14, 42 + 18 * i as i32),
            (FAMILY, 13).into_font(),
        ))
        .map_err(Error::plot)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_renders_to_a_png() {
        let panel = Panel {
            title: "FWHM".into(),
            x_label: "frame".into(),
            y_label: "px".into(),
            points: (0..20)
                .map(|i| Marker {
                    x: f64::from(i),
                    y: 3.0 + 0.1 * f64::from(i % 5),
                    flagged: i == 7,
                })
                .collect(),
            guides: vec![Guide {
                y: 3.35,
                label: "limit".into(),
            }],
            median: Some(3.2),
        };
        let png = render(600, 300, |a| draw_panel(a, &panel)).expect("render");
        assert_eq!(&png[1..4], b"PNG");
        assert!(png.len() > 2000, "{}", png.len());
    }

    #[test]
    fn ranges_cope_with_flat_and_empty_data() {
        assert_eq!(padded_range([]), (0.0, 1.0));
        let (a, b) = padded_range([2.0, 2.0, f64::NAN]);
        assert!(a < 2.0 && b > 2.0);
    }
}
