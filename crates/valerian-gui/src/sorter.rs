// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sorting a folder of FITS frames into lights, darks, dark flats, flats and bias.
//!
//! Everything that decides is pure and tested without files: [`classify_text`] turns
//! an `IMAGETYP` value (or a file or folder name) into a [`Kind`], and [`refine`]
//! uses the whole set (exposures, filters, masters) to settle what a header cannot,
//! such as which darks belong to the flats, and to leave out frames that would
//! spoil a calibration. [`scan`] reads the headers of the files under some paths,
//! [`Review`] holds the result for the user to change, and [`Review::resolve`] and
//! [`stage`] turn it into the folder boxes of the Calibrate step.
//!
//! How a frame is classified, in this order:
//!
//! 1. the header's `IMAGETYP` (else `FRAMETYP`, `FRAME`), read loosely: case, spaces,
//!    underscores and a trailing "Frame" or "Field" do not matter (`Light Frame`,
//!    `DARK`, `Flat Field`, `Bias Frame`, `Master Dark`, `Dark Flat`, `Zero`);
//! 2. the file name, then the name of the folder it is in (`Light_M31_001.fit`,
//!    `.../DARKFLAT/...`), for cameras that write no such key;
//! 3. otherwise the frame is unsorted and left out until the user assigns it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use valerian_core::fits::{self, Header};

use crate::pipeline::Progress;

/// Folders deeper than this under a dropped folder are not looked at.
pub const MAX_DEPTH: usize = 6;
/// Files beyond this many are not read: a drop of a whole disk is a mistake.
pub const MAX_FILES: usize = 50_000;
/// The folder the sorted links are made in.
pub const STAGE_DIR: &str = "valerian-sorted";

/// What a frame is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A picture of the sky.
    Light,
    /// Shutter closed, the lights' exposure: thermal signal.
    Dark,
    /// Shutter closed, the flats' exposure.
    DarkFlat,
    /// An evenly lit field: vignetting and dust.
    Flat,
    /// Shutter closed, the shortest exposure: the read-out offset.
    Bias,
    /// Not decided: left out until the user assigns it.
    Other,
}

impl Kind {
    /// Every kind, in the order the review lists them.
    pub const ALL: [Self; 6] = [
        Self::Light,
        Self::Dark,
        Self::DarkFlat,
        Self::Flat,
        Self::Bias,
        Self::Other,
    ];

    /// The kinds a frame can be assigned to for a run (not "unsorted").
    pub const CHOICES: [Self; 5] = [
        Self::Light,
        Self::Dark,
        Self::DarkFlat,
        Self::Flat,
        Self::Bias,
    ];

    /// The name in the review.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Lights",
            Self::Dark => "Darks",
            Self::DarkFlat => "Dark flats",
            Self::Flat => "Flats",
            Self::Bias => "Bias",
            Self::Other => "Unsorted",
        }
    }

    /// The folder made for this kind when frames have to be gathered.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Light => "lights",
            Self::Dark => "darks",
            Self::DarkFlat => "darkflats",
            Self::Flat => "flats",
            Self::Bias => "bias",
            Self::Other => "unsorted",
        }
    }
}

/// The kind a piece of text names, and whether it is a master (a stack of frames).
///
/// `None` when the text names no kind. Works on a header value, a file stem or a
/// folder name alike.
#[must_use]
pub fn classify_text(text: &str) -> Option<(Kind, bool)> {
    let lower = text.to_ascii_lowercase();
    // Words, split at anything that is not a letter or digit; also the whole text with
    // the separators removed, for "DarkFlat" and "dark_flat".
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let joined: String = words.concat();
    let has = |w: &str| words.contains(&w);
    let master = has("master") || has("stack") || joined.starts_with("master");
    let kind = if joined.contains("darkflat")
        || joined.contains("flatdark")
        || joined.contains("flatdk")
        || (has("dark") && has("flat"))
    {
        Kind::DarkFlat
    } else if joined.contains("bias") || has("zero") || has("offset") {
        Kind::Bias
    } else if joined.contains("flat") {
        Kind::Flat
    } else if joined.contains("dark") {
        Kind::Dark
    } else if has("light")
        || has("lights")
        || has("object")
        || has("science")
        || has("sci")
        || has("target")
        || joined.contains("lightframe")
        || joined.starts_with("light")
        || (has("calibrated") && has("light"))
    {
        Kind::Light
    } else {
        return None;
    };
    Some((kind, master))
}

/// Where a frame's kind came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The `IMAGETYP` card.
    Header,
    /// The file's name.
    FileName,
    /// The name of the folder it is in.
    FolderName,
    /// Nothing said: unsorted.
    None,
    /// Settled from the set: a dark whose exposure is the flats'.
    Exposure,
    /// Changed by the user.
    User,
}

