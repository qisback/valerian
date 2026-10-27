// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Stacking a session without holding every frame in memory.
//!
//! [`crate::stack::stack_with`] needs every frame resident, so a colour
//! session costs about 47 MB per frame and 1,349 frames of the S50 take about
//! 63 GB. This module gives the same stack with a working set set by a byte
//! budget, not by the number of frames. It does so in three passes over a
//! [`FrameSource`], which loads one frame at a time:
//!
//! 1. **Measure.** Each frame is loaded, its stars, sky and noise measured, and
//!    the pixels dropped. The detections are a few kilobytes per frame. The
//!    reference is chosen and every frame registered from them alone
//!    (`register_detections`).
//! 2. **Resample.** Each kept frame is loaded again, warped onto the reference
//!    grid and level-matched exactly as the in-memory stacker does, and the
//!    result is written to a staging file: frame by frame, plane by plane, row
//!    by row, as 32-bit floats. When the whole set fits the budget the staging
//!    stays in memory and no file is made.
//! 3. **Combine in bands.** Rejection and the weighted mean are per pixel: the
//!    samples at one position never depend on another position. So a band of
//!    rows is read back from every frame, combined with the usual rejection
//!    ([`crate::combine`]) and appended to the result, and the band is
//!    dropped. The number of rows in a band follows from the budget and the
//!    number of frames.
//!
//! Nothing here approximates anything. The rejection sees the same samples in
//! the same order, so the stack, the coverage, the per-frame rejected
//! fractions and the maps are bit for bit those of `stack_with`; the tests
//! check that. The cost is a second read and demosaic of every kept frame and
//! the staging file: `frames x width x height x planes x 4` bytes of scratch
//! disk.
//!
//! The method is the one [`crate::streaming`] uses for calibration masters:
//! because the statistics are per pixel, combining a band of rows at a time is
//! exact, and only the rows of a band need to be resident.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::combine::{Options, Rejection};
use crate::compute::Compute;
use crate::error::{Error, Result};
use crate::fits::Image;
use crate::stack::{
    Normalisation, StackMaps, StackOptions, Stacked, frame_weights, plane_sky, register_detections,
    stack_header_of,
};
use crate::stars::{Detection, detect, luminance};
use helia_core::Frame;

/// Where the frames of a session come from, one at a time.
pub trait FrameSource: Sync {
    /// Number of frames.
    fn len(&self) -> usize;

    /// Whether there are no frames.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Frame `i`, demosaiced and ready to measure and resample.
    ///
    /// # Errors
    /// If the frame cannot be read.
    fn load(&self, i: usize) -> Result<Image>;
}

/// Memory and disk settings of [`stack_streaming`].
#[derive(Clone, Debug)]
pub struct StreamOptions {
    /// Working set in bytes. The staged frames stay in memory when they fit;
    /// otherwise a band of rows of this size is combined at a time.
    pub budget: usize,
    /// Directory for the staging file; the system temporary directory when `None`.
    pub scratch: Option<PathBuf>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            budget: 4 << 30,
            scratch: None,
        }
    }
}

/// How many frames are loaded at once.
fn chunk_size() -> usize {
    rayon::current_num_threads().clamp(1, 8)
}

/// Rows of every frame that fit `budget` when `frames` frames of `width` pixels
/// are held: at least one, at most `height`.
#[must_use]
pub fn band_rows(frames: usize, width: usize, height: usize, budget: usize) -> usize {
    let per_row = frames.max(1) * width.max(1) * 4;
    (budget / per_row).clamp(1, height.max(1))
}

/// What pass 1 keeps of a frame.
struct Measured {
    detection: Detection,
    plane_sky: Vec<f32>,
    exposure: Option<f64>,
    planes: usize,
    bayer: bool,
    size: (usize, usize),
    /// Block-averaged planes, kept in a mosaic run to match levels.
    low: Vec<Frame>,
}

