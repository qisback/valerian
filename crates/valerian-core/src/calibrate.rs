// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Calibration: master bias, dark, flat and dark-flat frames, the hot and cold
//! pixel map, cosmetic correction, and applying them to light frames.
//!
//! The model of a light frame is
//! `light = bias + dark_rate * t + signal * flat + noise`, so the calibrated
//! frame is `(light - bias - k * dark_current) / flat` with `k = t / t_dark`
//! when the dark is scaled. Master frames are sigma-clipped (or winsorised, or
//! linear-fit) means of their inputs; see [`crate::combine`].
//!
//! Conventions:
//!
//! * A master dark made with a master bias is *bias-subtracted* (it holds only
//!   dark current, marked `BIASSUB = T`) and can be scaled to other exposure
//!   times. Without a bias it holds bias plus dark current and can only be
//!   subtracted as it is, at the same exposure.
//! * A master flat is normalised to a mean of one.
//! * Raw one-shot-colour data stays a mosaic through calibration; neighbours
//!   for cosmetic correction are taken from the same colour (two pixels away).
//!
//! # References
//!
//! - Howell, S. B. (2006), Handbook of CCD Astronomy, 2nd ed., Cambridge University Press, doi:10.1017/CBO9780511807909 (bias, dark and flat calibration).

use rayon::prelude::*;

use crate::combine::{Options, Rejection, combine};
use crate::error::{Error, Result};
use crate::fits::{Header, Image, Value};
use crate::stats;
use helia_core::Frame;

fn check_same_shape(frames: &[Image]) -> Result<()> {
    let first = frames
        .first()
        .ok_or_else(|| Error::invalid("no frames given"))?;
    for f in frames {
        if f.planes.len() != first.planes.len()
            || f.width() != first.width()
            || f.height() != first.height()
        {
            return Err(Error::invalid("calibration frames differ in size"));
        }
    }
    Ok(())
}

pub(crate) fn mean_header_value(
    headers: &[&Header],
    f: impl Fn(&Header) -> Option<f64>,
) -> Option<f64> {
    let v: Vec<f64> = headers.iter().filter_map(|h| f(h)).collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Header for a master built from frames with these `headers`.
pub(crate) fn master_header(headers: &[&Header], kind: &str, rejection: Rejection) -> Header {
    let mut h = headers[0].clone();
    for k in ["IMAGETYP", "FRAMETYP", "NCOMBINE", "BIASSUB", "STACKALG"] {
        h.remove(k);
    }
    h.set("IMAGETYP", Value::Str(kind.into()), "kind of frame");
    h.set(
        "NCOMBINE",
        Value::Int(headers.len() as i64),
        "frames in this master",
    );
    h.set(
        "STACKALG",
        Value::Str(format!("{rejection:?}")),
        "rejection used to combine",
    );
    if let Some(t) = mean_header_value(headers, Header::temperature) {
        h.set(
            "CCD-TEMP",
            Value::Float(t),
            "[degC] mean sensor temperature",
        );
    }
    h
}

fn headers_of(frames: &[Image]) -> Vec<&Header> {
    frames.iter().map(|f| &f.header).collect()
}

fn combine_planes(frames: &[Image], rejection: Rejection) -> Result<Vec<Frame>> {
    check_same_shape(frames)?;
    let opts = Options {
        rejection,
        ..Options::default()
    };
    (0..frames[0].planes.len())
        .map(|p| {
            let refs: Vec<&Frame> = frames.iter().map(|f| &f.planes[p]).collect();
            Ok(combine(&refs, &opts)?.mean)
        })
        .collect()
}

/// Master bias: the rejected mean of the bias frames.
///
/// # Errors
/// [`Error::Invalid`] when there are no frames or they differ in size.
pub fn master_bias(frames: &[Image], rejection: Rejection) -> Result<Image> {
    let planes = combine_planes(frames, rejection)?;
    let mut header = master_header(&headers_of(frames), "Master Bias", rejection);
    header.set("EXPTIME", Value::Float(0.0), "[s]");
    Ok(Image { header, planes })
}

fn subtract(a: &Frame, b: &Frame) -> Frame {
    let mut out = a.clone();
    out.data
        .par_iter_mut()
        .zip(&b.data)
        .for_each(|(x, y)| *x -= *y);
    out
}

fn subtract_master(frames: &[Image], master: &Image) -> Result<Vec<Image>> {
    check_same_shape(frames)?;
    if master.planes.len() != frames[0].planes.len()
        || master.width() != frames[0].width()
        || master.height() != frames[0].height()
    {
        return Err(Error::invalid(
            "master frame differs in size from the frames",
        ));
    }
    Ok(frames
        .iter()
        .map(|f| Image {
            header: f.header.clone(),
            planes: f
                .planes
                .iter()
                .zip(&master.planes)
                .map(|(a, b)| subtract(a, b))
                .collect(),
        })
        .collect())
}

/// Master dark: the rejected mean of the dark frames. With a master `bias`
/// the result is bias-subtracted dark current (`BIASSUB = T`), which can be
/// scaled to other exposure times.
///
/// # Errors
/// [`Error::Invalid`] on empty or mismatched inputs.
pub fn master_dark(frames: &[Image], bias: Option<&Image>, rejection: Rejection) -> Result<Image> {
    let planes = match bias {
        Some(b) => combine_planes(&subtract_master(frames, b)?, rejection)?,
        None => combine_planes(frames, rejection)?,
    };
    let mut header = master_header(&headers_of(frames), "Master Dark", rejection);
    header.set(
        "BIASSUB",
        Value::Bool(bias.is_some()),
        "bias removed from this dark",
    );
    if let Some(t) = mean_header_value(&headers_of(frames), Header::exposure) {
        header.set(
            "EXPTIME",
            Value::Float(t),
            "[s] exposure of the dark frames",
        );
    }
    Ok(Image { header, planes })
}

/// How to bring a flat to a mean of one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlatNormalisation {
    /// Divide by the mean of the whole frame. Colour balance is kept as the
    /// flat records it, so a one-shot-colour flat also corrects channel gain.
    #[default]
    Global,
    /// Divide each Bayer colour by its own mean, so the flat only corrects
    /// vignetting and dust and leaves the colour balance alone.
    PerChannel,
}

