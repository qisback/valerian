// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian photocal`.

use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use valerian_core::catalogue::{self, Catalogue};
use valerian_core::fits::{self};
use valerian_core::photocal::{Calibration, Options, calibrate};
use valerian_core::solve::{self, Hint, hint_from_header};
use valerian_core::wcs::Wcs;
use valerian_science::zeropoint;

use crate::PhotocalArgs;
use crate::files::write;

/// The plate solution: the image's own, one from `--wcs`, or a quick solve from
/// the header's position and scale.
fn solution(a: &PhotocalArgs, img: &fits::Image, cat: &Catalogue) -> Result<Wcs> {
    if let Some(p) = &a.wcs {
        let h = fits::read_header(p).with_context(|| format!("cannot read {}", p.display()))?;
        return Wcs::from_header(&h)
            .with_context(|| format!("{} has no world coordinate system", p.display()));
    }
    if let Some(w) = Wcs::from_header(&img.header) {
        return Ok(w);
    }
    let hint = hint_from_header(&img.header, (img.width(), img.height()));
    let (Some((ra, dec)), Some(scale)) = (hint.position, hint.scale) else {
        bail!(
            "{} has no world coordinate system and the header lacks a position and scale to \
             solve one: run `valerian solve` first",
            a.input.display()
        );
    };
    println!("no WCS in the image: solving it from the header first");
    let opts = solve::Options {
        hint: Some(Hint {
            ra,
            dec,
            radius: 1.0,
        }),
        scale: (scale * 0.85, scale * 1.15),
        ..solve::Options::default()
    };
    Ok(solve::solve(img, cat, &opts)?.wcs)
}

fn options(a: &PhotocalArgs) -> Options {
    Options {
        min_mag: a.min_mag,
        max_mag: a.max_mag,
        colour_range: (a.colour_min, a.colour_max),
        reference_colour: a.reference_colour,
        aperture: a.aperture,
        min_snr: a.min_snr,
        min_stars: a.min_stars,
        neutralise_background: !a.no_neutralise,
        ..Options::default()
    }
}

fn report(c: &Calibration) {
    println!(
        "{} Gaia stars measured, {} used; aperture radius {:.1} px; neutral at BP-RP {:.2}",
        c.stars.len(),
        c.used,
        c.aperture_px,
        c.reference_colour
    );
    for (name, line, gain, err) in [
        ("red", &c.lines[0], c.gains[0], c.gain_error[0]),
        ("blue", &c.lines[1], c.gains[2], c.gain_error[1]),
    ] {
        println!(
            "{name:>5} against green: slope {:+.3} mag per BP-RP, scatter {:.3} mag, gain {gain:.4} +/- {:.4}",
            line.slope,
            line.rms,
            err * gain,
        );
    }
    println!(
        "sky before {:.2} {:.2} {:.2}, after {:.2} {:.2} {:.2} (offsets {:.2} {:.2} {:.2})",
        c.sky_before[0],
        c.sky_before[1],
        c.sky_before[2],
        c.sky_after[0],
        c.sky_after[1],
        c.sky_after[2],
        c.offsets[0],
        c.offsets[1],
        c.offsets[2],
    );
    for w in &c.warnings {
        eprintln!("warning: {w}");
    }
}

fn star_table(c: &Calibration) -> String {
    let mut s = String::from("x,y,g,bp_rp,flux_r,flux_g,flux_b,used\n");
    for t in &c.stars {
        let _ = writeln!(
            s,
            "{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{}",
            t.x,
            t.y,
            t.g,
            t.colour,
            t.flux[0],
            t.flux[1],
            t.flux[2],
            u8::from(t.used)
        );
    }
    s
}

pub fn run(a: &PhotocalArgs) -> Result<()> {
    let img = fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    let cat = match &a.catalogue {
        Some(p) => Catalogue::open(p)?,
        None => catalogue::open_default()?,
    };
    let wcs = solution(a, &img, &cat)?;
    let cal = calibrate(&img, &wcs, &cat, &options(a))?;
    report(&cal);
    if let Some(dir) = &a.report {
        crate::astrometry::write_pcc_report(dir, &cal)?;
    }
    if a.report.is_some() || a.calibrated_stars.is_some() {
        let opts = zeropoint::Options {
            limit_snr: a.limit_snr,
            threshold: a.field_threshold,
            max_stars: a.max_field_stars,
            ..zeropoint::Options::default()
        };
        match zeropoint::calibrate_field(&img, &wcs, &cal, &opts) {
            Ok(p) => {
                crate::astrometry::print_calibration(&p);
                if let Some(dir) = &a.report {
                    crate::astrometry::write_calibration_report(dir, &p)?;
                }
                if let Some(f) = &a.calibrated_stars {
                    std::fs::write(f, zeropoint::stars_csv(&p))
                        .with_context(|| format!("cannot write {}", f.display()))?;
                    println!("wrote the calibrated star catalogue {}", f.display());
                }
            }
            Err(e) if a.calibrated_stars.is_some() => return Err(e.into()),
            Err(e) => eprintln!("note: no zero point: {e}"),
        }
    }
    if let Some(p) = &a.stars_out {
        std::fs::write(p, star_table(&cal))
            .with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote the star table {}", p.display());
    }
    if let Some(p) = &a.output {
        let mut out = cal.apply(&img)?;
        cal.record(&mut out);
        write(p, &out)?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
