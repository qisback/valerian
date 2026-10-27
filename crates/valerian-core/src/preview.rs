// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Previews: a small copy of an image that tools can redo quickly, and its
//! 8-bit display form.
//!
//! The desktop app stretches (and later cleans, solves) a reduced copy while a
//! slider moves and runs the real operation on the full frame only on apply.
//! The command line writes the same preview as a PNG (`--preview`), so a
//! preview can be checked without a window.

use std::path::Path;

use rayon::prelude::*;

use crate::fits::Image;
use crate::{Error, Frame, Result};

/// `image` reduced by a whole factor so that neither side exceeds `max_side`:
/// each output pixel is the mean of the finite pixels of a block. Unchanged
/// (cloned) when it already fits. The header is kept.
#[must_use]
pub fn downsample(image: &Image, max_side: usize) -> Image {
    let (w, h) = (image.width(), image.height());
    let factor = w.max(h).div_ceil(max_side.max(1)).max(1);
    if factor == 1 {
        return image.clone();
    }
    let (ow, oh) = (w / factor, h / factor);
    let planes = image
        .planes
        .iter()
        .map(|p| {
            let mut data = vec![0.0f32; ow * oh];
            data.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
                for (ox, out) in row.iter_mut().enumerate() {
                    let (mut sum, mut n) = (0.0f64, 0u32);
                    for y in oy * factor..(oy + 1) * factor {
                        for &v in &p.data[y * w + ox * factor..y * w + (ox + 1) * factor] {
                            if v.is_finite() {
                                sum += f64::from(v);
                                n += 1;
                            }
                        }
                    }
                    *out = if n == 0 {
                        f32::NAN
                    } else {
                        (sum / f64::from(n)) as f32
                    };
                }
            });
            Frame {
                width: ow,
                height: oh,
                data,
                bayer: None,
            }
        })
        .collect();
    Image {
        header: image.header.clone(),
        planes,
    }
}

/// Width of the gap in [`side_by_side`], in pixels.
const GAP: usize = 6;

/// Two images of one size side by side, a thin gap between them, as one image
/// (colour if either is). Both are taken to be in `0..=1`.
///
/// # Panics
/// If the images differ in size.
#[must_use]
pub fn side_by_side(left: &Image, right: &Image) -> Image {
    assert_eq!(
        (left.width(), left.height()),
        (right.width(), right.height())
    );
    let (w, h) = (left.width(), left.height());
    let n = if left.planes.len() >= 3 || right.planes.len() >= 3 {
        3
    } else {
        1
    };
    let pick = |img: &Image, c: usize| img.planes[c.min(img.planes.len() - 1)].data.clone();
    let planes = (0..n)
        .map(|c| {
            let (l, r) = (pick(left, c), pick(right, c));
            let stride = 2 * w + GAP;
            let mut data = vec![0.0f32; stride * h];
            for y in 0..h {
                let row = &mut data[y * stride..(y + 1) * stride];
                row[..w].copy_from_slice(&l[y * w..(y + 1) * w]);
                row[w..w + GAP].fill(0.5);
                row[w + GAP..].copy_from_slice(&r[y * w..(y + 1) * w]);
            }
            Frame {
                width: stride,
                height: h,
                data,
                bayer: None,
            }
        })
        .collect();
    Image {
        header: left.header.clone(),
        planes,
    }
}

fn byte(v: f32) -> u8 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    } else {
        0
    }
}

/// The pixels of `image` (taken to be in `0..=1`) as 8-bit RGBA, row by row.
/// One plane is grey; three or more are red, green and blue. Pixels that are
/// not finite are black.
#[must_use]
pub fn rgba8(image: &Image) -> Vec<u8> {
    let n = image.width() * image.height();
    let mut out = vec![255u8; n * 4];
    let colour = image.planes.len() >= 3;
    out.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        if colour {
            for (out, plane) in px.iter_mut().zip(&image.planes) {
                *out = byte(plane.data[i]);
            }
        } else {
            let g = byte(image.planes[0].data[i]);
            px[..3].fill(g);
        }
    });
    out
}

