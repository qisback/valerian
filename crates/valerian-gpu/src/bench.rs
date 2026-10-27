// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Timing and parity of the GPU kernels against the CPU code.
//!
//! Each stage runs on made-up data of the size asked for, once to warm up
//! (compiling the kernel, uploading the masters) and then `repeats` times,
//! on the CPU and on the GPU; the medians are compared, and the two results are
//! compared pixel by pixel. It is what `valerian bench` prints and the GPU tool
//! of the desktop app shows.

use std::time::Instant;

use valerian_core::Frame;
use valerian_core::calibrate::{Calibrator, DarkScaling, PixelMap};
use valerian_core::combine::{self, Rejection};
use valerian_core::compute::{Compute, Cpu, StackPlane};
use valerian_core::fits::{Header, Image, Value};
use valerian_core::testutil::{Rng, Scene};
use valerian_core::transform::Transform;
use valerian_core::warp::Interpolation;

use crate::GpuCompute;

/// What to measure.
#[derive(Clone, Debug)]
pub struct Config {
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Frames in the combine.
    pub frames: usize,
    /// Timed repeats of each stage.
    pub repeats: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 4096,
            height: 3072,
            frames: 20,
            repeats: 3,
        }
    }
}

/// One stage's result.
#[derive(Clone, Debug)]
pub struct Row {
    /// The stage.
    pub name: String,
    /// Pixels the stage worked on, summed over frames.
    pub pixels: u64,
    /// Median CPU time, seconds.
    pub cpu: f64,
    /// Median GPU time, seconds.
    pub gpu: f64,
    /// Of a GPU call, on average: seconds uploading, running kernels and reading back.
    pub phases: (f64, f64, f64),
    /// Largest difference where both have data, as a fraction of the data range.
    pub worst: f64,
    /// Fraction of pixels where only one of them has data.
    pub coverage: f64,
    /// Fraction of pixels that differ by more than 1e-4 of the data range.
    pub off: f64,
}