/// Normalise a flat so its (or each colour's) mean is one.
pub(crate) fn normalise_flat(f: &mut Frame, mode: FlatNormalisation) {
    let per_channel = if mode == FlatNormalisation::PerChannel {
        f.bayer
    } else {
        None
    };
    let means: Vec<f64> = if let Some(p) = per_channel {
        let (mut s, mut n) = ([0.0_f64; 3], [0.0_f64; 3]);
        for (i, &v) in f.data.iter().enumerate() {
            if v.is_finite() {
                let c = p.channel_at(i % f.width, i / f.width);
                s[c] += f64::from(v);
                n[c] += 1.0;
            }
        }
        (0..3).map(|c| s[c] / n[c].max(1.0)).collect()
    } else {
        let (mut s, mut n) = (0.0_f64, 0.0_f64);
        for &v in &f.data {
            if v.is_finite() {
                s += f64::from(v);
                n += 1.0;
            }
        }
        vec![s / n.max(1.0)]
    };
    let (w, bayer) = (f.width, f.bayer);
    f.data.par_iter_mut().enumerate().for_each(|(i, v)| {
        let m = match (means.len(), bayer) {
            (3, Some(p)) => means[p.channel_at(i % w, i / w)],
            _ => means[0],
        };
        if m > 0.0 {
            *v = (f64::from(*v) / m) as f32;
        }
    });
}

/// The level a flat is scaled by: the median of a subsample of its first plane.
pub(crate) fn flat_level(img: &Image) -> f32 {
    stats::median_finite(&stats::subsample(img.plane(), 1 << 18))
}

/// Master flat. Each flat has `subtract` removed (a master dark-flat, or a
/// master bias if there are no dark flats), is scaled to a common level by its
/// median, combined, and normalised.
///
/// # Errors
/// [`Error::Invalid`] on empty or mismatched inputs.
pub fn master_flat(
    flats: &[Image],
    subtract: Option<&Image>,
    rejection: Rejection,
    normalisation: FlatNormalisation,
) -> Result<Image> {
    let mut calibrated = if let Some(m) = subtract {
        subtract_master(flats, m)?
    } else {
        check_same_shape(flats)?;
        flats.to_vec()
    };
    // Scale each flat to the level of the first by its median, so a lamp that
    // drifts between frames does not look like noise to the rejection.
    let target = flat_level(&calibrated[0]);
    for img in &mut calibrated {
        let l = flat_level(img);
        if l > 0.0 && target > 0.0 {
            let k = target / l;
            for p in &mut img.planes {
                p.data.par_iter_mut().for_each(|v| *v *= k);
            }
        }
    }
    let mut planes = combine_planes(&calibrated, rejection)?;
    for p in &mut planes {
        normalise_flat(p, normalisation);
    }
    let mut header = master_header(&headers_of(flats), "Master Flat", rejection);
    header.set(
        "FLATNORM",
        Value::Str(format!("{normalisation:?}")),
        "normalisation",
    );
    Ok(Image { header, planes })
}

