// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The GPU implementation of [`Compute`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use bytemuck::{Pod, Zeroable};
use helia_gpu::wgpu::{Buffer, BufferDescriptor, BufferUsages};
use helia_gpu::{AdapterRequest, GpuContext, GpuError};
use valerian_core::Frame;
use valerian_core::calibrate::{Calibrator, Report};
use valerian_core::combine::{self, Combined, Rejection};
use valerian_core::compute::{Compute, Cpu, StackPlane, stack_plane_classic};
use valerian_core::error::{Error, Result};
use valerian_core::fits::Image;
use valerian_core::transform::Transform;
use valerian_core::warp::Interpolation;

use crate::kit::{Kernel, groups_1d, read_pod, storage_from, uniform_from};

const CALIBRATE_ARITH: &str = include_str!("shaders/calibrate_arith.wgsl");
const CALIBRATE_FIX: &str = include_str!("shaders/calibrate_fix.wgsl");
const WARP: &str = include_str!("shaders/warp.wgsl");
const COMBINE: &str = include_str!("shaders/combine.wgsl");

/// The most frames one combine kernel handles; more go to the CPU.
pub const MAX_FRAMES: usize = 128;

/// Largest working set of one combine band, in bytes.
const BAND_BYTES: usize = 256 << 20;

/// Most source frame data a fused stack keeps on the device, in bytes.
const SOURCE_BYTES: usize = 6 << 30;

/// What a [`GpuCompute`] has done.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Calls that ran on the GPU.
    pub gpu_calls: u64,
    /// Calls that fell back to the CPU.
    pub fallbacks: u64,
    /// The reason for the latest fallback.
    pub last_fallback: String,
    /// Seconds spent making and filling device buffers (host to device).
    pub upload: f64,
    /// Seconds the kernels ran on the device.
    pub kernel: f64,
    /// Seconds spent copying results back (device to host).
    pub readback: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ArithParams {
    count: u32,
    flags: u32,
    k: f32,
    floor: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FixParams {
    width: u32,
    height: u32,
    step: u32,
    count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WarpParams {
    src_w: u32,
    src_h: u32,
    out_w: u32,
    rows: u32,
    row0: u32,
    out_off: u32,
    interp: u32,
    flags: u32,
    level: [f32; 4],
    r0: [f32; 4],
    r1: [f32; 4],
    r2: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CombineParams {
    n: u32,
    npx: u32,
    kind: u32,
    max_iter: u32,
    low: f32,
    high: f32,
    low_n: u32,
    high_n: u32,
}

/// Masters held on the device between calls, so a run of lights uploads them once.
struct Masters {
    key: Vec<u64>,
    planes: Vec<MasterPlane>,
}

struct MasterPlane {
    bias: Option<Buffer>,
    dark: Option<Buffer>,
    flat: Option<Buffer>,
    flags: Option<Buffer>,
}

/// A buffer kept for reuse: its size, usage and the buffer.
type Entry = (u64, BufferUsages, Buffer);

/// Buffers that calls reuse instead of allocating and freeing hundreds of
/// megabytes each time. Everything a call takes goes back at its end.
#[derive(Default)]
struct Pool {
    free: Vec<Entry>,
    used: Vec<Entry>,
}

/// Most idle buffer memory the pool keeps, in bytes.
const POOL_BYTES: u64 = 3 << 30;

const STORAGE: BufferUsages = BufferUsages::STORAGE
    .union(BufferUsages::COPY_SRC)
    .union(BufferUsages::COPY_DST);
const STAGING: BufferUsages = BufferUsages::MAP_READ.union(BufferUsages::COPY_DST);

/// The GPU engine.
pub struct GpuCompute {
    ctx: GpuContext,
    /// One call at a time uses the device and the pool.
    gate: Mutex<()>,
    pool: Mutex<Pool>,
    arith: OnceLock<Kernel>,
    fix: OnceLock<Kernel>,
    warp: OnceLock<Kernel>,
    combine: [OnceLock<Kernel>; 3],
    masters: Mutex<Option<Masters>>,
    gpu_calls: AtomicU64,
    fallbacks: AtomicU64,
    last_fallback: Mutex<String>,
    /// Upload, kernel and read-back seconds, as bits of f64 in nanoseconds.
    phases: [AtomicU64; 3],
}

/// One call's hold on the device; returns the pooled buffers when dropped.
struct Op<'a> {
    gpu: &'a GpuCompute,
    _gate: std::sync::MutexGuard<'a, ()>,
}

impl Drop for Op<'_> {
    fn drop(&mut self) {
        self.gpu.release();
    }
}

/// Adds its lifetime to a phase counter when dropped.
struct Phase<'a> {
    slot: &'a AtomicU64,
    start: std::time::Instant,
}

