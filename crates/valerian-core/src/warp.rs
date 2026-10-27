// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Resampling a frame through a [`Transform`] onto the grid of another.
//!
//! Output pixels whose source position falls outside the source frame are NaN,
//! which the combiner reads as "no data here", so the footprint of a rotated or
//! shifted frame needs no separate mask.
//!
//! # References
//!
//! - Keys, R. G. (1981), Cubic convolution interpolation for digital image processing, IEEE Trans. Acoust. Speech Signal Process. 29, 1153-1160, doi:10.1109/TASSP.1981.1163711 (the bicubic kernel; Catmull-Rom is the a = -0.5 case).
//! - Duchon, C. E. (1979), Lanczos filtering in one and two dimensions, J. Appl. Meteorol. 18, 1016-1022, `doi:10.1175/1520-0450(1979)018<1016:LFIOAT>2.0.CO;2`.

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::transform::Transform;
use helia_core::Frame;

/// Interpolation kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// 2x2 bilinear: smooth, no ringing, softens stars a little.
    Bilinear,
    /// 4x4 bicubic (Catmull-Rom): the usual compromise.
    #[default]
    Bicubic,
    /// 6x6 Lanczos with a = 3: sharpest, can ring around hot stars.
    Lanczos3,
}

impl Interpolation {
    /// Taps on each side of the sample point.
    fn reach(self) -> i64 {
        match self {
            Self::Bilinear => 1,
            Self::Bicubic => 2,
            Self::Lanczos3 => 3,
        }
    }

    /// Weights for the `2 * reach` taps `floor(x) - reach + 1 ..= floor(x) + reach`
    /// at fractional offset `t` in `[0, 1)`.
    fn weights(self, t: f32, out: &mut [f32; 6]) {
        match self {
            Self::Bilinear => {
                out[0] = 1.0 - t;
                out[1] = t;
            }
            Self::Bicubic => {
                // Catmull-Rom (a = -0.5).
                let w = |x: f32| -> f32 {
                    let a = x.abs();
                    if a < 1.0 {
                        1.5 * a * a * a - 2.5 * a * a + 1.0
                    } else if a < 2.0 {
                        -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0
                    } else {
                        0.0
                    }
                };
                for (k, o) in out.iter_mut().take(4).enumerate() {
                    *o = w(t - (k as f32 - 1.0));
                }
            }
            Self::Lanczos3 => {
                let w = |x: f32| -> f32 {
                    if x.abs() < 1e-6 {
                        1.0
                    } else if x.abs() >= 3.0 {
                        0.0
                    } else {
                        let px = std::f32::consts::PI * x;
                        3.0 * px.sin() * (px / 3.0).sin() / (px * px)
                    }
                };
                let mut sum = 0.0;
                for (k, o) in out.iter_mut().enumerate() {
                    *o = w(t - (k as f32 - 2.0));
                    sum += *o;
                }
                for o in out.iter_mut() {
                    *o /= sum;
                }
            }
        }
    }
}