impl Row {
    /// CPU time over GPU time.
    #[must_use]
    pub fn speedup(&self) -> f64 {
        self.cpu / self.gpu.max(1e-9)
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Run `f` once to warm up and `repeats` times timed; the median time, the
/// result, and the average of what the GPU phases gained per call.
fn timed<T>(repeats: usize, gpu: &GpuCompute, f: impl FnMut() -> T) -> (f64, T, (f64, f64, f64)) {
    let before = gpu.stats();
    let (t, last) = time(repeats, f);
    let after = gpu.stats();
    let calls = (repeats.max(1) + 1) as f64;
    (
        t,
        last,
        (
            (after.upload - before.upload) / calls,
            (after.kernel - before.kernel) / calls,
            (after.readback - before.readback) / calls,
        ),
    )
}

fn time<T>(repeats: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut last = f();
    let mut times = Vec::with_capacity(repeats);
    for _ in 0..repeats.max(1) {
        let t = Instant::now();
        last = f();
        times.push(t.elapsed().as_secs_f64());
    }
    (median(times), last)
}

fn range(f: &Frame) -> f64 {
    let (lo, hi) = f
        .data
        .iter()
        .filter(|v| v.is_finite())
        .fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    f64::from(hi - lo).max(1.0)
}

/// How two frames differ: the largest difference where both have data as a
/// fraction of `a`'s range, the fraction of pixels where only one has data, and
/// the fraction of pixels that differ by more than 1e-4 of the range.
#[must_use]
pub fn compare(a: &Frame, b: &Frame) -> (f64, f64, f64) {
    let r = range(a);
    let limit = (1e-4 * r) as f32;
    let (mut worst, mut holes, mut off) = (0.0f32, 0usize, 0usize);
    for (x, y) in a.data.iter().zip(&b.data) {
        match (x.is_finite(), y.is_finite()) {
            (true, true) => {
                let d = (x - y).abs();
                worst = worst.max(d);
                if d > limit {
                    off += 1;
                }
            }
            (false, false) => {}
            _ => holes += 1,
        }
    }
    let n = a.data.len().max(1) as f64;
    (f64::from(worst) / r, holes as f64 / n, off as f64 / n)
}

fn plane(w: usize, h: usize, level: f32, spread: f64, rng: &mut Rng) -> Frame {
    Frame::new(
        w,
        h,
        (0..w * h)
            .map(|_| level + (rng.normal() * spread) as f32)
            .collect(),
    )
    .unwrap_or_else(|_| Frame::zeros(w, h))
}

fn calibrator(w: usize, h: usize) -> (Calibrator, Image) {
    let mut rng = Rng::new(5);
    let mut header = Header::new();
    header.set("EXPTIME", Value::Float(120.0), "");
    let mut dark = Image::mono(plane(w, h, 8.0, 2.0, &mut rng), header.clone());
    dark.header.set("BIASSUB", Value::Bool(true), "");
    dark.header.set("EXPTIME", Value::Float(60.0), "");
    for i in (0..w * h).step_by(997) {
        dark.planes[0].data[i] = 4000.0;
    }
    let mut flat = plane(w, h, 1.0, 0.05, &mut rng);
    for i in (0..w * h).step_by(1511) {
        flat.data[i] = 0.01;
    }
    let bias = Image::mono(plane(w, h, 1000.0, 1.5, &mut rng), header.clone());
    let flat = Image::mono(flat, header.clone());
    let light = Image::mono(plane(w, h, 3000.0, 40.0, &mut rng), header);
    let mut map = PixelMap::from_dark(&dark.planes[0], 8.0, 8.0);
    map.add_dead_from_flat(&flat.planes[0], 0.05);
    let mut c = Calibrator::new(Some(bias), Some(dark), Some(flat));
    c.pixel_map = Some(map);
    c.scaling = DarkScaling::Exposure;
    (c, light)
}

/// Run every stage on the CPU and on `gpu`.
///
/// `log` gets one line as each stage starts.
///
/// # Errors
/// The reason, when a stage fails on either engine.
#[allow(
    clippy::too_many_lines,
    reason = "one block per stage: its made-up data, the two timed calls and the row"
)]
pub fn run(gpu: &GpuCompute, cfg: &Config, log: &mut dyn FnMut(&str)) -> Result<Vec<Row>, String> {
    let (w, h) = (cfg.width, cfg.height);
    let px = (w * h) as u64;
    let mut rows = Vec::new();

    log("calibration: bias, dark, flat and the pixel map");
    let (c, light) = calibrator(w, h);
    let (cpu_t, cpu_r) = time(cfg.repeats, || Cpu.calibrate(&c, &light));
    let (gpu_t, gpu_r, phases) = timed(cfg.repeats, gpu, || gpu.calibrate(&c, &light));
    let (cpu_img, _) = cpu_r.map_err(|e| e.to_string())?;
    let (gpu_img, _) = gpu_r.map_err(|e| e.to_string())?;
    let (worst, coverage, off) = compare(&cpu_img.planes[0], &gpu_img.planes[0]);
    rows.push(Row {
        name: "calibrate (bias, dark, flat, pixel map)".into(),
        pixels: px,
        cpu: cpu_t,
        gpu: gpu_t,
        phases,
        worst,
        coverage,
        off,
    });

    log("registration warp: bicubic, rotated and shifted");
    let scene = Scene::random(400, w, h, 7);
    let src = scene.render(&Transform::identity(), 300.0, 6.0, 1.0, 0.0, 8);
    let t = Transform::rotation_about(w as f64 / 2.0, h as f64 / 2.0, 0.1, 17.3, -9.1);
    let (cpu_t, cpu_f) = time(cfg.repeats, || {
        Cpu.warp(&src, &t, w, h, Interpolation::Bicubic)
    });
    let (gpu_t, gpu_f, phases_w) = timed(cfg.repeats, gpu, || {
        gpu.warp(&src, &t, w, h, Interpolation::Bicubic)
    });
    let (worst, coverage, off) = compare(
        &cpu_f.map_err(|e| e.to_string())?,
        &gpu_f.map_err(|e| e.to_string())?,
    );
    rows.push(Row {
        name: "warp (bicubic)".into(),
        pixels: px,
        cpu: cpu_t,
        gpu: gpu_t,
        phases: phases_w,
        worst,
        coverage,
        off,
    });

    // The combine holds every frame in memory: keep the set to a few hundred MiB.
    let n = cfg.frames.clamp(3, 128);
    let rows_fit = ((768usize << 20) / (n * w * 4)).clamp(16, h);
    log(&format!("combine: {n} frames of {w} x {rows_fit}"));
    let scene = Scene::random(300, w, rows_fit, 9);
    let frames: Vec<Frame> = (0..n)
        .map(|k| scene.render(&Transform::identity(), 200.0, 8.0, 1.0, 0.0, 100 + k as u64))
        .collect();
    let refs: Vec<&Frame> = frames.iter().collect();
    for (name, rejection) in [
        (
            "combine (sigma clip)",
            Rejection::Sigma {
                low: 3.0,
                high: 3.0,
            },
        ),
        (
            "combine (winsorised)",
            Rejection::Winsorised {
                low: 3.0,
                high: 3.0,
            },
        ),
        (
            "combine (linear fit)",
            Rejection::LinearFit {
                low: 5.0,
                high: 2.5,
            },
        ),
    ] {
        let opts = combine::Options {
            rejection,
            ..combine::Options::default()
        };
        let (cpu_t, cpu_c) = time(cfg.repeats.min(2), || Cpu.combine(&refs, &opts));
        let (gpu_t, gpu_c, phases_c) = timed(cfg.repeats.min(2), gpu, || gpu.combine(&refs, &opts));
        let (worst, coverage, off) = compare(
            &cpu_c.map_err(|e| e.to_string())?.mean,
            &gpu_c.map_err(|e| e.to_string())?.mean,
        );
        rows.push(Row {
            name: name.into(),
            pixels: (n * w * rows_fit) as u64,
            cpu: cpu_t,
            gpu: gpu_t,
            phases: phases_c,
            worst,
            coverage,
            off,
        });
    }

    log(&format!(
        "stack plane: warp, level match and combine {n} frames of {w} x {rows_fit}"
    ));
    let mut rng = Rng::new(11);
    let transforms: Vec<Option<Transform>> = (0..n)
        .map(|k| {
            (k > 0).then(|| {
                Transform::rotation_about(
                    w as f64 / 2.0,
                    rows_fit as f64 / 2.0,
                    (rng.uniform() - 0.5) * 0.01,
                    (rng.uniform() - 0.5) * 30.0,
                    (rng.uniform() - 0.5) * 30.0,
                )
            })
        })
        .collect();
    let levels: Vec<Option<(f32, f32)>> = (0..n)
        .map(|k| (k > 0).then_some((200.0, 1.0 + 0.002 * k as f32)))
        .collect();
    let plane = StackPlane {
        frames: refs.clone(),
        transforms,
        levels,
        ref_sky: 200.0,
        size: (w, rows_fit),
        interp: Interpolation::Bicubic,
        combine: combine::Options {
            rejection: Rejection::Sigma {
                low: 3.0,
                high: 3.0,
            },
            weights: Some(vec![1.0; n]),
            ..combine::Options::default()
        },
    };
    let (cpu_t, cpu_c) = time(cfg.repeats.min(2), || Cpu.stack_plane(&plane));
    let (gpu_t, gpu_c, phases_s) = timed(cfg.repeats.min(2), gpu, || gpu.stack_plane(&plane));
    let (worst, coverage, off) = compare(
        &cpu_c.map_err(|e| e.to_string())?.mean,
        &gpu_c.map_err(|e| e.to_string())?.mean,
    );
    rows.push(Row {
        name: "stack plane (warp + combine)".into(),
        pixels: (n * w * rows_fit) as u64,
        cpu: cpu_t,
        gpu: gpu_t,
        phases: phases_s,
        worst,
        coverage,
        off,
    });
    Ok(rows)
}
