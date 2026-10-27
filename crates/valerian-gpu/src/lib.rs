// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! wgpu compute backend for Valerian.
//!
//! [`GpuCompute`] runs the three heavy per-pixel stages (calibration, the
//! registration warp and the stack's per-pixel combiner) as WGSL kernels on the
//! device Helia's `helia-gpu` opens, with Helia's adapter selection and device
//! teardown handling. It implements [`valerian_core::compute::Compute`], whose
//! CPU implementation is the specification: the parity tests in `tests/parity.rs`
//! check that both agree within the tolerances stated there. Cases a kernel does
//! not cover (more than 128 frames, a frame too big for the device, frames that
//! differ in size) fall back to the CPU, and [`GpuCompute::stats`] counts them.
#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "x, y, w, h, n and k are the natural names in image and kernel host code"
)]

pub mod bench;
mod compute;
mod kit;

pub use compute::{GpuCompute, Stats};
pub use helia_gpu::{AdapterRequest, GpuContext, GpuError};
use valerian_core::compute::{Compute, Cpu};

/// Version of wgpu this build links (read from `Cargo.lock` at build time).
pub const WGPU_VERSION: &str = env!("VALERIAN_WGPU_VERSION");

/// Which adapter the GPU tests ask for, from `VALERIAN_GPU_TESTS`.
///
/// `None` when unset or `none`.
///
/// # Panics
/// On a value other than `none`, `software` or `hardware`, so a typo cannot
/// silently skip the tests.
#[must_use]
pub fn test_adapter() -> Option<AdapterRequest> {
    match std::env::var("VALERIAN_GPU_TESTS").as_deref() {
        Ok("software") => Some(AdapterRequest::Software),
        Ok("hardware") => Some(AdapterRequest::Hardware),
        Ok("none") | Err(_) => None,
        Ok(other) => panic!("VALERIAN_GPU_TESTS={other:?}: expected software, hardware or none"),
    }
}

/// One-line description of the adapter that would be opened, or the reason
/// none can be.
#[must_use]
pub fn describe(request: AdapterRequest) -> String {
    match GpuContext::new(request) {
        Ok(ctx) if ctx.is_software() && request == AdapterRequest::Hardware => {
            format!("{} {SOFTWARE_NOTE}", ctx.summary())
        }
        Ok(ctx) => ctx.summary(),
        Err(e) => format!("unavailable ({e})"),
    }
}

/// Said wherever a software adapter (lavapipe, WARP) turns up in place of a real GPU.
pub const SOFTWARE_NOTE: &str =
    "[software adapter, slower than the CPU code: --backend auto uses the CPU instead]";

/// What to do with the adapter `Hardware` opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Use it.
    Use,
    /// Use it, and say it is a software adapter.
    UseWithNote,
    /// Do not use it: fall back to the CPU.
    Reject,
}

/// A software adapter is refused for `Auto` (it loses to the CPU code) and
/// allowed, with a note, for an explicit `Gpu`.
fn vet(choice: BackendChoice, software: bool) -> Verdict {
    match (software, choice) {
        (false, _) => Verdict::Use,
        (true, BackendChoice::Auto) => Verdict::Reject,
        (true, _) => Verdict::UseWithNote,
    }
}

/// Which engine the user asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// A real GPU if there is one and the work suits it (a stack), otherwise the
    /// CPU. A software adapter (lavapipe, WARP) is slower than the CPU code, so
    /// `Auto` never picks one.
    #[default]
    Auto,
    /// Always the CPU reference.
    Cpu,
    /// A GPU only: a real one if there is one, else the software adapter, else
    /// an error.
    Gpu,
}

impl std::str::FromStr for BackendChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            other => Err(format!("unknown backend {other:?}; use auto, cpu or gpu")),
        }
    }
}

/// The engine chosen, and why.
pub struct Selected {
    /// The engine.
    pub compute: Box<dyn Compute + Send>,
    /// What it is, for the log: `CPU`, or the adapter, or why the GPU was not used.
    pub description: String,
    /// The GPU engine itself when one was chosen, for its [`Stats`].
    pub gpu: Option<std::sync::Arc<GpuCompute>>,
}