impl Drop for Phase<'_> {
    fn drop(&mut self) {
        let ns = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.slot.fetch_add(ns, Ordering::Relaxed);
    }
}

fn bytes_of(f: &Frame) -> &[u8] {
    bytemuck::cast_slice(&f.data)
}

/// The kernel capacity (index and frames) for `n` frames.
fn combine_capacity(n: usize) -> (usize, usize) {
    let cap = if n <= 32 {
        0
    } else if n <= 64 {
        1
    } else {
        2
    };
    (cap, [32usize, 64, 128][cap])
}

/// The kernel parameters of a rejection scheme: kind, low, high, min/max counts.
fn rejection_params(r: Rejection) -> (u32, f32, f32, u32, u32) {
    match r {
        Rejection::None => (0, 0.0, 0.0, 0, 0),
        Rejection::MinMax { low, high } => (1, 0.0, 0.0, low as u32, high as u32),
        Rejection::Sigma { low, high } => (2, low, high, 0, 0),
        Rejection::Winsorised { low, high } => (3, low, high, 0, 0),
        Rejection::LinearFit { low, high } => (4, low, high, 0, 0),
    }
}

fn interp_code(i: Interpolation) -> u32 {
    match i {
        Interpolation::Bilinear => 0,
        Interpolation::Bicubic => 1,
        Interpolation::Lanczos3 => 2,
    }
}

fn gpu_err(e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("GPU: {e}"))
}

/// A cheap fingerprint of a frame: its size and a sample of its values.
fn fingerprint(f: &Frame, out: &mut Vec<u64>) {
    out.push(f.data.len() as u64);
    let step = (f.data.len() / 61).max(1);
    for v in f.data.iter().step_by(step).take(64) {
        out.push(u64::from(v.to_bits()));
    }
}

impl GpuCompute {
    /// Open a device on the adapter `request` asks for.
    ///
    /// # Errors
    /// [`GpuError`] when no adapter or device is available.
    pub fn new(request: AdapterRequest) -> std::result::Result<Self, GpuError> {
        Ok(Self {
            ctx: GpuContext::new(request)?,
            gate: Mutex::new(()),
            pool: Mutex::new(Pool::default()),
            arith: OnceLock::new(),
            fix: OnceLock::new(),
            warp: OnceLock::new(),
            combine: [OnceLock::new(), OnceLock::new(), OnceLock::new()],
            masters: Mutex::new(None),
            gpu_calls: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            last_fallback: Mutex::new(String::new()),
            phases: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
        })
    }