/// The staged, resampled frames: in memory, or in a file removed on drop.
enum Staging {
    Memory(Vec<Frame>),
    Disk { path: PathBuf },
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Self::Disk { path } = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn io_err(what: &str, path: &Path, e: &std::io::Error) -> Error {
    Error::invalid(format!(
        "{what} {}: {e}. The staging file needs frames x width x height x planes x 4 bytes; \
         point --scratch-dir at a disk with room",
        path.display()
    ))
}

fn scratch_path(dir: Option<&Path>) -> PathBuf {
    let dir = dir.map_or_else(std::env::temp_dir, Path::to_path_buf);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    dir.join(format!(
        "valerian-stack-{}-{nanos}.staging",
        std::process::id()
    ))
}

fn write_floats(w: &mut impl Write, data: &[f32]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(data.len().min(1 << 16) * 4);
    for chunk in data.chunks(1 << 16) {
        buf.clear();
        for v in chunk {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        w.write_all(&buf)?;
    }
    Ok(())
}

/// Rows `y0..y1` of plane `p` of staged frame `k`, from the staging file.
fn read_band(
    file: &mut File,
    path: &Path,
    layout: (usize, usize, usize),
    k: usize,
    p: usize,
    (y0, y1): (usize, usize),
) -> Result<Frame> {
    let (w, h, np) = layout;
    let offset = (((k * np + p) * h + y0) * w * 4) as u64;
    let n = (y1 - y0) * w;
    let mut bytes = vec![0_u8; n * 4];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|e| io_err("cannot read", path, &e))?;
    #[allow(
        clippy::chunks_exact_to_as_chunks,
        reason = "as_chunks needs a newer compiler than the supported one"
    )]
    let data = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(Frame {
        width: w,
        height: y1 - y0,
        data,
        bayer: None,
    })
}

/// Running totals of one plane's bands.
struct PlaneTotals {
    mean: Vec<f32>,
    used: Vec<u16>,
    low: Vec<u64>,
    high: Vec<u64>,
    valid: Vec<u64>,
}

fn add_band_maps(
    maps: &mut Option<StackMaps>,
    m: &crate::combine::Maps,
    first_plane: bool,
    (w, h, y0): (usize, usize, usize),
    rows: usize,
) {
    let sum = maps.get_or_insert_with(|| StackMaps {
        low: Frame::zeros(w, h),
        high: Frame::zeros(w, h),
        weight: Frame::zeros(w, h),
    });
    let at = y0 * w;
    for (a, b) in sum.low.data[at..at + rows * w].iter_mut().zip(&m.low) {
        *a += f32::from(*b);
    }
    for (a, b) in sum.high.data[at..at + rows * w].iter_mut().zip(&m.high) {
        *a += f32::from(*b);
    }
    if first_plane {
        sum.weight.data[at..at + rows * w].copy_from_slice(&m.weight);
    }
}

