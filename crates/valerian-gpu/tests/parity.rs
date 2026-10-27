// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and n are the natural names in image tests"
)]

//! CPU and GPU agree.
//!
//! The CPU code in `valerian-core` is the specification; every kernel must match
//! it within the tolerances stated here. `VALERIAN_GPU_TESTS` selects the adapter
//! (`software` for lavapipe or WARP, `hardware` for a real GPU); unset or `none`
//! skips these tests.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use valerian_core::Frame;
use valerian_core::calibrate::{Calibrator, DarkScaling, PixelMap};
use valerian_core::combine::{self, Rejection};
use valerian_core::compute::{Compute, Cpu};
use valerian_core::fits::{Header, Image, Value};
use valerian_core::stack::{Reference, StackOptions, stack_with};
use valerian_core::testutil::{Rng, Scene};
use valerian_core::transform::Transform;
use valerian_core::warp::Interpolation;
use valerian_gpu::{AdapterRequest, GpuCompute, test_adapter};

/// The device a test uses.
#[allow(
    clippy::large_enum_variant,
    reason = "a test helper: one value per test"
)]
enum Device {
    /// One software device for the whole binary: opening and closing lavapipe
    /// devices over and over in one process has hung it.
    Shared(&'static GpuCompute),
    /// A hardware device, opened for one test and torn down at its end: a static
    /// one would be destroyed by the driver at process exit, which faults.
    Owned(GpuCompute),
}

impl std::ops::Deref for Device {
    type Target = GpuCompute;

    fn deref(&self) -> &GpuCompute {
        match self {
            Self::Shared(g) => g,
            Self::Owned(g) => g,
        }
    }
}

/// The device, and a lock so one test at a time uses the adapter.
fn gpu() -> Option<(Device, MutexGuard<'static, ()>)> {
    static LOCK: Mutex<()> = Mutex::new(());
    static SOFTWARE: OnceLock<GpuCompute> = OnceLock::new();
    let request = test_adapter()?;
    let guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let open = || {
        GpuCompute::new(request)
            .unwrap_or_else(|e| panic!("VALERIAN_GPU_TESTS is set but no adapter opened: {e}"))
    };
    let device = if request == AdapterRequest::Software {
        Device::Shared(SOFTWARE.get_or_init(open))
    } else {
        Device::Owned(open())
    };
    Some((device, guard))
}

fn range(f: &Frame) -> f32 {
    let (lo, hi) = f
        .data
        .iter()
        .filter(|v| v.is_finite())
        .fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    hi - lo
}

/// How two frames differ: the largest difference where both have data, as a
/// fraction of the range, and the fraction of pixels where only one has data.
fn compare(a: &Frame, b: &Frame) -> (f32, f64) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut worst, mut holes) = (0.0f32, 0usize);
    for (x, y) in a.data.iter().zip(&b.data) {
        match (x.is_finite(), y.is_finite()) {
            (true, true) => worst = worst.max((x - y).abs()),
            (false, false) => {}
            _ => holes += 1,
        }
    }
    (
        worst / range(a).max(1.0),
        holes as f64 / a.data.len() as f64,
    )
}

fn sky(w: usize, h: usize, seed: u64) -> Frame {
    let scene = Scene::random(60, w, h, seed);
    scene.render(&Transform::identity(), 300.0, 6.0, 1.0, 0.0, seed)
}

