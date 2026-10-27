// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Stretching: turning linear data into something the eye can read.
//!
//! A stretch here is two steps. [`Levels`] map a black point and a white point
//! to 0 and 1 (clipping what lies outside). A [`Curve`] then bends `0..=1` onto
//! `0..=1` with 0 and 1 fixed: [`Curve::Mtf`] (the midtone transfer function),
//! [`Curve::Arcsinh`] and [`Curve::Ghs`] (the generalised hyperbolic stretch).
//! Every curve is strictly increasing, so it has an inverse, and the inverse is
//! provided; a stretch is therefore recorded in the output header
//! ([`Stretch::record`]) and can be undone on the data that survived the
//! clipping ([`Stretch::from_header`], [`Stretch::invert_frame`]). The source
//! frame is never modified.
//!
//! The GHS follows the published definition (the Generalised Hyperbolic Stretch
//! process documentation at ghsastro.co.uk, section 5.2). With
//! `D = e^s - 1` for the stretch factor `s`, a local intensity `b`, a symmetry
//! point `SP`, and protection limits `LP <= SP <= HP`, the base curve `T(u)`
//! for `u >= 0` is
//!
//! | `b` | `T(u)` | `T'(u)` |
//! |---|---|---|
//! | `-1` | `ln(1 + D u)` | `D / (1 + D u)` |
//! | `< 0`, not `-1` | `(1 - (1 - b D u)^((b+1)/b)) / (D (b+1))` | `(1 - b D u)^(1/b)` |
//! | `0` | `1 - e^(-D u)` | `D e^(-D u)` |
//! | `> 0` | `1 - (1 + b D u)^(-1/b)` | `D (1 + b D u)^(-(1+b)/b)` |
//!
//! The stretch is `T(x - SP)` above `SP`, `-T(SP - x)` below it (so the curve
//! is point-symmetric about `SP`), continued linearly with the end slope below
//! `LP` and above `HP`, and rescaled so that 0 maps to 0 and 1 to 1.
//!
//! # References
//!
//! - Lupton, R. et al. (2004), Preparing red-green-blue images from CCD data, Publ. Astron. Soc. Pac. 116, 133-137, doi:10.1086/382245 (the arcsinh stretch).
//! - Payne, D. and Cranfield, M., Generalised hyperbolic stretch, documented at <https://ghsastro.co.uk> (implemented from the published formulae; no peer-reviewed paper found, to verify; no GHS script code is used).
//! - The midtone transfer function is a standard rational (Moebius) curve; its use as an automatic stretch is matched from the behaviour `PixInsight` documents, not from code.

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::fits::{Header, Image, Value};
use crate::stats;
use helia_core::Frame;

/// Black and white points of a linear rescale: `black` maps to 0 and `white`
/// to 1; values outside are clipped. NaN stays NaN.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    /// The value that becomes 0.
    pub black: f32,
    /// The value that becomes 1.
    pub white: f32,
}

impl Levels {
    /// Rescale one value to `0..=1`.
    #[must_use]
    pub fn normalise(self, v: f32) -> f32 {
        if v.is_nan() {
            return v;
        }
        ((v - self.black) / (self.white - self.black)).clamp(0.0, 1.0)
    }

    /// The value that rescales to `x` (the inverse of [`Levels::normalise`]
    /// inside the range).
    #[must_use]
    pub fn restore(self, x: f32) -> f32 {
        self.black + x * (self.white - self.black)
    }

    fn check(self) -> Result<()> {
        if self.black.is_finite() && self.white.is_finite() && self.white > self.black {
            Ok(())
        } else {
            Err(Error::invalid(
                "the white point must be above the black point",
            ))
        }
    }
}

/// The generalised hyperbolic stretch (see the module documentation).
///
/// A very hard stretch (a factor of about 8 or more with `b >= 0`) makes the
/// curve reach 1 within double precision well before the top of the range, so
/// the top of it is flat as far as a computer can tell and cannot be inverted
/// there. Factors up to about 5 are well inside what the arithmetic resolves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ghs {
    /// Stretch factor `s`; `D = e^s - 1`. Zero is the identity.
    pub stretch: f64,
    /// Local intensity `b`: how tightly the stretch is focused at `SP`.
    /// Negative is logarithmic-to-integral, zero exponential, positive
    /// harmonic-to-hyperbolic.
    pub b: f64,
    /// Symmetry point, in `0..=1`: the brightness that gets the steepest slope.
    pub sp: f64,
    /// Shadow protection point, `0 <= lp <= sp`: below it the curve is a
    /// straight line that keeps the slope it has at `lp`, instead of
    /// flattening further and crushing the faintest levels.
    pub lp: f64,
    /// Highlight protection point, `sp <= hp <= 1`: above it the curve is a
    /// straight line that keeps the slope it has at `hp`, instead of
    /// flattening further and compressing the brightest levels to white.
    pub hp: f64,
}

