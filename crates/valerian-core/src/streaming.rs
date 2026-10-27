// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Master calibration frames built from files without holding every frame in
//! memory.
//!
//! The in-memory builders in [`crate::calibrate`] take every frame as an
//! [`Image`], so a master of 70 large frames needs all 70 resident at once.
//! Rejection is per pixel: the samples at one position never depend on any
//! other position. So the same master can be built band by band. A band is a
//! few rows; for each band, the matching rows are read from every file,
//! combined with the usual rejection, and written into the master, and then
//! the band is dropped. The result is bit for bit the same as the in-memory
//! builders give, and the working set is set by a byte budget rather than by
//! the number of frames.
//!
//! The one number that needs a whole frame is a flat's level (the median that
//! brings the flats to a common brightness). It is found in a first pass that
//! reads the flats one at a time, so it is the same median the in-memory
//! builder uses. The finished master is one frame and is held in memory.

use std::path::PathBuf;

use rayon::prelude::*;

use crate::calibrate::{
    FlatNormalisation, flat_level, master_header, mean_header_value, normalise_flat,
};
use crate::cancel::CancelToken;
use crate::combine::{Options, Rejection};
use crate::compute::{Compute, Cpu};
use crate::error::{Error, Result};
use crate::fits::{self, Header, Image, Reader, Value};
use helia_core::Frame;

/// A working-set budget of 1 GiB, enough for a few rows of a very large
/// number of frames.
pub const DEFAULT_BUDGET: usize = 1 << 30;

/// Bytes held per sample while a band is read: the decoded value (4) and the
/// raw bytes it is decoded from (up to 8 for 64-bit data).
const BYTES_PER_SAMPLE: usize = 12;

/// How many rows of every frame fit in `budget` bytes: at least one, at most
/// the whole image. The band is the unit that is read and combined at once.
#[must_use]
pub fn band_rows(frames: usize, width: usize, height: usize, budget: usize) -> usize {
    let per_row = frames.max(1) * width.max(1) * BYTES_PER_SAMPLE;
    (budget / per_row).clamp(1, height.max(1))
}

/// Bytes a band of `rows` rows of `frames` frames of `width` pixels holds.
#[must_use]
pub fn band_bytes(frames: usize, width: usize, rows: usize) -> usize {
    frames * width * rows * BYTES_PER_SAMPLE
}

fn open_all(paths: &[PathBuf]) -> Result<Vec<Reader>> {
    if paths.is_empty() {
        return Err(Error::invalid("no frames given"));
    }
    let readers = paths
        .iter()
        .map(|p| Reader::open(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display()))))
        .collect::<Result<Vec<_>>>()?;
    let first = &readers[0];
    for (r, p) in readers.iter().zip(paths) {
        if r.planes() != first.planes()
            || r.width() != first.width()
            || r.height() != first.height()
        {
            return Err(Error::invalid(format!(
                "{} differs in size from {}",
                p.display(),
                paths[0].display()
            )));
        }
    }
    Ok(readers)
}

/// Rows `y0..y1` of a plane of `master`, as a frame.
fn master_band(master: &Image, plane: usize, y0: usize, y1: usize) -> Frame {
    let m = &master.planes[plane];
    Frame {
        width: m.width,
        height: y1 - y0,
        data: m.data[y0 * m.width..y1 * m.width].to_vec(),
        bayer: None,
    }
}

