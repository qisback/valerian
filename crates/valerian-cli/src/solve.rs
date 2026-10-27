// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian solve`.

use anyhow::{Context, Result, bail};
use valerian_core::catalogue::{self, Catalogue};
use valerian_core::fits::{self, Header};
use valerian_core::solve::{self, Hint, Options, Solution, hint_from_header};

use crate::SolveArgs;
use crate::files::write;

/// A right ascension or declination given as a number or as sexagesimal text.
fn angle(text: &str, hours: bool) -> Result<f64> {
    solve::parse_angle(text, hours).with_context(|| format!("cannot read {text:?} as an angle"))
}

fn options(a: &SolveArgs, header: &Header, size: (usize, usize)) -> Result<Options> {
    let from_header = hint_from_header(header, size);
    let position = match (&a.ra, &a.dec) {
        (Some(ra), Some(dec)) => Some((angle(ra, true)?, angle(dec, false)?)),
        _ if a.blind => None,
        _ => from_header.position,
    };
    let scale = match (a.scale, &a.scale_range) {
        (Some(s), _) => (s * 0.85, s * 1.15),
        (None, Some(r)) => (r[0], r[1]),
        (None, None) => match from_header.scale {
            Some(s) => (s * 0.85, s * 1.15),
            None if position.is_some() => bail!(
                "the header has no scale (FOCALLEN and XPIXSZ): give --scale or --scale-range"
            ),
            None => bail!("a blind solve needs --scale or --scale-range"),
        },
    };
    let sip_order = match a.sip_order.as_str() {
        "auto" => None,
        n => Some(
            n.parse::<usize>()
                .ok()
                .filter(|o| *o == 0 || (2..=5).contains(o))
                .context("--sip-order is auto, 0, or 2 to 5")?,
        ),
    };
    let mut opts = Options {
        hint: position.map(|(ra, dec)| Hint {
            ra,
            dec,
            radius: a.radius,
        }),
        scale,
        min_matches: a.min_matches,
        sip_order,
        ..Options::default()
    };
    opts.detect.threshold = a.threshold;
    Ok(opts)
}

fn hms(deg: f64) -> String {
    let h = deg / 15.0;
    let (hh, m) = (h.floor(), (h.fract() * 60.0).floor());
    format!(
        "{hh:02.0}h{m:02.0}m{:04.1}s",
        ((h.fract() * 60.0).fract()) * 60.0
    )
}

fn dms(deg: f64) -> String {
    let (sign, a) = if deg < 0.0 { ('-', -deg) } else { ('+', deg) };
    let (d, m) = (a.floor(), (a.fract() * 60.0).floor());
    format!(
        "{sign}{d:02.0}d{m:02.0}m{:04.1}s",
        ((a.fract() * 60.0).fract()) * 60.0
    )
}

fn report(s: &Solution) {
    let w = &s.wcs;
    println!(
        "solved: centre RA {} Dec {} ({:.5}, {:+.5} deg)",
        hms(s.centre.0),
        dms(s.centre.1),
        s.centre.0,
        s.centre.1
    );
    println!(
        "scale {:.3} arcsec/pixel, field {:.2} x {:.2} deg, rotation {:.2} deg east of north{}",
        w.scale_arcsec(),
        s.field.0,
        s.field.1,
        w.rotation(),
        if w.mirrored() { ", mirrored" } else { "" }
    );
    println!(
        "{} of {} detected stars matched, rms {:.2} arcsec ({:.2} px), {}",
        s.matched,
        s.detected,
        s.rms_arcsec,
        s.rms_px,
        match &w.sip {
            Some(sip) => format!("SIP order {}", sip.order),
            None => "plain TAN".to_owned(),
        }
    );
}

pub fn run(a: &SolveArgs) -> Result<()> {
    let mut img =
        fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    let cat = match &a.catalogue {
        Some(p) => Catalogue::open(p)?,
        None => catalogue::open_default()?,
    };
    let opts = options(a, &img.header, (img.width(), img.height()))?;
    match &opts.hint {
        Some(h) => println!(
            "looking within {:.2} deg of RA {:.4} Dec {:+.4}, scale {:.2} to {:.2} arcsec/pixel",
            h.radius, h.ra, h.dec, opts.scale.0, opts.scale.1
        ),
        None => println!(
            "searching the whole sky, scale {:.2} to {:.2} arcsec/pixel",
            opts.scale.0, opts.scale.1
        ),
    }
    let sol = solve::solve(&img, &cat, &opts)?;
    report(&sol);
    if let Some(p) = &a.output {
        solve::record(&mut img, &sol);
        write(p, &img)?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
