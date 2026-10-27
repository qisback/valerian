// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian stretch`.

use anyhow::{Context, Result, bail};
use valerian_core::stats;
use valerian_core::stretch::{
    AutoLevels, Curve, Ghs, Levels, Plan, Stretch, invert_image, stretch_image,
};
use valerian_core::{fits, preview};

use crate::files::write;
use crate::{CurveArg, StretchArgs};

fn plan(a: &StretchArgs) -> Result<Plan> {
    let auto = AutoLevels {
        shadow_sigma: a.shadow_sigma,
        white_percentile: a.white_percentile,
    };
    let fixed = match (a.black, a.white) {
        (Some(black), Some(white)) => Some(Levels { black, white }),
        (None, None) => None,
        _ => bail!("give both --black and --white, or neither"),
    };
    let curve = match a.curve {
        CurveArg::Auto => None,
        CurveArg::Mtf => a.midtone.map(|midtone| Curve::Mtf { midtone }),
        CurveArg::Arcsinh => Some(Curve::Arcsinh { beta: a.beta }),
        CurveArg::Ghs => Some(Curve::Ghs(Ghs {
            stretch: a.ghs_stretch,
            b: a.ghs_b,
            sp: a.ghs_sp,
            lp: a.ghs_lp,
            hp: a.ghs_hp,
        })),
    };
    Ok(match (curve, fixed) {
        (Some(curve), Some(levels)) => Plan::Fixed(Stretch { levels, curve }),
        (Some(curve), None) => Plan::Curve(curve, auto),
        (None, None) => Plan::AutoMtf {
            target: a.target,
            auto,
        },
        (None, Some(_)) => bail!(
            "--black and --white need a curve with its own parameters: give --midtone, or use \
             --curve arcsinh or ghs"
        ),
    })
}

pub fn run(a: &StretchArgs) -> Result<()> {
    let img = fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    let out = if a.invert {
        let out = invert_image(&img)?;
        println!("{}: stretch undone", a.input.display());
        out
    } else {
        let p = plan(a)?;
        let out = stretch_image(&img, &p, !a.unlinked)?;
        match Stretch::from_header(&out.header) {
            Some(s) => println!(
                "black {:.4}, white {:.4}, {:?}",
                s.levels.black, s.levels.white, s.curve
            ),
            None => println!("each plane stretched on its own (see the HISTORY cards)"),
        }
        for (i, (before, after)) in img.planes.iter().zip(&out.planes).enumerate() {
            println!(
                "plane {i}: median {:.4} -> {:.4}",
                stats::median_finite(&before.data),
                stats::median_finite(&after.data)
            );
        }
        out
    };
    if let Some(path) = &a.output {
        write(path, &out)?;
        println!("wrote {}", path.display());
    }
    if let Some(path) = &a.preview {
        preview::write_png(path, &preview::downsample(&out, a.preview_size))
            .with_context(|| format!("cannot write {}", path.display()))?;
        println!("wrote preview {}", path.display());
    }
    Ok(())
}
