// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The star catalogue plate solving matches against.
//!
//! A catalogue is a flat file (`.vcat`) of star positions, G magnitudes and
//! BP-RP colours (the colours are what photometric colour calibration needs;
//! a version 1 file without them still reads).
//! `scripts/build_catalogue.py` makes one from Gaia DR3; a G < 11 file of 1.25
//! million stars (10 MB) is kept in `crates/valerian-core/data`. The format is
//! documented in that script. Stars sit in one-degree declination bands, by
//! right ascension within a band, so a cone search reads only the stars near the
//! cone and the whole file can be held in memory unparsed.
//!
//! Positions are at the Gaia epoch (2016.0) and proper motion is not applied: a
//! bright star moves a few hundredths of an arc second a year, far below the
//! matching tolerance.
//!
//! # References
//!
//! - Gaia Collaboration, Prusti, T. et al. (2016), The Gaia mission, Astron. Astrophys. 595, A1, doi:10.1051/0004-6361/201629272.
//! - Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940.
//! - Lindegren, L. et al. (2021), Gaia Early Data Release 3: the astrometric solution, Astron. Astrophys. 649, A2, doi:10.1051/0004-6361/202039709 (Gaia DR3 astrometry is that of EDR3).
//! - Data: ESA/Gaia/DPAC. ESA states the licence is CC BY-NC 3.0 IGO (<https://www.cosmos.esa.int/web/gaia-users/license>): see docs/LEGAL-AUDIT.md.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

const MAGIC1: &[u8; 8] = b"VCAT0001";
const MAGIC2: &[u8; 8] = b"VCAT0002";
const HEADER: usize = 32;

/// One catalogue star.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatStar {
    /// Right ascension in degrees, `0..360`.
    pub ra: f64,
    /// Declination in degrees, `-90..=90`.
    pub dec: f64,
    /// Gaia G magnitude.
    pub mag: f32,
    /// Gaia BP-RP colour, when the catalogue has it and the star has one.
    pub colour: Option<f32>,
}