/// What a [`Ghs`] needs precomputed.
struct GhsSetup {
    d: f64,
    b: f64,
    sp: f64,
    lp: f64,
    hp: f64,
    /// `T2'(LP)` and `T2(LP)` (the line below `LP`).
    shadow_slope: f64,
    shadow_y: f64,
    /// `T3'(HP)` and `T3(HP)` (the line above `HP`).
    highlight_slope: f64,
    highlight_y: f64,
    /// `T1(0)` and `T4(1)`: the unnormalised outputs for 0 and 1.
    lo: f64,
    hi: f64,
}

impl Ghs {
    /// A stretch of `stretch` (the factor `s`) and intensity `b` about `sp`,
    /// with no protection.
    #[must_use]
    pub fn new(stretch: f64, b: f64, sp: f64) -> Self {
        Self {
            stretch,
            b,
            sp,
            lp: 0.0,
            hp: 1.0,
        }
    }

    fn check(&self) -> Result<()> {
        let ok = self.stretch.is_finite()
            && self.stretch >= 0.0
            && self.b.is_finite()
            && (0.0..=1.0).contains(&self.lp)
            && self.lp <= self.sp
            && self.sp <= self.hp
            && self.hp <= 1.0;
        if ok {
            Ok(())
        } else {
            Err(Error::invalid(
                "GHS needs stretch >= 0, a finite b and 0 <= LP <= SP <= HP <= 1",
            ))
        }
    }

    fn setup(&self) -> GhsSetup {
        let d = self.stretch.exp_m1();
        let b = self.b;
        let shadow_y = -base(d, b, self.sp - self.lp);
        let highlight_y = base(d, b, self.hp - self.sp);
        let shadow_slope = base_slope(d, b, self.sp - self.lp);
        let highlight_slope = base_slope(d, b, self.hp - self.sp);
        // T1(0) and T4(1) are the lines continued to the ends.
        let lo = shadow_slope * (0.0 - self.lp) + shadow_y;
        let hi = highlight_slope * (1.0 - self.hp) + highlight_y;
        GhsSetup {
            d,
            b,
            sp: self.sp,
            lp: self.lp,
            hp: self.hp,
            shadow_slope,
            shadow_y,
            highlight_slope,
            highlight_y,
            lo,
            hi,
        }
    }

    /// Stretch one value in `0..=1`.
    #[must_use]
    pub fn apply(&self, x: f64) -> f64 {
        if self.stretch == 0.0 {
            return x;
        }
        let s = self.setup();
        s.forward(x)
    }

    /// The value that [`Ghs::apply`] maps to `y`.
    #[must_use]
    pub fn inverse(&self, y: f64) -> f64 {
        if self.stretch == 0.0 {
            return y;
        }
        let s = self.setup();
        s.backward(y)
    }
}

/// `T(u)` for `u >= 0`.
fn base(d: f64, b: f64, u: f64) -> f64 {
    if (b + 1.0).abs() < 1e-12 {
        (d * u).ln_1p()
    } else if b < 0.0 {
        (1.0 - (1.0 - b * d * u).powf((b + 1.0) / b)) / (d * (b + 1.0))
    } else if b == 0.0 {
        -(-d * u).exp_m1()
    } else {
        1.0 - (1.0 + b * d * u).powf(-1.0 / b)
    }
}

/// `T'(u)` for `u >= 0`.
fn base_slope(d: f64, b: f64, u: f64) -> f64 {
    if (b + 1.0).abs() < 1e-12 {
        d / (1.0 + d * u)
    } else if b < 0.0 {
        (1.0 - b * d * u).powf(1.0 / b)
    } else if b == 0.0 {
        d * (-d * u).exp()
    } else {
        d * (1.0 + b * d * u).powf(-(1.0 + b) / b)
    }
}

/// The inverse of `T`, for `0 <= y < T(infinity)`.
fn base_inverse(d: f64, b: f64, y: f64) -> f64 {
    if (b + 1.0).abs() < 1e-12 {
        y.exp_m1() / d
    } else if b < 0.0 {
        (1.0 - (1.0 - (b + 1.0) * d * y).powf(b / (b + 1.0))) / (d * b)
    } else if b == 0.0 {
        -(-y).ln_1p() / d
    } else {
        ((1.0 - y).powf(-b) - 1.0) / (b * d)
    }
}

impl GhsSetup {
    /// The unnormalised curve.
    fn raw(&self, x: f64) -> f64 {
        if x < self.lp {
            self.shadow_slope * (x - self.lp) + self.shadow_y
        } else if x < self.sp {
            -base(self.d, self.b, self.sp - x)
        } else if x < self.hp {
            base(self.d, self.b, x - self.sp)
        } else {
            self.highlight_slope * (x - self.hp) + self.highlight_y
        }
    }

    fn forward(&self, x: f64) -> f64 {
        (self.raw(x.clamp(0.0, 1.0)) - self.lo) / (self.hi - self.lo)
    }

    fn backward(&self, y: f64) -> f64 {
        let t = (self.lo + y.clamp(0.0, 1.0) * (self.hi - self.lo)).clamp(self.lo, self.hi);
        if t < self.shadow_y {
            self.lp + (t - self.shadow_y) / self.shadow_slope
        } else if t < 0.0 {
            self.sp - base_inverse(self.d, self.b, -t)
        } else if t < self.highlight_y {
            self.sp + base_inverse(self.d, self.b, t)
        } else {
            self.hp + (t - self.highlight_y) / self.highlight_slope
        }
    }
}