    /// Take the device for one call; what the call takes from the pool goes back
    /// when the guard drops.
    fn op(&self) -> Op<'_> {
        Op {
            gpu: self,
            _gate: self.gate.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// A pooled buffer of `bytes` bytes with `usage`; its contents are whatever
    /// it last held.
    fn take(&self, bytes: u64, usage: BufferUsages) -> Buffer {
        let bytes = bytes.next_multiple_of(4).max(4);
        let mut p = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = p
            .free
            .iter()
            .position(|(s, u, _)| *s == bytes && *u == usage)
        {
            let e = p.free.swap_remove(i);
            let b = e.2.clone();
            p.used.push(e);
            return b;
        }
        let b = self.ctx.device().create_buffer(&BufferDescriptor {
            label: Some("valerian"),
            size: bytes,
            usage,
            mapped_at_creation: false,
        });
        p.used.push((bytes, usage, b.clone()));
        b
    }

    /// Put the buffers a call took back, keeping the pool under its limit.
    fn release(&self) {
        let mut p = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        let used = std::mem::take(&mut p.used);
        p.free.extend(used);
        let mut total: u64 = p.free.iter().map(|e| e.0).sum();
        while total > POOL_BYTES {
            let Some(i) = (0..p.free.len()).max_by_key(|&i| p.free[i].0) else {
                break;
            };
            total -= p.free.swap_remove(i).0;
        }
    }

    /// A buffer holding `data`.
    fn up(&self, _label: &str, data: &[u8]) -> Buffer {
        let _t = self.phase(0);
        let b = self.take(data.len() as u64, STORAGE);
        if !data.is_empty() {
            self.ctx.queue().write_buffer(&b, 0, data);
        }
        b
    }

    /// A buffer of `bytes` bytes the kernel will write every word of.
    fn scratch(&self, bytes: u64) -> Buffer {
        let _t = self.phase(0);
        self.take(bytes, STORAGE)
    }

    /// A buffer of `bytes` zero bytes.
    fn zeroed(&self, bytes: u64) -> Buffer {
        let _t = self.phase(0);
        let b = self.take(bytes, STORAGE);
        let mut enc = self
            .ctx
            .device()
            .create_command_encoder(&helia_gpu::wgpu::CommandEncoderDescriptor::default());
        enc.clear_buffer(&b, 0, None);
        self.ctx.queue().submit(Some(enc.finish()));
        b
    }

    fn uni(&self, data: &[u8]) -> Buffer {
        let _t = self.phase(0);
        uniform_from(self.ctx.device(), data)
    }

    fn launch(&self, kernel: &Kernel, bind: &helia_gpu::wgpu::BindGroup, groups: (u32, u32)) {
        let _t = self.phase(1);
        kernel.run(self.ctx.device(), self.ctx.queue(), bind, groups);
    }

    /// `count` values of a plain-data type read back from `src`.
    fn rd_pod<T: Pod>(&self, src: &Buffer, count: usize) -> Result<Vec<T>> {
        let _t = self.phase(2);
        let staging = self.take((count * std::mem::size_of::<T>()) as u64, STAGING);
        read_pod(self.ctx.device(), self.ctx.queue(), src, &staging, count).map_err(gpu_err)
    }

    /// `name (backend, device type)` of the adapter.
    #[must_use]
    pub fn summary(&self) -> String {
        self.ctx.summary()
    }

    /// True for a CPU-hosted adapter (lavapipe, WARP).
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.ctx.is_software()
    }