#[test]
fn warp_matches_the_cpu_for_every_kernel_and_transform() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fallbacks = gpu.stats().fallbacks;
    let src = sky(480, 360, 1);
    let transforms = [
        Transform::similarity(1.0, 0.0, 3.0, -2.0),
        Transform::similarity(1.0, 0.0, 7.37, 4.62),
        Transform::rotation_about(240.0, 180.0, 0.12, 5.3, -3.1),
        Transform::similarity(0.93, -0.4, 20.5, 31.25),
        Transform {
            m: [
                [0.99, 0.02, 4.0],
                [-0.015, 1.01, -6.0],
                [2.0e-5, -1.0e-5, 1.0],
            ],
        },
    ];
    for interp in [
        Interpolation::Bilinear,
        Interpolation::Bicubic,
        Interpolation::Lanczos3,
    ] {
        for t in &transforms {
            let cpu = Cpu.warp(&src, t, 480, 360, interp).expect("cpu");
            let g = gpu.warp(&src, t, 480, 360, interp).expect("gpu");
            let (worst, holes) = compare(&cpu, &g);
            // f32 coordinates: a few thousandths of a pixel at the far edge.
            assert!(
                worst < 5e-4,
                "{interp:?} {t:?}: worst difference {worst} of the range"
            );
            assert!(
                holes < 2e-3,
                "{interp:?}: {holes} of the pixels differ in coverage"
            );
        }
    }
    assert_eq!(gpu.stats().fallbacks, fallbacks);
}

#[test]
fn warp_leaves_nan_where_the_source_has_no_data() {
    let Some((gpu, _guard)) = gpu() else { return };
    let mut src = sky(300, 200, 2);
    for y in 80..100 {
        for x in 120..160 {
            src.data[y * 300 + x] = f32::NAN;
        }
    }
    src.data[10 * 300 + 10] = f32::INFINITY;
    let t = Transform::similarity(1.0, 0.0, 2.5, 1.5);
    let cpu = Cpu
        .warp(&src, &t, 300, 200, Interpolation::Bicubic)
        .expect("cpu");
    let g = gpu
        .warp(&src, &t, 300, 200, Interpolation::Bicubic)
        .expect("gpu");
    let (worst, holes) = compare(&cpu, &g);
    assert!(worst < 5e-4 && holes < 1e-3, "{worst} {holes}");
    assert!(g.data.iter().filter(|v| v.is_nan()).count() > 1000);
}

/// Masters, a pixel map and a light for a `w` by `h` sensor.
fn rig(w: usize, h: usize, bayer: bool, seed: u64) -> (Calibrator, Image) {
    let mut rng = Rng::new(seed);
    let plane = |level: f32, spread: f64, rng: &mut Rng| -> Frame {
        Frame::new(
            w,
            h,
            (0..w * h)
                .map(|_| level + (rng.normal() * spread) as f32)
                .collect(),
        )
        .expect("frame")
    };
    let mut header = Header::new();
    header.set("EXPTIME", Value::Float(120.0), "");
    let mut dark = Image::mono(plane(8.0, 2.0, &mut rng), header.clone());
    dark.header.set("BIASSUB", Value::Bool(true), "");
    dark.header.set("EXPTIME", Value::Float(60.0), "");
    // A few hot pixels in the dark and some flat pixels that respond too little.
    for i in (0..w * h).step_by(997) {
        dark.planes[0].data[i] = 4000.0;
    }
    let mut flat_frame = plane(1.0, 0.05, &mut rng);
    for i in (0..w * h).step_by(1511) {
        flat_frame.data[i] = 0.01;
    }
    let bias = Image::mono(plane(1000.0, 1.5, &mut rng), header.clone());
    let flat = Image::mono(flat_frame, header.clone());
    let mut light = Image::mono(plane(3000.0, 40.0, &mut rng), header);
    for p in &mut light.planes {
        p.bayer = bayer.then_some(valerian_core::BayerPattern::Rggb);
    }
    let mut map = PixelMap::from_dark(&dark.planes[0], 8.0, 8.0);
    map.add_dead_from_flat(&flat.planes[0], 0.05);
    let mut c = Calibrator::new(Some(bias), Some(dark), Some(flat));
    c.pixel_map = Some(map);
    c.scaling = DarkScaling::Exposure;
    (c, light)
}