/// A tone curve on `0..=1` with 0 and 1 fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// Midtone transfer function
    /// `((m - 1) x) / ((2 m - 1) x - m)`: maps `m` to one half. `m` below 0.5
    /// brightens, above 0.5 darkens, 0.5 is the identity.
    Mtf {
        /// The midtone balance, in `0 < m < 1`.
        midtone: f64,
    },
    /// `asinh(beta x) / asinh(beta)`: linear for small `beta x`, logarithmic
    /// above. Larger `beta` stretches harder; as `beta` goes to 0 it is the
    /// identity.
    Arcsinh {
        /// The stretch strength, `beta > 0`.
        beta: f64,
    },
    /// Generalised hyperbolic stretch.
    Ghs(Ghs),
}

impl Curve {
    /// Check the parameters.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a parameter outside its range.
    pub fn check(&self) -> Result<()> {
        match *self {
            Self::Mtf { midtone } if !(midtone > 0.0 && midtone < 1.0) => {
                Err(Error::invalid("the midtone must be between 0 and 1"))
            }
            Self::Arcsinh { beta } if !(beta.is_finite() && beta > 0.0) => {
                Err(Error::invalid("the arcsinh strength must be above 0"))
            }
            Self::Ghs(g) => g.check(),
            _ => Ok(()),
        }
    }

    /// Apply the curve to a value in `0..=1` (clamped).
    #[must_use]
    pub fn apply(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match *self {
            Self::Mtf { midtone: m } => mtf(x, m),
            Self::Arcsinh { beta } => (beta * x).asinh() / beta.asinh(),
            Self::Ghs(g) => g.apply(x),
        }
    }

    /// The value that [`Curve::apply`] maps to `y` (clamped to `0..=1`).
    #[must_use]
    pub fn inverse(&self, y: f64) -> f64 {
        let y = y.clamp(0.0, 1.0);
        match *self {
            Self::Mtf { midtone: m } => mtf(y, 1.0 - m),
            Self::Arcsinh { beta } => (y * beta.asinh()).sinh() / beta,
            Self::Ghs(g) => g.inverse(y),
        }
    }
}

/// The midtone transfer function on `0..=1`.
#[must_use]
pub fn mtf(x: f64, m: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    ((m - 1.0) * x) / ((2.0 * m - 1.0) * x - m)
}

/// The midtone `m` for which `mtf(sky, m) = target`, limited to `1e-4..=0.9`
/// as the Python pipeline does.
#[must_use]
pub fn midtone_for(sky: f64, target: f64) -> f64 {
    let m = ((target - 1.0) * sky) / (2.0 * target * sky - target - sky);
    if m.is_finite() {
        m.clamp(1e-4, 0.9)
    } else {
        0.5
    }
}

/// A complete stretch: levels, then a curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stretch {
    /// The black and white points.
    pub levels: Levels,
    /// The tone curve.
    pub curve: Curve,
}

/// Where the sky sits and how to find the levels automatically.
#[derive(Clone, Copy, Debug)]
pub struct AutoLevels {
    /// Black point this many robust sigmas below the sky (the pipeline: 2.8).
    pub shadow_sigma: f32,
    /// White point at this percentile of the data (the pipeline: 99.995).
    pub white_percentile: f32,
}

impl Default for AutoLevels {
    fn default() -> Self {
        Self {
            shadow_sigma: 2.8,
            white_percentile: 99.995,
        }
    }
}

/// The sky level (median) and the robust sigma about it, from every finite
/// pixel.
fn sky_and_sigma(f: &Frame) -> (f32, f32) {
    let (sky, sigma) = stats::median_sigma(&f.data);
    if sigma > 0.0 {
        return (sky, sigma);
    }
    // A flat frame has no scatter; fall back to the standard deviation.
    let v: Vec<f32> = f.data.iter().copied().filter(|x| x.is_finite()).collect();
    let n = v.len().max(1) as f32;
    let mean = v.iter().sum::<f32>() / n;
    let sd = (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n).sqrt();
    (sky, sd.max(1e-6))
}

/// Levels found from the data: the black point `shadow_sigma` robust sigmas
/// below the sky, the white point at a high percentile. Also returns the sky.
///
/// # Errors
/// [`Error::Invalid`] when the frame has no finite pixels.
pub fn auto_levels(frame: &Frame, auto: AutoLevels) -> Result<(Levels, f32)> {
    if !frame.data.iter().any(|v| v.is_finite()) {
        return Err(Error::invalid("the image has no valid pixels"));
    }
    let (sky, sigma) = sky_and_sigma(frame);
    let black = sky - auto.shadow_sigma * sigma;
    let mut white = stats::percentile(&frame.data, auto.white_percentile);
    if white <= black {
        white = black + 1.0;
    }
    Ok((Levels { black, white }, sky))
}