/// Flag bits of a [`PixelMap`].
pub mod flag {
    /// Reads high in the dark.
    pub const HOT: u8 = 1;
    /// Reads low in the dark.
    pub const COLD: u8 = 2;
    /// Responds far too little in the flat.
    pub const DEAD: u8 = 4;
}

/// A map of defective pixels.
#[derive(Clone, Debug)]
pub struct PixelMap {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Distance to a same-colour neighbour: 2 for Bayer data, else 1.
    pub step: usize,
    /// One byte of [`flag`] bits per pixel.
    pub flags: Vec<u8>,
}

/// The eight same-colour neighbours of `(x, y)`, mirrored at the borders.
fn neighbours(f: &Frame, x: usize, y: usize, step: usize) -> [f32; 8] {
    let (w, h, s) = (f.width as i64, f.height as i64, step as i64);
    let m = |i: i64, n: i64| -> usize {
        (if i < 0 {
            -i
        } else if i >= n {
            2 * (n - 1) - i
        } else {
            i
        })
        .clamp(0, n - 1) as usize
    };
    let mut out = [0.0; 8];
    let mut k = 0;
    for dy in [-s, 0, s] {
        for dx in [-s, 0, s] {
            if dx == 0 && dy == 0 {
                continue;
            }
            out[k] = f.data[m(y as i64 + dy, h) * f.width + m(x as i64 + dx, w)];
            k += 1;
        }
    }
    out
}

fn median8(mut n: [f32; 8]) -> f32 {
    n.sort_by(f32::total_cmp);
    f32::midpoint(n[3], n[4])
}

impl PixelMap {
    /// An empty map for frames of this shape.
    #[must_use]
    pub fn empty(width: usize, height: usize, bayer: bool) -> Self {
        Self {
            width,
            height,
            step: if bayer { 2 } else { 1 },
            flags: vec![0; width * height],
        }
    }

    /// Pixels flagged at all.
    #[must_use]
    pub fn count(&self) -> usize {
        self.flags.iter().filter(|&&f| f != 0).count()
    }

    /// Pixels carrying a given [`flag`].
    #[must_use]
    pub fn count_flag(&self, bit: u8) -> usize {
        self.flags.iter().filter(|&&f| f & bit != 0).count()
    }

    /// Find hot and cold pixels in a master dark. Each pixel is compared with
    /// the median of its same-colour neighbours, so slow gradients such as amp
    /// glow do not trip it; the limit is `hot` (or `cold`) times the robust
    /// sigma of that difference over the frame.
    #[must_use]
    pub fn from_dark(dark: &Frame, hot: f32, cold: f32) -> Self {
        let mut map = Self::empty(dark.width, dark.height, dark.bayer.is_some());
        let residual = residuals(dark, map.step);
        let (_, sigma) = stats::median_sigma(&stats::subsample(
            &Frame {
                width: dark.width,
                height: dark.height,
                data: residual.clone(),
                bayer: None,
            },
            1 << 20,
        ));
        let sigma = sigma.max(f32::MIN_POSITIVE);
        map.flags
            .par_iter_mut()
            .zip(&residual)
            .for_each(|(fl, &r)| {
                if r > hot * sigma {
                    *fl |= flag::HOT;
                } else if r < -cold * sigma {
                    *fl |= flag::COLD;
                }
            });
        map
    }

    /// Flag pixels whose normalised flat is below `threshold` (dead pixels and
    /// opaque specks).
    pub fn add_dead_from_flat(&mut self, flat: &Frame, threshold: f32) {
        for (fl, &v) in self.flags.iter_mut().zip(&flat.data) {
            if v < threshold {
                *fl |= flag::DEAD;
            }
        }
    }