    /// What this engine has done so far.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            gpu_calls: self.gpu_calls.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
            last_fallback: self
                .last_fallback
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            upload: self.phases[0].load(Ordering::Relaxed) as f64 * 1e-9,
            kernel: self.phases[1].load(Ordering::Relaxed) as f64 * 1e-9,
            readback: self.phases[2].load(Ordering::Relaxed) as f64 * 1e-9,
        }
    }

    /// Start timing a phase: `0` upload, `1` kernel, `2` read-back.
    fn phase(&self, which: usize) -> Phase<'_> {
        Phase {
            slot: &self.phases[which],
            start: std::time::Instant::now(),
        }
    }

    fn fell_back(&self, why: &str) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
        self.last_fallback
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(&why.to_owned());
    }

    fn max_binding(&self) -> u64 {
        let l = self.ctx.device().limits();
        l.max_storage_buffer_binding_size.min(l.max_buffer_size)
    }

    fn fits(&self, bytes: usize) -> bool {
        (bytes as u64) <= self.max_binding()
    }

    fn masters_for(&self, c: &Calibrator) -> Masters {
        let dev = self.ctx.device();
        let mut key = Vec::new();
        let mut planes = Vec::new();
        let n = c
            .bias
            .as_ref()
            .or(c.dark.as_ref())
            .or(c.flat.as_ref())
            .map_or(0, |m| m.planes.len());
        for p in 0..n {
            let up = |m: &Option<Image>, key: &mut Vec<u64>| {
                m.as_ref().map(|m| {
                    fingerprint(&m.planes[p], key);
                    storage_from(dev, "master", bytes_of(&m.planes[p]))
                })
            };
            let bias = up(&c.bias, &mut key);
            let dark = up(&c.dark, &mut key);
            let flat = up(&c.flat, &mut key);
            let flags = c.pixel_map.as_ref().map(|m| {
                key.push(m.flags.len() as u64);
                key.extend(
                    m.flags
                        .iter()
                        .step_by((m.flags.len() / 61).max(1))
                        .take(64)
                        .map(|&b| u64::from(b)),
                );
                let mut f = m.flags.clone();
                f.resize(f.len().next_multiple_of(4), 0);
                storage_from(dev, "flags", &f)
            });
            planes.push(MasterPlane {
                bias,
                dark,
                flat,
                flags,
            });
        }
        Masters { key, planes }
    }

    fn calibrate_gpu(&self, c: &Calibrator, light: &Image) -> Result<(Image, Report)> {
        let k = c.dark_multiplier(light)?;
        let _op = self.op();
        let dev = self.ctx.device();
        let arith = self
            .arith
            .get_or_init(|| Kernel::new(dev, "calibrate arithmetic", CALIBRATE_ARITH));
        let fix = self
            .fix
            .get_or_init(|| Kernel::new(dev, "calibrate correct", CALIBRATE_FIX));
        // The masters stay on the device while the same ones are used.
        let mut probe = Vec::new();
        for m in [&c.bias, &c.dark, &c.flat].into_iter().flatten() {
            for p in &m.planes {
                fingerprint(p, &mut probe);
            }
        }
        if let Some(m) = &c.pixel_map {
            probe.push(m.flags.len() as u64);
            probe.extend(
                m.flags
                    .iter()
                    .step_by((m.flags.len() / 61).max(1))
                    .take(64)
                    .map(|&b| u64::from(b)),
            );
        }
        let mut guard = self.masters.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.as_ref().is_none_or(|m| m.key != probe) {
            *guard = Some(self.masters_for(c));
        }
        let masters = guard.as_ref().ok_or_else(|| gpu_err("no masters"))?;
        let mut out = light.clone();
        let mut repaired = 0usize;
        let dummy = self.up("none", &[]);
        for (p, plane) in out.planes.iter_mut().enumerate() {
            let count = plane.data.len();
            let m = masters.planes.get(p);
            let b = m.and_then(|m| m.bias.as_ref());
            let d = m.and_then(|m| m.dark.as_ref());
            let f = m.and_then(|m| m.flat.as_ref());
            let src = self.up("light", bytes_of(plane));
            let a_out = self.scratch((count * 4) as u64);
            let params = ArithParams {
                count: u32::try_from(count).map_err(|_| gpu_err("frame too large"))?,
                flags: u32::from(b.is_some())
                    | (u32::from(d.is_some()) << 1)
                    | (u32::from(f.is_some()) << 2),
                k: k as f32,
                floor: c.flat_floor,
            };
            let pb = self.uni(bytemuck::bytes_of(&params));
            let bind = arith.bind(
                dev,
                &[
                    &pb,
                    &src,
                    b.unwrap_or(&dummy),
                    d.unwrap_or(&dummy),
                    f.unwrap_or(&dummy),
                    &a_out,
                ],
            );
            self.launch(arith, &bind, groups_1d(count, 256));
            let result = if let (Some(map), Some(flags)) =
                (&c.pixel_map, m.and_then(|m| m.flags.as_ref()))
            {
                let fp = FixParams {
                    width: u32::try_from(map.width).map_err(|_| gpu_err("frame too wide"))?,
                    height: u32::try_from(map.height).map_err(|_| gpu_err("frame too tall"))?,
                    step: u32::try_from(map.step).map_err(|_| gpu_err("bad step"))?,
                    count: params.count,
                };
                let fpb = self.uni(bytemuck::bytes_of(&fp));
                let f_out = self.scratch((count * 4) as u64);
                let counter = self.zeroed(4);
                let bind = fix.bind(dev, &[&fpb, &a_out, flags, &f_out, &counter]);
                self.launch(fix, &bind, groups_1d(count, 256));
                let n: Vec<u32> = self.rd_pod(&counter, 1)?;
                repaired += n.first().copied().unwrap_or(0) as usize;
                f_out
            } else {
                a_out
            };
            plane.data = self.rd_pod(&result, count)?;
        }
        drop(guard);
        c.annotate(&mut out, k, repaired);
        Ok((
            out,
            Report {
                dark_scale: k,
                repaired,
            },
        ))
    }

    fn warp_gpu(
        &self,
        src: &Frame,
        to_reference: &Transform,
        width: usize,
        height: usize,
        interp: Interpolation,
    ) -> Result<Frame> {
        let inv = to_reference
            .inverse()
            .ok_or_else(|| Error::invalid("the transform cannot be inverted"))?;
        let _op = self.op();
        let dev = self.ctx.device();
        let kernel = self.warp.get_or_init(|| Kernel::new(dev, "warp", WARP));
        let row = |r: usize| {
            [
                inv.m[r][0] as f32,
                inv.m[r][1] as f32,
                inv.m[r][2] as f32,
                0.0,
            ]
        };
        let params = WarpParams {
            src_w: u32::try_from(src.width).map_err(|_| gpu_err("frame too wide"))?,
            src_h: u32::try_from(src.height).map_err(|_| gpu_err("frame too tall"))?,
            out_w: u32::try_from(width).map_err(|_| gpu_err("output too wide"))?,
            rows: u32::try_from(height).map_err(|_| gpu_err("output too tall"))?,
            row0: 0,
            out_off: 0,
            interp: interp_code(interp),
            flags: 0,
            level: [0.0; 4],
            r0: row(0),
            r1: row(1),
            r2: row(2),
        };
        let pb = self.uni(bytemuck::bytes_of(&params));
        let sbuf = self.up("source", bytes_of(src));
        let obuf = self.scratch((width * height * 4) as u64);
        let bind = kernel.bind(dev, &[&pb, &sbuf, &obuf]);
        self.launch(
            kernel,
            &bind,
            (
                u32::try_from(width.div_ceil(16)).map_err(|_| gpu_err("output too wide"))?,
                u32::try_from(height.div_ceil(16)).map_err(|_| gpu_err("output too tall"))?,
            ),
        );
        Ok(Frame {
            width,
            height,
            data: self.rd_pod(&obuf, width * height)?,
            bayer: None,
        })
    }

    /// Why a fused stack cannot be used for `sp`, if it cannot; an empty reason
    /// means the input is bad and the CPU stages will say how.
    fn fused_reason(&self, sp: &StackPlane<'_>, weights: &[f32]) -> Option<String> {
        let (n, w) = (sp.frames.len(), sp.size.0);
        if n == 0
            || sp.transforms.len() != n
            || sp.levels.len() != n
            || weights.len() != n
            || weights.iter().any(|x| !(x.is_finite() && *x > 0.0))
        {
            return Some(String::new());
        }
        if n > MAX_FRAMES {
            return Some(format!(
                "{n} frames is more than the {MAX_FRAMES} the kernel holds"
            ));
        }
        if sp.frames.iter().any(|f| !self.fits(f.data.len() * 4)) || !self.fits(w * n * 4) {
            return Some("a frame is bigger than the device allows in one buffer".to_owned());
        }
        let sources: usize = sp.frames.iter().map(|f| f.data.len() * 4).sum();
        (sources > SOURCE_BYTES)
            .then(|| "the frames do not fit the device memory budget".to_owned())
    }

    /// The warp parameters for frame `f` of `sp`, filling rows `y0..y0 + band` of
    /// its slot in the many-frame buffer.
    fn warp_params(
        sp: &StackPlane<'_>,
        f: usize,
        (y0, band): (usize, usize),
    ) -> Result<WarpParams> {
        let (w, npx) = (sp.size.0, band * sp.size.0);
        let mut flags = 0u32;
        let mut level = [0.0f32; 4];
        let inv = match &sp.transforms[f] {
            None => {
                flags |= 1;
                Transform::identity()
            }
            Some(t) => t
                .inverse()
                .ok_or_else(|| Error::invalid("the transform cannot be inverted"))?,
        };
        if let Some((sky, k)) = sp.levels[f] {
            flags |= 2;
            level = [sky, k, sp.ref_sky, 0.0];
        }
        let row = |r: usize| {
            [
                inv.m[r][0] as f32,
                inv.m[r][1] as f32,
                inv.m[r][2] as f32,
                0.0,
            ]
        };
        Ok(WarpParams {
            src_w: u32::try_from(sp.frames[f].width).map_err(|_| gpu_err("frame too wide"))?,
            src_h: u32::try_from(sp.frames[f].height).map_err(|_| gpu_err("frame too tall"))?,
            out_w: u32::try_from(w).map_err(|_| gpu_err("output too wide"))?,
            rows: u32::try_from(band).map_err(|_| gpu_err("band too tall"))?,
            row0: u32::try_from(y0).map_err(|_| gpu_err("output too tall"))?,
            out_off: u32::try_from(f * npx).map_err(|_| gpu_err("band too large"))?,
            interp: interp_code(sp.interp),
            flags,
            level,
            r0: row(0),
            r1: row(1),
            r2: row(2),
        })
    }

    /// Run the combine kernel over the `n` frames of `input` (`npx` pixels each)
    /// and read the mean, the counts and the counters back.
    fn combine_band(
        &self,
        kernel: &Kernel,
        input: &Buffer,
        weights: &Buffer,
        params: &CombineParams,
    ) -> Result<(Vec<f32>, Vec<u32>, Vec<u32>)> {
        let (n, npx) = (params.n as usize, params.npx as usize);
        let pb = self.uni(bytemuck::bytes_of(params));
        let mbuf = self.scratch((npx * 4) as u64);
        let ubuf = self.scratch((npx * 4) as u64);
        let cbuf = self.zeroed((3 * n * 4) as u64);
        let bind = kernel.bind(
            self.ctx.device(),
            &[&pb, input, weights, &mbuf, &ubuf, &cbuf],
        );
        self.launch(kernel, &bind, groups_1d(npx, 64));
        Ok((
            self.rd_pod(&mbuf, npx)?,
            self.rd_pod(&ubuf, npx)?,
            self.rd_pod(&cbuf, 3 * n)?,
        ))
    }

    /// The combine kernel for `n` frames.
    fn combine_kernel(&self, n: usize) -> &Kernel {
        let (cap, maxn) = combine_capacity(n);
        self.combine[cap].get_or_init(|| {
            Kernel::new(
                self.ctx.device(),
                "combine",
                &COMBINE.replace("__MAXN__", &maxn.to_string()),
            )
        })
    }

    /// The warp, level match and combine of one plane with the frames kept on
    /// the device: no frame crosses the bus more than once, and only the mean, the
    /// counts and the counters come back. `None` when the case is not covered
    /// (the reason is recorded), and the caller then runs the stages one by one.
    fn stack_plane_gpu(&self, sp: &StackPlane<'_>) -> Result<Option<Combined>> {
        let n = sp.frames.len();
        let (w, h) = sp.size;
        let weights: Vec<f32> = sp.combine.weights.clone().unwrap_or_else(|| vec![1.0; n]);
        if let Some(why) = self.fused_reason(sp, &weights) {
            if !why.is_empty() {
                self.fell_back(&why);
            }
            return Ok(None);
        }
        let _op = self.op();
        let dev = self.ctx.device();
        let warp = self.warp.get_or_init(|| Kernel::new(dev, "warp", WARP));
        let comb = self.combine_kernel(n);
        let (kind, low, high, low_n, high_n) = rejection_params(sp.combine.rejection);
        let src: Vec<Buffer> = sp
            .frames
            .iter()
            .map(|f| self.up("frame", bytes_of(f)))
            .collect();
        let budget = BAND_BYTES.min(usize::try_from(self.max_binding()).unwrap_or(usize::MAX));
        let rows = (budget / (n * w * 4)).clamp(1, h);
        let warped = self.scratch((n * rows * w * 4) as u64);
        let wbuf = self.up("weights", bytemuck::cast_slice(&weights));
        let mut acc = Accumulator::new(w * h, n);
        let mut y0 = 0;
        while y0 < h {
            let band = (y0 + rows).min(h) - y0;
            for (f, source) in src.iter().enumerate() {
                let pb = self.uni(bytemuck::bytes_of(&Self::warp_params(sp, f, (y0, band))?));
                let bind = warp.bind(dev, &[&pb, source, &warped]);
                self.launch(
                    warp,
                    &bind,
                    (
                        u32::try_from(w.div_ceil(16)).map_err(|_| gpu_err("output too wide"))?,
                        u32::try_from(band.div_ceil(16)).map_err(|_| gpu_err("band too tall"))?,
                    ),
                );
            }
            let params = CombineParams {
                n: n as u32,
                npx: u32::try_from(band * w).map_err(|_| gpu_err("band too large"))?,
                kind,
                max_iter: u32::try_from(sp.combine.max_iterations).unwrap_or(10),
                low,
                high,
                low_n,
                high_n,
            };
            let (m, u, c) = self.combine_band(comb, &warped, &wbuf, &params)?;
            acc.push(&m, &u, &c);
            y0 += band;
        }
        acc.finish(sp.size, sp.frames[0].bayer).map(Some)
    }

    fn combine_gpu(
        &self,
        frames: &[&Frame],
        opts: &combine::Options,
        weights: &[f32],
    ) -> Result<Combined> {
        let n = frames.len();
        let (w, h) = (frames[0].width, frames[0].height);
        let _op = self.op();
        let kernel = self.combine_kernel(n);
        let (kind, low, high, low_n, high_n) = rejection_params(opts.rejection);
        let budget = BAND_BYTES.min(usize::try_from(self.max_binding()).unwrap_or(usize::MAX));
        let rows = (budget / (n * w * 4)).clamp(1, h);
        let wbuf = self.up("weights", bytemuck::cast_slice(weights));
        let mut acc = Accumulator::new(w * h, n);
        let mut y0 = 0;
        while y0 < h {
            let y1 = (y0 + rows).min(h);
            let npx = (y1 - y0) * w;
            let fbuf = self.scratch((n * npx * 4) as u64);
            {
                // Each frame's rows go straight to their place in the buffer.
                let _t = self.phase(0);
                for (k, f) in frames.iter().enumerate() {
                    self.ctx.queue().write_buffer(
                        &fbuf,
                        (k * npx * 4) as u64,
                        bytemuck::cast_slice(&f.data[y0 * w..y1 * w]),
                    );
                }
            }
            let params = CombineParams {
                n: n as u32,
                npx: u32::try_from(npx).map_err(|_| gpu_err("band too large"))?,
                kind,
                max_iter: u32::try_from(opts.max_iterations).unwrap_or(10),
                low,
                high,
                low_n,
                high_n,
            };
            let (m, u, c) = self.combine_band(kernel, &fbuf, &wbuf, &params)?;
            acc.push(&m, &u, &c);
            y0 = y1;
        }
        acc.finish((w, h), frames[0].bayer)
    }
}

