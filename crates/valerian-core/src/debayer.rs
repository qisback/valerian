// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Demosaicing of one-shot-colour (OSC) frames to three colour planes.
//!
//! `helia-core` only reduces a mosaic to mono; deep-sky colour needs the red,
//! green and blue planes. Two interpolators: bilinear, and Malvar-He-Cutler
//! (MHC), a 5x5 gradient-corrected linear filter that keeps edges sharper.
//! Image borders mirror about the edge pixel, which preserves the Bayer phase.
//!
//! # References
//!
//! - Malvar, H. S., He, L.-W. and Cutler, R. (2004), High-quality linear interpolation for demosaicing of Bayer-patterned color images, Proc. IEEE ICASSP 2004, vol. 3, iii-485, doi:10.1109/ICASSP.2004.1326587 (the filter taps).

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are bounded well below i64::MAX and signed offsets are needed at borders"
)]

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::fits::Image;
use helia_core::{BayerPattern, Frame};

/// Interpolation method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    /// Mean of the nearest same-colour samples.
    Bilinear,
    /// Malvar-He-Cutler gradient-corrected interpolation.
    #[default]
    Mhc,
}

/// Mirror `i` into `0..n` about the edge samples, keeping parity (so the
/// colour of the mirrored sample matches).
fn mirror(i: i64, n: i64) -> i64 {
    if i < 0 {
        (-i).min(n - 1)
    } else if i >= n {
        (2 * (n - 1) - i).max(0)
    } else {
        i
    }
}

struct Sampler<'a> {
    data: &'a [f32],
    w: i64,
    h: i64,
}

impl Sampler<'_> {
    fn at(&self, x: i64, y: i64) -> f32 {
        self.data[(mirror(y, self.h) * self.w + mirror(x, self.w)) as usize]
    }
}

/// Offsets and weights (over 8) of the MHC filters.
type Taps = &'static [(i64, i64, f32)];

/// Green at a red or blue site.
const G_AT_RB: Taps = &[
    (0, 0, 4.0),
    (-1, 0, 2.0),
    (1, 0, 2.0),
    (0, -1, 2.0),
    (0, 1, 2.0),
    (-2, 0, -1.0),
    (2, 0, -1.0),
    (0, -2, -1.0),
    (0, 2, -1.0),
];
/// Red (or blue) at a green site whose row neighbours are that colour.
const RB_AT_G_ROW: Taps = &[
    (0, 0, 5.0),
    (-1, 0, 4.0),
    (1, 0, 4.0),
    (-2, 0, -1.0),
    (2, 0, -1.0),
    (-1, -1, -1.0),
    (1, -1, -1.0),
    (-1, 1, -1.0),
    (1, 1, -1.0),
    (0, -2, 0.5),
    (0, 2, 0.5),
];
/// Red (or blue) at a green site whose column neighbours are that colour.
const RB_AT_G_COL: Taps = &[
    (0, 0, 5.0),
    (0, -1, 4.0),
    (0, 1, 4.0),
    (0, -2, -1.0),
    (0, 2, -1.0),
    (-1, -1, -1.0),
    (1, -1, -1.0),
    (-1, 1, -1.0),
    (1, 1, -1.0),
    (-2, 0, 0.5),
    (2, 0, 0.5),
];
/// Red at a blue site, or blue at a red site.
const RB_AT_OPPOSITE: Taps = &[
    (0, 0, 6.0),
    (-1, -1, 2.0),
    (1, -1, 2.0),
    (-1, 1, 2.0),
    (1, 1, 2.0),
    (-2, 0, -1.5),
    (2, 0, -1.5),
    (0, -2, -1.5),
    (0, 2, -1.5),
];

fn apply(s: &Sampler<'_>, x: i64, y: i64, taps: Taps) -> f32 {
    taps.iter()
        .map(|&(dx, dy, w)| w * s.at(x + dx, y + dy))
        .sum::<f32>()
        / 8.0
}

fn bilinear_at(s: &Sampler<'_>, p: BayerPattern, x: i64, y: i64, want: usize) -> f32 {
    let have = p.channel_at(x as usize, y as usize);
    if have == want {
        return s.at(x, y);
    }
    // Average the nearest samples of the wanted colour in the 3x3 block.
    let (mut sum, mut n) = (0.0, 0.0);
    for dy in -1..=1_i64 {
        for dx in -1..=1_i64 {
            let (xx, yy) = (mirror(x + dx, s.w), mirror(y + dy, s.h));
            if p.channel_at(xx as usize, yy as usize) == want {
                sum += s.at(xx, yy);
                n += 1.0;
            }
        }
    }
    sum / n
}

fn mhc_at(s: &Sampler<'_>, p: BayerPattern, x: i64, y: i64, want: usize) -> f32 {
    let have = p.channel_at(x as usize, y as usize);
    if have == want {
        return s.at(x, y);
    }
    match (have, want) {
        (0 | 2, 1) => apply(s, x, y, G_AT_RB),
        (1, _) => {
            // A green site: is the wanted colour on this row or this column?
            let left = p.channel_at(mirror(x - 1, s.w) as usize, y as usize);
            if left == want {
                apply(s, x, y, RB_AT_G_ROW)
            } else {
                apply(s, x, y, RB_AT_G_COL)
            }
        }
        _ => apply(s, x, y, RB_AT_OPPOSITE),
    }
}

