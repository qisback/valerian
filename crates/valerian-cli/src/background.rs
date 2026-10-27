// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian background`.

use anyhow::{Context, Result, bail};
use valerian_core::background::{Fit, Level, Mode, Model, Options, extract};
use valerian_core::fits::{self, Image};

use crate::files::write;
use crate::{BackgroundArgs, BackgroundModelArg, LevelArg, ModeArg};

fn options(a: &BackgroundArgs) -> Options {
    Options {
        model: match a.model {
            BackgroundModelArg::Polynomial => Model::Polynomial { degree: a.degree },
            BackgroundModelArg::Spline => Model::ThinPlate {
                smoothing: a.smoothing,
            },
        },
        tile: a.tile,
        star_sigma: a.star_sigma,
        mask_grow: a.mask_grow,
        clip_low: a.clip_low,
        clip_high: a.clip_high,
        rounds: a.rounds,
        exclude_target: a.exclude_target,
        ..Options::default()
    }
}

fn describe(opts: &Options) -> String {
    match opts.model {
        Model::Polynomial { degree } => format!("polynomial degree {degree}"),
        Model::ThinPlate { smoothing } => format!("thin-plate spline, smoothing {smoothing}"),
    }
}

pub fn run(a: &BackgroundArgs) -> Result<()> {
    let img = fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    if img.planes.iter().any(|p| p.bayer.is_some()) {
        bail!(
            "{} is a raw colour mosaic; calibrate it with --debayer first",
            a.input.display()
        );
    }
    let opts = options(a);
    let mode: Mode = a.mode.into();
    let level: Level = a.level.into();
    let mut header = img.header.clone();
    let (mut planes, mut surfaces) = (Vec::new(), Vec::new());
    let mut declined = 0;
    for (i, plane) in img.planes.iter().enumerate() {
        let fit: Fit = extract(plane, &opts)?;
        match &fit.declined {
            Some(why) => {
                declined += 1;
                eprintln!("plane {i}: background fit declined, plane left as it is: {why}");
            }
            None => println!(
                "plane {i}: {} samples of {} tiles ({} px), {:.1}% masked, residual rms {:.2}, \
                 gradient removed {:.1}",
                fit.samples,
                fit.tiles,
                fit.tile,
                fit.masked_fraction * 100.0,
                fit.residual_rms,
                fit.gradient_range()
            ),
        }
        planes.push(fit.apply(plane, mode, level)?);
        surfaces.push(fit.surface);
    }
    header.add_history(&format!(
        "valerian background: {}, {:?}, {:?}{}{}",
        describe(&opts),
        a.mode,
        a.level,
        if a.exclude_target {
            ", target excluded"
        } else {
            ""
        },
        if declined > 0 {
            format!(", {declined} plane(s) declined")
        } else {
            String::new()
        },
    ));
    write(&a.output, &Image { header, planes })?;
    println!("wrote {}", a.output.display());
    if let Some(p) = &a.save_model {
        write(
            p,
            &Image {
                header: img.header.clone(),
                planes: surfaces,
            },
        )?;
        println!("wrote the background model {}", p.display());
    }
    Ok(())
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Subtract => Self::Subtract,
            ModeArg::Divide => Self::Divide,
        }
    }
}

impl From<LevelArg> for Level {
    fn from(l: LevelArg) -> Self {
        match l {
            LevelArg::Preserve => Self::Preserve,
            LevelArg::Zero => Self::Zero,
        }
    }
}
