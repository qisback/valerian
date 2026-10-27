// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian info`.

use anyhow::{Context, Result};
use valerian_core::fits::{self, Header, Value};
use valerian_core::stars::{DetectOptions, detect, luminance};
use valerian_core::stats;
use valerian_gpu::AdapterRequest;

use crate::InfoArgs;
use crate::files::collect;

/// Header cards worth showing, in this order.
const KEYS: &[&str] = &[
    "OBJECT", "IMAGETYP", "FILTER", "EXPTIME", "EXPOSURE", "CCD-TEMP", "SET-TEMP", "GAIN",
    "OFFSET", "XBINNING", "BAYERPAT", "DATE-OBS", "INSTRUME", "TELESCOP", "FOCALLEN", "NCOMBINE",
    "EXPTOTAL",
];

fn show(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Str(s) => s.trim().to_owned(),
    }
}

fn describe(path: &std::path::Path, stats_wanted: bool) -> Result<()> {
    println!("{}", path.display());
    if !stats_wanted {
        let h: Header =
            fits::read_header(path).with_context(|| format!("cannot read {}", path.display()))?;
        print_keys(&h);
        return Ok(());
    }
    let img = fits::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    println!(
        "  size: {} x {} pixels, {} plane{}{}",
        img.width(),
        img.height(),
        img.planes.len(),
        if img.planes.len() == 1 { "" } else { "s" },
        if img.plane().bayer.is_some() {
            " (raw Bayer mosaic)"
        } else {
            ""
        }
    );
    print_keys(&img.header);
    for (i, p) in img.planes.iter().enumerate() {
        let (sky, noise) = stats::sky_and_noise(p);
        let (lo, hi) = p.min_max();
        println!("  plane {i}: min {lo:.1}, sky {sky:.1}, noise {noise:.2}, max {hi:.1} ADU");
    }
    let luma = luminance(&img.planes);
    let det = detect(
        &luma,
        &DetectOptions {
            step: if img.plane().bayer.is_some() { 2 } else { 1 },
            ..DetectOptions::default()
        },
    );
    println!(
        "  stars: {} (median FWHM {:.2} px, ellipticity {:.3})",
        det.stars.len(),
        det.median_fwhm(),
        det.median_ellipticity()
    );
    Ok(())
}

fn print_keys(h: &Header) {
    for k in KEYS {
        if let Some(v) = h.get(k) {
            println!("  {k:<9} {}", show(v));
        }
    }
}

/// Where `valerian solve` finds its star catalogue, so an installed package can be checked.
fn tidy(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn catalogue_line() -> String {
    let found = valerian_core::catalogue::search_paths()
        .into_iter()
        .find(|p| p.is_file());
    match found.map(|p| tidy(&p)) {
        Some(p) => match valerian_core::catalogue::Catalogue::open(&p) {
            Ok(c) => format!("{} ({} stars)", p.display(), c.len()),
            Err(e) => format!("{} (unreadable: {e})", p.display()),
        },
        None => "not found".to_owned(),
    }
}

pub fn run(a: &InfoArgs) -> Result<()> {
    if a.files.is_empty() {
        println!(
            "{}",
            valerian_core::about::version_block(
                "valerian",
                env!("CARGO_PKG_VERSION"),
                valerian_gpu::WGPU_VERSION,
                None
            )
        );
        println!("catalogue: {}", catalogue_line());
    } else {
        for f in collect(&a.files)? {
            describe(&f, a.stats)?;
        }
    }
    if a.gpu {
        let request = if a.software {
            AdapterRequest::Software
        } else {
            AdapterRequest::Hardware
        };
        println!("gpu: {}", valerian_gpu::describe(request));
    }
    Ok(())
}