/// The stretch that puts the sky at `target` (0 to 1) with a midtone transfer
/// function: the black point just below the sky, the white point at a high
/// percentile, and the midtone solved so that the sky lands on `target`.
/// This is the pipeline's `autostretch`.
///
/// # Errors
/// [`Error::Invalid`] for an empty frame or a target outside `0..1`.
pub fn auto_mtf(frame: &Frame, target: f64, auto: AutoLevels) -> Result<Stretch> {
    if !(target > 0.0 && target < 1.0) {
        return Err(Error::invalid(
            "the target sky level must be between 0 and 1",
        ));
    }
    let (levels, sky) = auto_levels(frame, auto)?;
    let sky_n = f64::from(levels.normalise(sky));
    Ok(Stretch {
        levels,
        curve: Curve::Mtf {
            midtone: midtone_for(sky_n, target),
        },
    })
}

impl Stretch {
    /// Check the levels and curve.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a bad parameter.
    pub fn check(&self) -> Result<()> {
        self.levels.check()?;
        self.curve.check()
    }

    /// Stretch one value.
    #[must_use]
    pub fn apply(&self, v: f32) -> f32 {
        if v.is_nan() {
            return v;
        }
        self.curve.apply(f64::from(self.levels.normalise(v))) as f32
    }

    /// Undo [`Stretch::apply`] for a stretched value: exact for data that lay
    /// between the black and white points, since clipped values are gone.
    #[must_use]
    pub fn invert(&self, y: f32) -> f32 {
        if y.is_nan() {
            return y;
        }
        self.levels.restore(self.curve.inverse(f64::from(y)) as f32)
    }

    /// A new frame holding the stretched values; NaN stays NaN.
    #[must_use]
    pub fn apply_frame(&self, f: &Frame) -> Frame {
        let mut out = f.clone();
        out.data.par_iter_mut().for_each(|v| *v = self.apply(*v));
        out
    }

    /// A new frame holding the original values restored from a stretched one.
    #[must_use]
    pub fn invert_frame(&self, f: &Frame) -> Frame {
        let mut out = f.clone();
        out.data.par_iter_mut().for_each(|v| *v = self.invert(*v));
        out
    }

    /// Write this stretch into `header` as `STRETCH` and its parameters, and
    /// a `HISTORY` line, so the image says how it was made.
    pub fn record(&self, header: &mut Header) {
        for k in KEYS {
            header.remove(k);
        }
        header.set(
            "STRBLACK",
            Value::Float(f64::from(self.levels.black)),
            "stretch black point",
        );
        header.set(
            "STRWHITE",
            Value::Float(f64::from(self.levels.white)),
            "stretch white point",
        );
        let text = match self.curve {
            Curve::Mtf { midtone } => {
                header.set("STRETCH", Value::Str("mtf".into()), "tone curve");
                header.set("STRMID", Value::Float(midtone), "midtone balance");
                format!("midtone transfer function, midtone {midtone:.6}")
            }
            Curve::Arcsinh { beta } => {
                header.set("STRETCH", Value::Str("arcsinh".into()), "tone curve");
                header.set("STRBETA", Value::Float(beta), "arcsinh strength");
                format!("arcsinh, strength {beta:.6}")
            }
            Curve::Ghs(g) => {
                header.set("STRETCH", Value::Str("ghs".into()), "tone curve");
                header.set("STRGS", Value::Float(g.stretch), "GHS stretch factor");
                header.set("STRGB", Value::Float(g.b), "GHS local intensity");
                header.set("STRGSP", Value::Float(g.sp), "GHS symmetry point");
                header.set("STRGLP", Value::Float(g.lp), "GHS shadow protection");
                header.set("STRGHP", Value::Float(g.hp), "GHS highlight protection");
                format!(
                    "generalised hyperbolic stretch, factor {:.4}, b {:.4}, SP {:.4}, LP {:.4}, HP {:.4}",
                    g.stretch, g.b, g.sp, g.lp, g.hp
                )
            }
        };
        header.add_history(&format!(
            "valerian stretch: black {:.6}, white {:.6}, {text}",
            self.levels.black, self.levels.white
        ));
    }

    /// The stretch recorded by [`Stretch::record`], if the header has one.
    #[must_use]
    pub fn from_header(header: &Header) -> Option<Self> {
        let levels = Levels {
            black: header.get_f64("STRBLACK")? as f32,
            white: header.get_f64("STRWHITE")? as f32,
        };
        let curve = match header.get_str("STRETCH")? {
            "mtf" => Curve::Mtf {
                midtone: header.get_f64("STRMID")?,
            },
            "arcsinh" => Curve::Arcsinh {
                beta: header.get_f64("STRBETA")?,
            },
            "ghs" => Curve::Ghs(Ghs {
                stretch: header.get_f64("STRGS")?,
                b: header.get_f64("STRGB")?,
                sp: header.get_f64("STRGSP")?,
                lp: header.get_f64("STRGLP")?,
                hp: header.get_f64("STRGHP")?,
            }),
            _ => return None,
        };
        Some(Self { levels, curve })
    }
}