    /// Merge another map's flags into this one.
    pub fn merge(&mut self, other: &Self) {
        for (a, b) in self.flags.iter_mut().zip(&other.flags) {
            *a |= *b;
        }
    }

    /// Replace flagged pixels by the median of their unflagged same-colour
    /// neighbours, widening the search up to three rings if every neighbour is
    /// flagged. Returns the number of pixels replaced.
    pub fn correct(&self, frame: &mut Frame) -> usize {
        let step = self.step;
        let (w, h) = (self.width as i64, self.height as i64);
        let list: Vec<usize> = (0..self.flags.len())
            .filter(|&i| self.flags[i] != 0)
            .collect();
        let mut fixes = Vec::with_capacity(list.len());
        for &i in &list {
            let (x, y) = ((i % self.width) as i64, (i / self.width) as i64);
            for ring in 1..=3_i64 {
                let r = ring * step as i64;
                let mut vals = Vec::with_capacity(8 * ring as usize);
                for dy in (-r..=r).step_by(step) {
                    for dx in (-r..=r).step_by(step) {
                        if dx.abs().max(dy.abs()) != r {
                            continue;
                        }
                        let (xx, yy) = (x + dx, y + dy);
                        if xx < 0 || yy < 0 || xx >= w || yy >= h {
                            continue;
                        }
                        let j = yy as usize * self.width + xx as usize;
                        if self.flags[j] == 0 && frame.data[j].is_finite() {
                            vals.push(frame.data[j]);
                        }
                    }
                }
                if vals.len() >= 3 {
                    fixes.push((i, stats::median_in_place(&mut vals)));
                    break;
                }
            }
        }
        for &(i, v) in &fixes {
            frame.data[i] = v;
        }
        fixes.len()
    }

    /// Find isolated hot and cold pixels in a light frame itself, for use when
    /// no master dark is available. A pixel is flagged when it differs from the
    /// median of its neighbours by more than `hot` (or `cold`) robust sigmas
    /// *and* its neighbours do not share the excess, which is what separates a
    /// single bad pixel from the core of an undersampled star.
    #[must_use]
    pub fn detect_isolated(frame: &Frame, hot: f32, cold: f32) -> Self {
        let mut map = Self::empty(frame.width, frame.height, frame.bayer.is_some());
        let step = map.step;
        let residual = residuals(frame, step);
        let (_, sigma) = stats::median_sigma(&stats::subsample(
            &Frame {
                width: frame.width,
                height: frame.height,
                data: residual.clone(),
                bayer: None,
            },
            1 << 20,
        ));
        let sigma = sigma.max(f32::MIN_POSITIVE);
        let w = frame.width;
        map.flags.par_iter_mut().enumerate().for_each(|(i, fl)| {
            let r = residual[i];
            if r.abs() <= hot.min(cold) * sigma {
                return;
            }
            let (x, y) = (i % w, i / w);
            let mut n = neighbours(frame, x, y, step);
            let med = median8(n);
            n.sort_by(f32::total_cmp);
            // The neighbours must not share the excess: the second
            // highest (lowest) stays within three sigma of their median.
            if r > hot * sigma {
                if n[6] - med < 3.0 * sigma {
                    *fl |= flag::HOT;
                }
            } else if r < -cold * sigma && n[1] - med > -3.0 * sigma {
                *fl |= flag::COLD;
            }
        });
        map
    }
}

/// Each pixel minus the median of its same-colour neighbours.
fn residuals(f: &Frame, step: usize) -> Vec<f32> {
    let w = f.width;
    (0..f.data.len())
        .into_par_iter()
        .map(|i| f.data[i] - median8(neighbours(f, i % w, i / w, step)))
        .collect()
}

/// How the dark is scaled to a light's exposure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DarkScaling {
    /// Subtract the dark as it is.
    #[default]
    Off,
    /// Multiply by `exposure_light / exposure_dark`.
    Exposure,
    /// Fit the multiplier to the light itself from its hot pixels, falling
    /// back to the exposure ratio when there are too few.
    Optimise,
}

/// Everything needed to calibrate light frames.
#[derive(Clone, Debug, Default)]
pub struct Calibrator {
    /// Master bias.
    pub bias: Option<Image>,
    /// Master dark.
    pub dark: Option<Image>,
    /// Master flat, normalised to a mean of one.
    pub flat: Option<Image>,
    /// Defective pixels to repair after calibration.
    pub pixel_map: Option<PixelMap>,
    /// Dark scaling.
    pub scaling: DarkScaling,
    /// Flat values below this are treated as dead and left uncorrected.
    pub flat_floor: f32,
}