/// Register and stack the frames of `source` with a working set of
/// `stream.budget` bytes. The result is that of
/// [`crate::stack::stack_with`] on the same frames.
///
/// # Errors
/// As for [`crate::stack::stack_with`], and [`Error::Invalid`] when the
/// staging file cannot be written or read.
#[allow(clippy::too_many_lines, reason = "three passes that share their state")]
pub fn stack_streaming(
    source: &dyn FrameSource,
    opts: &StackOptions,
    stream: &StreamOptions,
    compute: &dyn Compute,
    log: &mut dyn FnMut(&str),
) -> Result<Stacked> {
    let n = source.len();
    if n == 0 {
        return Err(Error::invalid("no frames to stack"));
    }
    let chunk = chunk_size();

    // Pass 1: measure every frame, keep only the measurements.
    let mut measured: Vec<Measured> = Vec::with_capacity(n);
    for start in (0..n).step_by(chunk) {
        opts.cancel.check()?;
        let end = (start + chunk).min(n);
        let batch: Vec<Measured> = (start..end)
            .into_par_iter()
            .map(|i| {
                let img = source.load(i)?;
                let det = detect(&luminance(&img.planes), &opts.detect);
                Ok(Measured {
                    detection: det,
                    plane_sky: img.planes.iter().map(plane_sky).collect(),
                    exposure: img.header.exposure(),
                    planes: img.planes.len(),
                    bayer: img.planes.iter().any(|p| p.bayer.is_some()),
                    size: (img.width(), img.height()),
                    low: if opts.mosaic.is_some_and(|m| m.match_levels) {
                        crate::mosaic::bin_planes(&img.planes, crate::mosaic::LEVEL_BIN)
                    } else {
                        Vec::new()
                    },
                })
            })
            .collect::<Result<_>>()?;
        for (i, m) in (start..end).zip(&batch) {
            if m.bayer {
                return Err(Error::invalid(format!(
                    "frame {i} is a raw Bayer mosaic; demosaic it before stacking"
                )));
            }
            if m.planes != measured.first().map_or(m.planes, |f| f.planes) {
                return Err(Error::invalid(format!(
                    "frame {i} has {} planes, the first has {}",
                    m.planes,
                    measured.first().map_or(m.planes, |f| f.planes)
                )));
            }
            let d = &m.detection;
            log(&format!(
                "frame {i}: {} stars, FWHM {:.2} px, ellipticity {:.3}, sky {:.1}, noise {:.2}",
                d.stars.len(),
                d.median_fwhm(),
                d.median_ellipticity(),
                d.sky,
                d.noise
            ));
        }
        measured.extend(batch);
    }
    let dets: Vec<Detection> = measured.iter().map(|m| m.detection.clone()).collect();
    let crate::stack::Registered {
        regs,
        keep,
        mut reports,
        reference,
    } = match opts.mosaic {
        Some(_) => {
            let sizes: Vec<(usize, usize)> = measured.iter().map(|m| m.size).collect();
            crate::mosaic::register_mosaic(&dets, &sizes, opts, log)?
        }
        None => register_detections(&dets, opts, log)?,
    };
    drop(dets);
    let weights = frame_weights(&keep, &reports, opts.weights);
    for (&i, &w) in keep.iter().zip(&weights) {
        reports[i].weight = f64::from(w);
    }
    let rejection = opts
        .rejection
        .unwrap_or_else(|| Rejection::auto(keep.len()));

    // Pass 2: resample each kept frame onto the reference grid and stage it.
    let np = measured[reference].planes;
    let normalise = opts.normalisation != Normalisation::None;
    let ref_sky = measured[reference].plane_sky.clone();
    let ref_img = source.load(reference)?;
    let (mut w, mut h) = (ref_img.width(), ref_img.height());
    let ref_header = ref_img.header.clone();
    // A mosaic grows the grid to hold every pane and stages a feather-weight
    // plane after the colour planes of each frame.
    let mut canvas = None;
    let mut to_canvas: Vec<Option<crate::transform::Transform>> = vec![None; n];
    if let Some(m) = opts.mosaic {
        let placed: Vec<(crate::transform::Transform, (usize, usize))> = keep
            .iter()
            .map(|&i| {
                (
                    regs[i]
                        .as_ref()
                        .map_or_else(crate::transform::Transform::identity, |r| r.transform),
                    measured[i].size,
                )
            })
            .collect();
        let c = crate::mosaic::canvas_for(&placed)?;
        for (&i, (t, _)) in keep.iter().zip(&placed) {
            to_canvas[i] = Some(c.shift().after(t));
        }
        log(&format!(
            "mosaic canvas {} x {} pixels ({} x {} for one frame), feather {} px",
            c.width, c.height, w, h, m.feather
        ));
        (w, h) = (c.width, c.height);
        canvas = Some((c, m));
    }
    let npl = np + usize::from(canvas.is_some());
    // Additive level of each frame and plane from the overlaps (empty when off).
    let mut offsets: Vec<Vec<f32>> = vec![Vec::new(); n];
    if let Some((_, m)) = canvas.as_ref().filter(|(_, m)| m.match_levels) {
        let b = crate::mosaic::LEVEL_BIN;
        let (lw, lh) = (w.div_ceil(b), h.div_ceil(b));
        let pre = crate::transform::Transform::similarity(b as f64, 0.0, 3.5, 3.5);
        let post = crate::transform::Transform::similarity(
            1.0 / b as f64,
            0.0,
            -3.5 / b as f64,
            -3.5 / b as f64,
        );
        let lows: Vec<crate::mosaic::LowFrame> = keep
            .iter()
            .map(|&i| {
                let to_c = to_canvas[i].unwrap_or_else(crate::transform::Transform::identity);
                let t = post.after(&to_c.after(&pre));
                let mut planes = Vec::new();
                for (p, f) in measured[i].low.iter().enumerate() {
                    let mut x =
                        crate::warp::warp(f, &t, lw, lh, crate::warp::Interpolation::Bilinear)?;
                    if normalise && i != reference {
                        let (sky, k) = (measured[i].plane_sky[p], reports[i].flux_scale as f32);
                        for v in &mut x.data {
                            *v = (*v - sky) * k + ref_sky[p];
                        }
                    }
                    planes.push(x);
                }
                let src = &measured[i].low[0];
                let weight = crate::mosaic::feather_weights(
                    &t,
                    (src.width, src.height),
                    (lw, lh),
                    m.feather / b as f32,
                )?;
                Ok(crate::mosaic::LowFrame { planes, weight })
            })
            .collect::<Result<_>>()?;
        let ref_k = keep.iter().position(|&i| i == reference).unwrap_or(0);
        let solved = crate::mosaic::solve_offsets(&lows, ref_k, 200);
        let mut biggest = 0.0_f32;
        for (&i, o) in keep.iter().zip(solved) {
            biggest = o.iter().fold(biggest, |a, v| a.max(v.abs()));
            offsets[i] = o;
        }
        log(&format!(
            "mosaic levels matched: largest correction {biggest:.1} ADU"
        ));
        for m in &mut measured {
            m.low = Vec::new();
        }
    }
    drop(ref_img);
    let total_bytes = keep.len() * npl * w * h * 4;
    let in_memory = total_bytes <= stream.budget;
    log(&format!(
        "combining {} frames with {rejection:?}, weights {:?} on {}, {} ({:.1} GiB staged)",
        keep.len(),
        opts.weights,
        compute.name(),
        if in_memory {
            "staged in memory".to_owned()
        } else {
            format!("streamed in bands, budget {:.1} GiB", gib(stream.budget))
        },
        gib(total_bytes)
    ));
    let mut mem: Vec<Frame> = Vec::new();
    let mut disk: Option<(BufWriter<File>, PathBuf)> = None;
    if !in_memory {
        let path = scratch_path(stream.scratch.as_deref());
        let file = File::create(&path).map_err(|e| io_err("cannot create", &path, &e))?;
        disk = Some((BufWriter::with_capacity(1 << 20, file), path));
    }
    // From here the staging file is removed however this function ends.
    let mut guard = disk
        .as_ref()
        .map(|(_, p)| Staging::Disk { path: p.clone() });
    for batch in keep.chunks(chunk) {
        opts.cancel.check()?;
        let loaded: Vec<Image> = batch
            .par_iter()
            .map(|&i| source.load(i))
            .collect::<Result<_>>()?;
        for (&i, mut img) in batch.iter().zip(loaded) {
            let frame_size = (img.width(), img.height());
            if img.planes.len() != np {
                return Err(Error::invalid(format!(
                    "frame {i} has {} planes, the first has {np}",
                    img.planes.len()
                )));
            }
            for (p, plane) in img.planes.drain(..).enumerate() {
                let out = if let Some(t) = &to_canvas[i] {
                    let sky = measured[i].plane_sky[p];
                    let mut warped = compute.warp(&plane, t, w, h, opts.interpolation)?;
                    if normalise && i != reference {
                        let k = reports[i].flux_scale as f32;
                        for v in &mut warped.data {
                            *v = (*v - sky) * k + ref_sky[p];
                        }
                    }
                    if let Some(&o) = offsets[i].get(p) {
                        for v in &mut warped.data {
                            *v += o;
                        }
                    }
                    warped
                } else if i == reference {
                    plane
                } else {
                    let reg = regs[i]
                        .as_ref()
                        .ok_or_else(|| Error::invalid("a kept frame has no registration"))?;
                    let sky = measured[i].plane_sky[p];
                    let mut warped =
                        compute.warp(&plane, &reg.transform, w, h, opts.interpolation)?;
                    if normalise {
                        let k = reports[i].flux_scale as f32;
                        for v in &mut warped.data {
                            *v = (*v - sky) * k + ref_sky[p];
                        }
                    }
                    warped
                };
                if out.width != w || out.height != h {
                    return Err(Error::invalid("a resampled frame has the wrong size"));
                }
                match &mut disk {
                    Some((wr, path)) => {
                        write_floats(wr, &out.data)
                            .map_err(|e| io_err("cannot write", path, &e))?;
                    }
                    None => mem.push(out),
                }
            }
            if let (Some((_, m)), Some(t)) = (&canvas, &to_canvas[i]) {
                let fw = crate::mosaic::feather_weights(t, frame_size, (w, h), m.feather)?;
                match &mut disk {
                    Some((wr, path)) => {
                        write_floats(wr, &fw.data).map_err(|e| io_err("cannot write", path, &e))?;
                    }
                    None => mem.push(fw),
                }
            }
        }
    }
    if let Some((wr, path)) = &mut disk {
        wr.flush().map_err(|e| io_err("cannot write", path, &e))?;
    }
    drop(disk);

    // Pass 3: combine, plane by plane and band by band.
    let rows = if in_memory {
        h
    } else {
        // Half the budget holds the band; the rest covers loading, the result
        // and the combiner's own working memory.
        band_rows(keep.len() * npl.div_ceil(np), w, h, stream.budget / 2)
    };
    let mut staged = if in_memory {
        Staging::Memory(mem)
    } else {
        guard.take().unwrap_or(Staging::Memory(Vec::new()))
    };
    let mut planes = Vec::with_capacity(np);
    let mut coverage: Option<Frame> = None;
    let mut stack_maps: Option<StackMaps> = None;
    let mut rejected = vec![0.0_f64; keep.len()];
    for p in 0..np {
        let mut tot = PlaneTotals {
            mean: Vec::with_capacity(w * h),
            used: Vec::with_capacity(w * h),
            low: vec![0; keep.len()],
            high: vec![0; keep.len()],
            valid: vec![0; keep.len()],
        };
        let mut y0 = 0;
        while y0 < h {
            opts.cancel.check()?;
            let y1 = (y0 + rows).min(h);
            let band: Vec<Frame> = match &mut staged {
                Staging::Memory(frames) => (0..keep.len())
                    .map(|k| std::mem::replace(&mut frames[k * npl + p], Frame::zeros(0, 0)))
                    .collect(),
                Staging::Disk { path } => {
                    let path = path.clone();
                    (0..keep.len())
                        .into_par_iter()
                        .map_init(
                            || File::open(&path),
                            |f, k| match f {
                                Ok(f) => read_band(f, &path, (w, h, npl), k, p, (y0, y1)),
                                Err(e) => Err(io_err("cannot open", &path, e)),
                            },
                        )
                        .collect::<Result<_>>()?
                }
            };
            let refs: Vec<&Frame> = band.iter().collect();
            let combine_opts = Options {
                rejection,
                weights: Some(weights.clone()),
                maps: opts.maps,
                ..Options::default()
            };
            let c = if canvas.is_some() {
                // The feather weights of this band, one per frame (staged last).
                let pw: Vec<Frame> = match &staged {
                    Staging::Memory(frames) => (0..keep.len())
                        .map(|k| band_of(&frames[k * npl + np], y0, y1))
                        .collect(),
                    Staging::Disk { path } => (0..keep.len())
                        .into_par_iter()
                        .map_init(
                            || File::open(path),
                            |f, k| match f {
                                Ok(f) => read_band(f, path, (w, h, npl), k, np, (y0, y1)),
                                Err(e) => Err(io_err("cannot open", path, e)),
                            },
                        )
                        .collect::<Result<_>>()?,
                };
                let pwr: Vec<&Frame> = pw.iter().collect();
                crate::combine::combine_pixel_weighted(&refs, &pwr, &combine_opts)?
            } else {
                compute.combine(&refs, &combine_opts)?
            };
            drop(refs);
            drop(band);
            tot.mean.extend_from_slice(&c.mean.data);
            tot.used.extend_from_slice(&c.used);
            for k in 0..keep.len() {
                tot.low[k] += c.rejected_low[k];
                tot.high[k] += c.rejected_high[k];
                tot.valid[k] += c.valid[k];
            }
            if let Some(m) = &c.maps {
                add_band_maps(&mut stack_maps, m, p == 0, (w, h, y0), y1 - y0);
            }
            y0 = y1;
        }
        for (k, r) in rejected.iter_mut().enumerate() {
            *r += (tot.low[k] + tot.high[k]) as f64 / tot.valid[k].max(1) as f64 / np as f64;
        }
        if p == 0 {
            coverage = Some(Frame {
                width: w,
                height: h,
                data: tot.used.iter().map(|&u| f32::from(u)).collect(),
                bayer: None,
            });
        }
        planes.push(Frame::new(w, h, tot.mean).map_err(|e| Error::invalid(e.to_string()))?);
    }
    drop(staged);
    let coverage = coverage.ok_or_else(|| Error::invalid("frames have no planes"))?;
    for (&i, &r) in keep.iter().zip(&rejected) {
        reports[i].rejected = r;
    }
    let total: f64 = keep.iter().filter_map(|&i| measured[i].exposure).sum();
    let mut header = stack_header_of(
        &ref_header,
        total,
        n,
        keep.len(),
        reference,
        rejection,
        opts,
    );
    if let Some((c, m)) = &canvas {
        mosaic_header(&mut header, c, m.feather);
    }
    Ok(Stacked {
        image: Image { header, planes },
        coverage,
        frames: reports,
        reference,
        rejection,
        maps: stack_maps,
    })
}

