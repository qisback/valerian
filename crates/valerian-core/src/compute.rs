// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Where the heavy per-pixel stages run.
//!
//! Calibration, the registration warp and the per-pixel combiner of a stack are
//! the stages whose cost grows with the number of pixels times the number of
//! frames. They go through [`Compute`], whose [`Cpu`] implementation is this
//! crate's own code and *the specification*: a GPU implementation (the
//! `valerian-gpu` crate) must agree with it to the tolerances its parity tests
//! state, and may fall back to the CPU for cases it does not handle.
//!
//! Everything else (detection, registration, the fits and the clipping
//! decisions of the other tools) stays on the CPU: it is branchy and
//! sequential, and small beside the stages above.

use crate::calibrate::{Calibrator, Report};
use crate::combine::{self, Combined};
use crate::error::Result;
use crate::fits::Image;
use crate::transform::Transform;
use crate::warp::{self, Interpolation};
use helia_core::Frame;

/// One colour plane of a stack: every kept frame's plane, how each is brought onto
/// the reference grid and matched to the reference's level, and how they are
/// combined. See [`Compute::stack_plane`].
#[derive(Clone, Debug)]
pub struct StackPlane<'a> {
    /// The plane of each kept frame, reference included.
    pub frames: Vec<&'a Frame>,
    /// Source to reference coordinates for each frame; `None` for the
    /// reference, which is used as it is.
    pub transforms: Vec<Option<Transform>>,
    /// The sky level and the flux scale each frame is matched with after its
    /// warp, `(v - sky) * scale + ref_sky`; `None` for no matching.
    pub levels: Vec<Option<(f32, f32)>>,
    /// The sky level of the reference plane.
    pub ref_sky: f32,
    /// The size of the reference grid.
    pub size: (usize, usize),
    /// The resampling kernel.
    pub interp: Interpolation,
    /// The rejection and the weights (one per frame) of the combine.
    pub combine: combine::Options,
}

/// [`Compute::stack_plane`] built from the other methods: warp each frame,
/// match its level, then combine. This is the specification of the fused
/// versions.
///
/// # Errors
/// As [`Compute::warp`] and [`Compute::combine`].
pub fn stack_plane_classic(c: &dyn Compute, p: &StackPlane<'_>) -> Result<Combined> {
    let (w, h) = p.size;
    let mut warped: Vec<Frame> = Vec::with_capacity(p.frames.len());
    for ((f, t), level) in p.frames.iter().zip(&p.transforms).zip(&p.levels) {
        warped.push(match t {
            None => (*f).clone(),
            Some(t) => {
                let mut out = c.warp(f, t, w, h, p.interp)?;
                if let Some((sky, k)) = level {
                    for v in &mut out.data {
                        *v = (*v - sky) * k + p.ref_sky;
                    }
                }
                out
            }
        });
    }
    let refs: Vec<&Frame> = warped.iter().collect();
    c.combine(&refs, &p.combine)
}

/// An engine for the heavy per-pixel stages.
pub trait Compute: Sync {
    /// A short description for logs: `CPU` or the adapter's name.
    fn name(&self) -> String;

    /// Calibrate a light frame, as [`Calibrator::calibrate`].
    ///
    /// # Errors
    /// As [`Calibrator::calibrate`].
    fn calibrate(&self, calibrator: &Calibrator, light: &Image) -> Result<(Image, Report)>;

    /// Resample `src` onto a `width` by `height` grid, as [`warp::warp`].
    ///
    /// # Errors
    /// As [`warp::warp`].
    fn warp(
        &self,
        src: &Frame,
        to_reference: &Transform,
        width: usize,
        height: usize,
        interp: Interpolation,
    ) -> Result<Frame>;

    /// Combine frames pixel by pixel with rejection, as [`combine::combine`].
    ///
    /// # Errors
    /// As [`combine::combine`].
    fn combine(&self, frames: &[&Frame], opts: &combine::Options) -> Result<Combined>;

    /// Warp, level-match and combine one plane of a stack. An engine that can
    /// keep the frames on its device overrides this to skip the copies between
    /// the stages; the result must be that of [`stack_plane_classic`].
    ///
    /// # Errors
    /// As [`Compute::warp`] and [`Compute::combine`].
    fn stack_plane(&self, plane: &StackPlane<'_>) -> Result<Combined>;
}

/// The reference implementation: this crate's CPU code.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cpu;

impl Compute for Cpu {
    fn name(&self) -> String {
        "CPU".to_owned()
    }

    fn calibrate(&self, calibrator: &Calibrator, light: &Image) -> Result<(Image, Report)> {
        calibrator.calibrate(light)
    }

    fn warp(
        &self,
        src: &Frame,
        to_reference: &Transform,
        width: usize,
        height: usize,
        interp: Interpolation,
    ) -> Result<Frame> {
        warp::warp(src, to_reference, width, height, interp)
    }

    fn combine(&self, frames: &[&Frame], opts: &combine::Options) -> Result<Combined> {
        combine::combine(frames, opts)
    }

    fn stack_plane(&self, plane: &StackPlane<'_>) -> Result<Combined> {
        stack_plane_classic(self, plane)
    }
}