impl Source {
    /// A few words for the review.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Header => "from the header",
            Self::FileName => "from the file name",
            Self::FolderName => "from the folder name",
            Self::None => "no frame type in the header or names",
            Self::Exposure => "dark with the flats' exposure",
            Self::User => "set by you",
        }
    }
}

/// One FITS file in the review.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Where it is.
    pub path: PathBuf,
    /// What it is for.
    pub kind: Kind,
    /// It is a master (a stack of frames), not one exposure.
    pub master: bool,
    /// Where the kind came from.
    pub source: Source,
    /// Filter name from the header, when it has one.
    pub filter: Option<String>,
    /// Exposure in seconds, when the header has one.
    pub exposure: Option<f64>,
    /// The frame goes into the run.
    pub include: bool,
    /// Why it was left out, or a remark; empty when there is none.
    pub note: String,
}

impl Frame {
    /// Classify the file at `path` from its header.
    #[must_use]
    pub fn from_header(path: &Path, header: &Header) -> Self {
        let from_header = ["IMAGETYP", "FRAMETYP", "FRAME"]
            .iter()
            .find_map(|k| header.get_str(k).and_then(classify_text));
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder = path
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ((kind, master), source) = if let Some(k) = from_header {
            (k, Source::Header)
        } else if let Some(k) = classify_text(&stem) {
            (k, Source::FileName)
        } else if let Some(k) = classify_text(&folder) {
            (k, Source::FolderName)
        } else {
            ((Kind::Other, false), Source::None)
        };
        let filter = ["FILTER", "FILTNAME", "FILTNAM"]
            .iter()
            .find_map(|k| header.get_str(k))
            .filter(|f| !f.is_empty() && !f.eq_ignore_ascii_case("none"))
            .map(str::to_owned);
        Self {
            path: path.to_owned(),
            kind,
            master,
            source,
            filter,
            exposure: header.exposure().filter(|e| e.is_finite() && *e >= 0.0),
            include: kind != Kind::Other,
            note: String::new(),
        }
    }

    /// The file name.
    #[must_use]
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// "Ha, 300 s", "300 s", "Ha" or empty, whichever the header allows.
    #[must_use]
    pub fn setup(&self) -> String {
        let exp = self.exposure.map(fmt_exposure);
        match (&self.filter, exp) {
            (Some(f), Some(e)) => format!("{f}, {e}"),
            (Some(f), None) => f.clone(),
            (None, Some(e)) => e,
            (None, None) => String::new(),
        }
    }
}

/// An exposure as the review prints it: `300 s`, `2.04 s`.
#[must_use]
pub fn fmt_exposure(s: f64) -> String {
    if (s - s.round()).abs() < 0.005 {
        format!("{s:.0} s")
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1000.0)
    } else {
        format!("{s:.2} s")
    }
}

/// Whether two exposures are the same exposure: within 1 percent or 10 ms.
#[must_use]
pub fn same_exposure(a: f64, b: f64) -> bool {
    (a - b).abs() <= (0.01 * a.max(b)).max(0.01)
}

fn filter_is(f: &Frame, name: &str) -> bool {
    f.filter
        .as_deref()
        .is_some_and(|x| x.eq_ignore_ascii_case(name))
}

