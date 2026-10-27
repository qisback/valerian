// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Registering and stacking light frames.
//!
//! The steps, in order:
//!
//! 1. Detect the stars of every frame (on the luminance, for colour frames).
//! 2. Pick the **reference**: the sharpest frame among those with a healthy
//!    star count, or the one the caller names.
//! 3. **Register** every other frame to it by star-pattern matching. A frame
//!    that cannot be registered is left out and the reason reported.
//! 4. **Resample** each frame onto the reference grid. Pixels the frame does
//!    not cover become NaN, which the combiner treats as "no data".
//! 5. **Normalise**: match each frame's sky level to the reference (additive)
//!    and, optionally, its transparency (a multiplicative factor taken from the
//!    flux ratio of the matched stars). The reference's sky level is kept as
//!    the pedestal of the result; deciding where black sits is the stretch's
//!    job, not the stacker's.
//! 6. **Weight** each frame by its noise and star width: the weight
//!    `1 / (noise^2 * fwhm^2)` is the optimum for seeing a point source in a
//!    background-limited frame.
//! 7. **Combine** with per-pixel outlier rejection ([`crate::combine`]), which
//!    removes satellite trails, cosmic rays and hot pixels that registration has
//!    dragged to different sky positions in different frames.
//!
//! All frames are held in memory at once (registered ones replace the originals
//! one at a time, so the peak is about one copy of the set).
//!
//! # References
//!
//! - Naylor, T. (1998), An optimal extraction algorithm for imaging photometry, Mon. Not. R. Astron. Soc. 296, 339-346, doi:10.1046/j.1365-8711.1998.01314.x.
//! - King, I. R. (1983), Accuracy of measurement of star images on a pixel array, Publ. Astron. Soc. Pac. 95, 163, doi:10.1086/131139 (why a point source is best weighted by 1/(noise^2 FWHM^2)).
//! - Horne, K. (1986), An optimal extraction algorithm for CCD spectroscopy, Publ. Astron. Soc. Pac. 98, 609, doi:10.1086/131801.

use crate::cancel::CancelToken;
use crate::combine::{Options, Rejection};
use crate::compute::{Compute, Cpu, StackPlane};
use crate::error::{Error, Result};
use crate::fits::{Image, Value};
use crate::register::{RegisterOptions, Registration, register};
use crate::stars::{DetectOptions, Detection, detect, luminance};
use crate::stats;
use crate::warp::Interpolation;
use helia_core::Frame;

/// How frames are weighted in the mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Weights {
    /// Every frame counts the same.
    Equal,
    /// `1 / noise^2` after normalisation.
    Noise,
    /// `1 / fwhm^2`: favour sharp frames.
    Fwhm,
    /// `1 / (noise^2 * fwhm^2)`: the optimum for point sources.
    #[default]
    NoiseFwhm,
}

/// How frames are brought to a common level before combining.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Normalisation {
    /// Leave levels as they are.
    None,
    /// Match the sky level to the reference.
    Additive,
    /// Match the sky level, and scale by the flux ratio of the matched stars
    /// so a frame taken through thin cloud counts for what it saw.
    #[default]
    AdditiveScaling,
}

/// Which frame is the reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    /// The sharpest frame with a healthy number of stars.
    #[default]
    Best,
    /// The frame at this index.
    Index(usize),
}

/// Settings for [`stack`].
#[derive(Clone, Debug)]
pub struct StackOptions {
    /// Star detection.
    pub detect: DetectOptions,
    /// Registration.
    pub register: RegisterOptions,
    /// Resampling kernel.
    pub interpolation: Interpolation,
    /// Rejection; `None` chooses from the number of frames.
    pub rejection: Option<Rejection>,
    /// Frame weights.
    pub weights: Weights,
    /// Level matching.
    pub normalisation: Normalisation,
    /// The reference frame.
    pub reference: Reference,
    /// Frames with fewer stars than this are not registered.
    pub min_stars: usize,
    /// Leave out frames whose FWHM exceeds this multiple of the best.
    pub max_fwhm_ratio: Option<f32>,
    /// Stops the run between frames and planes; never set by default.
    pub cancel: CancelToken,
    /// Extra weight per input frame, multiplied into the frame weights (the
    /// grader's scores, for instance); empty for none, else one per input frame.
    pub frame_scores: Vec<f32>,
    /// Keep the per-pixel rejection and weight maps in [`Stacked::maps`]. The
    /// CPU combiner records them; an engine that does not leaves them empty.
    pub maps: bool,
    /// Register every pane onto a larger canvas and blend the seams, for a
    /// mosaic ([`crate::mosaic`]); `None` keeps the reference frame's grid.
    /// Only [`crate::stack_stream::stack_streaming`] honours it.
    pub mosaic: Option<crate::mosaic::MosaicOptions>,
}

impl Default for StackOptions {
    fn default() -> Self {
        Self {
            detect: DetectOptions::default(),
            register: RegisterOptions::default(),
            interpolation: Interpolation::default(),
            rejection: None,
            weights: Weights::default(),
            normalisation: Normalisation::default(),
            reference: Reference::default(),
            min_stars: 20,
            max_fwhm_ratio: None,
            cancel: CancelToken::default(),
            frame_scores: Vec::new(),
            maps: false,
            mosaic: None,
        }
    }
}

/// What happened to one frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// The reference frame.
    Reference,
    /// Registered and stacked.
    Stacked,
    /// Left out, with the reason.
    Skipped(String),
}

/// A per-frame account of the stack, for the log and the report.
#[derive(Clone, Debug)]
pub struct FrameReport {
    /// Position in the input list.
    pub index: usize,
    /// Stars detected.
    pub stars: usize,
    /// Median FWHM in pixels.
    pub fwhm: f32,
    /// Median ellipticity.
    pub ellipticity: f32,
    /// Sky level in ADU.
    pub sky: f32,
    /// Pixel noise in ADU.
    pub noise: f32,
    /// Outcome.
    pub status: Status,
    /// Stars matched to the reference.
    pub matched: usize,
    /// Registration residual in pixels.
    pub registration_rms: f64,
    /// Shift of the frame origin (x, y) in reference pixels.
    pub shift: (f64, f64),
    /// Rotation relative to the reference, degrees.
    pub rotation_deg: f64,
    /// Scale relative to the reference.
    pub scale: f64,
    /// Transparency factor applied by normalisation.
    pub flux_scale: f64,
    /// Weight in the mean (mean over used frames is 1).
    pub weight: f64,
    /// Fraction of this frame's samples rejected as outliers.
    pub rejected: f64,
}

