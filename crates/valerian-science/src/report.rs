// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The session report: one self-contained HTML page that gathers every output.
//!
//! A report folder holds each output's PNG, CSV and JSON, a `report.json`
//! manifest of the sections and `report.html`, which embeds the images as
//! data URIs so the page can be mailed or archived on its own. Each command
//! adds or replaces its sections and the page is rebuilt from the manifest,
//! so a session's outputs accumulate across commands.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::table::write_file;
use crate::{Error, Result};

/// An image of a section.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Figure {
    /// File name in the report folder.
    pub file: String,
    /// Caption under the image.
    pub caption: String,
}

/// A table of a section.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Table {
    /// Column titles.
    pub header: Vec<String>,
    /// The rows.
    pub rows: Vec<Vec<String>>,
    /// Rows whose cell in this column is `rejected` are shaded.
    pub flag_column: Option<usize>,
}

/// One part of the report.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Section {
    /// Stable identifier; adding a section with the same id replaces it.
    pub id: String,
    /// Heading.
    pub title: String,
    /// Paragraphs.
    pub text: Vec<String>,
    /// Images.
    pub figures: Vec<Figure>,
    /// Data files beside the report, linked at the foot of the section.
    pub files: Vec<String>,
    /// An optional table.
    pub table: Option<Table>,
}

/// A report folder.
#[derive(Debug)]
pub struct ReportDir {
    dir: PathBuf,
    sections: Vec<Section>,
}

impl ReportDir {
    /// Open (and create) a report folder, reading its manifest if it has one.
    ///
    /// # Errors
    /// If the folder cannot be made or the manifest is unreadable.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let manifest = dir.join("report.json");
        let sections = if manifest.is_file() {
            let text = std::fs::read_to_string(&manifest).map_err(|e| Error::io(&manifest, e))?;
            serde_json::from_str(&text)
                .map_err(|e| Error::Parse(format!("{}: {e}", manifest.display())))?
        } else {
            Vec::new()
        };
        Ok(Self {
            dir: dir.to_owned(),
            sections,
        })
    }

    /// The folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write a file into the folder.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn save(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let p = self.dir.join(name);
        write_file(&p, bytes)?;
        Ok(p)
    }

    /// Add a section, replacing one with the same id.
    pub fn put(&mut self, section: Section) {
        match self.sections.iter_mut().find(|s| s.id == section.id) {
            Some(s) => *s = section,
            None => self.sections.push(section),
        }
    }

    /// The sections, in order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Write `report.json` and `report.html`; returns the path of the page.
    ///
    /// # Errors
    /// If a file cannot be read or written.
    pub fn write(&self) -> Result<PathBuf> {
        let json = serde_json::to_string_pretty(&self.sections)
            .map_err(|e| Error::Parse(e.to_string()))?;
        self.save("report.json", json.as_bytes())?;
        let html = self.html()?;
        self.save("report.html", html.as_bytes())
    }

    fn html(&self) -> Result<String> {
        let mut h = String::from(HEAD);
        h.push_str("<h1>Valerian science report</h1>\n");
        h.push_str("<nav>");
        for s in &self.sections {
            let _ = write!(h, "<a href=\"#{}\">{}</a> ", esc(&s.id), esc(&s.title));
        }
        h.push_str("</nav>\n");
        for s in &self.sections {
            let _ = writeln!(
                h,
                "<section id=\"{}\"><h2>{}</h2>",
                esc(&s.id),
                esc(&s.title)
            );
            for t in &s.text {
                let _ = writeln!(h, "<p>{}</p>", esc(t));
            }
            for f in &s.figures {
                let path = self.dir.join(&f.file);
                let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
                let _ = writeln!(
                    h,
                    "<figure><img alt=\"{cap}\" src=\"data:image/png;base64,{}\"><figcaption>{cap}</figcaption></figure>",
                    base64(&bytes),
                    cap = esc(&f.caption)
                );
            }
            if let Some(t) = &s.table {
                h.push_str(&table_html(t));
            }
            if !s.files.is_empty() {
                h.push_str("<p class=\"files\">Data: ");
                for f in &s.files {
                    let _ = write!(h, "<a href=\"{0}\">{0}</a> ", esc(f));
                }
                h.push_str("</p>\n");
            }
            h.push_str("</section>\n");
        }
        h.push_str("</body></html>\n");
        Ok(h)
    }
}

fn table_html(t: &Table) -> String {
    let mut h = String::from("<table><thead><tr>");
    for c in &t.header {
        let _ = write!(h, "<th>{}</th>", esc(c));
    }
    h.push_str("</tr></thead><tbody>\n");
    for r in &t.rows {
        let bad = t
            .flag_column
            .and_then(|c| r.get(c))
            .is_some_and(|v| v == "rejected");
        let _ = write!(h, "<tr{}>", if bad { " class=\"bad\"" } else { "" });
        for c in r {
            let _ = write!(h, "<td>{}</td>", esc(c));
        }
        h.push_str("</tr>\n");
    }
    h.push_str("</tbody></table>\n");
    h
}

/// Escape text for HTML.
#[must_use]
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Standard base64 with padding.
#[must_use]
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

const HEAD: &str = "<!doctype html>\n<html lang=\"en-GB\"><head><meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>Valerian science report</title>\n<style>\n\
body{font:15px/1.5 system-ui,sans-serif;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#1b1f24}\n\
h1{font-size:1.6rem}h2{font-size:1.2rem;margin-top:2.2rem;border-bottom:1px solid #ccd}\n\
nav a{margin-right:1rem}figure{margin:1rem 0}img{max-width:100%;height:auto;border:1px solid #ddd}\n\
figcaption{font-size:.85rem;color:#555}table{border-collapse:collapse;font-size:.85rem;margin:1rem 0}\n\
th,td{border:1px solid #ccd;padding:.2rem .5rem;text-align:right}td:first-child,th:first-child,td:nth-child(2){text-align:left}\n\
tr.bad{background:#fde8dc}.files{font-size:.85rem}\n\
@media (prefers-color-scheme:dark){body{background:#12151a;color:#e4e7ec}tr.bad{background:#4a2a1c}th,td,img{border-color:#445}a{color:#8cc4ff}}\n\
</style></head><body>\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn sections_accumulate_and_replace() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut r = ReportDir::open(dir.path()).expect("open");
        r.save("a.png", b"\x89PNG").expect("png");
        let s = Section {
            id: "a".into(),
            title: "A <b>".into(),
            figures: vec![Figure {
                file: "a.png".into(),
                caption: "cap".into(),
            }],
            ..Section::default()
        };
        r.put(s.clone());
        r.put(Section {
            title: "A2".into(),
            ..s
        });
        r.write().expect("write");
        let again = ReportDir::open(dir.path()).expect("reopen");
        assert_eq!(again.sections().len(), 1);
        let html = std::fs::read_to_string(dir.path().join("report.html")).expect("html");
        assert!(html.contains("data:image/png;base64,") && html.contains("A2"));
        assert!(!html.contains("<b>"));
    }
}