#[test]
fn calibration_matches_the_cpu() {
    let Some((gpu, _guard)) = gpu() else { return };
    for (bayer, seed) in [(false, 3u64), (true, 4)] {
        let (c, light) = rig(400, 300, bayer, seed);
        let (cpu, cr) = Cpu.calibrate(&c, &light).expect("cpu");
        let (g, gr) = gpu.calibrate(&c, &light).expect("gpu");
        assert!(cr.repaired > 50, "the rig repairs {}", cr.repaired);
        assert_eq!(gr.repaired, cr.repaired, "pixels repaired");
        assert!((gr.dark_scale - cr.dark_scale).abs() < 1e-12);
        let (worst, holes) = compare(&cpu.planes[0], &g.planes[0]);
        assert!(
            worst < 1e-5 && holes == 0.0,
            "bayer {bayer}: {worst} of the range, {holes}"
        );
        assert_eq!(cpu.header.cards().len(), g.header.cards().len());
    }
    // The masters are held on the device: a second light uses them.
    let (c, light) = rig(400, 300, false, 3);
    let _ = gpu.calibrate(&c, &light).expect("first");
    let (cpu, _) = Cpu.calibrate(&c, &light).expect("cpu");
    let (g, _) = gpu.calibrate(&c, &light).expect("second");
    assert!(compare(&cpu.planes[0], &g.planes[0]).0 < 1e-5);
}

#[test]
fn calibration_without_masters_or_with_a_colour_image() {
    let Some((gpu, _guard)) = gpu() else { return };
    let (_, light) = rig(200, 150, false, 5);
    let c = Calibrator::new(None, None, None);
    let (cpu, _) = Cpu.calibrate(&c, &light).expect("cpu");
    let (g, _) = gpu.calibrate(&c, &light).expect("gpu");
    assert_eq!(cpu.planes[0].data, g.planes[0].data);
    // Three planes, a bias only.
    let mut rgb = light.clone();
    rgb.planes = vec![
        light.planes[0].clone(),
        light.planes[0].clone(),
        light.planes[0].clone(),
    ];
    let bias = Image {
        header: Header::new(),
        planes: vec![Frame::new(200, 150, vec![10.0; 30000]).expect("f"); 3],
    };
    let c = Calibrator::new(Some(bias), None, None);
    let (cpu, _) = Cpu.calibrate(&c, &rgb).expect("cpu");
    let (g, _) = gpu.calibrate(&c, &rgb).expect("gpu");
    for p in 0..3 {
        assert!(compare(&cpu.planes[p], &g.planes[p]).0 < 1e-7);
    }
    // A master of the wrong size is refused as before.
    let wrong = Calibrator::new(
        Some(Image::mono(Frame::zeros(10, 10), Header::new())),
        None,
        None,
    );
    assert!(gpu.calibrate(&wrong, &light).is_err());
}

/// `n` noisy frames of one sky with a few cosmic rays, hot pixels and holes.
fn frames(n: usize, w: usize, h: usize, seed: u64) -> Vec<Frame> {
    let scene = Scene::random(40, w, h, seed);
    let mut rng = Rng::new(seed + 1);
    (0..n)
        .map(|k| {
            let mut f = scene.render(
                &Transform::identity(),
                200.0,
                8.0,
                1.0 + 0.01 * k as f64,
                0.0,
                seed * 100 + k as u64,
            );
            for _ in 0..30 {
                let i = (rng.uniform() * (w * h) as f64) as usize;
                f.data[i] += 2000.0 + 3000.0 * rng.uniform() as f32;
            }
            if k % 5 == 2 {
                // A frame that does not cover the left strip.
                for y in 0..h {
                    for x in 0..12 {
                        f.data[y * w + x] = f32::NAN;
                    }
                }
            }
            f
        })
        .collect()
}