/// Open the engine `choice` asks for.
///
/// `gpu_pays` says whether the caller does work the GPU does faster than the CPU
/// (a stack, whose frames stay on the device from the warp to the combine) or not
/// (a single stage, whose copies across the bus cost more than the kernel saves):
/// [`BackendChoice::Auto`] opens the GPU only when it does.
///
/// # Errors
/// [`GpuError`] for [`BackendChoice::Gpu`] when no adapter opens.
pub fn select(choice: BackendChoice, gpu_pays: bool) -> Result<Selected, GpuError> {
    let cpu = |why: String| Selected {
        compute: Box::new(Cpu),
        description: why,
        gpu: None,
    };
    let open = |request| -> Result<Selected, GpuError> {
        let g = std::sync::Arc::new(GpuCompute::new(request)?);
        let verdict = vet(choice, g.is_software());
        if verdict == Verdict::Reject {
            return Ok(cpu(format!(
                "CPU (only a software adapter, {}, which is slower than the CPU; --backend gpu forces it)",
                g.name()
            )));
        }
        Ok(Selected {
            description: if verdict == Verdict::UseWithNote {
                format!("{} {SOFTWARE_NOTE}", g.name())
            } else {
                g.name()
            },
            compute: Box::new(Shared(std::sync::Arc::clone(&g))),
            gpu: Some(g),
        })
    };
    match choice {
        BackendChoice::Cpu => Ok(cpu("CPU".to_owned())),
        BackendChoice::Auto if !gpu_pays => Ok(cpu(
            "CPU (the GPU is slower for this command; --backend gpu forces it)".to_owned(),
        )),
        BackendChoice::Auto => Ok(
            open(AdapterRequest::Hardware).unwrap_or_else(|e| cpu(format!("CPU (no GPU: {e})")))
        ),
        BackendChoice::Gpu => {
            open(AdapterRequest::Hardware).or_else(|_| open(AdapterRequest::Software))
        }
    }
}

/// A [`GpuCompute`] shared between the caller and the pipeline.
struct Shared(std::sync::Arc<GpuCompute>);

impl Compute for Shared {
    fn name(&self) -> String {
        self.0.name()
    }

    fn calibrate(
        &self,
        calibrator: &valerian_core::calibrate::Calibrator,
        light: &valerian_core::fits::Image,
    ) -> valerian_core::Result<(valerian_core::fits::Image, valerian_core::calibrate::Report)> {
        self.0.calibrate(calibrator, light)
    }

    fn warp(
        &self,
        src: &valerian_core::Frame,
        to_reference: &valerian_core::transform::Transform,
        width: usize,
        height: usize,
        interp: valerian_core::warp::Interpolation,
    ) -> valerian_core::Result<valerian_core::Frame> {
        self.0.warp(src, to_reference, width, height, interp)
    }

    fn combine(
        &self,
        frames: &[&valerian_core::Frame],
        opts: &valerian_core::combine::Options,
    ) -> valerian_core::Result<valerian_core::combine::Combined> {
        self.0.combine(frames, opts)
    }

    fn stack_plane(
        &self,
        plane: &valerian_core::compute::StackPlane<'_>,
    ) -> valerian_core::Result<valerian_core::combine::Combined> {
        self.0.stack_plane(plane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_opens_when_requested() {
        let Some(request) = test_adapter() else {
            eprintln!("VALERIAN_GPU_TESTS is unset or 'none': skipping GPU tests");
            return;
        };
        let ctx = GpuContext::new(request)
            .unwrap_or_else(|e| panic!("VALERIAN_GPU_TESTS is set but no adapter opened: {e}"));
        assert_ne!(ctx.summary(), "");
        // The nightly GPU lane greps this line to prove the NVIDIA adapter ran the tests.
        eprintln!("GPU tests on: {}", ctx.summary());
        if request == AdapterRequest::Software {
            assert!(ctx.is_software());
        }
    }

    #[test]
    fn wgpu_version_is_recorded() {
        assert_ne!(WGPU_VERSION, "");
    }

    #[test]
    fn backend_names_parse() {
        assert_eq!("auto".parse(), Ok(BackendChoice::Auto));
        assert_eq!(" GPU ".parse(), Ok(BackendChoice::Gpu));
        assert_eq!("cpu".parse(), Ok(BackendChoice::Cpu));
        assert!("cuda".parse::<BackendChoice>().is_err());
    }

    #[test]
    fn auto_refuses_a_software_adapter_and_gpu_allows_it_with_a_note() {
        assert_eq!(vet(BackendChoice::Auto, true), Verdict::Reject);
        assert_eq!(vet(BackendChoice::Auto, false), Verdict::Use);
        assert_eq!(vet(BackendChoice::Gpu, true), Verdict::UseWithNote);
        assert_eq!(vet(BackendChoice::Gpu, false), Verdict::Use);
    }

    #[test]
    fn the_cpu_choice_never_opens_a_device() {
        let s = select(BackendChoice::Cpu, true).expect("cpu");
        assert_eq!(s.description, "CPU");
        assert!(s.gpu.is_none());
        assert_eq!(s.compute.name(), "CPU");
    }
}