/// What [`Calibrator::calibrate`] did, for the log and the FITS history.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// Dark multiplier used (1 when unscaled, 0 with no dark).
    pub dark_scale: f64,
    /// Pixels repaired by cosmetic correction.
    pub repaired: usize,
}

impl Calibrator {
    /// A calibrator from masters; scaling off, flat floor 0.05.
    #[must_use]
    pub fn new(bias: Option<Image>, dark: Option<Image>, flat: Option<Image>) -> Self {
        Self {
            bias,
            dark,
            flat,
            pixel_map: None,
            scaling: DarkScaling::Off,
            flat_floor: 0.05,
        }
    }

    fn dark_is_bias_subtracted(&self) -> bool {
        self.dark
            .as_ref()
            .is_some_and(|d| d.header.get("BIASSUB") == Some(&Value::Bool(true)))
    }

    fn dark_scale(&self, light_minus_bias: &Frame, light: &Image) -> Result<f64> {
        let Some(dark) = &self.dark else {
            return Ok(0.0);
        };
        if self.scaling == DarkScaling::Off {
            return Ok(1.0);
        }
        if !self.dark_is_bias_subtracted() {
            return Err(Error::invalid(
                "dark scaling needs a master dark made with a master bias",
            ));
        }
        let ratio = match (light.header.exposure(), dark.header.exposure()) {
            (Some(l), Some(d)) if d > 0.0 => Some(l / d),
            _ => None,
        };
        if self.scaling == DarkScaling::Exposure {
            return ratio
                .ok_or_else(|| Error::invalid("exposure time missing from the light or the dark"));
        }
        // Optimise: regress the light on the dark current over the hot pixels.
        let d = dark.plane();
        let (dm, ds) = stats::median_sigma(&stats::subsample(d, 1 << 20));
        let lm = stats::median_finite(&stats::subsample(light_minus_bias, 1 << 20));
        let (mut sxy, mut sxx, mut n) = (0.0_f64, 0.0_f64, 0usize);
        for (&dv, &lv) in d.data.iter().zip(&light_minus_bias.data) {
            if dv > dm + 8.0 * ds {
                let x = f64::from(dv - dm);
                sxy += x * f64::from(lv - lm);
                sxx += x * x;
                n += 1;
            }
        }
        if n >= 50 && sxx > 0.0 {
            Ok((sxy / sxx).clamp(0.05, 20.0))
        } else {
            Ok(ratio.unwrap_or(1.0))
        }
    }

    /// Check that every master has the shape of `light`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when one differs.
    pub fn check_shapes(&self, light: &Image) -> Result<()> {
        for m in [&self.bias, &self.dark, &self.flat].into_iter().flatten() {
            if m.planes.len() != light.planes.len()
                || m.width() != light.width()
                || m.height() != light.height()
            {
                return Err(Error::invalid(
                    "a master frame differs in size from the light",
                ));
            }
        }
        Ok(())
    }

    /// The multiplier the dark is scaled by for `light` (0 with no dark, 1 when
    /// scaling is off), after checking the masters' shapes. Another engine
    /// ([`crate::compute::Compute`]) calls this and then does the per-pixel work
    /// itself.
    ///
    /// # Errors
    /// As [`Calibrator::calibrate`].
    pub fn dark_multiplier(&self, light: &Image) -> Result<f64> {
        self.check_shapes(light)?;
        if self.scaling == DarkScaling::Optimise && self.dark.is_some() {
            // The fit needs the light with its bias removed.
            let mut first = light.plane().clone();
            if let Some(b) = &self.bias {
                first
                    .data
                    .iter_mut()
                    .zip(&b.plane().data)
                    .for_each(|(x, y)| *x -= *y);
            }
            return self.dark_scale(&first, light);
        }
        self.dark_scale(light.plane(), light)
    }