fn same_combine(gpu: &GpuCompute, fr: &[Frame], opts: &combine::Options, label: &str) {
    let refs: Vec<&Frame> = fr.iter().collect();
    let cpu = Cpu.combine(&refs, opts).expect("cpu");
    let g = gpu.combine(&refs, opts).expect("gpu");
    let px = cpu.mean.data.len();
    let (mut differ_used, mut worst) = (0usize, 0.0f32);
    for i in 0..px {
        let (a, b) = (cpu.mean.data[i], g.mean.data[i]);
        assert_eq!(a.is_nan(), b.is_nan(), "{label}: coverage at pixel {i}");
        if cpu.used[i] != g.used[i] {
            differ_used += 1;
        } else if a.is_finite() {
            worst = worst.max((a - b).abs() / a.abs().max(1.0));
        }
    }
    // A sample sitting on a clipping limit can fall either way in f32.
    assert!(
        differ_used as f64 / px as f64 <= 2e-3,
        "{label}: {differ_used} of {px} pixels use a different number of samples"
    );
    assert!(worst < 2e-5, "{label}: worst relative difference {worst}");
    for (name, a, b) in [
        ("valid", &cpu.valid, &g.valid),
        ("low", &cpu.rejected_low, &g.rejected_low),
        ("high", &cpu.rejected_high, &g.rejected_high),
    ] {
        for (f, (x, y)) in a.iter().zip(b).enumerate() {
            let tol = (*x / 500).max(3);
            assert!(
                x.abs_diff(*y) <= tol,
                "{label}: frame {f} {name} {x} against {y}"
            );
        }
    }
}

#[test]
fn combine_matches_the_cpu_for_every_rejection_and_frame_count() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fallbacks = gpu.stats().fallbacks;
    let schemes = [
        Rejection::None,
        Rejection::MinMax { low: 1, high: 2 },
        Rejection::Sigma {
            low: 3.0,
            high: 3.0,
        },
        Rejection::Winsorised {
            low: 3.0,
            high: 3.0,
        },
        Rejection::LinearFit {
            low: 5.0,
            high: 2.5,
        },
    ];
    for n in [3usize, 12, 33, 70] {
        let fr = frames(n, 160, 120, n as u64);
        for rejection in schemes {
            let opts = combine::Options {
                rejection,
                ..combine::Options::default()
            };
            same_combine(&gpu, &fr, &opts, &format!("{n} frames, {rejection:?}"));
        }
    }
    assert_eq!(gpu.stats().fallbacks, fallbacks);
}

#[test]
fn combine_weights_and_iterations_are_honoured() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fr = frames(14, 120, 90, 21);
    let weights: Vec<f32> = (0..14).map(|i| 0.5 + 0.25 * i as f32).collect();
    let opts = combine::Options {
        rejection: Rejection::Winsorised {
            low: 2.5,
            high: 3.5,
        },
        max_iterations: 3,
        weights: Some(weights),
        maps: false,
    };
    same_combine(&gpu, &fr, &opts, "weighted");
    // Bad weights are the CPU's error.
    let bad = combine::Options {
        weights: Some(vec![1.0; 3]),
        ..combine::Options::default()
    };
    let refs: Vec<&Frame> = fr.iter().collect();
    assert!(gpu.combine(&refs, &bad).is_err());
}

#[test]
fn more_frames_than_the_kernel_holds_fall_back_to_the_cpu() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fr = frames(130, 40, 30, 31);
    let before = gpu.stats().fallbacks;
    same_combine(&gpu, &fr, &combine::Options::default(), "130 frames");
    let after = gpu.stats();
    assert_eq!(after.fallbacks, before + 1);
    assert!(after.last_fallback.contains("130 frames"));
}