/// Use the whole set to settle what one header cannot, and leave out what would
/// spoil a calibration. Every change is explained in the frame's `note`.
///
/// * Darks whose exposure matches no light but matches the flats' become dark flats.
/// * Masters: when a kind has raw frames its masters are left out; when it has only
///   masters, the first is used and the others are left out.
/// * The lights of several filters: the filter with most frames is used, the others
///   are left out (a run calibrates and stacks one filter).
/// * Flats and dark flats of a filter that none of the used lights has are left out.
/// * Darks whose exposure matches none of the lights are left out.
pub fn refine(frames: &mut [Frame]) {
    let exposures = |frames: &[Frame], kind: Kind| -> Vec<f64> {
        frames
            .iter()
            .filter(|f| f.kind == kind && !f.master)
            .filter_map(|f| f.exposure)
            .collect()
    };
    let light_exp = exposures(frames, Kind::Light);
    let flat_exp = exposures(frames, Kind::Flat);
    let matches = |set: &[f64], e: f64| set.iter().any(|&x| same_exposure(x, e));
    for f in frames.iter_mut() {
        if f.kind == Kind::Dark
            && !f.master
            && f.source != Source::User
            && let Some(e) = f.exposure
            && !light_exp.is_empty()
            && !matches(&light_exp, e)
            && matches(&flat_exp, e)
        {
            f.kind = Kind::DarkFlat;
            f.source = Source::Exposure;
        }
    }
    // Masters against raw frames, per kind.
    for kind in Kind::CHOICES {
        let raw = frames
            .iter()
            .any(|f| f.kind == kind && !f.master && f.include);
        let mut first_master = true;
        for f in frames
            .iter_mut()
            .filter(|f| f.kind == kind && f.master && f.include)
        {
            if raw {
                f.include = false;
                "a master: the single frames are used".clone_into(&mut f.note);
            } else if first_master {
                first_master = false;
            } else {
                f.include = false;
                "one master per kind is used".clone_into(&mut f.note);
            }
        }
    }
    // One filter of lights.
    let mut by_filter: BTreeMap<Option<String>, usize> = BTreeMap::new();
    for f in frames
        .iter()
        .filter(|f| f.kind == Kind::Light && f.include && !f.master)
    {
        *by_filter.entry(f.filter.clone()).or_default() += 1;
    }
    let main = by_filter
        .iter()
        .filter(|(k, _)| k.is_some())
        .max_by_key(|(_, n)| **n)
        .and_then(|(k, _)| k.clone());
    if by_filter.len() > 1
        && let Some(main) = &main
    {
        for f in frames
            .iter_mut()
            .filter(|f| f.kind == Kind::Light && f.include && !f.master)
        {
            if !filter_is(f, main) {
                f.include = false;
                f.note = format!("another filter than {main}: calibrate it as its own run");
            }
        }
    }
    let used: BTreeSet<String> = frames
        .iter()
        .filter(|f| f.kind == Kind::Light && f.include)
        .filter_map(|f| f.filter.as_deref().map(str::to_ascii_lowercase))
        .collect();
    if !used.is_empty() {
        for f in frames
            .iter_mut()
            .filter(|f| matches!(f.kind, Kind::Flat | Kind::DarkFlat) && f.include)
        {
            if let Some(name) = &f.filter
                && !used.contains(&name.to_ascii_lowercase())
            {
                f.include = false;
                f.note = format!("filter {name}: no light uses it");
            }
        }
    }
    // Darks for other exposures than the lights'.
    let used_light_exp: Vec<f64> = frames
        .iter()
        .filter(|f| f.kind == Kind::Light && f.include && !f.master)
        .filter_map(|f| f.exposure)
        .collect();
    if !used_light_exp.is_empty() {
        let shown = used_light_exp
            .iter()
            .map(|e| fmt_exposure(*e))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        for f in frames
            .iter_mut()
            .filter(|f| f.kind == Kind::Dark && !f.master && f.include)
        {
            if let Some(e) = f.exposure
                && !matches(&used_light_exp, e)
            {
                f.include = false;
                f.note = format!("{} does not match the lights ({shown})", fmt_exposure(e));
            }
        }
    }
}

