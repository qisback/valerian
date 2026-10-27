// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian drizzle`.

use std::time::Instant;

use anyhow::{Context, Result};
use valerian_core::drizzle::{Options, drizzle};
use valerian_core::fits::{Header, Image};
use valerian_core::register::RegisterOptions;
use valerian_core::stack::{Reference, StackOptions};
use valerian_core::stars::DetectOptions;

use crate::DrizzleArgs;
use crate::files::{collect, load, write};
use crate::stack::table;

pub fn run(a: &DrizzleArgs) -> Result<()> {
    let files = collect(&a.frames)?;
    let names: Vec<String> = files
        .iter()
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_owned()
        })
        .collect();
    let frames = load(&files)?;
    let stack_opts = StackOptions {
        detect: DetectOptions {
            threshold: a.threshold,
            ..DetectOptions::default()
        },
        register: RegisterOptions {
            model: a.model.into(),
            ..RegisterOptions::default()
        },
        weights: a.weights.into(),
        normalisation: a.normalisation.into(),
        reference: a.reference.map_or(Reference::Best, Reference::Index),
        min_stars: a.min_stars,
        max_fwhm_ratio: a.max_fwhm_ratio,
        ..StackOptions::default()
    };
    let opts = Options {
        scale: a.scale,
        pixfrac: a.pixfrac,
        reject: (!a.no_reject).then_some(a.reject),
    };
    let t = Instant::now();
    let d = drizzle(&frames, &stack_opts, &opts, &mut |line| eprintln!("{line}"))
        .context("drizzling failed")?;
    let text = table(&d.frames, &names);
    print!("{text}");
    let out = &d.image.planes[0];
    println!(
        "{} of {} frames drizzled to {} x {} (scale {}, pixfrac {}), reference frame {} ({}), \
         {:.2} % of the input samples rejected, {:.3} % of the output without data, {:.1?}",
        d.used(),
        frames.len(),
        out.width,
        out.height,
        d.scale,
        d.pixfrac,
        d.reference,
        names[d.reference],
        100.0 * d.rejected,
        100.0 * d.holes(),
        t.elapsed()
    );
    write(&a.output, &d.image)?;
    println!("wrote {}", a.output.display());
    if let Some(p) = &a.weight_map {
        write(p, &Image::mono(d.weight.clone(), Header::new()))?;
        println!("wrote {}", p.display());
    }
    if let Some(dir) = &a.science_report {
        crate::science::write_drizzle_report(dir, &d)?;
    }
    if let Some(p) = &a.report {
        std::fs::write(p, &text).with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