/// A catalogue held in memory in its file layout.
#[derive(Clone, Debug)]
pub struct Catalogue {
    bytes: Vec<u8>,
    n: usize,
    /// Bytes per star: 7 (version 1) or 8 (with colour).
    stride: usize,
    bands: usize,
    mag_limit: f32,
    /// Index of the first star of each band, then the total.
    starts: Vec<u32>,
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl Catalogue {
    /// Parse a catalogue file's bytes.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the bytes are not a catalogue.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < HEADER || (&bytes[..8] != MAGIC1 && &bytes[..8] != MAGIC2) {
            return Err(Error::invalid("not a Valerian star catalogue (bad magic)"));
        }
        let stride = if &bytes[..8] == MAGIC2 { 8 } else { 7 };
        let n = u32_at(&bytes, 8) as usize;
        let mag_limit = f32::from_bits(u32_at(&bytes, 12));
        let bands = u32_at(&bytes, 16) as usize;
        if bands == 0 || bands > 1800 {
            return Err(Error::invalid("catalogue band count is out of range"));
        }
        let table = HEADER + 4 * (bands + 1);
        if bytes.len() != table + stride * n {
            return Err(Error::invalid(
                "catalogue size does not match its star count (truncated file?)",
            ));
        }
        let starts: Vec<u32> = (0..=bands)
            .map(|i| u32_at(&bytes, HEADER + 4 * i))
            .collect();
        if starts[bands] as usize != n || starts.windows(2).any(|w| w[0] > w[1]) {
            return Err(Error::invalid("catalogue band table is inconsistent"));
        }
        Ok(Self {
            bytes,
            n,
            stride,
            bands,
            mag_limit,
            starts,
        })
    }

    /// Read a catalogue file.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the file cannot be read or is not a catalogue.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|e| Error::invalid(format!("cannot read {}: {e}", path.display())))?;
        Self::from_bytes(bytes).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))
    }

    /// Pack `stars` (any order) into a catalogue with one-degree bands.
    ///
    /// # Panics
    /// Never for any input; the catalogue built is always valid.
    #[must_use]
    pub fn build(stars: &[CatStar], mag_limit: f32) -> Self {
        const BANDS: usize = 180;
        let mut packed: Vec<(usize, u32, u16, u8, u8)> = stars
            .iter()
            .map(|s| {
                let band = ((s.dec + 90.0).floor().max(0.0) as usize).min(BANDS - 1);
                let ra = ((s.ra.rem_euclid(360.0) / 360.0) * 4_294_967_296.0)
                    .round()
                    .min(4_294_967_295.0) as u32;
                let dec = (((s.dec + 90.0 - band as f64) * 65535.0)
                    .round()
                    .clamp(0.0, 65535.0)) as u16;
                let mag = ((f64::from(s.mag) + 2.0) * 10.0).round().clamp(0.0, 255.0) as u8;
                let colour = s.colour.map_or(0, |c| {
                    ((f64::from(c) + 1.0) * 40.0).round().clamp(1.0, 255.0) as u8
                });
                (band, ra, dec, mag, colour)
            })
            .collect();
        packed.sort_unstable();
        let mut first = vec![0u32; BANDS + 1];
        for p in &packed {
            first[p.0 + 1] += 1;
        }
        for i in 0..BANDS {
            first[i + 1] += first[i];
        }
        let mut bytes = Vec::with_capacity(HEADER + 4 * (BANDS + 1) + 8 * packed.len());
        bytes.extend_from_slice(MAGIC2);
        bytes.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&mag_limit.to_le_bytes());
        bytes.extend_from_slice(&(BANDS as u32).to_le_bytes());
        bytes.extend_from_slice(&2016.0f32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        for s in &first {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        for &(_, ra, dec, mag, colour) in &packed {
            bytes.extend_from_slice(&ra.to_le_bytes());
            bytes.extend_from_slice(&dec.to_le_bytes());
            bytes.push(mag);
            bytes.push(colour);
        }
        Self::from_bytes(bytes).expect("a catalogue just built is valid")
    }

    /// The file's bytes, for writing it.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Number of stars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.n
    }

    /// True for an empty catalogue.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The magnitude limit the catalogue was built to.
    #[must_use]
    pub fn mag_limit(&self) -> f32 {
        self.mag_limit
    }

    fn star(&self, band: usize, i: usize) -> CatStar {
        let o = HEADER + 4 * (self.bands + 1) + self.stride * i;
        let b = &self.bytes;
        let ra = f64::from(u32_at(b, o)) / 4_294_967_296.0 * 360.0;
        let dec_off = f64::from(u16::from_le_bytes([b[o + 4], b[o + 5]])) / 65535.0;
        let band_deg = 180.0 / self.bands as f64;
        CatStar {
            ra,
            dec: -90.0 + (band as f64 + dec_off) * band_deg,
            mag: f32::from(b[o + 6]) / 10.0 - 2.0,
            colour: (self.stride > 7 && b[o + 7] > 0).then(|| f32::from(b[o + 7]) / 40.0 - 1.0),
        }
    }

    fn band_ra(&self, i: usize) -> u32 {
        u32_at(&self.bytes, HEADER + 4 * (self.bands + 1) + self.stride * i)
    }

    /// Stars within `radius` degrees of `(ra, dec)` and at least as bright as
    /// `mag_limit`, brightest first.
    #[must_use]
    pub fn cone(&self, ra: f64, dec: f64, radius: f64, mag_limit: f32) -> Vec<CatStar> {
        let band_deg = 180.0 / self.bands as f64;
        let (lo, hi) = ((dec - radius).max(-90.0), (dec + radius).min(90.0));
        let (b0, b1) = (
            (((lo + 90.0) / band_deg).floor().max(0.0)) as usize,
            ((((hi + 90.0) / band_deg).floor()) as usize).min(self.bands - 1),
        );
        // Right-ascension half width of the cone, or everything near a pole.
        let near_pole = dec.abs() + radius >= 90.0;
        let cosd = dec.to_radians().cos().max(1e-6);
        let half = if near_pole {
            180.0
        } else {
            (radius.to_radians().sin() / cosd)
                .min(1.0)
                .asin()
                .to_degrees()
                .min(180.0)
        };
        let (rd, dd) = (ra.to_radians(), dec.to_radians());
        let cos_r = radius.to_radians().cos();
        let mut out = Vec::new();
        for band in b0..=b1 {
            let (s, e) = (self.starts[band] as usize, self.starts[band + 1] as usize);
            let ranges: Vec<(f64, f64)> = if half >= 180.0 {
                vec![(0.0, 360.0)]
            } else {
                let (a, b) = (ra - half, ra + half);
                if a < 0.0 {
                    vec![(0.0, b), (a + 360.0, 360.0)]
                } else if b > 360.0 {
                    vec![(a, 360.0), (0.0, b - 360.0)]
                } else {
                    vec![(a, b)]
                }
            };
            for (a, b) in ranges {
                let key =
                    |deg: f64| (deg / 360.0 * 4_294_967_296.0).clamp(0.0, 4_294_967_295.0) as u32;
                let (ka, kb) = (key(a), key(b));
                // First star in the band with right ascension >= ka.
                let (mut l, mut r) = (s, e);
                while l < r {
                    let m = usize::midpoint(l, r);
                    if self.band_ra(m) < ka {
                        l = m + 1;
                    } else {
                        r = m;
                    }
                }
                for i in l..e {
                    if self.band_ra(i) > kb {
                        break;
                    }
                    let st = self.star(band, i);
                    if st.mag > mag_limit {
                        continue;
                    }
                    let (sr, sd) = (st.ra.to_radians(), st.dec.to_radians());
                    let c = dd.sin() * sd.sin() + dd.cos() * sd.cos() * (rd - sr).cos();
                    if c >= cos_r {
                        out.push(st);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.mag.total_cmp(&b.mag));
        out
    }
}

/// The file name of the catalogue Valerian looks for.
pub const DEFAULT_FILE: &str = "gaia-dr3-g11.vcat";

/// Where a catalogue is looked for when none is named: the `VALERIAN_CATALOGUE`
/// environment variable, then beside the program, in its `data` folder, in
/// `../share/valerian`, and (for a build from source) the repository's data.
#[must_use]
pub fn search_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("VALERIAN_CATALOGUE") {
        v.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        v.push(dir.join(DEFAULT_FILE));
        v.push(dir.join("data").join(DEFAULT_FILE));
        v.push(dir.join("../share/valerian").join(DEFAULT_FILE));
    }
    v.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(DEFAULT_FILE),
    );
    v
}

