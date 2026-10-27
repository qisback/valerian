// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian register`.

use anyhow::{Context, Result};
use valerian_core::fits::{Header, Value};
use valerian_core::register::RegisterOptions;
use valerian_core::stack::{Normalisation, Reference, StackOptions, Status, align_with};
use valerian_core::stars::DetectOptions;

use crate::RegisterArgs;
use crate::backend;
use crate::files::{any_bayer, collect, demosaic_all, load, output_name, write};
use crate::stack::table;

pub fn run(a: &RegisterArgs) -> Result<()> {
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
    let method = a.debayer.method(any_bayer(&frames));
    let frames = demosaic_all(frames, method)?;
    let opts = StackOptions {
        detect: DetectOptions {
            threshold: a.threshold,
            ..DetectOptions::default()
        },
        register: RegisterOptions {
            model: a.model.into(),
            ..RegisterOptions::default()
        },
        interpolation: a.interpolation.into(),
        // Registered frames are saved as they are: no level matching.
        normalisation: Normalisation::None,
        reference: a.reference.map_or(Reference::Best, Reference::Index),
        ..StackOptions::default()
    };
    let engine = backend::open(a.backend, false)?;
    let aligned = align_with(frames, &opts, &*engine.compute, &mut |line| {
        eprintln!("{line}");
    })
    .context("registration failed")?;
    backend::report(&engine);
    print!("{}", table(&aligned.reports, &names));
    let mut written = 0;
    for &i in &aligned.keep {
        let mut img = aligned.frames[i].clone();
        img.header.set(
            "REGREF",
            Value::Int(i64::try_from(aligned.reference).unwrap_or(i64::MAX)),
            "index of the registration reference",
        );
        if let Status::Stacked = aligned.reports[i].status {
            let r = &aligned.reports[i];
            img.header.set(
                "REGDX",
                Value::Float(r.shift.0),
                "[px] shift to the reference",
            );
            img.header.set(
                "REGDY",
                Value::Float(r.shift.1),
                "[px] shift to the reference",
            );
            img.header.set(
                "REGROT",
                Value::Float(r.rotation_deg),
                "[deg] rotation to the reference",
            );
        }
        img.header = {
            let mut h: Header = img.header;
            h.add_history("registered by Valerian");
            h
        };
        let out = output_name(&a.output, &files[i], "_reg");
        write(&out, &img)?;
        written += 1;
    }
    println!(
        "registered {written} of {} frames to frame {}; wrote them to {}",
        files.len(),
        aligned.reference,
        a.output.display()
    );
    Ok(())
}