/// Combine the files band by band. `subtract` is removed from every frame and
/// `scales[i]` multiplies frame `i` after that, as the in-memory builders do.
/// Returns the combined planes and the frames' headers.
fn stream_combine(
    paths: &[PathBuf],
    subtract: Option<&Image>,
    scales: Option<&[f32]>,
    rejection: Rejection,
    budget: usize,
    compute: &dyn Compute,
    cancel: &CancelToken,
) -> Result<(Vec<Frame>, Vec<Header>)> {
    let readers = open_all(paths)?;
    let (w, h, planes) = (readers[0].width(), readers[0].height(), readers[0].planes());
    if let Some(m) = subtract
        && (m.planes.len() != planes || m.width() != w || m.height() != h)
    {
        return Err(Error::invalid(
            "master frame differs in size from the frames",
        ));
    }
    let bayer = readers[0].bayer();
    let headers: Vec<Header> = readers.iter().map(|r| r.header().clone()).collect();
    drop(readers);
    let rows = band_rows(paths.len(), w, h, budget);
    let opts = Options {
        rejection,
        ..Options::default()
    };
    let mut out = Vec::with_capacity(planes);
    for plane in 0..planes {
        let mut data = Vec::with_capacity(w * h);
        let mut y0 = 0;
        while y0 < h {
            cancel.check()?;
            let y1 = (y0 + rows).min(h);
            let mut band: Vec<Frame> = paths
                .par_iter()
                .map(|p| {
                    Reader::open(p)
                        .and_then(|mut r| r.rows(plane, y0, y1))
                        .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
                })
                .collect::<Result<_>>()?;
            if let Some(m) = subtract {
                let mb = master_band(m, plane, y0, y1);
                band.par_iter_mut().for_each(|f| {
                    f.data.iter_mut().zip(&mb.data).for_each(|(x, y)| *x -= *y);
                });
            }
            if let Some(k) = scales {
                band.par_iter_mut().zip(k).for_each(|(f, &k)| {
                    f.data.iter_mut().for_each(|v| *v *= k);
                });
            }
            let refs: Vec<&Frame> = band.iter().collect();
            data.extend_from_slice(&compute.combine(&refs, &opts)?.mean.data);
            y0 = y1;
        }
        out.push(
            Frame::new(w, h, data)
                .map_err(|e| Error::invalid(e.to_string()))?
                .with_bayer(bayer),
        );
    }
    Ok((out, headers))
}

fn header_refs(headers: &[Header]) -> Vec<&Header> {
    headers.iter().collect()
}

/// Master bias from files, streamed. Same result as
/// [`crate::calibrate::master_bias`] on the same frames.
///
/// `budget` is the working set in bytes (see [`band_rows`]).
///
/// # Errors
/// [`Error`] when a file cannot be read or the frames differ in size.
pub fn master_bias(paths: &[PathBuf], rejection: Rejection, budget: usize) -> Result<Image> {
    master_bias_with(paths, rejection, budget, &Cpu)
}

/// [`master_bias`] with the combining done by `compute`.
///
/// # Errors
/// As for [`master_bias`].
pub fn master_bias_with(
    paths: &[PathBuf],
    rejection: Rejection,
    budget: usize,
    compute: &dyn Compute,
) -> Result<Image> {
    master_bias_cancellable(paths, rejection, budget, compute, &CancelToken::default())
}

/// [`master_bias_with`] that stops between bands once `cancel` is set.
///
/// # Errors
/// As for [`master_bias`], and [`Error::Cancelled`].
pub fn master_bias_cancellable(
    paths: &[PathBuf],
    rejection: Rejection,
    budget: usize,
    compute: &dyn Compute,
    cancel: &CancelToken,
) -> Result<Image> {
    let (planes, headers) = stream_combine(paths, None, None, rejection, budget, compute, cancel)?;
    let mut header = master_header(&header_refs(&headers), "Master Bias", rejection);
    header.set("EXPTIME", Value::Float(0.0), "[s]");
    Ok(Image { header, planes })
}

/// Master dark from files, streamed. With a master `bias` the result is
/// bias-subtracted dark current (`BIASSUB = T`). Same result as
/// [`crate::calibrate::master_dark`].
///
/// # Errors
/// [`Error`] when a file cannot be read or the frames differ in size.
pub fn master_dark(
    paths: &[PathBuf],
    bias: Option<&Image>,
    rejection: Rejection,
    budget: usize,
) -> Result<Image> {
    master_dark_with(paths, bias, rejection, budget, &Cpu)
}

/// [`master_dark`] with the combining done by `compute`.
///
/// # Errors
/// As for [`master_dark`].
pub fn master_dark_with(
    paths: &[PathBuf],
    bias: Option<&Image>,
    rejection: Rejection,
    budget: usize,
    compute: &dyn Compute,
) -> Result<Image> {
    master_dark_cancellable(
        paths,
        bias,
        rejection,
        budget,
        compute,
        &CancelToken::default(),
    )
}

/// [`master_dark_with`] that stops between bands once `cancel` is set.
///
/// # Errors
/// As for [`master_dark`], and [`Error::Cancelled`].
pub fn master_dark_cancellable(
    paths: &[PathBuf],
    bias: Option<&Image>,
    rejection: Rejection,
    budget: usize,
    compute: &dyn Compute,
    cancel: &CancelToken,
) -> Result<Image> {
    let (planes, headers) = stream_combine(paths, bias, None, rejection, budget, compute, cancel)?;
    let refs = header_refs(&headers);
    let mut header = master_header(&refs, "Master Dark", rejection);
    header.set(
        "BIASSUB",
        Value::Bool(bias.is_some()),
        "bias removed from this dark",
    );
    if let Some(t) = mean_header_value(&refs, Header::exposure) {
        header.set(
            "EXPTIME",
            Value::Float(t),
            "[s] exposure of the dark frames",
        );
    }
    Ok(Image { header, planes })
}