/// Header keywords [`Stretch::record`] writes.
const KEYS: [&str; 10] = [
    "STRETCH", "STRBLACK", "STRWHITE", "STRMID", "STRBETA", "STRGS", "STRGB", "STRGSP", "STRGLP",
    "STRGHP",
];

/// How to find the levels and curve for each plane of an image.
#[derive(Clone, Copy, Debug)]
pub enum Plan {
    /// A fixed curve and fixed levels.
    Fixed(Stretch),
    /// A fixed curve, levels from the data.
    Curve(Curve, AutoLevels),
    /// An MTF that puts the sky at a target.
    AutoMtf {
        /// Sky level after the stretch, `0..1`.
        target: f64,
        /// How the levels are found.
        auto: AutoLevels,
    },
}

/// The frame the levels and curve of a linked stretch are found from: the
/// plane itself for a single plane, otherwise the per-pixel mean of the finite
/// values of every plane.
#[must_use]
pub fn reference_frame(image: &Image) -> Frame {
    let mut m = image.planes[0].clone();
    if image.planes.len() > 1 {
        for (i, v) in m.data.iter_mut().enumerate() {
            let (mut s, mut n) = (0.0_f32, 0.0_f32);
            for p in &image.planes {
                if p.data[i].is_finite() {
                    s += p.data[i];
                    n += 1.0;
                }
            }
            *v = if n > 0.0 { s / n } else { f32::NAN };
        }
    }
    m
}

/// Stretch every plane of `image`. With `linked`, the levels and curve are
/// found once, from the mean of the planes, and applied to all of them, which
/// keeps colour balance; otherwise each plane gets its own.
///
/// The header records the stretch when it is the same for every plane.
///
/// # Errors
/// [`Error::Invalid`] for bad parameters or an image with no valid pixels.
pub fn stretch_image(image: &Image, plan: &Plan, linked: bool) -> Result<Image> {
    let find = |f: &Frame| -> Result<Stretch> {
        let s = match *plan {
            Plan::Fixed(s) => s,
            Plan::Curve(curve, auto) => Stretch {
                levels: auto_levels(f, auto)?.0,
                curve,
            },
            Plan::AutoMtf { target, auto } => auto_mtf(f, target, auto)?,
        };
        s.check()?;
        Ok(s)
    };
    let mut header = image.header.clone();
    let planes = if linked || image.planes.len() == 1 {
        let reference = reference_frame(image);
        let s = find(&reference)?;
        s.record(&mut header);
        image.planes.iter().map(|p| s.apply_frame(p)).collect()
    } else {
        let mut out = Vec::with_capacity(image.planes.len());
        for (i, p) in image.planes.iter().enumerate() {
            let s = find(p)?;
            header.add_history(&format!(
                "valerian stretch: plane {i} black {:.6}, white {:.6}, {:?}",
                s.levels.black, s.levels.white, s.curve
            ));
            out.push(s.apply_frame(p));
        }
        out
    };
    Ok(Image { header, planes })
}

/// Stretch `shown` with the automatic MTF stretch measured on `reference`, so
/// that two images that belong together (an image and the stars taken out of
/// it) are displayed on one scale. For looking at, not for writing: the stretch
/// is not recorded.
///
/// # Errors
/// [`Error::Invalid`] for an image with no valid pixels or a bad target.
pub fn stretch_like(reference: &Image, shown: &Image, target: f64) -> Result<Image> {
    let s = auto_mtf(
        &reference_frame(reference),
        target,
        AutoLevels {
            shadow_sigma: 2.8,
            white_percentile: 99.995,
        },
    )?;
    Ok(Image {
        header: shown.header.clone(),
        planes: shown.planes.iter().map(|p| s.apply_frame(p)).collect(),
    })
}

