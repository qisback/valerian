// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian starless`.

use anyhow::{Context, Result, bail};
use valerian_core::fits::{self, Image};
use valerian_core::preview::{downsample, side_by_side, write_png};
use valerian_core::starless::{Options, remove_stars};
use valerian_core::stretch::stretch_like;

use crate::StarlessArgs;
use crate::files::write;

fn options(a: &StarlessArgs) -> Options {
    let mut o = Options::default();
    o.detect.threshold = a.threshold;
    o.mask_scale = a.mask_scale;
    o.grow = a.grow;
    o.max_radius = a.max_radius;
    o.noise = !a.no_noise;
    o.strength = a.strength;
    o.seed = a.seed;
    o
}

pub fn run(a: &StarlessArgs) -> Result<()> {
    let img = fits::read(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    if img.planes.iter().any(|p| p.bayer.is_some()) {
        bail!(
            "{} is a raw colour mosaic; calibrate it with --debayer first",
            a.input.display()
        );
    }
    let r = remove_stars(&img, &options(a))?;
    println!(
        "{} stars found, {:.2} % of the pixels filled",
        r.found,
        100.0 * r.masked_fraction
    );
    if let Some(p) = &a.output {
        write(p, &r.starless)?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = &a.stars_out {
        write(p, &r.stars)?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = &a.mask_out {
        write(p, &Image::mono(r.mask.clone(), r.starless.header.clone()))?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = &a.preview {
        let before = downsample(&stretch_like(&img, &img, 0.15)?, a.preview_size);
        let after = downsample(&stretch_like(&img, &r.starless, 0.15)?, a.preview_size);
        write_png(p, &side_by_side(&before, &after))
            .with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote preview {}", p.display());
    }
    Ok(())
}
