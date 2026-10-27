// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The pixel readout: what the picture holds under the pointer.
//!
//! Coordinates are image pixels counted from 0 at the top left, the same
//! numbers the solver and the star finder use. The raw values are the file's
//! (the open image's) own; the shown values are what the stretch makes of
//! them, from 0 to 1.

use valerian_core::fits::Image;
use valerian_core::wcs::Wcs;

/// What is under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Column of the pixel.
    pub x: usize,
    /// Row of the pixel.
    pub y: usize,
    /// The raw value of each plane.
    pub raw: Vec<f32>,
    /// The stretched value of each plane, when the stretch applies.
    pub shown: Option<Vec<f32>>,
    /// Right ascension and declination in degrees, for a solved image.
    pub sky: Option<(f64, f64)>,
}

/// The pixel of `img` under `x`, `y` (image pixels, possibly fractional or
/// outside), or `None` when it is outside the picture.
#[must_use]
pub fn pixel_at(img: &Image, x: f32, y: f32) -> Option<(usize, usize)> {
    if x.is_nan() || y.is_nan() || x < 0.0 || y < 0.0 {
        return None;
    }
    let (px, py) = (x as usize, y as usize);
    (px < img.width() && py < img.height()).then_some((px, py))
}

/// The raw value of every plane at `x`, `y`.
#[must_use]
pub fn raw_at(img: &Image, x: usize, y: usize) -> Vec<f32> {
    img.planes.iter().map(|p| p.data[y * p.width + x]).collect()
}

/// Read the pixel at `x`, `y`: the raw values and, for a solved image, the
/// sky position of the pixel's centre.
#[must_use]
pub fn read(img: &Image, wcs: Option<&Wcs>, x: usize, y: usize) -> Reading {
    Reading {
        x,
        y,
        raw: raw_at(img, x, y),
        shown: None,
        sky: wcs.map(|w| w.pixel_to_world(x as f64, y as f64)),
    }
}

/// A value in a width that stays readable from ADU counts to unit-range data.
#[must_use]
pub fn value(v: f32) -> String {
    if !v.is_finite() {
        "nan".into()
    } else if v.abs() >= 1000.0 {
        format!("{v:.1}")
    } else if v.abs() >= 10.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.5}")
    }
}

/// The plane names: one grey plane or red, green and blue.
fn names(n: usize) -> Vec<String> {
    match n {
        1 => vec![String::new()],
        3 => ["R ", "G ", "B "].map(String::from).to_vec(),
        _ => (0..n).map(|i| format!("{i} ")).collect(),
    }
}

/// `deg` as hours, minutes and seconds (`21h 00m 12.3s`).
#[must_use]
pub fn hms(deg: f64) -> String {
    let h = (deg.rem_euclid(360.0) / 15.0 * 36_000.0).round() / 36_000.0;
    let (hh, rest) = (h.trunc(), (h - h.trunc()) * 60.0);
    let (mm, ss) = (rest.trunc(), (rest - rest.trunc()) * 60.0);
    // Rounding to a tenth of a second can reach 60.0.
    let total = (hh * 3600.0 + mm * 60.0 + ss) * 10.0;
    let t = (total.round() as u64) % 864_000;
    format!(
        "{:02}h {:02}m {:02}.{}s",
        t / 36_000,
        t / 600 % 60,
        t / 10 % 60,
        t % 10
    )
}

/// `deg` as signed degrees, arcminutes and arcseconds (`+44d 31' 02"`).
#[must_use]
pub fn dms(deg: f64) -> String {
    let sign = if deg < 0.0 { '-' } else { '+' };
    let t = (deg.abs() * 3600.0).round() as u64;
    format!("{sign}{:02}d {:02}' {:02}\"", t / 3600, t / 60 % 60, t % 60)
}

/// The lines of the readout box.
#[must_use]
pub fn lines(r: &Reading) -> Vec<String> {
    let names = names(r.raw.len());
    let join = |vals: &[f32], f: &dyn Fn(f32) -> String| {
        vals.iter()
            .zip(&names)
            .map(|(v, n)| format!("{n}{}", f(*v)))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut out = vec![format!("x {}  y {}", r.x, r.y)];
    out.push(format!("raw    {}", join(&r.raw, &value)));
    if let Some(s) = &r.shown {
        out.push(format!("shown  {}", join(s, &|v| format!("{v:.4}"))));
    }
    if let Some((ra, dec)) = r.sky {
        out.push(format!("RA {}  Dec {}", hms(ra), dms(dec)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::Frame;
    use valerian_core::fits::Header;

    fn plane(w: usize, h: usize, base: f32) -> Frame {
        Frame {
            width: w,
            height: h,
            data: (0..w * h).map(|i| base + i as f32).collect(),
            bayer: None,
        }
    }

    #[test]
    fn the_pixel_under_the_pointer_is_floored_and_bounded() {
        let img = Image::mono(plane(10, 5, 0.0), Header::default());
        assert_eq!(pixel_at(&img, 0.0, 0.0), Some((0, 0)));
        assert_eq!(pixel_at(&img, 9.99, 4.5), Some((9, 4)));
        assert_eq!(pixel_at(&img, 10.0, 1.0), None);
        assert_eq!(pixel_at(&img, 1.0, 5.0), None);
        assert_eq!(pixel_at(&img, -0.1, 1.0), None);
        assert_eq!(pixel_at(&img, f32::NAN, 1.0), None);
    }

    #[test]
    fn a_colour_reading_has_a_value_per_channel() {
        let img = Image {
            header: Header::default(),
            planes: vec![plane(4, 4, 0.0), plane(4, 4, 100.0), plane(4, 4, 200.0)],
        };
        let r = read(&img, None, 2, 1);
        assert_eq!(r.raw, [6.0, 106.0, 206.0]);
        assert!(r.sky.is_none());
        let text = lines(&r);
        assert_eq!(text[0], "x 2  y 1");
        assert!(
            text[1].contains("R 6.00000") && text[1].contains("B 206.00"),
            "{text:?}"
        );
    }

    #[test]
    fn values_keep_their_width_from_counts_to_unit_range() {
        assert_eq!(value(0.012_345_6), "0.01235");
        assert_eq!(value(12.3456), "12.35");
        assert_eq!(value(54_321.04), "54321.0");
        assert_eq!(value(f32::NAN), "nan");
    }

    #[test]
    fn the_sky_position_is_written_in_hours_and_degrees() {
        assert_eq!(hms(0.0), "00h 00m 00.0s");
        assert_eq!(hms(180.0), "12h 00m 00.0s");
        assert_eq!(hms(315.0 + 3.0 / 240.0), "21h 00m 03.0s");
        assert_eq!(hms(359.999_999), "00h 00m 00.0s");
        assert_eq!(dms(44.517_222), "+44d 31' 02\"");
        assert_eq!(dms(-0.5), "-00d 30' 00\"");
    }

    #[test]
    fn a_solved_image_reads_its_sky_position() {
        let img = Image::mono(plane(100, 100, 0.0), Header::default());
        let wcs = Wcs::simple((180.0, 0.0), (50.0, 50.0), 1.0, 0.0, false);
        let r = read(&img, Some(&wcs), 49, 49);
        let (ra, dec) = r.sky.expect("sky");
        assert!((ra - 180.0).abs() < 0.01 && dec.abs() < 0.01, "{ra} {dec}");
        assert!(lines(&r).iter().any(|l| l.starts_with("RA ")));
    }
}