/// Where the combine rejected samples and how much weight each pixel got.
#[derive(Clone, Debug)]
pub struct StackMaps {
    /// Samples rejected below the centre at each pixel, summed over the colour planes.
    pub low: Frame,
    /// Samples rejected above the centre at each pixel, summed over the colour planes.
    pub high: Frame,
    /// Summed weight of the samples kept at each pixel (first plane); the mean
    /// weight of a frame is 1, so this is about the number of frames used.
    pub weight: Frame,
}

/// The stacked result.
#[derive(Clone, Debug)]
pub struct Stacked {
    /// The stack: reference header plus stacking keywords, one plane per colour.
    pub image: Image,
    /// How many frames contributed at each pixel.
    pub coverage: Frame,
    /// One entry per input frame.
    pub frames: Vec<FrameReport>,
    /// Index of the reference frame.
    pub reference: usize,
    /// The rejection that was used.
    pub rejection: Rejection,
    /// The rejection and weight maps, when [`StackOptions::maps`] asked for them.
    pub maps: Option<StackMaps>,
}

impl Stacked {
    /// Number of frames in the stack, the reference included.
    #[must_use]
    pub fn used(&self) -> usize {
        self.frames
            .iter()
            .filter(|f| matches!(f.status, Status::Stacked | Status::Reference))
            .count()
    }
}

fn check_inputs(frames: &[Image]) -> Result<()> {
    let first = frames
        .first()
        .ok_or_else(|| Error::invalid("no frames to stack"))?;
    for (i, f) in frames.iter().enumerate() {
        if f.planes.iter().any(|p| p.bayer.is_some()) {
            return Err(Error::invalid(format!(
                "frame {i} is a raw Bayer mosaic; demosaic it before stacking"
            )));
        }
        if f.planes.len() != first.planes.len() {
            return Err(Error::invalid(format!(
                "frame {i} has {} planes, the first has {}",
                f.planes.len(),
                first.planes.len()
            )));
        }
    }
    Ok(())
}

/// Median ratio of reference flux to source flux over matched stars that are
/// not saturated, taking the brightest (signal-to-noise above 20, then 10,
/// then 5) that give at least eight pairs. `None` when even the faintest cut
/// finds too few.
pub(crate) fn flux_ratio(
    reg: &Registration,
    reference: &Detection,
    source: &Detection,
) -> Option<f64> {
    for cut in [20.0, 10.0, 5.0] {
        let mut r: Vec<f32> = reg
            .pairs
            .iter()
            .filter_map(|&(i, j)| {
                let (s, t) = (&source.stars[i], &reference.stars[j]);
                (!s.saturated && !t.saturated && s.snr > cut && t.snr > cut && s.flux > 0.0)
                    .then(|| (t.flux / s.flux) as f32)
            })
            .collect();
        if r.len() >= 8 {
            return Some(f64::from(stats::median_in_place(&mut r)).clamp(0.2, 5.0));
        }
    }
    None
}

/// Frames ranked by sharpness, sharpest first, among those with a healthy
/// number of stars; at most four are offered.
fn ranked_references(dets: &[Detection], min_stars: usize) -> Vec<usize> {
    let mut counts: Vec<f32> = dets.iter().map(|d| d.stars.len() as f32).collect();
    let median = stats::median_in_place(&mut counts);
    let mut v: Vec<usize> = (0..dets.len())
        .filter(|&i| dets[i].stars.len() >= min_stars && dets[i].stars.len() as f32 >= 0.5 * median)
        .filter(|&i| dets[i].median_fwhm().is_finite())
        .collect();
    v.sort_by(|&a, &b| dets[a].median_fwhm().total_cmp(&dets[b].median_fwhm()));
    v.truncate(4);
    v
}

fn best_fwhm_of(dets: &[Detection], reference: usize) -> f32 {
    dets[reference].median_fwhm()
}

/// Register every other frame to `reference`; `None` for the reference itself.
fn register_all(
    dets: &[Detection],
    reference: usize,
    opts: &StackOptions,
    best_fwhm: f32,
) -> Vec<Attempt> {
    (0..dets.len())
        .map(|i| {
            if i == reference {
                return None;
            }
            if opts.cancel.is_cancelled() {
                return Some(Err(Error::Cancelled.to_string()));
            }
            if dets[i].stars.len() < opts.min_stars {
                return Some(Err(format!("only {} stars", dets[i].stars.len())));
            }
            if let Some(r) = opts.max_fwhm_ratio
                && dets[i].median_fwhm() > r * best_fwhm
            {
                return Some(Err(format!(
                    "FWHM {:.2} px is too soft",
                    dets[i].median_fwhm()
                )));
            }
            Some(
                register(&dets[reference].stars, &dets[i].stars, &opts.register)
                    .map_err(|e| e.to_string()),
            )
        })
        .collect()
}

pub(crate) fn plane_sky(p: &Frame) -> f32 {
    stats::sky_and_noise(p).0
}

/// A registration outcome for one frame; `None` for the reference itself.
type Attempt = Option<std::result::Result<Registration, String>>;

fn detect_all(
    frames: &[Image],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Vec<Detection>> {
    frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            opts.cancel.check()?;
            let det = detect(&luminance(&f.planes), &opts.detect);
            log(&format!(
                "frame {i}: {} stars, FWHM {:.2} px, ellipticity {:.3}, sky {:.1}, noise {:.2}",
                det.stars.len(),
                det.median_fwhm(),
                det.median_ellipticity(),
                det.sky,
                det.noise
            ));
            Ok(det)
        })
        .collect()
}