/// The bands of a combine put back together.
struct Accumulator {
    mean: Vec<f32>,
    used: Vec<u16>,
    counters: Vec<u64>,
    n: usize,
}

impl Accumulator {
    fn new(pixels: usize, n: usize) -> Self {
        Self {
            mean: Vec::with_capacity(pixels),
            used: Vec::with_capacity(pixels),
            counters: vec![0; 3 * n],
            n,
        }
    }

    fn push(&mut self, mean: &[f32], used: &[u32], counters: &[u32]) {
        self.mean.extend_from_slice(mean);
        self.used.extend(used.iter().map(|&v| v as u16));
        for (a, b) in self.counters.iter_mut().zip(counters) {
            *a += u64::from(*b);
        }
    }

    fn finish(
        self,
        (w, h): (usize, usize),
        bayer: Option<valerian_core::BayerPattern>,
    ) -> Result<Combined> {
        let n = self.n;
        let mut frame = Frame::new(w, h, self.mean).map_err(|e| Error::invalid(e.to_string()))?;
        frame.bayer = bayer;
        Ok(Combined {
            mean: frame,
            used: self.used,
            valid: self.counters[..n].to_vec(),
            rejected_low: self.counters[n..2 * n].to_vec(),
            rejected_high: self.counters[2 * n..].to_vec(),
            maps: None,
        })
    }
}