/// Undo the stretch a header records, on every plane, and remove the record.
/// Values that were clipped by the black and white points are not recovered.
///
/// # Errors
/// [`Error::Invalid`] when the image carries no recorded stretch, or it was
/// stretched plane by plane.
pub fn invert_image(image: &Image) -> Result<Image> {
    let s = Stretch::from_header(&image.header)
        .ok_or_else(|| Error::invalid("the image records no stretch (no STRETCH keyword)"))?;
    s.check()?;
    let mut header = image.header.clone();
    for k in KEYS {
        header.remove(k);
    }
    header.add_history("valerian stretch: inverted");
    Ok(Image {
        header,
        planes: image.planes.iter().map(|p| s.invert_frame(p)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    const GRID: usize = 2000;

    fn ghs_cases() -> Vec<Ghs> {
        let mut v = Vec::new();
        for &s in &[0.5, 2.0, 4.0] {
            for &b in &[-3.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 10.0] {
                for &(lp, sp, hp) in &[
                    (0.0, 0.0, 1.0),
                    (0.0, 0.2, 1.0),
                    (0.05, 0.3, 0.9),
                    (0.1, 0.1, 0.8),
                    (0.0, 0.5, 0.5),
                    (0.2, 0.6, 0.95),
                ] {
                    v.push(Ghs {
                        stretch: s,
                        b,
                        sp,
                        lp,
                        hp,
                    });
                }
            }
        }
        v
    }

    fn all_curves() -> Vec<Curve> {
        let mut v: Vec<Curve> = ghs_cases().into_iter().map(Curve::Ghs).collect();
        for &m in &[0.001, 0.05, 0.25, 0.5, 0.75, 0.95] {
            v.push(Curve::Mtf { midtone: m });
        }
        for &b in &[0.01, 1.0, 10.0, 100.0, 5000.0] {
            v.push(Curve::Arcsinh { beta: b });
        }
        v
    }

    #[test]
    fn every_curve_fixes_the_ends_and_increases_strictly() {
        for c in all_curves() {
            c.check().expect("valid");
            assert!(c.apply(0.0).abs() < 1e-12, "{c:?}: f(0) = {}", c.apply(0.0));
            assert!((c.apply(1.0) - 1.0).abs() < 1e-12, "{c:?}: f(1)");
            let mut prev = c.apply(0.0);
            for i in 1..=GRID {
                let y = c.apply(i as f64 / GRID as f64);
                // A hard stretch reaches 1 within double precision before the
                // top of the range; below that the increase must be strict.
                assert!(
                    y > prev || prev > 1.0 - 1e-9,
                    "{c:?} not strictly increasing at {i}: {prev} -> {y}"
                );
                assert!(y >= prev, "{c:?} decreases at {i}: {prev} -> {y}");
                assert!((0.0..=1.0 + 1e-12).contains(&y), "{c:?} left 0..1: {y}");
                prev = y;
            }
        }
    }

    #[test]
    fn every_inverse_undoes_its_curve() {
        for c in all_curves() {
            for i in 0..=400 {
                let x = f64::from(i) / 400.0;
                let back = c.inverse(c.apply(x));
                // Where the curve is nearly flat the inverse is ill-conditioned
                // in x, so compare in y as well: apply(inverse(y)) == y.
                let y = x;
                let round = c.apply(c.inverse(y));
                assert!((round - y).abs() < 1e-9, "{c:?}: f(f^-1({y})) = {round}");
                let steep = c.apply((x + 1e-4).min(1.0)) - c.apply((x - 1e-4).max(0.0));
                if steep > 1e-3 {
                    assert!((back - x).abs() < 1e-6, "{c:?}: f^-1(f({x})) = {back}");
                }
            }
        }
    }

    #[test]
    fn mtf_maps_the_midtone_to_one_half_and_its_inverse_is_the_mirror() {
        for &m in &[0.01, 0.1, 0.3, 0.7, 0.9] {
            assert!((mtf(m, m) - 0.5).abs() < 1e-12);
            for i in 1..20 {
                let x = f64::from(i) / 20.0;
                assert!((Curve::Mtf { midtone: m }.inverse(x) - mtf(x, 1.0 - m)).abs() < 1e-12);
            }
        }
        // 0.5 is the identity; below it brightens, above it darkens.
        assert!((mtf(0.37, 0.5) - 0.37).abs() < 1e-12);
        assert!(mtf(0.2, 0.1) > 0.2);
        assert!(mtf(0.2, 0.9) < 0.2);
    }

    #[test]
    fn arcsinh_matches_its_closed_form_and_tends_to_identity() {
        let beta = 30.0_f64;
        let c = Curve::Arcsinh { beta };
        for i in 0..=10 {
            let x = f64::from(i) / 10.0;
            let expect = (beta * x).asinh() / beta.asinh();
            assert!((c.apply(x) - expect).abs() < 1e-14);
        }
        let weak = Curve::Arcsinh { beta: 1e-6 };
        assert!((weak.apply(0.4) - 0.4).abs() < 1e-9);
        // Harder stretch lifts the shadows more.
        assert!(
            Curve::Arcsinh { beta: 100.0 }.apply(0.01) > Curve::Arcsinh { beta: 10.0 }.apply(0.01)
        );
    }

    #[test]
    fn ghs_with_no_stretch_is_the_identity() {
        let g = Ghs {
            stretch: 0.0,
            b: 3.0,
            sp: 0.3,
            lp: 0.1,
            hp: 0.9,
        };
        for i in 0..=10 {
            let x = f64::from(i) / 10.0;
            assert_eq!(g.apply(x), x);
            assert_eq!(g.inverse(x), x);
        }
    }

    #[test]
    fn ghs_reproduces_the_documented_closed_forms() {
        // SP = 0, LP = 0, HP = 1 leaves only T(x), normalised by T(1).
        let s = 3.0_f64;
        let d = s.exp_m1();
        let at = |b: f64, x: f64| Curve::Ghs(Ghs::new(s, b, 0.0)).apply(x);
        for i in 0..=20 {
            let x = f64::from(i) / 20.0;
            // b = -1: ln(1 + D x) / ln(1 + D)
            let log = (d * x).ln_1p() / d.ln_1p();
            assert!((at(-1.0, x) - log).abs() < 1e-12, "log at {x}");
            // b = 0: (1 - e^-Dx) / (1 - e^-D)
            let exp = (1.0 - (-d * x).exp()) / (1.0 - (-d).exp());
            assert!((at(0.0, x) - exp).abs() < 1e-12, "exp at {x}");
            // b = 1: x (1 + D) / (1 + D x)
            let harm = x * (1.0 + d) / (1.0 + d * x);
            assert!((at(1.0, x) - harm).abs() < 1e-12, "harmonic at {x}");
            // b = 2: (1 - (1 + 2 D x)^-1/2) / (1 - (1 + 2 D)^-1/2)
            let hyp = (1.0 - (1.0 + 2.0 * d * x).powf(-0.5)) / (1.0 - (1.0 + 2.0 * d).powf(-0.5));
            assert!((at(2.0, x) - hyp).abs() < 1e-12, "hyperbolic at {x}");
            // b = -2: (1 - (1 + 2 D x)^(1/2)) / (-D) over the same at 1
            let integral = (1.0 - (1.0 + 2.0 * d * x).sqrt()) / (1.0 - (1.0 + 2.0 * d).sqrt());
            assert!((at(-2.0, x) - integral).abs() < 1e-12, "integral at {x}");
        }
    }

    #[test]
    fn ghs_is_point_symmetric_about_the_symmetry_point() {
        // Inside the protected limits, f(SP + u) + f(SP - u) = 2 f(SP).
        for &b in &[-1.0, 0.0, 1.0, 4.0] {
            let g = Ghs {
                stretch: 4.0,
                b,
                sp: 0.4,
                lp: 0.0,
                hp: 1.0,
            };
            let c = g.apply(0.4);
            for i in 1..=10 {
                let u = 0.04 * f64::from(i);
                let sum = g.apply(0.4 + u) + g.apply(0.4 - u);
                assert!((sum - 2.0 * c).abs() < 1e-12, "b {b}, u {u}");
            }
        }
    }

    #[test]
    fn ghs_slope_is_steepest_at_the_symmetry_point_and_b_focuses_it() {
        let slope = |g: &Ghs, x: f64| (g.apply(x + 1e-6) - g.apply(x - 1e-6)) / 2e-6;
        for &b in &[-1.0, 0.0, 2.0] {
            let g = Ghs::new(4.0, b, 0.25);
            let peak = slope(&g, 0.25);
            for &x in &[0.02, 0.1, 0.2, 0.3, 0.5, 0.9] {
                assert!(
                    slope(&g, x) < peak,
                    "b {b}: slope at {x} not below the peak"
                );
            }
        }
        // The logarithmic form has a much heavier tail than the exponential one.
        let tail = |b: f64| {
            let g = Ghs::new(4.0, b, 0.25);
            slope(&g, 0.45) / slope(&g, 0.25)
        };
        assert!(tail(-1.0) > 100.0 * tail(0.0));
    }

    #[test]
    fn ghs_protection_limits_make_the_ends_straight_and_continuous() {
        let g = Ghs {
            stretch: 6.0,
            b: 1.0,
            sp: 0.4,
            lp: 0.15,
            hp: 0.7,
        };
        let slope = |x: f64| (g.apply(x + 1e-7) - g.apply(x - 1e-7)) / 2e-7;
        // Constant slope below LP and above HP.
        for &x in &[0.01, 0.07, 0.14] {
            assert!((slope(x) - slope(0.05)).abs() < 1e-5 * slope(0.05));
        }
        for &x in &[0.71, 0.85, 0.99] {
            assert!((slope(x) - slope(0.8)).abs() < 1e-5 * slope(0.8));
        }
        // Continuous in value and in slope across LP, SP and HP.
        for &p in &[0.15, 0.4, 0.7] {
            assert!((g.apply(p - 1e-9) - g.apply(p + 1e-9)).abs() < 1e-6);
            assert!((slope(p - 1e-4) - slope(p + 1e-4)).abs() < 1e-2 * slope(p));
        }
        // Below LP the curve keeps the gain it has at LP instead of flattening
        // further, so its shadow gain relative to SP is higher than unprotected.
        let free = Ghs::new(6.0, 1.0, 0.4);
        let free_slope = |x: f64| (free.apply(x + 1e-7) - free.apply(x - 1e-7)) / 2e-7;
        assert!(slope(0.05) / slope(0.4) > free_slope(0.05) / free_slope(0.4));
    }

    #[test]
    fn ghs_rejects_out_of_range_parameters() {
        for g in [
            Ghs {
                stretch: -1.0,
                ..Ghs::new(1.0, 0.0, 0.5)
            },
            Ghs {
                lp: 0.6,
                ..Ghs::new(1.0, 0.0, 0.5)
            },
            Ghs {
                hp: 0.4,
                ..Ghs::new(1.0, 0.0, 0.5)
            },
            Ghs::new(1.0, f64::NAN, 0.5),
            Ghs::new(1.0, 0.0, 1.5),
        ] {
            assert!(Curve::Ghs(g).check().is_err(), "{g:?}");
        }
        assert!(Curve::Mtf { midtone: 0.0 }.check().is_err());
        assert!(Curve::Mtf { midtone: 1.0 }.check().is_err());
        assert!(Curve::Arcsinh { beta: 0.0 }.check().is_err());
    }

    fn sky_frame(w: usize, h: usize, sky: f32, sigma: f32, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let data = (0..w * h)
            .map(|_| sky + (f64::from(sigma) * rng.normal()) as f32)
            .collect();
        Frame::new(w, h, data).expect("frame")
    }

    #[test]
    fn auto_mtf_puts_the_sky_at_the_target() {
        let mut f = sky_frame(200, 150, 500.0, 20.0, 3);
        // A few bright stars so the white point is well above the sky.
        for (i, v) in f.data.iter_mut().enumerate() {
            if i % 997 == 0 {
                *v += 30_000.0;
            }
        }
        for &target in &[0.1, 0.25, 0.4] {
            let s = auto_mtf(&f, target, AutoLevels::default()).expect("stretch");
            let out = s.apply_frame(&f);
            let med = stats::median_finite(&out.data);
            assert!(
                (f64::from(med) - target).abs() < 0.01,
                "target {target}: {med}"
            );
            assert!(s.levels.black < 500.0 && s.levels.black > 500.0 - 4.0 * 20.0 * 1.2);
        }
        assert!(auto_mtf(&f, 0.0, AutoLevels::default()).is_err());
    }

    #[test]
    fn stretching_keeps_nan_and_never_touches_the_source() {
        let mut f = sky_frame(40, 30, 100.0, 5.0, 4);
        f.data[17] = f32::NAN;
        let before = f.clone();
        let s = Stretch {
            levels: Levels {
                black: 80.0,
                white: 200.0,
            },
            curve: Curve::Arcsinh { beta: 50.0 },
        };
        let out = s.apply_frame(&f);
        assert!(out.data[17].is_nan());
        assert_eq!(
            f.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            before.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert!(
            out.data
                .iter()
                .filter(|v| !v.is_nan())
                .all(|v| (0.0..=1.0).contains(v))
        );
    }

    #[test]
    fn a_recorded_stretch_can_be_read_back_and_undone() {
        let levels = Levels {
            black: 90.0,
            white: 400.0,
        };
        let w = 64;
        let data: Vec<f32> = (0..w * w)
            .map(|i| 90.0 + (i as f32 / (w * w) as f32) * 310.0)
            .collect();
        let f = Frame::new(w, w, data).expect("frame");
        for curve in [
            Curve::Mtf { midtone: 0.12 },
            Curve::Arcsinh { beta: 40.0 },
            Curve::Ghs(Ghs {
                stretch: 4.0,
                b: 0.5,
                sp: 0.1,
                lp: 0.02,
                hp: 0.95,
            }),
        ] {
            let s = Stretch { levels, curve };
            let img = Image::mono(s.apply_frame(&f), Header::new());
            let mut header = img.header.clone();
            s.record(&mut header);
            let back = Stretch::from_header(&header).expect("recorded");
            assert_eq!(back, s);
            let restored = back.invert_frame(&img.planes[0]);
            let worst = restored
                .data
                .iter()
                .zip(&f.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(worst < 0.5, "{curve:?}: restored to within {worst} of 310");
        }
        assert!(Stretch::from_header(&Header::new()).is_none());
    }

    #[test]
    fn invert_image_restores_the_data_and_drops_the_record() {
        let f = sky_frame(30, 20, 200.0, 10.0, 8);
        let img = Image::mono(f.clone(), Header::new());
        let plan = Plan::Curve(
            Curve::Ghs(Ghs::new(3.0, 1.0, 0.2)),
            AutoLevels {
                shadow_sigma: 6.0,
                white_percentile: 100.0,
            },
        );
        let stretched = stretch_image(&img, &plan, true).expect("stretch");
        let back = invert_image(&stretched).expect("invert");
        assert!(Stretch::from_header(&back.header).is_none());
        for (a, b) in back.planes[0].data.iter().zip(&f.data) {
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
        }
        assert!(invert_image(&img).is_err());
    }

    #[test]
    fn linked_stretch_uses_one_curve_and_unlinked_one_per_plane() {
        let a = sky_frame(50, 40, 100.0, 5.0, 5);
        let mut b = sky_frame(50, 40, 300.0, 5.0, 6);
        b.data[0] += 5000.0;
        let img = Image {
            header: Header::new(),
            planes: vec![a, b],
        };
        let plan = Plan::AutoMtf {
            target: 0.2,
            auto: AutoLevels::default(),
        };
        let linked = stretch_image(&img, &plan, true).expect("linked");
        assert!(Stretch::from_header(&linked.header).is_some());
        // Linked keeps the channels' relative brightness; unlinked puts both skies at 0.2.
        let unlinked = stretch_image(&img, &plan, false).expect("unlinked");
        let m = |f: &Frame| stats::median_finite(&f.data);
        assert!((m(&unlinked.planes[0]) - 0.2).abs() < 0.02);
        assert!((m(&unlinked.planes[1]) - 0.2).abs() < 0.02);
        assert!(m(&linked.planes[1]) > m(&linked.planes[0]) + 0.05);
        assert!(Stretch::from_header(&unlinked.header).is_none());
    }
}