/// Resample `src` onto a `width` by `height` grid. `to_reference` maps source
/// coordinates to the coordinates of the output grid; each output pixel is
/// filled by looking up where it came from.
///
/// # Errors
/// [`Error::Invalid`] if the transform is singular.
pub fn warp(
    src: &Frame,
    to_reference: &Transform,
    width: usize,
    height: usize,
    interp: Interpolation,
) -> Result<Frame> {
    let inv = to_reference
        .inverse()
        .ok_or_else(|| Error::invalid("the transform cannot be inverted"))?;
    let (sw, sh) = (src.width as i64, src.height as i64);
    let reach = interp.reach();
    let mut data = vec![f32::NAN; width * height];
    data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let mut wx = [0.0_f32; 6];
        let mut wy = [0.0_f32; 6];
        for (x, out) in row.iter_mut().enumerate() {
            let (sx, sy) = inv.apply((x as f64, y as f64));
            // Outside the source frame (half a pixel of margin): no data.
            if !(sx >= -0.5 && sy >= -0.5 && sx <= sw as f64 - 0.5 && sy <= sh as f64 - 0.5) {
                continue;
            }
            let (fx, fy) = (sx.floor(), sy.floor());
            interp.weights((sx - fx) as f32, &mut wx);
            interp.weights((sy - fy) as f32, &mut wy);
            let (ix, iy) = (fx as i64, fy as i64);
            let n = 2 * reach;
            let mut acc = 0.0_f32;
            let mut bad = false;
            for j in 0..n {
                let yy = (iy - reach + 1 + j).clamp(0, sh - 1);
                let mut line = 0.0_f32;
                for i in 0..n {
                    let xx = (ix - reach + 1 + i).clamp(0, sw - 1);
                    let v = src.data[(yy * sw + xx) as usize];
                    if !v.is_finite() {
                        bad = true;
                    }
                    line += wx[i as usize] * v;
                }
                acc += wy[j as usize] * line;
            }
            if !bad {
                *out = acc;
            }
        }
    });
    Ok(Frame {
        width,
        height,
        data,
        bayer: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stars::{DetectOptions, detect};
    use crate::testutil::gaussian_field;

    #[test]
    fn identity_and_integer_shifts_are_exact() {
        let src = gaussian_field(64, 48, 100.0, &[(30.0, 20.0, 5000.0, 2.0)]);
        for interp in [
            Interpolation::Bilinear,
            Interpolation::Bicubic,
            Interpolation::Lanczos3,
        ] {
            let same = warp(&src, &Transform::identity(), 64, 48, interp).expect("warp");
            assert!(
                same.data
                    .iter()
                    .zip(&src.data)
                    .all(|(a, b)| (a - b).abs() < 1e-4),
                "{interp:?}"
            );
            let moved = warp(
                &src,
                &Transform::similarity(1.0, 0.0, 3.0, -2.0),
                64,
                48,
                interp,
            )
            .expect("warp");
            // Output (x, y) comes from source (x - 3, y + 2).
            for y in 0..46 {
                for x in 3..64 {
                    let want = src.data[(y + 2) * 64 + (x - 3)];
                    let got = moved.data[y * 64 + x];
                    assert!((got - want).abs() < 1e-3, "{interp:?} at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn the_footprint_outside_the_source_is_nan() {
        let src = Frame::new(20, 20, vec![1.0; 400]).expect("frame");
        let out = warp(
            &src,
            &Transform::similarity(1.0, 0.0, 10.0, 0.0),
            40,
            20,
            Interpolation::Bicubic,
        )
        .expect("warp");
        assert!(out.data[5].is_nan() && out.data[9].is_nan());
        assert!((out.data[15] - 1.0).abs() < 1e-5);
        assert!(out.data[35].is_nan());
        // NaN in the source poisons only the pixels whose taps touch it.
        let mut holey = src.clone();
        holey.data[10 * 20 + 10] = f32::NAN;
        let out = warp(
            &holey,
            &Transform::identity(),
            20,
            20,
            Interpolation::Bilinear,
        )
        .expect("warp");
        assert!(out.data[10 * 20 + 10].is_nan());
        assert!(out.data[2 * 20 + 2].is_finite());
    }

    #[test]
    fn a_subpixel_shift_moves_a_star_by_exactly_that_much() {
        let mut src = gaussian_field(120, 100, 300.0, &[(60.0, 50.0, 40_000.0, 1.8)]);
        let mut rng = crate::testutil::Rng::new(6);
        src.data
            .iter_mut()
            .for_each(|v| *v += rng.gauss(0.0, 2.0) as f32);
        for interp in [
            Interpolation::Bilinear,
            Interpolation::Bicubic,
            Interpolation::Lanczos3,
        ] {
            let t = Transform::similarity(1.0, 0.0, 0.37, -0.62);
            let out = warp(&src, &t, 120, 100, interp).expect("warp");
            let det = detect(&out, &DetectOptions::default());
            assert_eq!(det.stars.len(), 1);
            let s = det.stars[0];
            let tol = if interp == Interpolation::Bilinear {
                0.02
            } else {
                0.01
            };
            assert!(
                (s.x - 60.37).abs() < tol && (s.y - 49.38).abs() < tol,
                "{interp:?}: ({}, {})",
                s.x,
                s.y
            );
        }
    }

    #[test]
    fn singular_transform_is_refused() {
        let src = Frame::zeros(4, 4);
        let t = Transform {
            m: [[1.0, 2.0, 0.0], [2.0, 4.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(warp(&src, &t, 4, 4, Interpolation::Bilinear).is_err());
    }

    #[test]
    fn rotation_preserves_flux_for_a_smooth_star() {
        let src = gaussian_field(200, 200, 0.0, &[(100.0, 100.0, 50_000.0, 2.5)]);
        let t = Transform::rotation_about(100.0, 100.0, 0.5, 0.0, 0.0);
        let out = warp(&src, &t, 200, 200, Interpolation::Bicubic).expect("warp");
        let total: f64 = out
            .data
            .iter()
            .filter(|v| v.is_finite())
            .map(|&v| f64::from(v))
            .sum();
        assert!((total / 50_000.0 - 1.0).abs() < 0.002, "{total}");
    }
}