/// Remarks about the set as a whole, for the top of the review.
#[must_use]
pub fn warnings(frames: &[Frame]) -> Vec<String> {
    let mut out = Vec::new();
    let used = |k: Kind| frames.iter().filter(move |f| f.kind == k && f.include);
    if used(Kind::Light).next().is_none() {
        out.push(
            "No frames are set as lights: assign some, or there is nothing to calibrate.".into(),
        );
    }
    let exps: BTreeSet<String> = used(Kind::Light)
        .filter_map(|f| f.exposure.map(fmt_exposure))
        .collect();
    if exps.len() > 1 {
        out.push(format!(
            "The lights have different exposures ({}): darks must match each, so check them.",
            exps.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if used(Kind::Dark).next().is_none() && used(Kind::Bias).next().is_none() {
        out.push("No darks or bias: the lights are not corrected for thermal signal.".into());
    }
    if used(Kind::Flat).next().is_none() {
        out.push("No flats: vignetting and dust are not corrected.".into());
    }
    if used(Kind::DarkFlat).next().is_none()
        && used(Kind::Flat).next().is_some()
        && used(Kind::Bias).next().is_none()
    {
        out.push("Flats with no dark flats or bias: the flats keep their own offset.".into());
    }
    let unsorted = frames.iter().filter(|f| f.kind == Kind::Other).count();
    if unsorted > 0 {
        out.push(format!(
            "{unsorted} frame{} could not be sorted: assign them or leave them out.",
            if unsorted == 1 { "" } else { "s" }
        ));
    }
    out
}

/// What [`scan`] found.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// Every FITS file whose header could be read, sorted by path.
    pub frames: Vec<Frame>,
    /// How many FITS files each folder holds (of the folders that were walked).
    pub dir_totals: HashMap<PathBuf, usize>,
    /// Files that look like FITS but could not be read.
    pub unreadable: usize,
    /// The scan stopped at [`MAX_FILES`].
    pub truncated: bool,
}

fn is_fits(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts"))
}

/// Find the FITS files under `inputs` (files, or folders searched to [`MAX_DEPTH`]) and
/// read their headers. Hidden folders and the [`STAGE_DIR`] are skipped.
///
/// # Errors
/// Nothing was found, or the user cancelled.
pub fn scan(inputs: &[PathBuf], progress: &Progress) -> Result<Scan, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut totals: HashMap<PathBuf, usize> = HashMap::new();
    let mut truncated = false;
    let mut stack: Vec<(PathBuf, usize)> = Vec::new();
    for p in inputs {
        if p.is_dir() {
            stack.push((p.clone(), 0));
        } else if is_fits(p) {
            files.push(p.clone());
        }
    }
    while let Some((dir, depth)) = stack.pop() {
        progress.say(&format!("Looking in {}", dir.display()));
        progress.check()?;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here = 0;
        for e in entries.filter_map(Result::ok) {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir() {
                if depth < MAX_DEPTH && !name.starts_with('.') && name != STAGE_DIR {
                    stack.push((path, depth + 1));
                }
            } else if is_fits(&path) {
                if files.len() >= MAX_FILES {
                    truncated = true;
                    continue;
                }
                here += 1;
                files.push(path);
            }
        }
        totals.insert(dir, here);
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err("No FITS files (.fit, .fits, .fts) were found there.".into());
    }
    progress.set_total(files.len());
    let mut frames = Vec::with_capacity(files.len());
    let mut unreadable = 0;
    for (i, path) in files.iter().enumerate() {
        if i % 16 == 0 {
            progress.say(&format!("Reading headers: {} of {}", i + 1, files.len()));
            progress.check()?;
        }
        match fits::read_header(path) {
            Ok(h) => frames.push(Frame::from_header(path, &h)),
            Err(_) => unreadable += 1,
        }
        progress.tick();
    }
    if frames.is_empty() {
        return Err("None of the FITS files could be read.".into());
    }
    Ok(Scan {
        frames,
        dir_totals: totals,
        unreadable,
        truncated,
    })
}

/// The frames of a scan being reviewed.
#[derive(Clone, Debug, Default)]
pub struct Review {
    /// The folder that was dropped (or the common parent of the files).
    pub root: PathBuf,
    /// Every frame.
    pub frames: Vec<Frame>,
    /// Frames per folder, to tell a whole folder from some of it.
    pub dir_totals: HashMap<PathBuf, usize>,
    /// Files that could not be read.
    pub unreadable: usize,
    /// The scan stopped early.
    pub truncated: bool,
}

/// The deepest folder that holds all of `paths`.
#[must_use]
pub fn common_root(paths: &[PathBuf]) -> PathBuf {
    let dirs: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            if p.is_dir() {
                p.clone()
            } else {
                p.parent().map(Path::to_path_buf).unwrap_or_default()
            }
        })
        .collect();
    let Some(first) = dirs.first() else {
        return PathBuf::new();
    };
    let mut root = first.clone();
    for d in &dirs[1..] {
        while !d.starts_with(&root) {
            if !root.pop() {
                break;
            }
        }
    }
    root
}

impl Review {
    /// Start a review from a scan of `inputs`: classify, then [`refine`].
    #[must_use]
    pub fn new(inputs: &[PathBuf], scan: Scan) -> Self {
        let mut frames = scan.frames;
        refine(&mut frames);
        Self {
            root: common_root(inputs),
            frames,
            dir_totals: scan.dir_totals,
            unreadable: scan.unreadable,
            truncated: scan.truncated,
        }
    }

    /// How many frames of `kind` there are, and how many of them are included.
    #[must_use]
    pub fn count(&self, kind: Kind) -> (usize, usize) {
        let all = self.frames.iter().filter(|f| f.kind == kind);
        (all.clone().count(), all.filter(|f| f.include).count())
    }

    /// Assign frame `i` to `kind`; it is included unless the kind is "unsorted".
    pub fn assign(&mut self, i: usize, kind: Kind) {
        if let Some(f) = self.frames.get_mut(i) {
            f.kind = kind;
            f.source = Source::User;
            f.include = kind != Kind::Other;
            f.note.clear();
        }
    }

    /// Assign every frame of kind `from` to `to`.
    pub fn assign_all(&mut self, from: Kind, to: Kind) {
        for i in 0..self.frames.len() {
            if self.frames[i].kind == from {
                self.assign(i, to);
            }
        }
    }

    /// Include or leave out every frame of `kind`.
    pub fn include_all(&mut self, kind: Kind, include: bool) {
        for f in self.frames.iter_mut().filter(|f| f.kind == kind) {
            f.include = include;
            if include {
                f.note.clear();
            }
        }
    }