/// Rows `y0..y1` of a staged frame held in memory.
fn band_of(f: &Frame, y0: usize, y1: usize) -> Frame {
    Frame {
        width: f.width,
        height: y1 - y0,
        data: f.data[y0 * f.width..y1 * f.width].to_vec(),
        bayer: None,
    }
}

/// Mark a mosaic in the header and move the world coordinate reference pixel,
/// when the reference header has one, to the canvas.
fn mosaic_header(header: &mut crate::fits::Header, c: &crate::mosaic::Canvas, feather: f32) {
    use crate::fits::Value;
    header.set(
        "MOSAIC",
        Value::Bool(true),
        "panes registered onto one canvas",
    );
    header.set(
        "MOSFEATH",
        Value::Float(f64::from(feather)),
        "[px] seam feather width",
    );
    for (key, shift) in [("CRPIX1", c.x0), ("CRPIX2", c.y0)] {
        if let Some(v) = header.get_f64(key) {
            header.set(key, Value::Float(v - shift as f64), "");
        }
    }
    header.add_history(&format!(
        "mosaic canvas {} x {}, origin ({}, {}) in reference pixels",
        c.width, c.height, c.x0, c.y0
    ));
}

fn gib(bytes: usize) -> f64 {
    bytes as f64 / f64::from(1_u32 << 30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::Cpu;
    use crate::fits::Value;
    use crate::stack::stack_with;
    use crate::testutil::{Rng, Scene};
    use crate::transform::Transform;

    struct VecSource(Vec<Image>);

    impl FrameSource for VecSource {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn load(&self, i: usize) -> Result<Image> {
            Ok(self.0[i].clone())
        }
    }

    fn session(n: usize, planes: usize, seed: u64) -> Vec<Image> {
        let scene = Scene::random(100, 400, 300, seed);
        let mut rng = Rng::new(seed + 1);
        (0..n)
            .map(|k| {
                let view = Transform::rotation_about(
                    scene.width as f64 / 2.0,
                    scene.height as f64 / 2.0,
                    (rng.uniform() - 0.5) * 0.01,
                    (rng.uniform() - 0.5) * 30.0,
                    (rng.uniform() - 0.5) * 30.0,
                );
                let f = scene.render(&view, 500.0, 8.0, 1.0, 0.0, seed * 100 + k as u64);
                let mut img = Image::mono(f.clone(), crate::fits::Header::new());
                for c in 1..planes {
                    let mut g = f.clone();
                    g.data.iter_mut().for_each(|v| *v *= 1.0 - 0.15 * c as f32);
                    img.planes.push(g);
                }
                img.header.set("EXPTIME", Value::Float(10.0), "");
                img
            })
            .collect()
    }

    fn same(a: &Frame, b: &Frame) {
        assert_eq!(a.data.len(), b.data.len());
        for (x, y) in a.data.iter().zip(&b.data) {
            assert!(
                x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
                "{x} {y}"
            );
        }
    }

    fn compare(frames: &[Image], budget: usize, opts: &StackOptions) -> usize {
        let want = stack_with(frames, opts, &Cpu, &mut |_| {}).expect("stack_with");
        let dir =
            std::env::temp_dir().join(format!("valerian-stream-test-{budget}-{}", frames.len()));
        std::fs::create_dir_all(&dir).expect("dir");
        let got = stack_streaming(
            &VecSource(frames.to_vec()),
            opts,
            &StreamOptions {
                budget,
                scratch: Some(dir.clone()),
            },
            &Cpu,
            &mut |_| {},
        )
        .expect("stack_streaming");
        assert_eq!(want.reference, got.reference);
        assert_eq!(want.used(), got.used());
        assert_eq!(want.rejection, got.rejection);
        assert_eq!(want.image.planes.len(), got.image.planes.len());
        for (a, b) in want.image.planes.iter().zip(&got.image.planes) {
            same(a, b);
        }
        same(&want.coverage, &got.coverage);
        for (r, q) in want.frames.iter().zip(&got.frames) {
            assert_eq!(r.status, q.status);
            assert!(
                r.rejected.to_bits() == q.rejected.to_bits(),
                "{} {}",
                r.rejected,
                q.rejected
            );
            assert_eq!(r.weight.to_bits(), q.weight.to_bits());
            assert_eq!(r.flux_scale.to_bits(), q.flux_scale.to_bits());
        }
        assert_eq!(
            want.image.header.get_str("STACKALG"),
            got.image.header.get_str("STACKALG")
        );
        assert_eq!(
            want.image.header.get_f64("EXPTOTAL"),
            got.image.header.get_f64("EXPTOTAL")
        );
        match (&want.maps, &got.maps) {
            (Some(a), Some(b)) => {
                same(&a.low, &b.low);
                same(&a.high, &b.high);
                same(&a.weight, &b.weight);
            }
            (None, None) => {}
            _ => panic!("maps differ"),
        }
        let left = std::fs::read_dir(&dir).expect("dir").count();
        std::fs::remove_dir_all(&dir).expect("rm");
        left
    }

    #[test]
    fn in_memory_staging_gives_the_same_stack() {
        let frames = session(7, 1, 6);
        assert_eq!(compare(&frames, 1 << 30, &StackOptions::default()), 0);
    }

    #[test]
    fn banded_staging_on_disk_gives_the_same_stack() {
        let frames = session(9, 1, 7);
        // 9 frames of 400 x 300: a budget this small forces the file and a few rows per band.
        let left = compare(&frames, 400 * 300 * 4 * 2, &StackOptions::default());
        assert_eq!(left, 0, "the staging file is removed");
    }

    #[test]
    fn colour_frames_and_maps_match_for_every_rejection() {
        let frames = session(8, 3, 8);
        for rejection in [
            Rejection::None,
            Rejection::MinMax { low: 1, high: 1 },
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
        ] {
            let opts = StackOptions {
                rejection: Some(rejection),
                maps: true,
                ..StackOptions::default()
            };
            compare(&frames, 400 * 300 * 4 * 3, &opts);
        }
    }

    #[test]
    fn a_band_is_at_least_a_row_and_at_most_the_image() {
        assert_eq!(band_rows(1000, 1080, 1920, 0), 1);
        assert_eq!(band_rows(10, 100, 50, usize::MAX / 2), 50);
        assert_eq!(band_rows(1349, 1080, 1920, 1 << 30), 184);
    }

    #[test]
    fn bad_input_is_refused() {
        let empty = VecSource(Vec::new());
        assert!(
            stack_streaming(
                &empty,
                &StackOptions::default(),
                &StreamOptions::default(),
                &Cpu,
                &mut |_| {}
            )
            .is_err()
        );
        let mut frames = session(3, 1, 9);
        frames[1].planes[0].bayer = Some(helia_core::BayerPattern::Rggb);
        assert!(
            stack_streaming(
                &VecSource(frames),
                &StackOptions::default(),
                &StreamOptions::default(),
                &Cpu,
                &mut |_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn cancelling_stops_the_run_and_leaves_no_file() {
        let frames = session(5, 1, 10);
        let dir = std::env::temp_dir().join("valerian-stream-test-cancel");
        std::fs::create_dir_all(&dir).expect("dir");
        let opts = StackOptions::default();
        opts.cancel.cancel();
        let r = stack_streaming(
            &VecSource(frames),
            &opts,
            &StreamOptions {
                budget: 1000,
                scratch: Some(dir.clone()),
            },
            &Cpu,
            &mut |_| {},
        );
        assert!(r.is_err());
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        std::fs::remove_dir_all(&dir).expect("rm");
    }

    /// Three panes of a sky `400 + 2 * step` pixels wide, 400 wide each, 6
    /// frames a pane, `step` pixels apart.
    fn mosaic_session(seed: u64, step: f64) -> Vec<Image> {
        let sky_w = 400 + 2 * step as usize;
        let wide = Scene::random(sky_w, sky_w, 300, seed);
        let scene = Scene {
            width: 400,
            height: 300,
            stars: wide.stars,
        };
        let mut rng = Rng::new(seed + 7);
        let mut frames = Vec::new();
        for pane in 0..3 {
            for k in 0..6 {
                let view = Transform::similarity(
                    1.0,
                    0.0,
                    -step * f64::from(pane) + (rng.uniform() - 0.5) * 6.0,
                    (rng.uniform() - 0.5) * 6.0,
                );
                let f = scene.render(
                    &view,
                    500.0,
                    6.0,
                    1.0,
                    0.0,
                    seed * 100 + (pane * 6 + k) as u64,
                );
                let mut img = Image::mono(f, crate::fits::Header::new());
                img.header.set("EXPTIME", Value::Float(10.0), "");
                img.header.set("CRPIX1", Value::Float(200.0), "");
                frames.push(img);
            }
        }
        frames
    }

    fn mosaic_opts(feather: f32) -> StackOptions {
        StackOptions {
            min_stars: 10,
            // The first pane's frames are the sharpest by construction.
            reference: crate::stack::Reference::Index(0),
            mosaic: Some(crate::mosaic::MosaicOptions {
                feather,
                match_levels: true,
            }),
            ..StackOptions::default()
        }
    }

    #[test]
    fn a_mosaic_places_every_pane_on_one_canvas() {
        let frames = mosaic_session(21, 150.0);
        let n = frames.len();
        let got = stack_streaming(
            &VecSource(frames),
            &mosaic_opts(60.0),
            &StreamOptions::default(),
            &Cpu,
            &mut |_| {},
        )
        .expect("mosaic stack");
        assert_eq!(got.used(), n, "every frame is placed");
        let (w, h) = (got.image.width(), got.image.height());
        assert!(
            (690..=720).contains(&w) && (295..=315).contains(&h),
            "{w} x {h}"
        );
        assert_eq!(got.image.header.get("MOSAIC"), Some(&Value::Bool(true)));
        // Sky is flat, so a feathered seam leaves no step: the mean of the sky
        // columns is the same left, in the two overlaps and at the right.
        let plane = &got.image.planes[0];
        let col = |x0: usize, x1: usize| {
            let mut v: Vec<f32> = Vec::new();
            for y in 20..h - 20 {
                for x in x0..x1 {
                    let p = plane.data[y * w + x];
                    if p.is_finite() {
                        v.push(p);
                    }
                }
            }
            v.sort_by(f32::total_cmp);
            v[v.len() / 2]
        };
        let (left, seam1, seam2, right) = (
            col(40, 100),
            col(190, 250),
            col(340, 400),
            col(w - 100, w - 40),
        );
        for (name, m) in [("seam 1", seam1), ("seam 2", seam2), ("right", right)] {
            assert!((m - left).abs() < 3.0, "{name} {m} against {left}");
        }
        // The far pane has no star in common with the reference's frames beyond
        // its overlap, yet stars from all three panes are in the stack.
        let det = crate::stars::detect(plane, &crate::stars::DetectOptions::default());
        let (min_x, max_x) = det
            .stars
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), s| (a.min(s.x), b.max(s.x)));
        assert!(
            min_x < 80.0 && max_x > w as f64 - 80.0,
            "stars span {min_x}..{max_x}"
        );
    }

    #[test]
    fn a_mosaic_streams_the_same_through_a_file() {
        let frames = mosaic_session(22, 150.0);
        let mem = stack_streaming(
            &VecSource(frames.clone()),
            &mosaic_opts(60.0),
            &StreamOptions::default(),
            &Cpu,
            &mut |_| {},
        )
        .expect("memory");
        let dir = std::env::temp_dir().join("valerian-mosaic-stream-test");
        std::fs::create_dir_all(&dir).expect("dir");
        let disk = stack_streaming(
            &VecSource(frames),
            &mosaic_opts(60.0),
            &StreamOptions {
                budget: 1 << 20,
                scratch: Some(dir.clone()),
            },
            &Cpu,
            &mut |_| {},
        )
        .expect("disk");
        same(&mem.image.planes[0], &disk.image.planes[0]);
        same(&mem.coverage, &disk.coverage);
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        std::fs::remove_dir_all(&dir).expect("rm");
    }

    #[test]
    fn a_pane_that_misses_the_reference_is_reached_through_its_neighbour() {
        let frames = mosaic_session(23, 300.0);
        let plain = stack_streaming(
            &VecSource(frames.clone()),
            &StackOptions {
                mosaic: None,
                ..mosaic_opts(0.0)
            },
            &StreamOptions::default(),
            &Cpu,
            &mut |_| {},
        )
        .expect("plain stack");
        assert!(
            plain.used() < 18,
            "the last pane has no star in common with the reference"
        );
        assert_eq!(plain.image.width(), 400);
        let got = stack_streaming(
            &VecSource(frames),
            &mosaic_opts(60.0),
            &StreamOptions::default(),
            &Cpu,
            &mut |_| {},
        )
        .expect("mosaic stack");
        assert_eq!(got.used(), 18);
        assert!(
            (995..=1020).contains(&got.image.width()),
            "{}",
            got.image.width()
        );
    }
}
