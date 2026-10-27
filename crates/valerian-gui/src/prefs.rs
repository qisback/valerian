// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The settings file: the window size, night-vision mode and the images opened lately.
//!
//! A plain `key=value` text file so it can be read and edited by hand:
//!
//! ```text
//! window=1200x800
//! night=1
//! recent=/home/me/m31/stack.fits
//! session=/data/m31/lights<TAB>/data/m31/darks<TAB>/data/m31/flats<TAB><TAB>
//! ```
//!
//! A `session` line is a set of frame folders (lights, darks, flats, dark flats,
//! bias, separated by tabs, empty when not used) that was worked on lately.
//!
//! It is `$VALERIAN_GUI_CONFIG` if that is set, else `%APPDATA%\valerian\gui.conf`,
//! else `$XDG_CONFIG_HOME/valerian/gui.conf`, else `~/.config/valerian/gui.conf`. A
//! missing or damaged file is not an error: the defaults apply, and nothing is
//! written until something changes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The window size used when none is remembered.
pub const DEFAULT_SIZE: [f32; 2] = [1200.0, 800.0];

/// How many recent images are remembered.
pub const MAX_RECENT: usize = 8;

/// How many recent sessions are remembered.
pub const MAX_SESSIONS: usize = 8;

/// The folders of one working session: where the frames of each kind are.
///
/// The lights folder names the session; the others are empty when not used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderSet {
    /// Light frames.
    pub lights: String,
    /// Dark frames, or a master dark.
    pub darks: String,
    /// Flat frames, or a master flat.
    pub flats: String,
    /// Dark flats.
    pub dark_flats: String,
    /// Bias frames, or a master bias.
    pub bias: String,
}

impl FolderSet {
    /// The folders that are named, with what they are for, in a fixed order.
    #[must_use]
    pub fn named(&self) -> Vec<(&'static str, &str)> {
        [
            ("lights", self.lights.as_str()),
            ("darks", self.darks.as_str()),
            ("flats", self.flats.as_str()),
            ("dark flats", self.dark_flats.as_str()),
            ("bias", self.bias.as_str()),
        ]
        .into_iter()
        .filter(|(_, p)| !p.trim().is_empty())
        .collect()
    }

    /// Whether any calibration folder (not the lights) is named.
    #[must_use]
    pub fn has_calibration(&self) -> bool {
        !(self.darks.trim().is_empty()
            && self.flats.trim().is_empty()
            && self.dark_flats.trim().is_empty()
            && self.bias.trim().is_empty())
    }

    /// The kinds whose folder is named but no longer exists.
    #[must_use]
    pub fn missing(&self) -> Vec<&'static str> {
        self.named()
            .into_iter()
            .filter(|(_, p)| !Path::new(p).exists())
            .map(|(k, _)| k)
            .collect()
    }

    /// Every path made absolute, so the entry means the same after the working folder changes.
    #[must_use]
    pub fn absolute(&self) -> Self {
        let abs = |s: &str| {
            if s.trim().is_empty() {
                String::new()
            } else {
                std::path::absolute(s.trim()).map_or_else(
                    |_| s.trim().to_owned(),
                    |p| p.to_string_lossy().into_owned(),
                )
            }
        };
        Self {
            lights: abs(&self.lights),
            darks: abs(&self.darks),
            flats: abs(&self.flats),
            dark_flats: abs(&self.dark_flats),
            bias: abs(&self.bias),
        }
    }

    /// The short name shown in the list: the last two parts of the lights path.
    #[must_use]
    pub fn title(&self) -> String {
        let p = Path::new(self.lights.trim_end_matches(std::path::is_separator));
        let leaf = p.file_name().map(|n| n.to_string_lossy().into_owned());
        let up = p
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        match (up, leaf) {
            (Some(u), Some(l)) => format!("{u} / {l}"),
            (None, Some(l)) => l,
            _ => self.lights.clone(),
        }
    }

    fn to_line(&self) -> String {
        [
            &self.lights,
            &self.darks,
            &self.flats,
            &self.dark_flats,
            &self.bias,
        ]
        .map(|s| s.replace(['\t', '\n'], " "))
        .join("\t")
    }

    fn from_line(v: &str) -> Option<Self> {
        let mut it = v.split('\t').map(|s| s.trim().to_owned());
        let set = Self {
            lights: it.next()?,
            darks: it.next().unwrap_or_default(),
            flats: it.next().unwrap_or_default(),
            dark_flats: it.next().unwrap_or_default(),
            bias: it.next().unwrap_or_default(),
        };
        let ok = |s: &str| s.is_empty() || Path::new(s).is_absolute();
        (!set.lights.is_empty()
            && ok(&set.lights)
            && ok(&set.darks)
            && ok(&set.flats)
            && ok(&set.dark_flats)
            && ok(&set.bias))
        .then_some(set)
    }
}