    /// Remarks about the set as it stands.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut w = warnings(&self.frames);
        if self.unreadable > 0 {
            w.push(format!(
                "{} FITS file{} could not be read and {} left out.",
                self.unreadable,
                if self.unreadable == 1 { "" } else { "s" },
                if self.unreadable == 1 { "is" } else { "are" }
            ));
        }
        if self.truncated {
            w.push(format!(
                "Only the first {MAX_FILES} files were read: drop a smaller folder."
            ));
        }
        w
    }

    /// The included frames of `kind`, in path order.
    #[must_use]
    pub fn files(&self, kind: Kind) -> Vec<&Path> {
        self.frames
            .iter()
            .filter(|f| f.kind == kind && f.include)
            .map(|f| f.path.as_path())
            .collect()
    }

    /// Where each kind's frames are to be taken from.
    #[must_use]
    pub fn resolve(&self) -> Vec<(Kind, Folder)> {
        Kind::CHOICES
            .into_iter()
            .filter_map(|k| {
                let files = self.files(k);
                if files.is_empty() {
                    return None;
                }
                Some((k, resolve_files(&files, &self.dir_totals)))
            })
            .collect()
    }
}

/// Where the frames of one kind are taken from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Folder {
    /// All of a folder, and nothing else: use the folder itself.
    Whole(PathBuf),
    /// A single file (a master, or one frame).
    File(PathBuf),
    /// Some files, from one folder or several: gather them first.
    Gather(Vec<PathBuf>),
}

/// Decide whether `files` are a whole folder, one file or a selection.
#[must_use]
pub fn resolve_files(files: &[&Path], totals: &HashMap<PathBuf, usize>) -> Folder {
    if let [one] = files {
        return Folder::File((*one).to_owned());
    }
    let dir = files.first().and_then(|f| f.parent());
    if let Some(dir) = dir
        && files.iter().all(|f| f.parent() == Some(dir))
        && totals.get(dir) == Some(&files.len())
    {
        return Folder::Whole(dir.to_owned());
    }
    Folder::Gather(files.iter().map(|f| (*f).to_owned()).collect())
}