impl Compute for GpuCompute {
    fn name(&self) -> String {
        format!("GPU: {}", self.ctx.summary())
    }

    fn calibrate(&self, calibrator: &Calibrator, light: &Image) -> Result<(Image, Report)> {
        // Shapes and dark scaling are checked by the CPU code either way.
        calibrator.check_shapes(light)?;
        let largest = light
            .planes
            .iter()
            .map(|p| p.data.len() * 4)
            .max()
            .unwrap_or(0);
        if !self.fits(largest) || light.planes.is_empty() {
            self.fell_back("a frame is bigger than the device allows in one buffer");
            return Cpu.calibrate(calibrator, light);
        }
        let r = self.calibrate_gpu(calibrator, light)?;
        self.gpu_calls.fetch_add(1, Ordering::Relaxed);
        Ok(r)
    }

    fn warp(
        &self,
        src: &Frame,
        to_reference: &Transform,
        width: usize,
        height: usize,
        interp: Interpolation,
    ) -> Result<Frame> {
        if !self.fits(src.data.len() * 4) || !self.fits(width * height * 4) {
            self.fell_back("a frame is bigger than the device allows in one buffer");
            return Cpu.warp(src, to_reference, width, height, interp);
        }
        let r = self.warp_gpu(src, to_reference, width, height, interp)?;
        self.gpu_calls.fetch_add(1, Ordering::Relaxed);
        Ok(r)
    }

