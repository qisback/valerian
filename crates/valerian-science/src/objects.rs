// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The bundled list of deep-sky objects: every Messier object and the brighter
//! or larger NGC and IC galaxies, clusters and nebulae.
//!
//! The list is `data/objects.csv`, a subset of OpenNGC (Mattia Verga,
//! <https://github.com/mattiaverga/OpenNGC>, CC-BY-SA-4.0) made by
//! `scripts/build_objects.py`; the data keeps its own licence, which is
//! compatible with the GPL for a combined work, and is acknowledged in the book.

use std::sync::OnceLock;

use valerian_core::wcs::Wcs;

/// One object.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    /// `M31`, `NGC 7000`, `IC 5070`.
    pub name: String,
    /// OpenNGC's type code (`G`, `OCl`, `GCl`, `PN`, `HII`, `Neb`, `SNR`, ...).
    pub kind: String,
    /// Right ascension, degrees (J2000).
    pub ra: f64,
    /// Declination, degrees (J2000).
    pub dec: f64,
    /// Major axis, arc minutes.
    pub major: Option<f64>,
    /// Minor axis, arc minutes.
    pub minor: Option<f64>,
    /// Position angle of the major axis, degrees east of north.
    pub pos_angle: Option<f64>,
    /// V magnitude (B where V is missing).
    pub mag: Option<f64>,
    /// A common name, if there is one.
    pub common: String,
}

impl Object {
    /// The name with the common name in brackets.
    #[must_use]
    pub fn label(&self) -> String {
        if self.common.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.common)
        }
    }

    /// What kind of object, in words.
    #[must_use]
    pub fn kind_word(&self) -> &'static str {
        match self.kind.as_str() {
            "G" | "GPair" | "GTrpl" | "GGroup" => "galaxy",
            "OCl" | "*Ass" => "open cluster",
            "GCl" => "globular cluster",
            "PN" => "planetary nebula",
            "HII" | "EmN" => "emission nebula",
            "RfN" => "reflection nebula",
            "SNR" => "supernova remnant",
            "Cl+N" => "cluster with nebulosity",
            "DrkN" => "dark nebula",
            "Neb" => "nebula",
            _ => "object",
        }
    }
}

const DATA: &str = include_str!("../data/objects.csv");

fn parse(line: &str) -> Option<Object> {
    let f = crate::table::split_csv_line(line);
    if f.len() < 9 {
        return None;
    }
    let num = |s: &String| s.trim().parse::<f64>().ok();
    Some(Object {
        name: f[0].clone(),
        kind: f[1].clone(),
        ra: num(&f[2])?,
        dec: num(&f[3])?,
        major: num(&f[4]),
        minor: num(&f[5]),
        pos_angle: num(&f[6]),
        mag: num(&f[7]),
        common: f[8].clone(),
    })
}

/// The bundled objects, Messier first.
#[must_use]
pub fn bundled() -> &'static [Object] {
    static LIST: OnceLock<Vec<Object>> = OnceLock::new();
    LIST.get_or_init(|| DATA.lines().skip(1).filter_map(parse).collect())
}

/// An object found in an image.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// The object.
    pub object: Object,
    /// Pixel position of its centre.
    pub x: f64,
    /// Pixel position of its centre.
    pub y: f64,
}

/// The objects whose centres fall within `margin` pixels of an image of
/// `size`, on the plate solution `wcs`, largest first.
#[must_use]
pub fn in_field(wcs: &Wcs, size: (usize, usize), margin: f64) -> Vec<Placed> {
    let (w, h) = (size.0 as f64, size.1 as f64);
    let mut out: Vec<Placed> = bundled()
        .iter()
        .filter_map(|o| {
            let (x, y) = wcs.world_to_pixel(o.ra, o.dec)?;
            (x >= -margin && y >= -margin && x < w + margin && y < h + margin).then(|| Placed {
                object: o.clone(),
                x,
                y,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.object
            .major
            .unwrap_or(0.0)
            .total_cmp(&a.object.major.unwrap_or(0.0))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_holds_every_messier_object_and_the_famous_ones() {
        let all = bundled();
        assert!(all.len() > 1000 && all.len() < 3000, "{}", all.len());
        let messier = all
            .iter()
            .filter(|o| o.name.starts_with('M') && o.name[1..].chars().all(|c| c.is_ascii_digit()))
            .count();
        assert!(messier >= 105, "{messier}");
        let m31 = all.iter().find(|o| o.name == "M31").expect("M31");
        // Andromeda: 00h42m44s +41d16m09s (J2000).
        assert!((m31.ra - 10.6848).abs() < 0.01 && (m31.dec - 41.2691).abs() < 0.01);
        assert_eq!(m31.common, "Andromeda Galaxy");
        assert!(
            all.iter()
                .any(|o| o.name == "IC 5070" && o.common == "Pelican Nebula")
        );
        assert!(all.iter().any(|o| o.name == "NGC 7000"));
    }

    #[test]
    fn a_field_around_the_pelican_nebula_finds_it() {
        // A 2-degree field of 2.39 arcsec per pixel centred on IC 5070.
        let wcs = Wcs::simple((312.75, 44.40), (540.0, 960.0), 2.393, 0.0, false);
        let found = in_field(&wcs, (1080, 1920), 0.0);
        assert!(
            found.iter().any(|p| p.object.name == "IC 5070"),
            "{found:?}"
        );
        assert!(
            found
                .windows(2)
                .all(|w| w[0].object.major.unwrap_or(0.0) >= w[1].object.major.unwrap_or(0.0))
        );
    }
}
