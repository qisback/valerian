// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian calibrate`.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use valerian_core::calibrate::{Calibrator, FlatNormalisation, PixelMap, flag};
use valerian_core::combine::Rejection;
use valerian_core::compute::Compute;
use valerian_core::debayer::demosaic_image;
use valerian_core::fits::{self, Image};
use valerian_core::streaming::{master_bias_with, master_dark_with, master_flat_with};

use crate::CalibrateArgs;
use crate::backend;
use crate::files::{collect, is_master, output_name, write};

/// A master from a group of paths: either a single master file as it is, or
/// the combination of the frames, streamed from disk a band of rows at a time.
/// `None` when no paths were given.
fn group(
    label: &str,
    paths: &[PathBuf],
    rejection: impl Fn(usize) -> Rejection,
    build: impl FnOnce(&[PathBuf], Rejection) -> Result<Image>,
) -> Result<Option<Image>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let files = collect(paths)?;
    let t = Instant::now();
    if files.len() == 1 {
        let img =
            fits::read(&files[0]).with_context(|| format!("cannot read {}", files[0].display()))?;
        if is_master(&img) {
            eprintln!("{label}: using the master {}", files[0].display());
            return Ok(Some(img));
        }
    }
    let rej = rejection(files.len());
    let master = build(&files, rej)?;
    eprintln!(
        "{label}: combined {} frames with {rej:?} in {:.1?}",
        files.len(),
        t.elapsed()
    );
    Ok(Some(master))
}

pub fn run(a: &CalibrateArgs) -> Result<()> {
    let engine = backend::open(a.backend, false)?;
    let compute: &dyn Compute = &*engine.compute;
    let rej = |n: usize| a.rejection.resolve(None, None, n);
    let budget = a.memory.saturating_mul(1 << 20);
    let bias = group("bias", &a.bias, rej, |f, r| {
        master_bias_with(f, r, budget, compute).map_err(Into::into)
    })?;
    let dark = group("dark", &a.dark, rej, |f, r| {
        master_dark_with(f, bias.as_ref(), r, budget, compute).map_err(Into::into)
    })?;
    // Dark flats hold the bias too, so they are not bias-subtracted.
    let dark_flat = group("dark flat", &a.dark_flat, rej, |f, r| {
        master_dark_with(f, None, r, budget, compute).map_err(Into::into)
    })?;
    let norm = if a.flat_per_channel {
        FlatNormalisation::PerChannel
    } else {
        FlatNormalisation::Global
    };
    let flat = group("flat", &a.flat, rej, |f, r| {
        let sub = dark_flat.as_ref().or(bias.as_ref());
        master_flat_with(f, sub, r, norm, budget, compute).map_err(Into::into)
    })?;
    if bias.is_none() && dark.is_none() && flat.is_none() && !a.auto_cosmetic {
        bail!(
            "nothing to calibrate with: give at least one of --bias, --dark and --flat, or --auto-cosmetic"
        );
    }

    let mut calibrator = Calibrator::new(bias.clone(), dark.clone(), flat.clone());
    calibrator.scaling = a.scale_dark.into();
    let mut map = dark
        .as_ref()
        .map(|d| PixelMap::from_dark(d.plane(), a.hot_sigma, a.cold_sigma));
    if let (Some(m), Some(f)) = (map.as_mut(), flat.as_ref()) {
        m.add_dead_from_flat(f.plane(), 0.2);
    } else if let (None, Some(f)) = (map.as_ref(), flat.as_ref()) {
        let mut m = PixelMap::empty(f.width(), f.height(), f.plane().bayer.is_some());
        m.add_dead_from_flat(f.plane(), 0.2);
        map = Some(m);
    }
    if let Some(m) = &map {
        eprintln!(
            "pixel map: {} hot, {} cold, {} dead",
            m.count_flag(flag::HOT),
            m.count_flag(flag::COLD),
            m.count_flag(flag::DEAD)
        );
    }
    calibrator.pixel_map = map;

    if let Some(dir) = &a.masters {
        for (name, m) in [
            ("master_bias.fits", &bias),
            ("master_dark.fits", &dark),
            ("master_dark_flat.fits", &dark_flat),
            ("master_flat.fits", &flat),
        ] {
            if let Some(img) = m {
                write(&dir.join(name), img)?;
                eprintln!("wrote {}", dir.join(name).display());
            }
        }
    }

    let lights = collect(&a.lights)?;
    let method = a.debayer.method(false);
    for path in &lights {
        let t = Instant::now();
        let light = fits::read(path)?;
        let mut work = light;
        if a.auto_cosmetic && calibrator.dark.is_none() {
            let map = PixelMap::detect_isolated(work.plane(), a.hot_sigma.min(8.0), a.cold_sigma);
            let fixed = map.correct(&mut work.planes[0]);
            eprintln!("{}: {fixed} isolated pixels repaired", path.display());
        }
        let (mut cal, report) = compute.calibrate(&calibrator, &work)?;
        if let Some(m) = method
            && cal.planes.len() == 1
            && cal.plane().bayer.is_some()
        {
            cal = demosaic_image(&cal, m)?;
        }
        let out = output_name(&a.output, path, "_cal");
        write(&out, &cal)?;
        println!(
            "{} -> {}  dark x{:.4}, {} pixels repaired, {:.1?}",
            path.display(),
            out.display(),
            report.dark_scale,
            report.repaired,
            t.elapsed()
        );
    }
    backend::report(&engine);
    Ok(())
}