#[test]
fn a_stack_on_the_gpu_matches_the_cpu_stack() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fallbacks = gpu.stats().fallbacks;
    let scene = Scene::random(120, 400, 300, 41);
    let mut rng = Rng::new(42);
    let frames: Vec<Image> = (0..9)
        .map(|k| {
            let view = Transform::rotation_about(
                200.0,
                150.0,
                (rng.uniform() - 0.5) * 0.01,
                (rng.uniform() - 0.5) * 10.0,
                (rng.uniform() - 0.5) * 10.0,
            );
            let mut img = Image::mono(
                scene.render(&view, 300.0, 6.0, 1.0, 0.0, 600 + k),
                Header::new(),
            );
            img.header.set("EXPTIME", Value::Float(60.0), "");
            img
        })
        .collect();
    let opts = StackOptions {
        reference: Reference::Index(0),
        ..StackOptions::default()
    };
    let cpu = stack_with(&frames, &opts, &Cpu, &mut |_| {}).expect("cpu stack");
    let g = stack_with(&frames, &opts, &*gpu, &mut |_| {}).expect("gpu stack");
    assert_eq!(cpu.used(), g.used());
    let (worst, holes) = compare(&cpu.image.planes[0], &g.image.planes[0]);
    assert!(
        worst < 1e-3 && holes < 1e-3,
        "{worst} of the range, {holes} coverage"
    );
    assert_eq!(gpu.stats().fallbacks, fallbacks);
}

#[test]
fn a_colour_stack_of_many_frames_matches_the_cpu() {
    let Some((gpu, _guard)) = gpu() else { return };
    let fallbacks = gpu.stats().fallbacks;
    let scene = Scene::random(100, 240, 180, 51);
    let mut rng = Rng::new(52);
    let frames: Vec<Image> = (0..40)
        .map(|k| {
            let view = Transform::rotation_about(
                120.0,
                90.0,
                (rng.uniform() - 0.5) * 0.008,
                (rng.uniform() - 0.5) * 8.0,
                (rng.uniform() - 0.5) * 8.0,
            );
            let planes: Vec<Frame> = (0..3)
                .map(|c| {
                    scene.render(
                        &view,
                        250.0 + 40.0 * c as f32,
                        5.0,
                        0.9 + 0.1 * f64::from(c),
                        0.0,
                        900 + 3 * k + c as u64,
                    )
                })
                .collect();
            let mut header = Header::new();
            header.set("EXPTIME", Value::Float(30.0), "");
            Image { header, planes }
        })
        .collect();
    for (interpolation, normalisation) in [
        (
            Interpolation::Lanczos3,
            valerian_core::stack::Normalisation::AdditiveScaling,
        ),
        (
            Interpolation::Bilinear,
            valerian_core::stack::Normalisation::None,
        ),
    ] {
        let opts = StackOptions {
            reference: Reference::Index(0),
            interpolation,
            normalisation,
            ..StackOptions::default()
        };
        let cpu = stack_with(&frames, &opts, &Cpu, &mut |_| {}).expect("cpu stack");
        let g = stack_with(&frames, &opts, &*gpu, &mut |_| {}).expect("gpu stack");
        assert_eq!(cpu.used(), g.used());
        for p in 0..3 {
            let (worst, holes) = compare(&cpu.image.planes[p], &g.image.planes[p]);
            assert!(
                worst < 2e-3 && holes < 1e-3,
                "{interpolation:?} plane {p}: {worst}, {holes}"
            );
        }
        // A sample on a clipping limit can fall either way (the warped values differ
        // in the seventh digit and one flip can change the next pass): a pixel in a
        // thousand differs by a frame, one in many thousands by two.
        let off = cpu
            .coverage
            .data
            .iter()
            .zip(&g.coverage.data)
            .filter(|(x, y)| (**x - **y).abs() > 1.0)
            .count();
        let differ = cpu
            .coverage
            .data
            .iter()
            .zip(&g.coverage.data)
            .filter(|(x, y)| (**x - **y).abs() > 0.5)
            .count();
        assert!(
            off * 5000 <= cpu.coverage.data.len() && differ * 100 < cpu.coverage.data.len(),
            "{off} pixels differ by more than a frame, {differ} by any"
        );
        for (a, b) in cpu.frames.iter().zip(&g.frames) {
            assert!(
                (a.rejected - b.rejected).abs() < 5e-3,
                "{} {}",
                a.rejected,
                b.rejected
            );
        }
    }
    assert_eq!(gpu.stats().fallbacks, fallbacks);
}