    /// Put the calibration in `out`'s header: the image type and a `HISTORY`
    /// line with the dark multiplier `k` and the pixels `repaired`.
    pub fn annotate(&self, out: &mut Image, k: f64, repaired: usize) {
        out.header
            .set("IMAGETYP", Value::Str("Calibrated Light".into()), "");
        out.header.add_history(&format!(
            "calibrated by Valerian: bias {}, dark {} (x{k:.4}), flat {}, {repaired} pixels repaired",
            self.bias.is_some(),
            self.dark.is_some(),
            self.flat.is_some()
        ));
    }

    /// Calibrate a light frame: bias, dark, flat, then the pixel map.
    ///
    /// # Errors
    /// [`Error::Invalid`] when a master differs in shape from the light, or
    /// the chosen dark scaling cannot be worked out.
    pub fn calibrate(&self, light: &Image) -> Result<(Image, Report)> {
        self.check_shapes(light)?;
        let mut out = light.clone();
        if let Some(b) = &self.bias {
            for (p, m) in out.planes.iter_mut().zip(&b.planes) {
                p.data
                    .par_iter_mut()
                    .zip(&m.data)
                    .for_each(|(x, y)| *x -= *y);
            }
        }
        let k = self.dark_scale(out.plane(), light)?;
        if let Some(d) = &self.dark {
            let kf = k as f32;
            for (p, m) in out.planes.iter_mut().zip(&d.planes) {
                p.data
                    .par_iter_mut()
                    .zip(&m.data)
                    .for_each(|(x, y)| *x -= kf * *y);
            }
        }
        if let Some(fl) = &self.flat {
            let floor = self.flat_floor;
            for (p, m) in out.planes.iter_mut().zip(&fl.planes) {
                p.data.par_iter_mut().zip(&m.data).for_each(|(x, y)| {
                    if *y > floor {
                        *x /= *y;
                    }
                });
            }
        }
        let mut repaired = 0;
        if let Some(map) = &self.pixel_map {
            for p in &mut out.planes {
                repaired += map.correct(p);
            }
        }
        self.annotate(&mut out, k, repaired);
        Ok((
            out,
            Report {
                dark_scale: k,
                repaired,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Rng, Sensor, gaussian_field};

    fn rms(a: &Frame, b: &Frame) -> f32 {
        let n = a.data.len() as f32;
        (a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (x - y) * (x - y))
            .sum::<f32>()
            / n)
            .sqrt()
    }

    fn mean(f: &Frame) -> f32 {
        f.data.iter().sum::<f32>() / f.data.len() as f32
    }

    struct Rig {
        sensor: Sensor,
        bias: Image,
        dark: Image,
        flat: Image,
        calibrator: Calibrator,
    }

    fn build_rig(bayer: bool, seed: u64) -> Rig {
        let mut rng = Rng::new(seed);
        let sensor = Sensor::new(192, 144, bayer, seed);
        let zero = Frame::zeros(192, 144);
        let biases: Vec<Image> = (0..30)
            .map(|_| sensor.expose(&zero, 0.0, &mut rng))
            .collect();
        let bias = master_bias(&biases, Rejection::auto(30)).expect("bias");
        let darks: Vec<Image> = (0..20)
            .map(|_| sensor.expose(&zero, 300.0, &mut rng))
            .collect();
        let dark = master_dark(&darks, Some(&bias), Rejection::auto(20)).expect("dark");
        // Flat panel: uniform light, short exposure, with its own dark flats.
        let lamp = {
            let mut f = Frame::new(192, 144, vec![20_000.0; 192 * 144]).expect("lamp");
            f.bayer = sensor.bayer;
            f
        };
        let flats: Vec<Image> = (0..30)
            .map(|_| sensor.expose(&lamp, 2.0, &mut rng))
            .collect();
        let darkflats: Vec<Image> = (0..30)
            .map(|_| sensor.expose(&zero, 2.0, &mut rng))
            .collect();
        let dark_flat = master_dark(&darkflats, None, Rejection::auto(30)).expect("darkflat");
        let flat = master_flat(
            &flats,
            Some(&dark_flat),
            Rejection::auto(30),
            FlatNormalisation::Global,
        )
        .expect("flat");
        let mut calibrator =
            Calibrator::new(Some(bias.clone()), Some(dark.clone()), Some(flat.clone()));
        let mut map = PixelMap::from_dark(dark.plane(), 6.0, 6.0);
        map.add_dead_from_flat(flat.plane(), 0.2);
        calibrator.pixel_map = Some(map);
        Rig {
            sensor,
            bias,
            dark,
            flat,
            calibrator,
        }
    }

    fn truth_sky(bayer: Option<helia_core::BayerPattern>) -> Frame {
        let mut f = gaussian_field(
            192,
            144,
            60.0,
            &[
                (40.3, 30.7, 4000.0, 1.6),
                (120.1, 90.4, 9000.0, 1.8),
                (160.6, 20.2, 2500.0, 1.5),
                (70.0, 110.0, 6000.0, 1.7),
            ],
        );
        f.bayer = bayer;
        f
    }

    #[test]
    fn master_flat_recovers_the_true_flat() {
        for bayer in [false, true] {
            let rig = build_rig(bayer, 100);
            let mut truth = rig.sensor.flat_frame();
            normalise_flat(&mut truth, FlatNormalisation::Global);
            let err = rms(rig.flat.plane(), &truth);
            assert!(err < 0.004, "bayer={bayer}: master flat rms error {err}");
            assert!((mean(rig.flat.plane()) - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn master_dark_and_bias_recover_the_truth() {
        let rig = build_rig(false, 7);
        let bias_err = rms(rig.bias.plane(), &rig.sensor.bias_frame());
        assert!(bias_err < 0.8, "bias rms error {bias_err}");
        let dark_err = rms(rig.dark.plane(), &rig.sensor.dark_current_frame(300.0));
        assert!(dark_err < 1.6, "dark rms error {dark_err}");
        assert_eq!(rig.dark.header.get("BIASSUB"), Some(&Value::Bool(true)));
        assert_eq!(rig.dark.header.get("NCOMBINE"), Some(&Value::Int(20)));
    }

    #[test]
    fn hot_and_cold_pixels_are_found_and_repaired() {
        let rig = build_rig(false, 21);
        let map = rig.calibrator.pixel_map.as_ref().expect("map");
        let truth = rig.sensor.defect_indices();
        let found = truth.iter().filter(|&&i| map.flags[i] != 0).count();
        assert!(
            found as f32 >= 0.95 * truth.len() as f32,
            "found {found} of {}",
            truth.len()
        );
        // Few false positives: the map should not be mostly clean pixels.
        assert!(
            map.count() < truth.len() * 2 + 40,
            "{} flagged",
            map.count()
        );
    }

    #[test]
    fn calibration_removes_the_instrument_signature() {
        for bayer in [false, true] {
            let rig = build_rig(bayer, 55);
            let mut rng = Rng::new(900);
            let sky = truth_sky(rig.sensor.bayer);
            let light = rig.sensor.expose(&sky, 300.0, &mut rng);
            let (cal, report) = rig.calibrator.calibrate(&light).expect("calibrate");
            assert!((report.dark_scale - 1.0).abs() < 1e-9);
            assert!(report.repaired > 0);
            // After calibration the frame is the truth times the mean flat level.
            let scale = rig.sensor.mean_flat();
            let expected = {
                let mut e = sky.clone();
                e.data.iter_mut().for_each(|v| *v *= scale);
                e
            };
            let raw_err = rms(light.plane(), &expected);
            let err = rms(cal.plane(), &expected);
            assert!(
                raw_err > 20.0 * err.max(1.0) || raw_err > 100.0,
                "raw {raw_err}"
            );
            // The residual is the light's own noise (read noise plus shot noise).
            assert!(
                err < 9.0,
                "bayer={bayer}: calibrated rms error {err} (raw {raw_err})"
            );
            assert!(
                (mean(cal.plane()) - mean(&expected)).abs() < 0.5,
                "bias in the mean: {}",
                mean(cal.plane()) - mean(&expected)
            );
            // Every injected defect is repaired to within the noise.
            for i in rig.sensor.defect_indices() {
                let d = (cal.plane().data[i] - expected.data[i]).abs();
                assert!(d < 60.0, "defect {i} left {d} ADU off");
            }
        }
    }

    #[test]
    fn dark_scaling_by_exposure_matches_a_dark_at_the_right_time() {
        let rig = build_rig(false, 33);
        let mut rng = Rng::new(77);
        let sky = truth_sky(None);
        // The light is 100 s but the master dark is 300 s: k should be 1/3.
        let light = rig.sensor.expose(&sky, 100.0, &mut rng);
        let mut c = rig.calibrator.clone();
        c.scaling = DarkScaling::Exposure;
        let (cal, report) = c.calibrate(&light).expect("calibrate");
        assert!(
            (report.dark_scale - 1.0 / 3.0).abs() < 1e-6,
            "{}",
            report.dark_scale
        );
        let scale = rig.sensor.mean_flat();
        let mut expected = sky.clone();
        expected.data.iter_mut().for_each(|v| *v *= scale);
        let good = rms(cal.plane(), &expected);
        assert!(good < 9.0, "scaled dark rms error {good}");
        // Unscaled, the 300 s dark over-subtracts: the mean level shows it.
        let (bad, _) = rig.calibrator.calibrate(&light).expect("calibrate");
        let offset = |f: &Frame| (mean(f) - mean(&expected)).abs();
        assert!(
            offset(cal.plane()) < 0.6,
            "scaled offset {}",
            offset(cal.plane())
        );
        assert!(
            offset(bad.plane()) > 3.0,
            "unscaled offset {}",
            offset(bad.plane())
        );
    }

    #[test]
    fn optimised_scaling_finds_the_factor_from_the_hot_pixels() {
        let rig = build_rig(false, 44);
        let mut rng = Rng::new(78);
        let sky = truth_sky(None);
        let light = rig.sensor.expose(&sky, 150.0, &mut rng);
        let mut c = rig.calibrator.clone();
        c.scaling = DarkScaling::Optimise;
        c.dark
            .as_mut()
            .expect("dark")
            .header
            .set("EXPTIME", Value::Float(300.0), "");
        let (_, report) = c.calibrate(&light).expect("calibrate");
        assert!(
            (report.dark_scale - 0.5).abs() < 0.05,
            "k = {}",
            report.dark_scale
        );
    }

    #[test]
    fn scaling_without_a_bias_is_refused() {
        let rig = build_rig(false, 9);
        let mut rng = Rng::new(1);
        let zero = Frame::zeros(192, 144);
        let darks: Vec<Image> = (0..6)
            .map(|_| rig.sensor.expose(&zero, 300.0, &mut rng))
            .collect();
        let raw_dark = master_dark(&darks, None, Rejection::auto(6)).expect("dark");
        let mut c = Calibrator::new(None, Some(raw_dark), None);
        c.scaling = DarkScaling::Exposure;
        let light = rig.sensor.expose(&zero, 100.0, &mut rng);
        assert!(c.calibrate(&light).is_err());
    }

    #[test]
    fn mismatched_masters_are_refused() {
        let rig = build_rig(false, 3);
        let small = Image::mono(Frame::zeros(10, 10), Header::new());
        let c = Calibrator::new(Some(rig.bias.clone()), None, None);
        assert!(c.calibrate(&small).is_err());
        assert!(master_bias(&[], Rejection::None).is_err());
    }

    #[test]
    fn isolated_pixels_are_found_without_flagging_stars() {
        let mut rng = Rng::new(5);
        let mut f = gaussian_field(
            160,
            120,
            200.0,
            &[
                (30.0, 30.0, 30_000.0, 1.0),
                (90.5, 60.5, 60_000.0, 0.9),
                (130.2, 90.7, 5_000.0, 1.0),
            ],
        );
        for v in &mut f.data {
            *v += rng.gauss(0.0, 4.0) as f32;
        }
        let hot = [(10, 10), (50, 80), (100, 20), (141, 100)];
        let cold = [(20, 100), (77, 33)];
        for &(x, y) in &hot {
            f.data[y * 160 + x] += 400.0;
        }
        for &(x, y) in &cold {
            f.data[y * 160 + x] -= 150.0;
        }
        let map = PixelMap::detect_isolated(&f, 6.0, 6.0);
        for &(x, y) in hot.iter().chain(&cold) {
            assert!(map.flags[y * 160 + x] != 0, "missed ({x},{y})");
        }
        // The stars' cores are not flagged.
        for (sx, sy) in [(30, 30), (90, 60), (91, 61), (130, 91)] {
            assert_eq!(map.flags[sy * 160 + sx], 0, "star core ({sx},{sy}) flagged");
        }
        assert!(
            map.count() <= hot.len() + cold.len() + 4,
            "{} flagged",
            map.count()
        );
        let mut fixed = f.clone();
        assert!(map.correct(&mut fixed) >= 6);
        assert!((fixed.data[10 * 160 + 10] - 200.0).abs() < 25.0);
    }
}