/// Take the first candidate that most of the other frames register to; a
/// frame of the wrong field can be the sharpest and must not win.
pub(crate) fn pick_reference(
    dets: &[Detection],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<(usize, Vec<Attempt>)> {
    let n = dets.len();
    let candidates: Vec<usize> = match opts.reference {
        Reference::Index(i) if i < n => vec![i],
        Reference::Index(i) => {
            return Err(Error::invalid(format!(
                "reference frame {i} out of range ({n} frames)"
            )));
        }
        Reference::Best => ranked_references(dets, opts.min_stars),
    };
    if candidates.is_empty() {
        return Err(Error::NoMatch(
            "no frame has enough stars to be the reference".into(),
        ));
    }
    let mut best: Option<(usize, usize, Vec<Attempt>)> = None;
    for &c in &candidates {
        let regs = register_all(dets, c, opts, best_fwhm_of(dets, c));
        opts.cancel.check()?;
        let ok = regs.iter().flatten().filter(|r| r.is_ok()).count();
        log(&format!(
            "reference candidate frame {c}: {ok} of {} other frames register",
            n - 1
        ));
        // A candidate that most frames register to is taken at once; otherwise
        // the one that most do (the sharpest breaking ties).
        if matches!(opts.reference, Reference::Index(_)) || 2 * ok >= n - 1 {
            return Ok((c, regs));
        }
        if best.as_ref().is_none_or(|b| ok > b.1) {
            best = Some((c, ok, regs));
        }
    }
    match best {
        Some((c, ok, regs)) if ok > 0 => {
            log(&format!(
                "no frame registers with most of the others (several fields?); using frame {c}, which {ok} register to"
            ));
            Ok((c, regs))
        }
        _ => Err(Error::NoMatch(
            "no frame could serve as the reference: none of the others register to any candidate"
                .into(),
        )),
    }
}

pub(crate) fn initial_reports(dets: &[Detection], reference: usize) -> Vec<FrameReport> {
    dets.iter()
        .enumerate()
        .map(|(i, d)| FrameReport {
            index: i,
            stars: d.stars.len(),
            fwhm: d.median_fwhm(),
            ellipticity: d.median_ellipticity(),
            sky: d.sky,
            noise: d.noise,
            status: if i == reference {
                Status::Reference
            } else {
                Status::Skipped(String::new())
            },
            matched: if i == reference { d.stars.len() } else { 0 },
            registration_rms: 0.0,
            shift: (0.0, 0.0),
            rotation_deg: 0.0,
            scale: 1.0,
            flux_scale: 1.0,
            weight: 0.0,
            rejected: 0.0,
        })
        .collect()
}

/// Weights for the frames in `keep`, scaled to a mean of one.
#[must_use]
pub fn frame_weights(keep: &[usize], reports: &[FrameReport], kind: Weights) -> Vec<f32> {
    let raw: Vec<f64> = keep
        .iter()
        .map(|&i| {
            let r = &reports[i];
            let sigma = f64::from(r.noise) * r.flux_scale;
            let fwhm = f64::from(r.fwhm).max(0.5);
            match kind {
                Weights::Equal => 1.0,
                Weights::Noise => 1.0 / (sigma * sigma).max(1e-12),
                Weights::Fwhm => 1.0 / (fwhm * fwhm),
                Weights::NoiseFwhm => 1.0 / (sigma * sigma * fwhm * fwhm).max(1e-12),
            }
        })
        .collect();
    let mean = raw.iter().sum::<f64>() / raw.len() as f64;
    raw.iter().map(|w| (w / mean) as f32).collect()
}

/// [`frame_weights`] for `opts.weights`, times `opts.frame_scores` when given,
/// scaled again to a mean of one.
#[must_use]
pub fn frame_weights_for(keep: &[usize], reports: &[FrameReport], opts: &StackOptions) -> Vec<f32> {
    let mut w = frame_weights(keep, reports, opts.weights);
    if opts.frame_scores.len() == reports.len() {
        for (x, &i) in w.iter_mut().zip(keep) {
            *x *= opts.frame_scores[i].max(0.0);
        }
        let mean = w.iter().sum::<f32>() / w.len() as f32;
        if mean > 0.0 {
            for x in &mut w {
                *x /= mean;
            }
        }
    }
    w
}

/// The reference header with the stacking keywords.
pub(crate) fn stack_header(
    frames: &[Image],
    keep: &[usize],
    reference: usize,
    rejection: Rejection,
    opts: &StackOptions,
) -> crate::fits::Header {
    let total: f64 = keep
        .iter()
        .filter_map(|&i| frames[i].header.exposure())
        .sum();
    stack_header_of(
        &frames[reference].header,
        total,
        frames.len(),
        keep.len(),
        reference,
        rejection,
        opts,
    )
}

/// [`stack_header`] from the reference header, the summed exposure and the counts.
pub(crate) fn stack_header_of(
    reference_header: &crate::fits::Header,
    total: f64,
    n_input: usize,
    n_kept: usize,
    reference: usize,
    rejection: Rejection,
    opts: &StackOptions,
) -> crate::fits::Header {
    let mut header = reference_header.clone();
    for k in [
        "CBLACK", "CWHITE", "PEDESTAL", "HISTORY", "IMAGETYP", "BAYERPAT",
    ] {
        header.remove(k);
    }
    header.set("IMAGETYP", Value::Str("Master Light".into()), "");
    header.set(
        "NCOMBINE",
        Value::Int(n_kept as i64),
        "frames in this stack",
    );
    if total > 0.0 {
        header.set("EXPTOTAL", Value::Float(total), "[s] total integration");
    }
    header.set(
        "STACKREF",
        Value::Int(reference as i64),
        "index of the registration reference",
    );
    header.set(
        "STACKALG",
        Value::Str(format!("{rejection:?} / {:?} weights", opts.weights)),
        "rejection and weighting",
    );
    header.add_history(&format!(
        "stacked by Valerian: {} of {} frames, reference {reference}, {:?} normalisation",
        n_kept, n_input, opts.normalisation
    ));
    header
}

/// The planes, the coverage, the rejected fractions and the maps of a combine.
type Combo = (Vec<Frame>, Frame, Vec<f64>, Option<StackMaps>);

/// Add the maps of one plane's combine into the running `maps`.
fn add_maps(maps: &mut Option<StackMaps>, c: &crate::combine::Combined, plane: usize) {
    let Some(m) = &c.maps else { return };
    let (w, h) = (c.mean.width, c.mean.height);
    let to_frame = |v: &[u16]| Frame {
        width: w,
        height: h,
        data: v.iter().map(|&x| f32::from(x)).collect(),
        bayer: None,
    };
    match maps {
        None => {
            *maps = Some(StackMaps {
                low: to_frame(&m.low),
                high: to_frame(&m.high),
                weight: Frame {
                    width: w,
                    height: h,
                    data: m.weight.clone(),
                    bayer: None,
                },
            });
        }
        Some(sum) => {
            for (a, b) in sum.low.data.iter_mut().zip(&m.low) {
                *a += f32::from(*b);
            }
            for (a, b) in sum.high.data.iter_mut().zip(&m.high) {
                *a += f32::from(*b);
            }
            let _ = plane;
        }
    }
}

/// Combine every plane of the frames in `keep`: the planes, a coverage plane,
/// the fraction of each kept frame rejected (averaged over the planes) and,
/// when `maps` is set, the per-pixel rejection and weight maps.
fn combine_all(
    frames: &[Image],
    keep: &[usize],
    weights: &[f32],
    rejection: Rejection,
    compute: &dyn Compute,
    cancel: &CancelToken,
    maps: bool,
) -> Result<Combo> {
    let nplanes = frames[keep[0]].planes.len();
    let mut planes = Vec::with_capacity(nplanes);
    let mut coverage = None;
    let mut stack_maps: Option<StackMaps> = None;
    let mut rejected = vec![0.0_f64; keep.len()];
    for p in 0..nplanes {
        cancel.check()?;
        let refs: Vec<&Frame> = keep.iter().map(|&i| &frames[i].planes[p]).collect();
        let c = compute.combine(
            &refs,
            &Options {
                rejection,
                weights: Some(weights.to_vec()),
                maps,
                ..Options::default()
            },
        )?;
        add_maps(&mut stack_maps, &c, p);
        for (r, f) in rejected.iter_mut().zip(c.rejection_fraction()) {
            *r += f / nplanes as f64;
        }
        if p == 0 {
            coverage = Some(Frame {
                width: c.mean.width,
                height: c.mean.height,
                data: c.used.iter().map(|&u| f32::from(u)).collect(),
                bayer: None,
            });
        }
        planes.push(c.mean);
    }
    let coverage = coverage.ok_or_else(|| Error::invalid("frames have no planes"))?;
    Ok((planes, coverage, rejected, stack_maps))
}

/// Frames registered to a common reference grid and normalised, ready to
/// combine (or to save as they are).
#[derive(Debug)]
pub struct Aligned {
    /// All input frames by index. The reference is untouched, the registered
    /// ones have been resampled onto its grid (NaN outside their footprint) and
    /// normalised, and skipped ones are unchanged and should be ignored.
    pub frames: Vec<Image>,
    /// Indices of the frames that are in, sorted; includes the reference.
    pub keep: Vec<usize>,
    /// One report per input frame.
    pub reports: Vec<FrameReport>,
    /// Index of the reference frame.
    pub reference: usize,
}

/// Frames registered to a common reference, not yet resampled.
#[derive(Debug)]
pub struct Registered {
    /// The registration of each frame to the reference; `None` for the
    /// reference itself and for frames that were left out.
    pub regs: Vec<Option<Registration>>,
    /// Indices of the frames that are in, sorted; includes the reference.
    pub keep: Vec<usize>,
    /// One report per input frame (shift, rotation, flux scale and so on).
    pub reports: Vec<FrameReport>,
    /// Index of the reference frame.
    pub reference: usize,
}

/// Detect, choose the reference and register every other frame to it, without
/// resampling anything. [`align`] and drizzle ([`crate::drizzle`]) both start
/// here.
///
/// # Errors
/// [`Error::Invalid`] on empty or inconsistent input, and
/// [`Error::NoMatch`] when the reference cannot be chosen or fewer than two
/// frames register.
pub fn register_frames(
    frames: &[Image],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Registered> {
    check_inputs(frames)?;
    let dets = detect_all(frames, opts, log)?;
    register_detections(&dets, opts, log)
}

/// [`register_frames`] from detections already made, so a caller that cannot
/// hold every frame at once ([`crate::stack_stream`]) detects them one by one.
///
/// # Errors
/// As for [`register_frames`].
pub(crate) fn register_detections(
    dets: &[Detection],
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Registered> {
    let n = dets.len();
    let (reference, attempts) = pick_reference(dets, opts, log)?;
    log(&format!(
        "reference: frame {reference} (FWHM {:.2} px)",
        dets[reference].median_fwhm()
    ));
    let mut reports = initial_reports(dets, reference);
    let mut regs: Vec<Option<Registration>> = vec![None; n];
    let mut keep: Vec<usize> = vec![reference];
    for (i, attempt) in attempts
        .into_iter()
        .enumerate()
        .filter(|&(i, _)| i != reference)
    {
        opts.cancel.check()?;
        let reg = match attempt {
            Some(Ok(r)) => r,
            Some(Err(why)) => {
                log(&format!("frame {i}: skipped, {why}"));
                reports[i].status = Status::Skipped(why);
                continue;
            }
            None => continue,
        };
        let (scale, angle, (tx, ty)) = reg.transform.decompose();
        let flux_scale = match opts.normalisation {
            Normalisation::AdditiveScaling => flux_ratio(&reg, &dets[reference], &dets[i])
                .unwrap_or_else(|| {
                    log(&format!(
                        "frame {i}: too few matched stars to measure its transparency; flux scale 1"
                    ));
                    1.0
                }),
            _ => 1.0,
        };
        let r = &mut reports[i];
        r.status = Status::Stacked;
        r.matched = reg.matched;
        r.registration_rms = reg.rms;
        r.shift = (tx, ty);
        r.rotation_deg = angle.to_degrees();
        r.scale = scale;
        r.flux_scale = flux_scale;
        log(&format!(
            "frame {i}: registered, {} stars matched, rms {:.3} px, shift ({tx:.2}, {ty:.2}), rotation {:.4} deg, flux x{flux_scale:.4}",
            reg.matched,
            reg.rms,
            angle.to_degrees()
        ));
        regs[i] = Some(reg);
        keep.push(i);
    }
    if keep.len() < 2 {
        return Err(Error::NoMatch(
            "fewer than two frames registered; nothing to stack".into(),
        ));
    }
    keep.sort_unstable();
    Ok(Registered {
        regs,
        keep,
        reports,
        reference,
    })
}

/// Register `frames` to a reference and resample them onto its grid.
///
/// `log` receives one line per step and per frame.
///
/// # Errors
/// [`Error::Invalid`] on empty or inconsistent input, and
/// [`Error::NoMatch`] when the reference cannot be chosen or fewer than two
/// frames register.
pub fn align(
    frames: Vec<Image>,
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Aligned> {
    align_with(frames, opts, &Cpu, log)
}

/// [`align`] with the resampling done by `compute`.
///
/// # Errors
/// As for [`align`].
pub fn align_with(
    mut frames: Vec<Image>,
    opts: &StackOptions,
    compute: &dyn Compute,
    log: &mut dyn FnMut(&str),
) -> Result<Aligned> {
    let Registered {
        regs,
        keep,
        reports,
        reference,
    } = register_frames(&frames, opts, log)?;
    let (rw, rh) = (frames[reference].width(), frames[reference].height());
    // Resample and normalise, replacing each frame by its warped self.
    let ref_sky_planes: Vec<f32> = frames[reference].planes.iter().map(plane_sky).collect();
    for &i in keep.iter().filter(|&&i| i != reference) {
        opts.cancel.check()?;
        let Some(reg) = regs[i].as_ref() else {
            continue;
        };
        let flux_scale = reports[i].flux_scale;
        for (p, plane) in frames[i].planes.iter_mut().enumerate() {
            let sky = plane_sky(plane);
            let mut w = compute.warp(plane, &reg.transform, rw, rh, opts.interpolation)?;
            if opts.normalisation != Normalisation::None {
                let k = flux_scale as f32;
                for v in &mut w.data {
                    *v = (*v - sky) * k + ref_sky_planes[p];
                }
            }
            *plane = w;
        }
    }
    Ok(Aligned {
        frames,
        keep,
        reports,
        reference,
    })
}

/// Weight and combine aligned frames.
///
/// # Errors
/// [`Error::Invalid`] if the frames cannot be combined.
pub fn integrate(
    aligned: Aligned,
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Stacked> {
    integrate_with(aligned, opts, &Cpu, log)
}

/// [`integrate`] with the combining done by `compute`.
///
/// # Errors
/// As for [`integrate`].
pub fn integrate_with(
    aligned: Aligned,
    opts: &StackOptions,
    compute: &dyn Compute,
    log: &mut dyn FnMut(&str),
) -> Result<Stacked> {
    let Aligned {
        frames,
        keep,
        mut reports,
        reference,
    } = aligned;
    let weights = frame_weights_for(&keep, &reports, opts);
    for (&i, &w) in keep.iter().zip(&weights) {
        reports[i].weight = f64::from(w);
    }
    let rejection = opts
        .rejection
        .unwrap_or_else(|| Rejection::auto(keep.len()));
    log(&format!(
        "combining {} frames with {rejection:?}, weights {:?}",
        keep.len(),
        opts.weights
    ));
    opts.cancel.check()?;
    let (planes, coverage, rejected, maps) = combine_all(
        &frames,
        &keep,
        &weights,
        rejection,
        compute,
        &opts.cancel,
        opts.maps,
    )?;
    for (&i, &r) in keep.iter().zip(&rejected) {
        reports[i].rejected = r;
    }
    let header = stack_header(&frames, &keep, reference, rejection, opts);
    Ok(Stacked {
        image: Image { header, planes },
        coverage,
        frames: reports,
        reference,
        rejection,
        maps,
    })
}

/// Register and stack `frames`: [`align`] then [`integrate`].
///
/// # Errors
/// As for [`align`] and [`integrate`].
pub fn stack(
    frames: Vec<Image>,
    opts: &StackOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Stacked> {
    integrate(align(frames, opts, log)?, opts, log)
}

/// [`stack`] with the work done by `compute`, one colour plane at a time through
/// [`Compute::stack_plane`], which an engine with a device can keep resident
/// between the warp and the combine. The result is that of [`stack`].
///
/// # Errors
/// As for [`stack`].
pub fn stack_with(
    frames: &[Image],
    opts: &StackOptions,
    compute: &dyn Compute,
    log: &mut dyn FnMut(&str),
) -> Result<Stacked> {
    let Registered {
        regs,
        keep,
        mut reports,
        reference,
    } = register_frames(frames, opts, log)?;
    let weights = frame_weights_for(&keep, &reports, opts);
    for (&i, &w) in keep.iter().zip(&weights) {
        reports[i].weight = f64::from(w);
    }
    let rejection = opts
        .rejection
        .unwrap_or_else(|| Rejection::auto(keep.len()));
    log(&format!(
        "combining {} frames with {rejection:?}, weights {:?} on {}",
        keep.len(),
        opts.weights,
        compute.name()
    ));
    let size = (frames[reference].width(), frames[reference].height());
    let normalise = opts.normalisation != Normalisation::None;
    let nplanes = frames[reference].planes.len();
    let mut planes = Vec::with_capacity(nplanes);
    let mut coverage = None;
    let mut stack_maps: Option<StackMaps> = None;
    let mut rejected = vec![0.0_f64; keep.len()];
    for p in 0..nplanes {
        opts.cancel.check()?;
        let plane = StackPlane {
            frames: keep.iter().map(|&i| &frames[i].planes[p]).collect(),
            transforms: keep
                .iter()
                .map(|&i| {
                    if i == reference {
                        None
                    } else {
                        regs[i].as_ref().map(|r| r.transform)
                    }
                })
                .collect(),
            levels: keep
                .iter()
                .map(|&i| {
                    (normalise && i != reference).then(|| {
                        (
                            plane_sky(&frames[i].planes[p]),
                            reports[i].flux_scale as f32,
                        )
                    })
                })
                .collect(),
            ref_sky: plane_sky(&frames[reference].planes[p]),
            size,
            interp: opts.interpolation,
            combine: Options {
                rejection,
                weights: Some(weights.clone()),
                maps: opts.maps,
                ..Options::default()
            },
        };
        let c = compute.stack_plane(&plane)?;
        add_maps(&mut stack_maps, &c, p);
        for (r, f) in rejected.iter_mut().zip(c.rejection_fraction()) {
            *r += f / nplanes as f64;
        }
        if p == 0 {
            coverage = Some(Frame {
                width: c.mean.width,
                height: c.mean.height,
                data: c.used.iter().map(|&u| f32::from(u)).collect(),
                bayer: None,
            });
        }
        planes.push(c.mean);
    }
    let coverage = coverage.ok_or_else(|| Error::invalid("frames have no planes"))?;
    for (&i, &r) in keep.iter().zip(&rejected) {
        reports[i].rejected = r;
    }
    let header = stack_header(frames, &keep, reference, rejection, opts);
    Ok(Stacked {
        image: Image { header, planes },
        coverage,
        frames: reports,
        reference,
        rejection,
        maps: stack_maps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scene;
    use crate::transform::Transform;

    fn silent(_: &str) {}

    /// A set of `n` renderings of one scene with small random pointing changes.
    fn session(scene: &Scene, n: usize, noise: f64, seed: u64) -> (Vec<Image>, Vec<Transform>) {
        let mut rng = crate::testutil::Rng::new(seed);
        let mut frames = Vec::new();
        let mut views = Vec::new();
        for k in 0..n {
            let view = Transform::rotation_about(
                scene.width as f64 / 2.0,
                scene.height as f64 / 2.0,
                (rng.uniform() - 0.5) * 0.01,
                (rng.uniform() - 0.5) * 30.0,
                (rng.uniform() - 0.5) * 30.0,
            );
            let f = scene.render(&view, 500.0, noise, 1.0, 0.0, seed * 100 + k as u64);
            let mut img = Image::mono(f, crate::fits::Header::new());
            img.header.set("EXPTIME", Value::Float(300.0), "");
            frames.push(img);
            views.push(view);
        }
        (frames, views)
    }

    fn noise_of(f: &Frame, x0: usize, y0: usize, w: usize, h: usize) -> f32 {
        let mut v = Vec::new();
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                v.push(f.data[y * f.width + x]);
            }
        }
        stats::median_sigma(&v).1
    }

    /// Like `session`, but the pointing changes are whole pixels, so resampling
    /// adds no smoothing and the noise should fall exactly as sqrt(N).
    fn integer_session(scene: &Scene, n: usize, noise: f64, seed: u64) -> Vec<Image> {
        let mut rng = crate::testutil::Rng::new(seed);
        (0..n)
            .map(|k| {
                let view = Transform::similarity(
                    1.0,
                    0.0,
                    (rng.uniform() * 24.0 - 12.0).round(),
                    (rng.uniform() * 24.0 - 12.0).round(),
                );
                let f = scene.render(&view, 500.0, noise, 1.0, 0.0, seed * 100 + k as u64);
                let mut img = Image::mono(f, crate::fits::Header::new());
                img.header.set("EXPTIME", Value::Float(300.0), "");
                img
            })
            .collect()
    }

    #[test]
    fn stacking_through_the_compute_trait_gives_the_same_stack() {
        let scene = Scene::random(100, 400, 300, 5);
        let (frames, _) = session(&scene, 7, 8.0, 6);
        let opts = StackOptions::default();
        let a = stack(frames.clone(), &opts, &mut silent).expect("stack");
        let b = stack_with(&frames, &opts, &Cpu, &mut silent).expect("stack_with");
        assert_eq!(a.used(), b.used());
        assert_eq!(a.image.planes[0].data.len(), b.image.planes[0].data.len());
        for (x, y) in a.image.planes[0].data.iter().zip(&b.image.planes[0].data) {
            assert!(
                x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
                "{x} {y}"
            );
        }
        assert_eq!(a.coverage.data, b.coverage.data);
        for (r, q) in a.frames.iter().zip(&b.frames) {
            assert!((r.rejected - q.rejected).abs() < 1e-12 && (r.weight - q.weight).abs() < 1e-12);
        }
    }

    #[test]
    fn noise_falls_as_the_square_root_of_the_frame_count() {
        let scene = Scene::random(150, 900, 700, 5);
        for n in [4usize, 16] {
            let frames = integer_session(&scene, n, 12.0, 6);
            let single_noise = noise_of(&frames[0].planes[0], 60, 60, 300, 200);
            let r = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
            assert_eq!(r.used(), n);
            let stacked = noise_of(&r.image.planes[0], 60, 60, 300, 200);
            let gain = single_noise / stacked;
            let expect = (n as f32).sqrt();
            assert!(
                (gain / expect - 1.0).abs() < 0.10,
                "n={n}: noise improved by {gain:.2}, expected {expect:.2}"
            );
            assert_eq!(r.image.header.get("NCOMBINE"), Some(&Value::Int(n as i64)));
            assert_eq!(r.image.header.get_f64("EXPTOTAL"), Some(300.0 * n as f64));
        }
    }

    #[test]
    fn rotated_and_subpixel_frames_do_at_least_as_well() {
        // Resampling at fractional positions smooths each frame's noise, so
        // the per-pixel noise of the stack falls faster than sqrt(N); it must
        // never fall slower.
        let scene = Scene::random(150, 900, 700, 5);
        let (frames, _) = session(&scene, 12, 12.0, 6);
        let single_noise = noise_of(&frames[0].planes[0], 60, 60, 300, 200);
        let r = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
        assert_eq!(r.used(), 12);
        assert!(
            r.frames
                .iter()
                .all(|f| !matches!(f.status, Status::Skipped(_)))
        );
        let gain = single_noise / noise_of(&r.image.planes[0], 60, 60, 300, 200);
        let root = 12.0_f32.sqrt();
        assert!(
            gain > 0.95 * root && gain < 1.45 * root,
            "noise improved by {gain:.2}, root N is {root:.2}"
        );
        let det = detect(&r.image.planes[0], &DetectOptions::default());
        assert!(det.stars.len() > 100, "{} stars", det.stars.len());
    }

    #[test]
    fn registration_error_in_the_stack_is_under_five_hundredths_of_a_pixel() {
        let scene = Scene::random(160, 1000, 800, 15);
        let (frames, views) = session(&scene, 10, 8.0, 16);
        let r = stack(
            frames,
            &StackOptions {
                reference: Reference::Index(0),
                ..StackOptions::default()
            },
            &mut silent,
        )
        .expect("stack");
        // The stack lives on frame 0's grid: a star at truth position p is at
        // views[0](p) there.
        let det = detect(&r.image.planes[0], &DetectOptions::default());
        let (mut sq, mut n) = (0.0, 0.0);
        for &(x, y, flux, sigma) in &scene.stars {
            let peak = flux / (2.0 * std::f64::consts::PI * sigma * sigma);
            if peak < 250.0 {
                continue;
            }
            let (ex, ey) = views[0].apply((x, y));
            if ex < 40.0 || ey < 40.0 || ex > 960.0 || ey > 760.0 {
                continue;
            }
            // Skip blends.
            if scene
                .stars
                .iter()
                .any(|o| (o.0 - x, o.1 - y) != (0.0, 0.0) && (o.0 - x).hypot(o.1 - y) < 14.0)
            {
                continue;
            }
            if let Some(s) = det.stars.iter().find(|s| (s.x - ex).hypot(s.y - ey) < 1.0) {
                sq += (s.x - ex).powi(2) + (s.y - ey).powi(2);
                n += 1.0;
            }
        }
        assert!(n >= 25.0, "only {n} stars compared");
        let rms = (sq / n).sqrt();
        assert!(
            rms < 0.05,
            "stars in the stack are {rms:.4} px from where they belong"
        );
    }

    #[test]
    fn satellite_trails_and_hot_pixels_are_rejected() {
        let scene = Scene::random(120, 700, 500, 25);
        let (mut frames, _) = session(&scene, 14, 6.0, 26);
        // Detector-fixed hot pixels in every frame, and trails in three frames.
        for f in &mut frames {
            for &(x, y) in &[(100usize, 100usize), (250, 300), (400, 150), (600, 420)] {
                f.planes[0].data[y * 700 + x] += 6000.0;
            }
        }
        for (k, row) in [(2usize, 150usize), (5, 250), (9, 350)] {
            for x in 0..700 {
                for dy in 0..3 {
                    frames[k].planes[0].data[(row + dy) * 700 + x] += 4000.0;
                }
            }
        }
        let clean = {
            let (clean_frames, _) = session(&scene, 14, 6.0, 26);
            stack(clean_frames, &StackOptions::default(), &mut silent).expect("stack")
        };
        let dirty = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
        let (c, d) = (&clean.image.planes[0], &dirty.image.planes[0]);
        let mut worst_trail = 0.0_f32;
        for row in [150usize, 151, 152, 250, 251, 252, 350, 351, 352] {
            for x in 60..640 {
                worst_trail =
                    worst_trail.max((d.data[row * 700 + x] - c.data[row * 700 + x]).abs());
            }
        }
        let mut worst_hot = 0.0_f32;
        for &(x, y) in &[(100usize, 100usize), (250, 300), (400, 150), (600, 420)] {
            for dy in 0..3 {
                for dx in 0..3 {
                    let i = (y + dy - 1) * 700 + x + dx - 1;
                    worst_hot = worst_hot.max((d.data[i] - c.data[i]).abs());
                }
            }
        }
        // The residual is a small fraction of the 4000 ADU trail and 6000 ADU hot pixels.
        assert!(worst_trail < 40.0, "trail residual {worst_trail} ADU");
        assert!(worst_hot < 80.0, "hot pixel residual {worst_hot} ADU");
        let rejected_trail_frame = dirty.frames[2].rejected;
        let rejected_clean = clean.frames[2].rejected;
        assert!(
            rejected_trail_frame > rejected_clean,
            "{rejected_trail_frame} vs {rejected_clean}"
        );
    }

    #[test]
    fn noise_weighting_beats_equal_weights_on_unequal_frames() {
        let scene = Scene::random(120, 700, 500, 35);
        let mut frames = Vec::new();
        let mut rng = crate::testutil::Rng::new(36);
        for k in 0..10 {
            let noise = if k < 5 { 5.0 } else { 20.0 };
            let view = Transform::similarity(
                1.0,
                0.0,
                (rng.uniform() - 0.5) * 20.0,
                (rng.uniform() - 0.5) * 20.0,
            );
            let f = scene.render(&view, 500.0, noise, 1.0, 0.0, 400 + k);
            frames.push(Image::mono(f, crate::fits::Header::new()));
        }
        let run = |w: Weights| {
            let r = stack(
                frames.clone(),
                &StackOptions {
                    weights: w,
                    reference: Reference::Index(0),
                    ..StackOptions::default()
                },
                &mut silent,
            )
            .expect("stack");
            noise_of(&r.image.planes[0], 100, 100, 400, 250)
        };
        let equal = run(Weights::Equal);
        let weighted = run(Weights::Noise);
        // Equal weights: sqrt(5 * 25 + 5 * 400) / 10 = 4.6. Noise weights:
        // 1 / sqrt(5 / 25 + 5 / 400) = 2.2, before rejection takes a little.
        assert!(
            weighted < 0.6 * equal,
            "weighted {weighted:.2} vs equal {equal:.2}"
        );
        assert!(weighted > 1.9 && weighted < 2.8, "{weighted}");
        assert!(equal > 4.0 && equal < 5.4, "{equal}");
    }

    #[test]
    fn frame_scores_multiply_into_the_weights() {
        let scene = Scene::random(120, 700, 500, 44);
        let mut frames = Vec::new();
        let mut rng = crate::testutil::Rng::new(47);
        for k in 0..10 {
            let noise = if k < 5 { 5.0 } else { 20.0 };
            let view = Transform::similarity(
                1.0,
                0.0,
                (rng.uniform() - 0.5) * 20.0,
                (rng.uniform() - 0.5) * 20.0,
            );
            let f = scene.render(&view, 500.0, noise, 1.0, 0.0, 500 + k);
            frames.push(Image::mono(f, crate::fits::Header::new()));
        }
        let run = |scores: Vec<f32>| {
            let r = stack(
                frames.clone(),
                &StackOptions {
                    weights: Weights::Equal,
                    reference: Reference::Index(0),
                    frame_scores: scores,
                    ..StackOptions::default()
                },
                &mut silent,
            )
            .expect("stack");
            (
                noise_of(&r.image.planes[0], 100, 100, 400, 250),
                r.frames.iter().map(|f| f.weight).collect::<Vec<_>>(),
            )
        };
        let (flat, _) = run(Vec::new());
        // The noisy half scores 0.1: the stack gets quieter, and the weights stay at a mean of one.
        let scores = (0..10).map(|k| if k < 5 { 1.0 } else { 0.1 }).collect();
        let (quiet, weights) = run(scores);
        assert!(quiet < 0.75 * flat, "scored {quiet:.2} vs flat {flat:.2}");
        let mean = weights.iter().sum::<f64>() / weights.len() as f64;
        assert!((mean - 1.0).abs() < 1e-5, "{mean}");
    }

    #[test]
    fn sharper_frames_get_more_weight_and_the_stack_stays_sharp() {
        let scene = Scene::random(120, 700, 500, 45);
        let mut frames = Vec::new();
        let mut rng = crate::testutil::Rng::new(46);
        for k in 0..10 {
            // Half the frames are blurred by an extra 1.5 px sigma.
            let blur = if k % 2 == 0 { 0.0 } else { 1.5 };
            let view = Transform::similarity(
                1.0,
                0.0,
                (rng.uniform() - 0.5) * 16.0,
                (rng.uniform() - 0.5) * 16.0,
            );
            let f = scene.render(&view, 500.0, 6.0, 1.0, blur, 500 + k);
            frames.push(Image::mono(f, crate::fits::Header::new()));
        }
        let fwhm_of = |w: Weights| -> f32 {
            let r = stack(
                frames.clone(),
                &StackOptions {
                    weights: w,
                    ..StackOptions::default()
                },
                &mut silent,
            )
            .expect("stack");
            detect(&r.image.planes[0], &DetectOptions::default()).median_fwhm()
        };
        let equal = fwhm_of(Weights::Equal);
        let sharp = fwhm_of(Weights::NoiseFwhm);
        assert!(
            sharp < equal,
            "weighted FWHM {sharp:.2} vs equal {equal:.2}"
        );
        let r = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
        let weights: Vec<f64> = r.frames.iter().map(|f| f.weight).collect();
        let (sharp_w, blur_w): (Vec<f64>, Vec<f64>) = (
            weights.iter().step_by(2).copied().collect(),
            weights.iter().skip(1).step_by(2).copied().collect(),
        );
        assert!(sharp_w.iter().sum::<f64>() > 1.5 * blur_w.iter().sum::<f64>());
        assert_eq!(r.reference % 2, 0, "the reference should be a sharp frame");
    }

    #[test]
    fn sky_and_transparency_changes_are_normalised_away() {
        let scene = Scene::random(120, 700, 500, 55);
        let mut frames = Vec::new();
        let mut rng = crate::testutil::Rng::new(56);
        let transparency = [1.0, 0.7, 0.9, 0.55, 1.0, 0.8];
        for (k, &t) in transparency.iter().enumerate() {
            let view = Transform::similarity(
                1.0,
                0.0,
                (rng.uniform() - 0.5) * 16.0,
                (rng.uniform() - 0.5) * 16.0,
            );
            // Brighter sky in the frames with thin cloud.
            let sky = 500.0 + 300.0 * (1.0 - t as f32);
            let f = scene.render(&view, sky, 6.0, t, 0.0, 600 + k as u64);
            frames.push(Image::mono(f, crate::fits::Header::new()));
        }
        let first = frames[0].planes[0].clone();
        let r = stack(
            frames,
            &StackOptions {
                reference: Reference::Index(0),
                ..StackOptions::default()
            },
            &mut silent,
        )
        .expect("stack");
        let det = detect(&r.image.planes[0], &DetectOptions::default());
        assert!((det.sky - 500.0).abs() < 4.0, "sky {}", det.sky);
        // Star fluxes in the stack match the full-transparency reference frame.
        let det0 = detect(&first, &DetectOptions::default());
        let mut ratios: Vec<f32> = Vec::new();
        for s in det.stars.iter().filter(|s| s.snr > 40.0) {
            if let Some(t) = det0
                .stars
                .iter()
                .find(|t| (t.x - s.x).hypot(t.y - s.y) < 0.5)
            {
                ratios.push((s.flux / t.flux) as f32);
            }
        }
        assert!(ratios.len() > 20, "{} stars compared", ratios.len());
        let ratio = stats::median_in_place(&mut ratios);
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "star flux in the stack is {ratio} of the reference's"
        );
        let scaled: Vec<f64> = r.frames.iter().map(|f| f.flux_scale).collect();
        for (s, &t) in scaled.iter().zip(&transparency) {
            assert!(
                (s * t - 1.0).abs() < 0.03,
                "flux scale {s} for transparency {t}"
            );
        }
    }

    #[test]
    fn an_unrelated_frame_is_dropped_and_reported() {
        let scene = Scene::random(120, 700, 500, 65);
        let (mut frames, _) = session(&scene, 6, 6.0, 66);
        let other = Scene::random(120, 700, 500, 999);
        frames.push(Image::mono(
            other.render(&Transform::identity(), 500.0, 6.0, 1.0, 0.0, 77),
            crate::fits::Header::new(),
        ));
        let r = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
        assert_eq!(r.used(), 6);
        assert!(matches!(&r.frames[6].status, Status::Skipped(why) if !why.is_empty()));
    }

    #[test]
    fn colour_planes_are_stacked_together_on_one_grid() {
        let scene = Scene::random(100, 600, 450, 75);
        let mut frames = Vec::new();
        let mut rng = crate::testutil::Rng::new(76);
        for k in 0..8 {
            let view = Transform::similarity(
                1.0,
                0.0,
                (rng.uniform() - 0.5) * 20.0,
                (rng.uniform() - 0.5) * 20.0,
            );
            let planes: Vec<Frame> = [1.0, 0.8, 0.5]
                .iter()
                .enumerate()
                .map(|(c, &g)| scene.render(&view, 400.0, 5.0, g, 0.0, 700 + 10 * k + c as u64))
                .collect();
            frames.push(Image {
                header: crate::fits::Header::new(),
                planes,
            });
        }
        let r = stack(frames, &StackOptions::default(), &mut silent).expect("stack");
        assert_eq!(r.image.planes.len(), 3);
        let dets: Vec<Detection> = r
            .image
            .planes
            .iter()
            .map(|p| detect(p, &DetectOptions::default()))
            .collect();
        // The same stars in the same places in all three planes.
        for s in dets[0].stars.iter().filter(|s| s.snr > 50.0).take(20) {
            for d in &dets[1..] {
                assert!(d.stars.iter().any(|t| (t.x - s.x).hypot(t.y - s.y) < 0.1));
            }
        }
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(stack(Vec::new(), &StackOptions::default(), &mut silent).is_err());
        let mosaic = Image::mono(
            Frame::zeros(10, 10).with_bayer(Some(helia_core::BayerPattern::Rggb)),
            crate::fits::Header::new(),
        );
        assert!(
            stack(
                vec![mosaic.clone(), mosaic],
                &StackOptions::default(),
                &mut silent
            )
            .is_err()
        );
        let scene = Scene::random(60, 400, 300, 85);
        let (frames, _) = session(&scene, 3, 5.0, 86);
        let o = StackOptions {
            reference: Reference::Index(9),
            ..StackOptions::default()
        };
        assert!(stack(frames, &o, &mut silent).is_err());
    }

    fn small_session() -> Vec<Image> {
        let scene = Scene::random(100, 400, 300, 5);
        session(&scene, 6, 8.0, 6).0
    }

    fn cancelled() -> StackOptions {
        let o = StackOptions::default();
        o.cancel.cancel();
        o
    }

    #[test]
    fn a_cancelled_token_stops_registration_alignment_and_stacking() {
        let frames = small_session();
        let o = cancelled();
        assert!(matches!(
            register_frames(&frames, &o, &mut silent),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            align(frames.clone(), &o, &mut silent),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            stack(frames.clone(), &o, &mut silent),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            stack_with(&frames, &o, &Cpu, &mut silent),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn cancelling_while_the_frames_are_registering_stops_the_run() {
        let frames = small_session();
        let o = StackOptions::default();
        let token = o.cancel.clone();
        let mut lines = 0;
        let r = stack_with(&frames, &o, &Cpu, &mut |line: &str| {
            lines += 1;
            if line.starts_with("frame 2:") {
                token.cancel();
            }
        });
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        // It stopped before the combine: that line is never logged.
        assert!(lines < 20, "{lines} lines");
    }

    #[test]
    fn cancelling_after_alignment_stops_the_combine() {
        let frames = small_session();
        let o = StackOptions::default();
        let aligned = align(frames, &o, &mut silent).expect("align");
        o.cancel.cancel();
        assert!(matches!(
            integrate(aligned, &o, &mut silent),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn an_unused_token_changes_nothing() {
        let frames = small_session();
        let plain = stack(frames.clone(), &StackOptions::default(), &mut silent).expect("stack");
        let o = StackOptions::default();
        let held = stack(
            frames,
            &StackOptions {
                cancel: o.cancel.clone(),
                ..o
            },
            &mut silent,
        )
        .expect("stack");
        for (x, y) in plain.image.planes[0]
            .data
            .iter()
            .zip(&held.image.planes[0].data)
        {
            assert!(x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()));
        }
    }
}
