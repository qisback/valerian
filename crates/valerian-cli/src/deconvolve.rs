// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian deconvolve`.

use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use valerian_core::deconvolve::{DeconvolveOptions, PsfSource, Saturation, deconvolve};
use valerian_core::fits;
use valerian_core::stars::DetectOptions;

use crate::DeconvolveArgs;
use crate::files::write;

fn tiles(text: &str) -> Result<(usize, usize)> {
    let bad = || anyhow::anyhow!("--tiles takes ACROSSxDOWN, for example 3x4, not {text:?}");
    let (a, b) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    let (a, b): (usize, usize) = (
        a.trim().parse().map_err(|_| bad())?,
        b.trim().parse().map_err(|_| bad())?,
    );
    if a == 0 || b == 0 {
        return Err(bad());
    }
    Ok((a, b))
}

fn saturation(text: &str) -> Result<Saturation> {
    match text.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(Saturation::Auto),
        "off" | "none" => Ok(Saturation::Off),
        t => match t.parse::<f32>() {
            Ok(v) if v.is_finite() && v > 0.0 => Ok(Saturation::Level(v)),
            _ => bail!("--saturation takes auto, off or a positive pixel value, not {text:?}"),
        },
    }
}

pub fn run(a: &DeconvolveArgs) -> Result<()> {
    let img = fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    if img.planes.iter().any(|p| p.bayer.is_some()) {
        bail!(
            "{} is a raw colour mosaic; calibrate it with --debayer first",
            a.input.display()
        );
    }
    let opts = DeconvolveOptions {
        psf: a.psf_fwhm.map_or(PsfSource::Measured, PsfSource::Gaussian),
        mode: a.mode.into(),
        target_fwhm: a.target_fwhm,
        target_ratio: a.target_ratio,
        iterations: a.iterations,
        tv: a.tv,
        roundness: a.roundness,
        saturation: saturation(&a.saturation)?,
        ring_guard: a.ring_guard,
        tiles: a.tiles.as_deref().map(tiles).transpose()?,
        psf_radius: a.psf_radius,
        psf_stars: a.psf_stars,
        min_psf_stars: a.min_psf_stars,
        psf_min_snr: a.psf_min_snr,
        detect: DetectOptions {
            threshold: a.threshold,
            ..DetectOptions::default()
        },
        ..DeconvolveOptions::default()
    };
    let t = Instant::now();
    let r =
        deconvolve(&img, &opts, &mut |line| eprintln!("{line}")).context("deconvolution failed")?;
    println!(
        "deconvolved {} plane(s), {} iterations, {} tile(s), {:.1?}",
        r.image.planes.len(),
        a.iterations,
        r.tiles.len(),
        t.elapsed()
    );
    if let Some(f) = r.target_fwhm {
        println!("stars deconvolved to round, FWHM {f:.2} px");
    }
    write(&a.output, &r.image)?;
    println!("wrote {}", a.output.display());
    if let Some(p) = &a.report {
        let mut s = String::from(
            "x0,y0,x1,y1,stars,psf_fwhm_px,psf_ellipticity
",
        );
        for t in &r.tiles {
            let _ = writeln!(
                s,
                "{},{},{},{},{},{:.3},{:.4}",
                t.x0, t.y0, t.x1, t.y1, t.stars, t.fwhm, t.ellipticity
            );
        }
        std::fs::write(p, s).with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