    fn combine(&self, frames: &[&Frame], opts: &combine::Options) -> Result<Combined> {
        let n = frames.len();
        let same_size = frames.first().is_some_and(|f| {
            frames
                .iter()
                .all(|g| g.width == f.width && g.height == f.height)
        });
        let weights: Option<Vec<f32>> = match &opts.weights {
            Some(v) if v.len() == n && v.iter().all(|x| x.is_finite() && *x > 0.0) => {
                Some(v.clone())
            }
            Some(_) => None,
            None => Some(vec![1.0; n]),
        };
        // Bad input gets its error, in its words, from the CPU code.
        let (Some(weights), true) = (weights, same_size) else {
            return Cpu.combine(frames, opts);
        };
        if n > MAX_FRAMES {
            self.fell_back(&format!(
                "{n} frames is more than the {MAX_FRAMES} the kernel holds"
            ));
            return Cpu.combine(frames, opts);
        }
        if frames[0].width * n * 4 > usize::try_from(self.max_binding()).unwrap_or(usize::MAX) {
            self.fell_back("one row of the frames is bigger than the device allows");
            return Cpu.combine(frames, opts);
        }
        let r = self.combine_gpu(frames, opts, &weights)?;
        self.gpu_calls.fetch_add(1, Ordering::Relaxed);
        Ok(r)
    }

    fn stack_plane(&self, plane: &StackPlane<'_>) -> Result<Combined> {
        match self.stack_plane_gpu(plane)? {
            Some(c) => {
                self.gpu_calls.fetch_add(1, Ordering::Relaxed);
                Ok(c)
            }
            None => stack_plane_classic(self, plane),
        }
    }
}