/// Persisted choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prefs {
    /// Inner size of the window when it was last closed, in points.
    pub window: Option<[f32; 2]>,
    /// Images opened lately, newest first (at most [`MAX_RECENT`]).
    pub recent: Vec<PathBuf>,
    /// Frame-folder sessions worked on lately, newest first (at most [`MAX_SESSIONS`]).
    pub sessions: Vec<FolderSet>,
    /// The red night-vision palette is on.
    pub night: bool,
    path: Option<PathBuf>,
}

fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VALERIAN_GUI_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let dir = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(dir.join("valerian").join("gui.conf"))
}

/// A size worth restoring: not tiny, not absurd.
fn valid(s: [f32; 2]) -> bool {
    (640.0..=16384.0).contains(&s[0]) && (480.0..=16384.0).contains(&s[1])
}

fn parse_window(v: &str) -> Option<[f32; 2]> {
    let (w, h) = v.split_once('x')?;
    let s = [w.trim().parse().ok()?, h.trim().parse().ok()?];
    valid(s).then_some(s)
}

impl Prefs {
    /// Read the default file (or start empty, still remembering where to save).
    #[must_use]
    pub fn load_default() -> Self {
        let Some(path) = default_path() else {
            return Self::default();
        };
        // A settings file is a few lines; refuse to read anything large.
        let text = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() <= 64 * 1024)
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        let mut p = Self::parse(&text);
        p.path = Some(path);
        p
    }

    /// Parse file text; unknown or malformed lines are skipped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines().map(str::trim) {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            if k == "session" {
                if let Some(set) = FolderSet::from_line(v)
                    && p.sessions.len() < MAX_SESSIONS
                    && !p.sessions.iter().any(|s| s.lights == set.lights)
                {
                    p.sessions.push(set);
                }
            } else if k == "window" {
                p.window = parse_window(v);
            } else if k == "night" {
                p.night = v == "1";
            } else if k == "recent" {
                let f = PathBuf::from(v);
                if f.is_absolute() && p.recent.len() < MAX_RECENT && !p.recent.contains(&f) {
                    p.recent.push(f);
                }
            }
        }
        p
    }

    /// The file text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::from("# Valerian GUI settings\n");
        if let Some([w, h]) = self.window {
            let _ = writeln!(out, "window={}x{}", w.round(), h.round());
        }
        if self.night {
            out.push_str(
                "night=1
",
            );
        }
        for f in &self.recent {
            let _ = writeln!(out, "recent={}", f.display());
        }
        for s in &self.sessions {
            let _ = writeln!(out, "session={}", s.to_line());
        }
        out
    }

    /// Write the file if this is a file-backed instance. Failure is ignored:
    /// losing a remembered choice is not worth interrupting the user.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.to_text());
    }

    /// Whether this instance is backed by a settings file (tests and snapshots are not).
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    /// The remembered window size, or the default.
    #[must_use]
    pub fn window_or_default(&self) -> [f32; 2] {
        self.window.unwrap_or(DEFAULT_SIZE)
    }

    /// Remember the window size when it differs from the saved one.
    pub fn remember_window(&mut self, size: [f32; 2]) {
        let size = [size[0].round(), size[1].round()];
        if valid(size) && self.window != Some(size) {
            self.window = Some(size);
            self.save();
        }
    }

    /// Remember the night-vision choice.
    pub fn remember_night(&mut self, on: bool) {
        if self.night != on {
            self.night = on;
            self.save();
        }
    }

    /// Put `file` at the head of the recent list and save.
    pub fn remember_recent(&mut self, file: &Path) {
        if !file.is_absolute() || self.recent.first().is_some_and(|f| f == file) {
            return;
        }
        self.recent.retain(|f| f != file);
        self.recent.insert(0, file.to_owned());
        self.recent.truncate(MAX_RECENT);
        self.save();
    }

    /// Put `set` at the head of the session list and save. A set without lights is ignored.
    pub fn remember_session(&mut self, set: &FolderSet) {
        let set = set.absolute();
        if set.lights.is_empty() || self.sessions.first() == Some(&set) {
            return;
        }
        self.sessions.retain(|s| s.lights != set.lights);
        self.sessions.insert(0, set);
        self.sessions.truncate(MAX_SESSIONS);
        self.save();
    }

    /// Forget the session whose lights are in `lights` and save.
    pub fn forget_session(&mut self, lights: &str) {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.lights != lights);
        if self.sessions.len() != before {
            self.save();
        }
    }

    /// Forget every session and every recent image, and save.
    pub fn clear_recents(&mut self) {
        if !self.sessions.is_empty() || !self.recent.is_empty() {
            self.sessions.clear();
            self.recent.clear();
            self.save();
        }
    }

    /// Forget one recent file (it is gone) and save.
    pub fn forget_recent(&mut self, file: &Path) {
        let before = self.recent.len();
        self.recent.retain(|f| f != file);
        if self.recent.len() != before {
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_recent_round_trip_and_junk_is_ignored() {
        let a = std::env::temp_dir().join("a.fits");
        let b = std::env::temp_dir().join("b.fits");
        let mut p = Prefs::default();
        p.remember_recent(&a);
        p.remember_recent(&b);
        p.remember_recent(&a);
        p.remember_recent(Path::new("relative.fits"));
        assert_eq!(p.recent, vec![a.clone(), b.clone()]);
        p.remember_window([1500.4, 900.0]);
        p.remember_window([10.0, 10.0]);
        assert_eq!(p.window, Some([1500.0, 900.0]));
        let q = Prefs::parse(&p.to_text());
        assert_eq!(q.recent, vec![a.clone(), b]);
        assert_eq!(q.window, Some([1500.0, 900.0]));
        assert!(!q.night);
        p.remember_night(true);
        assert!(Prefs::parse(&p.to_text()).night);
        assert_eq!(Prefs::parse("window=axb\nwindow=100x100\n").window, None);
        p.forget_recent(&a);
        assert_eq!(p.recent.len(), 1);
    }

    fn set(dir: &Path, name: &str) -> FolderSet {
        FolderSet {
            lights: dir.join(name).join("lights").to_string_lossy().into_owned(),
            darks: dir.join(name).join("darks").to_string_lossy().into_owned(),
            ..FolderSet::default()
        }
    }

    #[test]
    fn sessions_round_trip_move_to_the_front_and_are_capped() {
        let d = std::env::temp_dir();
        let mut p = Prefs::default();
        p.remember_session(&set(&d, "m31"));
        p.remember_session(&set(&d, "m42"));
        p.remember_session(&set(&d, "m31"));
        assert_eq!(p.sessions.len(), 2);
        assert!(p.sessions[0].lights.contains("m31"));
        p.remember_session(&FolderSet::default());
        assert_eq!(p.sessions.len(), 2);
        let q = Prefs::parse(&p.to_text());
        assert_eq!(q.sessions, p.sessions);
        assert_eq!(q.sessions[0].flats, "");
        for i in 0..20 {
            p.remember_session(&set(&d, &format!("s{i}")));
        }
        assert_eq!(p.sessions.len(), MAX_SESSIONS);
        let first = p.sessions[0].lights.clone();
        p.forget_session(&first);
        assert_eq!(p.sessions.len(), MAX_SESSIONS - 1);
        p.clear_recents();
        assert!(p.sessions.is_empty() && p.recent.is_empty());
    }

    #[test]
    fn damaged_session_lines_are_skipped() {
        let abs = std::env::temp_dir().join("ok").join("lights");
        let abs = abs.to_string_lossy();
        let t = format!(
            "session=\nsession=relative/lights\nsession={abs}\t\t\t\t\nsession={abs}\tbad\n"
        );
        let p = Prefs::parse(&t);
        assert_eq!(p.sessions.len(), 1);
        assert_eq!(p.sessions[0].lights, abs);
    }

    #[test]
    fn a_session_knows_which_folders_are_gone() {
        let dir = tempfile::tempdir().expect("tmp");
        let here = dir.path().join("lights");
        std::fs::create_dir(&here).expect("mkdir");
        let s = FolderSet {
            lights: here.to_string_lossy().into_owned(),
            darks: dir.path().join("gone").to_string_lossy().into_owned(),
            ..FolderSet::default()
        };
        assert_eq!(s.missing(), vec!["darks"]);
        assert!(s.has_calibration());
        assert_eq!(s.named().len(), 2);
        assert!(s.title().ends_with("lights"));
    }

    #[test]
    fn the_recent_list_is_capped() {
        let mut p = Prefs::default();
        for i in 0..20 {
            p.remember_recent(&std::env::temp_dir().join(format!("{i}.fits")));
        }
        assert_eq!(p.recent.len(), MAX_RECENT);
    }
}