/// Demosaic a raw mosaic into red, green and blue planes.
///
/// # Errors
/// [`Error::Invalid`] if the frame carries no Bayer pattern.
pub fn demosaic(frame: &Frame, method: Method) -> Result<[Frame; 3]> {
    let pattern = frame
        .bayer
        .ok_or_else(|| Error::invalid("frame has no Bayer pattern"))?;
    let (w, h) = (frame.width, frame.height);
    let s = Sampler {
        data: &frame.data,
        w: w as i64,
        h: h as i64,
    };
    let plane = |want: usize| -> Frame {
        let mut data = vec![0.0_f32; w * h];
        data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = match method {
                    Method::Bilinear => bilinear_at(&s, pattern, x as i64, y as i64, want),
                    Method::Mhc => mhc_at(&s, pattern, x as i64, y as i64, want),
                };
            }
        });
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    };
    Ok([plane(0), plane(1), plane(2)])
}

/// Demosaic an image in place of its single mosaic plane: the result has the
/// same header (minus the Bayer keywords) and three planes.
///
/// # Errors
/// [`Error::Invalid`] if the image is not a single Bayer-tagged plane.
pub fn demosaic_image(image: &Image, method: Method) -> Result<Image> {
    if image.planes.len() != 1 {
        return Err(Error::invalid(
            "only a single raw mosaic plane can be demosaiced",
        ));
    }
    let planes = demosaic(image.plane(), method)?.to_vec();
    let mut header = image.header.clone();
    for k in ["BAYERPAT", "XBAYROFF", "YBAYROFF"] {
        header.remove(k);
    }
    header.add_history("demosaiced by Valerian");
    Ok(Image { header, planes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mosaic sampled from a smooth colour ramp, plus the true planes.
    fn ramp(w: usize, h: usize, p: BayerPattern) -> (Frame, [Vec<f32>; 3]) {
        let f = |c: usize, x: usize, y: usize| -> f32 {
            let (xf, yf) = (x as f32, y as f32);
            match c {
                0 => 100.0 + 2.0 * xf + 1.0 * yf,
                1 => 300.0 + 1.0 * xf - 2.0 * yf,
                _ => 200.0 - 1.0 * xf + 3.0 * yf,
            }
        };
        let truth: [Vec<f32>; 3] =
            std::array::from_fn(|c| (0..w * h).map(|i| f(c, i % w, i / w)).collect());
        let data = (0..w * h)
            .map(|i| f(p.channel_at(i % w, i / w), i % w, i / w))
            .collect();
        (
            Frame::new(w, h, data).expect("frame").with_bayer(Some(p)),
            truth,
        )
    }

    #[test]
    fn mhc_reproduces_linear_ramps_exactly_away_from_the_border() {
        // MHC filters are exact for linear signals, in every Bayer phase.
        for p in [
            BayerPattern::Rggb,
            BayerPattern::Grbg,
            BayerPattern::Gbrg,
            BayerPattern::Bggr,
        ] {
            let (mosaic, truth) = ramp(40, 36, p);
            let planes = demosaic(&mosaic, Method::Mhc).expect("demosaic");
            for c in 0..3 {
                for y in 3..33 {
                    for x in 3..37 {
                        let got = planes[c].get(x, y);
                        let want = truth[c][y * 40 + x];
                        assert!(
                            (got - want).abs() < 1e-3,
                            "{p:?} channel {c} at ({x},{y}): {got} vs {want}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bilinear_is_exact_for_the_sampled_colour_and_close_elsewhere() {
        let (mosaic, truth) = ramp(32, 32, BayerPattern::Rggb);
        let planes = demosaic(&mosaic, Method::Bilinear).expect("demosaic");
        for y in 2..30 {
            for x in 2..30 {
                let c = BayerPattern::Rggb.channel_at(x, y);
                assert!((planes[c].get(x, y) - truth[c][y * 32 + x]).abs() < 1e-3);
            }
        }
        let err: f32 = (2..30)
            .flat_map(|y| (2..30).map(move |x| (x, y)))
            .map(|(x, y)| (planes[1].get(x, y) - truth[1][y * 32 + x]).abs())
            .fold(0.0, f32::max);
        assert!(err < 3.0, "{err}");
    }

    #[test]
    fn needs_a_pattern_and_a_single_plane() {
        let f = Frame::zeros(8, 8);
        assert!(demosaic(&f, Method::Mhc).is_err());
        let img = Image {
            header: crate::fits::Header::new(),
            planes: vec![f.clone(), f],
        };
        assert!(demosaic_image(&img, Method::Mhc).is_err());
    }

    #[test]
    fn flat_field_stays_flat_including_borders() {
        let f = Frame::new(16, 16, vec![500.0; 256])
            .expect("frame")
            .with_bayer(Some(BayerPattern::Bggr));
        for plane in demosaic(&f, Method::Mhc).expect("demosaic") {
            assert!(plane.data.iter().all(|&v| (v - 500.0).abs() < 1e-3));
        }
    }
}