/// Master flat from files, streamed. Same result as
/// [`crate::calibrate::master_flat`]: `subtract` is removed from every flat,
/// the flats are scaled to the first one's median, combined and normalised.
///
/// # Errors
/// [`Error`] when a file cannot be read or the frames differ in size.
pub fn master_flat(
    paths: &[PathBuf],
    subtract: Option<&Image>,
    rejection: Rejection,
    normalisation: FlatNormalisation,
    budget: usize,
) -> Result<Image> {
    master_flat_with(paths, subtract, rejection, normalisation, budget, &Cpu)
}

/// [`master_flat`] with the combining done by `compute`.
///
/// # Errors
/// As for [`master_flat`].
pub fn master_flat_with(
    paths: &[PathBuf],
    subtract: Option<&Image>,
    rejection: Rejection,
    normalisation: FlatNormalisation,
    budget: usize,
    compute: &dyn Compute,
) -> Result<Image> {
    master_flat_cancellable(
        paths,
        subtract,
        rejection,
        normalisation,
        budget,
        compute,
        &CancelToken::default(),
    )
}

/// [`master_flat_with`] that stops between frames and bands once `cancel` is set.
///
/// # Errors
/// As for [`master_flat`], and [`Error::Cancelled`].
pub fn master_flat_cancellable(
    paths: &[PathBuf],
    subtract: Option<&Image>,
    rejection: Rejection,
    normalisation: FlatNormalisation,
    budget: usize,
    compute: &dyn Compute,
    cancel: &CancelToken,
) -> Result<Image> {
    // Pass one: each flat's level, one frame in memory at a time.
    let readers = open_all(paths)?;
    drop(readers);
    let mut levels = Vec::with_capacity(paths.len());
    for p in paths {
        cancel.check()?;
        let mut img = fits::read(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?;
        if let Some(m) = subtract {
            if m.planes.len() != img.planes.len()
                || m.width() != img.width()
                || m.height() != img.height()
            {
                return Err(Error::invalid(
                    "master frame differs in size from the frames",
                ));
            }
            for (a, b) in img.planes.iter_mut().zip(&m.planes) {
                a.data.iter_mut().zip(&b.data).for_each(|(x, y)| *x -= *y);
            }
        }
        levels.push(flat_level(&img));
    }
    let target = levels[0];
    let scales: Vec<f32> = levels
        .iter()
        .map(|&l| {
            if l > 0.0 && target > 0.0 {
                target / l
            } else {
                1.0
            }
        })
        .collect();
    let (mut planes, headers) = stream_combine(
        paths,
        subtract,
        Some(&scales),
        rejection,
        budget,
        compute,
        cancel,
    )?;
    for p in &mut planes {
        normalise_flat(p, normalisation);
    }
    let mut header = master_header(&header_refs(&headers), "Master Flat", rejection);
    header.set(
        "FLATNORM",
        Value::Str(format!("{normalisation:?}")),
        "normalisation",
    );
    Ok(Image { header, planes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibrate;
    use crate::testutil::{Rng, Sensor};
    use std::path::Path;

    /// A scratch directory that removes itself.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("valerian-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn save(dir: &Path, name: &str, img: &Image) -> PathBuf {
        let p = dir.join(name);
        fits::write(&p, img, fits::Format::Float32).expect("write");
        p
    }

    fn exposures(sensor: &Sensor, scene: &Frame, seconds: f32, n: usize, seed: u64) -> Vec<Image> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| sensor.expose(scene, seconds, &mut rng))
            .collect()
    }

    fn files(dir: &Path, tag: &str, frames: &[Image]) -> Vec<PathBuf> {
        frames
            .iter()
            .enumerate()
            .map(|(i, f)| save(dir, &format!("{tag}{i:03}.fits"), f))
            .collect()
    }

    fn assert_same(a: &Image, b: &Image) {
        assert_eq!(a.planes.len(), b.planes.len());
        for (x, y) in a.planes.iter().zip(&b.planes) {
            assert_eq!((x.width, x.height, x.bayer), (y.width, y.height, y.bayer));
            let same = x
                .data
                .iter()
                .zip(&y.data)
                .all(|(p, q)| p.to_bits() == q.to_bits());
            assert!(same, "streamed master differs from the in-memory one");
        }
    }

    #[test]
    fn band_rows_respect_the_budget() {
        // 70 frames of 4656 pixels: a 4.8 GiB frame set cannot fit a 256 MiB budget.
        let (n, w, h) = (70, 4656, 3520);
        for budget in [1 << 20, 64 << 20, 256 << 20, 1 << 30] {
            let rows = band_rows(n, w, h, budget);
            assert!((1..=h).contains(&rows));
            // One row is the floor; above that the band must fit.
            if rows > 1 {
                assert!(band_bytes(n, w, rows) <= budget);
            }
            // And it is the largest that fits.
            if rows < h {
                assert!(band_bytes(n, w, rows + 1) > budget || rows == h);
            }
        }
        assert_eq!(band_rows(3, 10, 8, usize::MAX), 8);
        assert_eq!(band_rows(1000, 100_000, 50, 1), 1);
    }

    #[test]
    fn streamed_masters_equal_the_in_memory_ones_for_any_budget() {
        let tmp = Scratch::new("stream-mono");
        let sensor = Sensor::new(96, 61, false, 7);
        let zero = Frame::zeros(96, 61);
        let biases = exposures(&sensor, &zero, 0.0, 9, 1);
        let darks = exposures(&sensor, &zero, 120.0, 8, 2);
        let lamp = Frame::new(96, 61, vec![18_000.0; 96 * 61]).expect("lamp");
        // Flats at different lamp levels so the per-flat scaling matters.
        let mut flats = exposures(&sensor, &lamp, 2.0, 7, 3);
        for (i, f) in flats.iter_mut().enumerate() {
            for v in &mut f.planes[0].data {
                *v *= 1.0 + 0.03 * i as f32;
            }
        }
        let bias_p = files(tmp.path(), "bias", &biases);
        let dark_p = files(tmp.path(), "dark", &darks);
        let flat_p = files(tmp.path(), "flat", &flats);

        let rej = Rejection::Winsorised {
            low: 3.0,
            high: 3.0,
        };
        let bias_ref = calibrate::master_bias(&biases, rej).expect("bias");
        let dark_ref = calibrate::master_dark(&darks, Some(&bias_ref), rej).expect("dark");
        let flat_ref =
            calibrate::master_flat(&flats, Some(&bias_ref), rej, FlatNormalisation::Global)
                .expect("flat");

        // One row at a time, a ragged band size, and the whole frame.
        for budget in [1, band_bytes(9, 96, 7), usize::MAX] {
            let bias = master_bias(&bias_p, rej, budget).expect("bias");
            assert_same(&bias, &bias_ref);
            assert_eq!(bias.header, bias_ref.header);
            let dark = master_dark(&dark_p, Some(&bias), rej, budget).expect("dark");
            assert_same(&dark, &dark_ref);
            assert_eq!(dark.header, dark_ref.header);
            let flat = master_flat(&flat_p, Some(&bias), rej, FlatNormalisation::Global, budget)
                .expect("flat");
            assert_same(&flat, &flat_ref);
            assert_eq!(flat.header, flat_ref.header);
        }
    }

    #[test]
    fn streamed_masters_keep_the_bayer_pattern_and_every_plane() {
        let tmp = Scratch::new("stream-bayer");
        let sensor = Sensor::new(40, 31, true, 11);
        let zero = Frame::zeros(40, 31);
        let darks = exposures(&sensor, &zero, 60.0, 6, 5);
        let paths = files(tmp.path(), "dark", &darks);
        let rej = Rejection::Sigma {
            low: 3.0,
            high: 3.0,
        };
        let reference = calibrate::master_dark(&darks, None, rej).expect("dark");
        assert!(reference.plane().bayer.is_some());
        // An odd number of rows per band shifts the Bayer phase of each band;
        // the master must still carry the pattern of the whole frame.
        let streamed = master_dark(&paths, None, rej, band_bytes(6, 40, 3)).expect("dark");
        assert_same(&streamed, &reference);

        let a = Frame::new(5, 4, (0..20).map(|i| i as f32).collect()).expect("plane");
        let b = Frame::new(5, 4, (0..20).map(|i| 100.0 + i as f32).collect()).expect("plane");
        let cube = Image {
            header: Header::new(),
            planes: vec![a, b],
        };
        let cubes: Vec<Image> = (0..4).map(|_| cube.clone()).collect();
        let cube_paths = files(tmp.path(), "cube", &cubes);
        let m = master_bias(&cube_paths, Rejection::None, 1).expect("cube master");
        assert_eq!(m.planes.len(), 2);
        assert_eq!(m.planes[1].data[7], 107.0);
    }

    #[test]
    fn a_hot_pixel_in_one_frame_is_rejected_across_band_edges() {
        let tmp = Scratch::new("stream-reject");
        let (w, h) = (16, 12);
        let mut frames: Vec<Image> = (0..12)
            .map(|i| {
                let data = (0..w * h).map(|p| 1000.0 + ((p + i) % 3) as f32).collect();
                Image::mono(Frame::new(w, h, data).expect("frame"), Header::new())
            })
            .collect();
        // Outliers on the first and last row of a 4-row band and in the middle.
        for (x, y) in [(3, 3), (4, 4), (9, 8), (0, 0), (15, 11)] {
            frames[5].planes[0].data[y * w + x] = 60_000.0;
        }
        let paths = files(tmp.path(), "f", &frames);
        let rej = Rejection::Winsorised {
            low: 3.0,
            high: 3.0,
        };
        let m = master_bias(&paths, rej, band_bytes(12, w, 4)).expect("master");
        for (x, y) in [(3, 3), (4, 4), (9, 8), (0, 0), (15, 11)] {
            let v = m.plane().get(x, y);
            assert!((999.0..=1003.0).contains(&v), "outlier leaked: {v}");
        }
    }

    #[test]
    fn mismatched_and_missing_files_are_refused() {
        let tmp = Scratch::new("stream-bad");
        let small = Image::mono(Frame::zeros(8, 8), Header::new());
        let large = Image::mono(Frame::zeros(9, 8), Header::new());
        let a = save(tmp.path(), "a.fits", &small);
        let b = save(tmp.path(), "b.fits", &large);
        let err = master_bias(&[a.clone(), b], Rejection::None, DEFAULT_BUDGET)
            .expect_err("sizes differ");
        assert!(err.to_string().contains("differs in size"), "{err}");
        let err = master_bias(&[a, tmp.path().join("missing.fits")], Rejection::None, 1)
            .expect_err("missing file");
        assert!(err.to_string().contains("missing.fits"), "{err}");
        assert!(master_bias(&[], Rejection::None, 1).is_err());
    }

    #[test]
    fn reader_rows_match_a_whole_read() {
        let tmp = Scratch::new("reader");
        let data: Vec<f32> = (0..7 * 5).map(|i| i as f32 * 0.25).collect();
        let img = Image::mono(Frame::new(7, 5, data).expect("frame"), Header::new());
        let p = save(tmp.path(), "r.fits", &img);
        let mut r = Reader::open(&p).expect("open");
        assert_eq!((r.width(), r.height(), r.planes()), (7, 5, 1));
        let band = r.rows(0, 2, 4).expect("rows");
        assert_eq!(band.data, img.plane().data[14..28]);
        assert!(r.rows(0, 4, 6).is_err());
        assert!(r.rows(1, 0, 1).is_err());
        assert!(r.rows(0, 3, 3).is_err());
    }

    #[test]
    fn a_cancelled_token_stops_every_master_and_a_clear_one_changes_nothing() {
        let tmp = Scratch::new("stream-cancel");
        let frame =
            |v: f32| Image::mono(Frame::new(8, 6, vec![v; 48]).expect("frame"), Header::new());
        let frames: Vec<Image> = (0..4).map(|i| frame(100.0 + i as f32)).collect();
        let paths = files(tmp.path(), "c", &frames);
        let clear = CancelToken::new();
        let rej = Rejection::None;
        let plain = master_bias(&paths, rej, 1).expect("bias");
        let held = master_bias_cancellable(&paths, rej, 1, &Cpu, &clear).expect("bias");
        assert_same(&plain, &held);
        let stop = CancelToken::new();
        stop.cancel();
        assert!(matches!(
            master_bias_cancellable(&paths, rej, 1, &Cpu, &stop),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            master_dark_cancellable(&paths, None, rej, 1, &Cpu, &stop),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            master_flat_cancellable(&paths, None, rej, FlatNormalisation::Global, 1, &Cpu, &stop),
            Err(Error::Cancelled)
        ));
    }
}