/// Gather `files` in `root/valerian-sorted/<kind>/` as links (hard links, else copies)
/// and return that folder. Names that clash get the folder name in front. A folder
/// left from an earlier run is emptied of its FITS files first.
///
/// # Errors
/// The folder cannot be made or a file cannot be linked or copied.
pub fn stage(
    root: &Path,
    kind: Kind,
    files: &[PathBuf],
    progress: &Progress,
) -> Result<PathBuf, String> {
    let dir = root.join(STAGE_DIR).join(kind.dir_name());
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot make {}: {e}", dir.display()))?;
    if let Ok(old) = std::fs::read_dir(&dir) {
        for e in old.filter_map(Result::ok) {
            if is_fits(&e.path()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut used: BTreeSet<String> = BTreeSet::new();
    for f in files {
        progress.check()?;
        let mut name = f
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if used.contains(&name) {
            let up = f
                .parent()
                .and_then(Path::file_name)
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            name = format!("{up}-{name}");
        }
        let mut n = 2;
        while used.contains(&name) {
            name = format!("{n}-{name}");
            n += 1;
        }
        used.insert(name.clone());
        let target = dir.join(&name);
        if std::fs::hard_link(f, &target).is_err() {
            std::fs::copy(f, &target)
                .map_err(|e| format!("Cannot copy {} to {}: {e}", f.display(), target.display()))?;
        }
        progress.tick();
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use valerian_core::fits::Value;

    fn kind(text: &str) -> Option<(Kind, bool)> {
        classify_text(text)
    }

    #[test]
    fn imagetyp_variants_are_understood() {
        for (t, k) in [
            ("Light Frame", Kind::Light),
            ("LIGHT", Kind::Light),
            ("light", Kind::Light),
            ("Object", Kind::Light),
            ("Science", Kind::Light),
            ("Light", Kind::Light),
            ("Dark Frame", Kind::Dark),
            ("DARK", Kind::Dark),
            ("Flat Field", Kind::Flat),
            ("Flat Frame", Kind::Flat),
            ("FLAT", Kind::Flat),
            ("Flat", Kind::Flat),
            ("Bias Frame", Kind::Bias),
            ("BIAS", Kind::Bias),
            ("Zero", Kind::Bias),
            ("Offset", Kind::Bias),
            ("DARKFLAT", Kind::DarkFlat),
            ("Dark Flat", Kind::DarkFlat),
            ("dark_flat", Kind::DarkFlat),
            ("Flat Dark", Kind::DarkFlat),
            ("  Light_Frame  ", Kind::Light),
        ] {
            assert_eq!(kind(t), Some((k, false)), "{t}");
        }
    }

    #[test]
    fn masters_are_flagged() {
        for (t, k) in [
            ("Master Dark", Kind::Dark),
            ("MasterFlat", Kind::Flat),
            ("master_bias", Kind::Bias),
            ("Master Light", Kind::Light),
            ("Master Dark Flat", Kind::DarkFlat),
        ] {
            assert_eq!(kind(t), Some((k, true)), "{t}");
        }
    }

    #[test]
    fn unknown_text_names_no_kind() {
        for t in [
            "",
            "Unknown",
            "M31_final",
            "IC 5070_sub",
            "Snapshot",
            "2023-01-26",
        ] {
            assert_eq!(kind(t), None, "{t:?}");
        }
    }

    fn header(imagetyp: Option<&str>, filter: Option<&str>, exp: Option<f64>) -> Header {
        let mut h = Header::new();
        if let Some(t) = imagetyp {
            h.set("IMAGETYP", Value::Str(t.into()), "");
        }
        if let Some(f) = filter {
            h.set("FILTER", Value::Str(f.into()), "");
        }
        if let Some(e) = exp {
            h.set("EXPTIME", Value::Float(e), "");
        }
        h
    }

    fn frame(path: &str, imagetyp: Option<&str>, filter: Option<&str>, exp: Option<f64>) -> Frame {
        Frame::from_header(Path::new(path), &header(imagetyp, filter, exp))
    }

    #[test]
    fn the_header_wins_then_the_file_name_then_the_folder() {
        let f = frame("/d/Light_01.fit", Some("Dark Frame"), None, None);
        assert_eq!((f.kind, f.source), (Kind::Dark, Source::Header));
        let f = frame("/d/Light_M31_001.fit", None, None, None);
        assert_eq!((f.kind, f.source), (Kind::Light, Source::FileName));
        let f = frame("/d/FLAT/0001.fits", None, None, None);
        assert_eq!((f.kind, f.source), (Kind::Flat, Source::FolderName));
        let f = frame("/d/misc/0001.fits", Some("Unknown"), None, None);
        assert_eq!(
            (f.kind, f.source, f.include),
            (Kind::Other, Source::None, false)
        );
        // A key we do not read, or another key.
        let mut h = Header::new();
        h.set("FRAME", Value::Str("Bias".into()), "");
        assert_eq!(
            Frame::from_header(Path::new("/d/x.fit"), &h).kind,
            Kind::Bias
        );
    }

    #[test]
    fn filter_and_exposure_come_from_the_header_with_fallbacks() {
        let f = frame("/d/a.fit", Some("Light"), Some("Ha"), Some(300.0));
        assert_eq!((f.filter.as_deref(), f.exposure), (Some("Ha"), Some(300.0)));
        assert_eq!(f.setup(), "Ha, 300 s");
        let f = frame("/d/a.fit", Some("Light"), Some("None"), None);
        assert_eq!((f.filter, f.exposure), (None, None));
        let mut h = Header::new();
        h.set("EXPOSURE", Value::Float(2.04), "");
        assert_eq!(
            Frame::from_header(Path::new("/d/a.fit"), &h).setup(),
            "2.04 s"
        );
        assert_eq!(fmt_exposure(0.25), "250 ms");
    }

    fn set(specs: &[(&str, &str, Option<&str>, Option<f64>)]) -> Vec<Frame> {
        let mut v: Vec<Frame> = specs
            .iter()
            .enumerate()
            .map(|(i, (t, _, f, e))| frame(&format!("/n/{i:03}.fits"), Some(t), *f, *e))
            .collect();
        refine(&mut v);
        v
    }

    #[test]
    fn darks_with_the_flats_exposure_become_dark_flats() {
        let v = set(&[
            ("Light", "", Some("Ha"), Some(300.0)),
            ("Dark", "", Some("Ha"), Some(300.0)),
            ("Dark", "", Some("Ha"), Some(2.04)),
            ("Flat", "", Some("Ha"), Some(2.04)),
        ]);
        assert_eq!(v[1].kind, Kind::Dark);
        assert_eq!(v[2].kind, Kind::DarkFlat);
        assert_eq!(v[2].source, Source::Exposure);
        assert!(v[2].include);
    }

    #[test]
    fn darks_that_match_no_light_are_left_out_with_a_reason() {
        let v = set(&[
            ("Light", "", None, Some(60.0)),
            ("Dark", "", None, Some(60.0)),
            ("Dark", "", None, Some(120.0)),
        ]);
        assert!(v[1].include);
        assert!(!v[2].include);
        assert!(
            v[2].note.contains("does not match the lights (60 s)"),
            "{}",
            v[2].note
        );
    }

    #[test]
    fn flats_of_a_filter_nobody_uses_are_left_out() {
        let v = set(&[
            ("Light", "", Some("Ha"), Some(300.0)),
            ("Flat", "", Some("ha"), Some(2.0)),
            ("Flat", "", Some("OIII"), Some(2.0)),
        ]);
        assert!(v[1].include, "case does not matter");
        assert!(!v[2].include);
        assert!(v[2].note.contains("OIII"));
    }

    #[test]
    fn several_filters_of_lights_keep_the_larger_set() {
        let v = set(&[
            ("Light", "", Some("Ha"), Some(300.0)),
            ("Light", "", Some("Ha"), Some(300.0)),
            ("Light", "", Some("OIII"), Some(300.0)),
        ]);
        assert!(v[0].include && v[1].include);
        assert!(!v[2].include);
        assert!(v[2].note.contains("another filter than Ha"));
    }

    #[test]
    fn masters_give_way_to_single_frames_and_one_master_is_enough() {
        let v = set(&[
            ("Dark", "", None, None),
            ("Master Dark", "", None, None),
            ("Master Flat", "", None, None),
            ("Master Flat", "", None, None),
        ]);
        assert!(v[0].include);
        assert!(!v[1].include && v[1].note.contains("single frames"));
        assert!(v[2].include);
        assert!(!v[3].include && v[3].note.contains("one master"));
    }

    #[test]
    fn missing_keys_leave_everything_in() {
        let v = set(&[
            ("Light", "", None, None),
            ("Dark", "", None, None),
            ("Flat", "", None, None),
        ]);
        assert!(v.iter().all(|f| f.include));
    }

    #[test]
    fn warnings_say_what_is_missing() {
        let v = set(&[("Light", "", None, Some(60.0))]);
        let w = warnings(&v).join("\n");
        assert!(w.contains("No darks or bias"), "{w}");
        assert!(w.contains("No flats"), "{w}");
        let v = set(&[("Dark", "", None, None)]);
        assert!(warnings(&v)[0].contains("No frames are set as lights"));
        let mut v = set(&[
            ("Light", "", None, Some(60.0)),
            ("Light", "", None, Some(30.0)),
        ]);
        v.push(frame("/n/x.fits", None, None, None));
        let w = warnings(&v).join("\n");
        assert!(w.contains("different exposures (30 s, 60 s)"), "{w}");
        assert!(w.contains("1 frame could not be sorted"), "{w}");
    }

    #[test]
    fn a_whole_folder_one_file_or_a_selection() {
        let mut totals = HashMap::new();
        totals.insert(PathBuf::from("/d/dark"), 3);
        totals.insert(PathBuf::from("/d/mixed"), 5);
        let a = [
            Path::new("/d/dark/1.fit"),
            Path::new("/d/dark/2.fit"),
            Path::new("/d/dark/3.fit"),
        ];
        assert_eq!(resolve_files(&a, &totals), Folder::Whole("/d/dark".into()));
        assert_eq!(
            resolve_files(&a[..2], &totals),
            Folder::Gather(vec!["/d/dark/1.fit".into(), "/d/dark/2.fit".into()])
        );
        assert_eq!(
            resolve_files(&a[..1], &totals),
            Folder::File("/d/dark/1.fit".into())
        );
        let b = [Path::new("/d/dark/1.fit"), Path::new("/d/mixed/2.fit")];
        assert!(matches!(resolve_files(&b, &totals), Folder::Gather(_)));
    }

    #[test]
    fn the_common_root_is_the_deepest_shared_folder() {
        let r = common_root(&[PathBuf::from("/d/a/x.fit"), PathBuf::from("/d/b/y.fit")]);
        assert_eq!(r, PathBuf::from("/d"));
        assert_eq!(
            common_root(&[PathBuf::from("/d/a/x.fit")]),
            PathBuf::from("/d/a")
        );
    }

    #[test]
    fn reassigning_and_excluding_change_what_is_resolved() {
        let frames = set(&[
            ("Light", "", None, None),
            ("Dark", "", None, None),
            ("Dark", "", None, None),
        ]);
        let mut r = Review {
            root: "/n".into(),
            frames,
            ..Review::default()
        };
        assert_eq!(r.count(Kind::Dark), (2, 2));
        r.assign(1, Kind::Flat);
        assert_eq!(r.count(Kind::Dark), (1, 1));
        assert_eq!(r.count(Kind::Flat), (1, 1));
        assert_eq!(r.frames[1].source, Source::User);
        r.frames[2].include = false;
        assert_eq!(r.files(Kind::Dark).len(), 0);
        r.include_all(Kind::Dark, true);
        assert_eq!(r.files(Kind::Dark).len(), 1);
        r.assign_all(Kind::Dark, Kind::Other);
        assert_eq!(r.count(Kind::Other), (1, 0));
        let kinds: Vec<Kind> = r.resolve().into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, vec![Kind::Light, Kind::Flat]);
    }

    fn write_frame(path: &Path, imagetyp: Option<&str>, filter: Option<&str>, exp: f64) {
        let mut img = valerian_core::fits::Image::mono(
            valerian_core::testutil::deep_sky(16, 16, 4, 1).frame,
            Header::new(),
        );
        if let Some(t) = imagetyp {
            img.header.set("IMAGETYP", Value::Str(t.into()), "");
        }
        if let Some(f) = filter {
            img.header.set("FILTER", Value::Str(f.into()), "");
        }
        img.header.set("EXPTIME", Value::Float(exp), "");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fits::write(path, &img, fits::Format::Float32).expect("write");
    }

    #[test]
    fn a_folder_is_scanned_sorted_and_resolved_to_folders() {
        let dir = tempfile::tempdir().expect("tmp");
        let d = dir.path();
        for k in 0..4 {
            write_frame(
                &d.join(format!("night/LIGHT/l{k}.fits")),
                Some("Light Frame"),
                Some("Ha"),
                60.0,
            );
        }
        for k in 0..3 {
            write_frame(
                &d.join(format!("night/DARK/d{k}.fits")),
                Some("Dark Frame"),
                Some("Ha"),
                60.0,
            );
            write_frame(
                &d.join(format!("cal/f{k}.fits")),
                Some("Flat Field"),
                Some("Ha"),
                2.0,
            );
            write_frame(
                &d.join(format!("cal/df{k}.fits")),
                Some("Dark Frame"),
                Some("Ha"),
                2.0,
            );
        }
        // Not FITS, hidden, and a bad FITS.
        std::fs::write(d.join("night/notes.txt"), b"x").expect("write");
        std::fs::create_dir_all(d.join(".hidden")).expect("dir");
        write_frame(&d.join(".hidden/h.fits"), Some("Light"), None, 1.0);
        std::fs::write(d.join("night/broken.fits"), b"not fits").expect("write");
        let inputs = [d.to_path_buf()];
        let s = scan(&inputs, &Progress::default()).expect("scan");
        assert_eq!(s.frames.len(), 4 + 9);
        assert_eq!(s.unreadable, 1);
        let r = Review::new(&inputs, s);
        assert_eq!(r.root, d);
        assert_eq!(r.count(Kind::Light), (4, 4));
        assert_eq!(r.count(Kind::Dark), (3, 3));
        assert_eq!(r.count(Kind::DarkFlat), (3, 3));
        assert_eq!(r.count(Kind::Flat), (3, 3));
        let resolved = r.resolve();
        assert_eq!(
            resolved[0],
            (Kind::Light, Folder::Whole(d.join("night/LIGHT")))
        );
        assert_eq!(
            resolved[1],
            (Kind::Dark, Folder::Whole(d.join("night/DARK")))
        );
        // Flats and dark flats share a folder: each has to be gathered.
        assert!(matches!(resolved[2], (Kind::DarkFlat, Folder::Gather(_))));
        assert!(matches!(resolved[3], (Kind::Flat, Folder::Gather(_))));
        assert!(r.warnings().iter().any(|w| w.contains("could not be read")));

        let (_, Folder::Gather(files)) = &resolved[3] else {
            panic!("flats gather");
        };
        let out = stage(d, Kind::Flat, files, &Progress::default()).expect("stage");
        assert_eq!(out, d.join(STAGE_DIR).join("flats"));
        assert_eq!(std::fs::read_dir(&out).expect("read").count(), 3);
        // Staging again does not pile up, and a rescan does not see the links.
        stage(d, Kind::Flat, files, &Progress::default()).expect("again");
        assert_eq!(std::fs::read_dir(&out).expect("read").count(), 3);
        let again = scan(&inputs, &Progress::default()).expect("rescan");
        assert_eq!(again.frames.len(), 13);
    }

    #[test]
    fn staging_keeps_clashing_names_apart() {
        let dir = tempfile::tempdir().expect("tmp");
        let a = dir.path().join("a/f.fits");
        let b = dir.path().join("b/f.fits");
        write_frame(&a, Some("Flat"), None, 1.0);
        write_frame(&b, Some("Flat"), None, 1.0);
        let out = stage(dir.path(), Kind::Flat, &[a, b], &Progress::default()).expect("stage");
        let mut names: Vec<String> = std::fs::read_dir(out)
            .expect("read")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["b-f.fits", "f.fits"]);
    }

    #[test]
    fn a_folder_without_fits_says_so() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("a.txt"), b"x").expect("write");
        let e = scan(&[dir.path().to_path_buf()], &Progress::default()).expect_err("none");
        assert!(e.contains("No FITS files"), "{e}");
    }

    #[test]
    fn a_cancelled_scan_stops() {
        let dir = tempfile::tempdir().expect("tmp");
        write_frame(&dir.path().join("a.fits"), Some("Light"), None, 1.0);
        let p = Progress::default();
        p.cancel();
        assert!(scan(&[dir.path().to_path_buf()], &p).is_err());
    }
}