/// Open the first catalogue [`search_paths`] finds.
///
/// # Errors
/// [`Error::Invalid`] listing where it looked, or why the file found is bad.
pub fn open_default() -> Result<Catalogue> {
    let paths = search_paths();
    for p in &paths {
        if p.is_file() {
            return Catalogue::open(p);
        }
    }
    Err(Error::invalid(format!(
        "no star catalogue found; looked for {} in: {}. Build one with \
         scripts/build_catalogue.py or name one with --catalogue",
        DEFAULT_FILE,
        paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sky(n: usize) -> Vec<CatStar> {
        // A deterministic scatter over the sphere (golden-angle spiral).
        (0..n)
            .map(|i| {
                let z = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
                CatStar {
                    ra: (i as f64 * 137.507_764).rem_euclid(360.0),
                    dec: z.asin().to_degrees(),
                    mag: 4.0 + (i % 70) as f32 / 10.0,
                    colour: Some(0.5 + (i % 30) as f32 / 10.0),
                }
            })
            .collect()
    }

    fn angle(a: (f64, f64), b: (f64, f64)) -> f64 {
        let (r1, d1, r2, d2) = (
            a.0.to_radians(),
            a.1.to_radians(),
            b.0.to_radians(),
            b.1.to_radians(),
        );
        (d1.sin() * d2.sin() + d1.cos() * d2.cos() * (r1 - r2).cos())
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }

    #[test]
    fn a_cone_returns_exactly_the_stars_inside_it() {
        let stars = sky(20_000);
        let cat = Catalogue::build(&stars, 11.0);
        assert_eq!(cat.len(), stars.len());
        for &(ra, dec, r) in &[
            (10.0, 20.0, 5.0),
            (359.5, -30.0, 8.0),
            (0.2, 0.0, 4.0),
            (180.0, 88.0, 6.0),
            (45.0, -89.0, 3.0),
            (200.0, 50.0, 25.0),
        ] {
            let got = cat.cone(ra, dec, r, 99.0);
            let want = stars
                .iter()
                .filter(|s| angle((ra, dec), (s.ra, s.dec)) <= r)
                .count();
            // The packed positions are rounded to 0.1 mas, so a star on the
            // very edge can fall either way; allow one.
            assert!(
                got.len().abs_diff(want) <= 1,
                "{ra} {dec} {r}: {} vs {want}",
                got.len()
            );
            assert!(got.windows(2).all(|w| w[0].mag <= w[1].mag));
            assert!(
                got.iter()
                    .all(|s| angle((ra, dec), (s.ra, s.dec)) <= r + 1e-6)
            );
        }
    }

    #[test]
    fn positions_and_magnitudes_survive_packing() {
        let s = CatStar {
            ra: 101.287_155,
            dec: -16.716_116,
            mag: -1.46,
            colour: Some(0.0),
        };
        let cat = Catalogue::build(&[s], 11.0);
        let got = cat.cone(s.ra, s.dec, 0.01, 99.0);
        assert_eq!(got.len(), 1);
        assert!((got[0].ra - s.ra).abs() < 1e-6);
        assert!((got[0].dec - s.dec).abs() < 2e-5, "{}", got[0].dec - s.dec);
        assert!((got[0].mag - s.mag).abs() < 0.06);
        assert!((got[0].colour.expect("colour") - 0.0).abs() < 0.02);
    }

    #[test]
    fn a_version_one_file_reads_without_colours() {
        let s = CatStar {
            ra: 10.0,
            dec: 20.0,
            mag: 8.0,
            colour: Some(1.0),
        };
        let v2 = Catalogue::build(&[s], 11.0);
        let mut v1 = v2.as_bytes()[..v2.as_bytes().len() - 1].to_vec();
        v1[..8].copy_from_slice(MAGIC1);
        let cat = Catalogue::from_bytes(v1).expect("version 1");
        let got = cat.cone(10.0, 20.0, 0.1, 99.0);
        assert_eq!(got.len(), 1);
        assert!(got[0].colour.is_none());
    }

    #[test]
    fn the_magnitude_limit_filters() {
        let cat = Catalogue::build(&sky(5000), 11.0);
        let all = cat.cone(0.0, 0.0, 30.0, 99.0).len();
        let bright = cat.cone(0.0, 0.0, 30.0, 6.0).len();
        assert!(bright > 0 && bright < all);
    }

    #[test]
    fn bytes_round_trip_and_bad_input_is_refused() {
        let cat = Catalogue::build(&sky(500), 11.0);
        let again = Catalogue::from_bytes(cat.as_bytes().to_vec()).expect("round trip");
        assert_eq!(again.len(), 500);
        assert!(Catalogue::from_bytes(vec![0; 10]).is_err());
        let mut bad = cat.as_bytes().to_vec();
        bad.truncate(bad.len() - 3);
        assert!(Catalogue::from_bytes(bad).is_err());
        let mut magic = cat.as_bytes().to_vec();
        magic[0] = b'X';
        assert!(Catalogue::from_bytes(magic).is_err());
    }

    #[test]
    fn the_shipped_gaia_catalogue_reads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(DEFAULT_FILE);
        if !path.is_file() {
            return;
        }
        let cat = Catalogue::open(&path).expect("shipped catalogue");
        assert!(cat.len() > 1_000_000 && (cat.mag_limit() - 11.0).abs() < 1e-6);
        // The naked-eye stars: about 7 000 over the whole sky to G = 6.
        let half = cat.cone(0.0, 0.0, 90.0, 6.0).len();
        assert!(
            (2500..4500).contains(&half),
            "{half} bright stars in a hemisphere"
        );
        // About 30 stars per square degree at the galactic pole to hundreds in
        // the plane: a one degree cone has some.
        let field = cat.cone(313.0, 44.5, 1.0, 99.0);
        assert!(field.len() > 40, "{}", field.len());
    }
}
