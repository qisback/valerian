// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Finding, reading and writing frames.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use valerian_core::debayer::{Method, demosaic_image};
use valerian_core::fits::{self, Format, Image};

/// Whether a path looks like a FITS file.
fn is_fits(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts"))
}

/// Expand files and directories to a sorted list of FITS files.
///
/// # Errors
/// If a path does not exist or a directory holds no FITS files.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| format!("cannot read {}", p.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|q| q.is_file() && is_fits(q))
                .collect();
            if found.is_empty() {
                bail!("{} contains no FITS files", p.display());
            }
            found.sort();
            out.extend(found);
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            bail!("{} does not exist", p.display());
        }
    }
    Ok(out)
}

/// Read many FITS files in parallel, keeping their order.
///
/// # Errors
/// The first file that cannot be read.
pub fn load(paths: &[PathBuf]) -> Result<Vec<Image>> {
    let t = Instant::now();
    let images = load_quiet(paths)?;
    eprintln!("read {} frames in {:.1?}", images.len(), t.elapsed());
    Ok(images)
}

/// [`load`] without the progress line, for a read that is not the user's frames
/// (a probe of the first one).
///
/// # Errors
/// The first file that cannot be read.
pub fn load_quiet(paths: &[PathBuf]) -> Result<Vec<Image>> {
    paths
        .par_iter()
        .map(|p| fits::read(p).with_context(|| format!("cannot read {}", p.display())))
        .collect()
}

/// Whether an image says it is a master frame.
#[must_use]
pub fn is_master(img: &Image) -> bool {
    img.header
        .get_str("IMAGETYP")
        .is_some_and(|t| t.to_ascii_lowercase().contains("master"))
}

/// Write an image as 32-bit float FITS, creating the directory.
///
/// # Errors
/// If the file cannot be written.
pub fn write(path: &Path, img: &Image) -> Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fits::write(path, img, Format::Float32)
        .with_context(|| format!("cannot write {}", path.display()))
}

/// `dir/<stem><suffix>.fits` for an input path.
#[must_use]
pub fn output_name(dir: &Path, input: &Path, suffix: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame");
    dir.join(format!("{stem}{suffix}.fits"))
}

/// Demosaic the frames that are raw Bayer mosaics, when a method is wanted.
///
/// # Errors
/// If a mosaic cannot be demosaiced.
pub fn demosaic_all(images: Vec<Image>, method: Option<Method>) -> Result<Vec<Image>> {
    let Some(m) = method else {
        return Ok(images);
    };
    images
        .into_par_iter()
        .map(|img| {
            if img.planes.len() == 1 && img.plane().bayer.is_some() {
                demosaic_image(&img, m).map_err(Into::into)
            } else {
                Ok(img)
            }
        })
        .collect()
}

/// Whether any frame is a raw Bayer mosaic.
#[must_use]
pub fn any_bayer(images: &[Image]) -> bool {
    images
        .iter()
        .any(|i| i.planes.iter().any(|p| p.bayer.is_some()))
}

/// A file name cut to at most `n` characters for a table, keeping the end: the part
/// that tells frames apart (a time stamp) comes last in most capture names. A cut
/// name starts with two dots.
#[must_use]
pub fn elide(name: &str, n: usize) -> String {
    let count = name.chars().count();
    if count <= n {
        return name.to_owned();
    }
    let keep = n.saturating_sub(2);
    let tail: String = name.chars().skip(count - keep).collect();
    format!("..{tail}")
}

#[cfg(test)]
mod elide_tests {
    use super::elide;

    #[test]
    fn short_names_are_unchanged() {
        assert_eq!(elide("Light_001.fit", 28), "Light_001.fit");
    }

    #[test]
    fn long_names_keep_their_end() {
        let a = elide("Light_M 34_10.0s_IRCUT_20250925-020913.fit", 28);
        let b = elide("Light_M 34_10.0s_IRCUT_20250925-020924.fit", 28);
        assert_eq!(a.chars().count(), 28);
        assert!(a.starts_with("..") && a.ends_with("020913.fit"), "{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn multibyte_names_are_cut_on_a_character() {
        let s = elide("M\u{e9}sier_frame_with_a_very_long_name_\u{e9}.fit", 12);
        assert_eq!(s.chars().count(), 12);
    }
}