/// Write `image` (taken to be in `0..=1`) as an 8-bit grey or RGB PNG.
///
/// # Errors
/// [`Error::Invalid`] when the image is empty or the file cannot be written.
pub fn write_png(path: &Path, image: &Image) -> Result<()> {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Err(Error::invalid("cannot write an empty image as PNG"));
    }
    let rgba = rgba8(image);
    let colour = image.planes.len() >= 3;
    let bytes: Vec<u8> = if colour {
        rgba.iter()
            .enumerate()
            .filter(|(i, _)| i % 4 != 3)
            .map(|(_, &v)| v)
            .collect()
    } else {
        rgba.iter().step_by(4).copied().collect()
    };
    let io = |e: &dyn std::fmt::Display| Error::invalid(format!("{}: {e}", path.display()));
    let file = std::fs::File::create(path).map_err(|e| io(&e))?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        u32::try_from(w).map_err(|e| io(&e))?,
        u32::try_from(h).map_err(|e| io(&e))?,
    );
    enc.set_color(if colour {
        png::ColorType::Rgb
    } else {
        png::ColorType::Grayscale
    });
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| io(&e))?;
    writer.write_image_data(&bytes).map_err(|e| io(&e))?;
    writer.finish().map_err(|e| io(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits::Header;

    fn ramp(w: usize, h: usize) -> Image {
        let data = (0..w * h).map(|i| i as f32 / (w * h) as f32).collect();
        Image::mono(
            Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            },
            Header::new(),
        )
    }

    #[test]
    fn downsample_keeps_a_small_image_and_averages_blocks() {
        let small = ramp(8, 8);
        assert_eq!(downsample(&small, 8).width(), 8);
        let d = downsample(&small, 4);
        assert_eq!((d.width(), d.height()), (4, 4));
        let p = &small.plane().data;
        let want = (p[0] + p[1] + p[8] + p[9]) / 4.0;
        assert!((d.plane().data[0] - want).abs() < 1e-6);
    }

    #[test]
    fn downsample_skips_non_finite_pixels() {
        let mut img = ramp(4, 4);
        img.planes[0].data[0] = f32::NAN;
        let d = downsample(&img, 2);
        let p = &img.plane().data;
        assert!((d.plane().data[0] - (p[1] + p[4] + p[5]) / 3.0).abs() < 1e-6);
    }

    #[test]
    fn side_by_side_places_both_with_a_gap() {
        let (a, b) = (ramp(4, 2), ramp(4, 2));
        let s = side_by_side(&a, &b);
        assert_eq!((s.width(), s.height()), (14, 2));
        let row1 = &s.plane().data[14..28];
        assert!((row1[0] - a.plane().data[4]).abs() < 1e-9);
        assert!((row1[4] - 0.5).abs() < 1e-9 && (row1[10] - b.plane().data[4]).abs() < 1e-9);
    }

    #[test]
    fn rgba_maps_zero_one_and_blanks_nan() {
        let mut img = ramp(2, 1);
        img.planes[0].data = vec![0.0, 1.0];
        assert_eq!(rgba8(&img), vec![0, 0, 0, 255, 255, 255, 255, 255]);
        img.planes[0].data = vec![f32::NAN, 0.5];
        assert_eq!(rgba8(&img)[..4], [0, 0, 0, 255]);
        assert_eq!(rgba8(&img)[4], 128);
    }

    #[test]
    fn png_puts_the_first_stored_row_at_the_top() {
        let dir = std::env::temp_dir().join(format!("valerian-png-top-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("ok");
        let path = dir.join("a.png");
        let mut img = ramp(3, 4);
        img.planes[0].data = vec![0.0; 12];
        img.planes[0].data[..3].fill(1.0);
        write_png(&path, &img).expect("ok");
        let dec = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).expect("ok"),
        ));
        let mut r = dec.read_info().expect("ok");
        let mut buf = vec![0u8; r.output_buffer_size().expect("ok")];
        r.next_frame(&mut buf).expect("ok");
        assert_eq!(&buf[..3], &[255, 255, 255], "row 0 is the top row");
        assert_eq!(&buf[9..], &[0, 0, 0], "the last row is the bottom row");
        std::fs::remove_dir_all(dir).expect("ok");
    }

    #[test]
    fn png_round_trip_has_the_right_size() {
        let dir = std::env::temp_dir().join(format!("valerian-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("ok");
        let path = dir.join("a.png");
        write_png(&path, &ramp(5, 3)).expect("ok");
        let dec = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).expect("ok"),
        ));
        let r = dec.read_info().expect("ok");
        assert_eq!((r.info().width, r.info().height), (5, 3));
        std::fs::remove_dir_all(dir).expect("ok");
    }
}
