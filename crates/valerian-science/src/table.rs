// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small CSV and file helpers shared by every output.

use std::fmt::Write as _;
use std::path::Path;

use crate::{Error, Result};

/// One CSV field, quoted when it holds a comma, quote or line break.
#[must_use]
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// A CSV document: the header line, then one line per row.
#[must_use]
pub fn csv_text(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}", header.join(","));
    for r in rows {
        let line: Vec<String> = r.iter().map(|f| csv_field(f)).collect();
        let _ = writeln!(s, "{}", line.join(","));
    }
    s
}

/// Split one CSV line into fields, honouring double quotes.
#[must_use]
pub fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Format a float for a table: `NaN` becomes an empty field.
#[must_use]
pub fn num(v: f64, digits: usize) -> String {
    if v.is_finite() {
        format!("{v:.digits$}")
    } else {
        String::new()
    }
}

/// A signed number for text: `+1.50`, `-0.20`, and a plain `0.00` when it rounds to zero.
#[must_use]
pub fn signed(v: f64, digits: usize) -> String {
    let s = format!("{v:+.digits$}");
    if s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_owned()
    } else {
        s
    }
}

/// Write bytes to a file, creating its folder.
///
/// # Errors
/// If the file cannot be written.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips() {
        let rows = vec![vec![
            "a,b".to_owned(),
            "say \"hi\"".to_owned(),
            "x".to_owned(),
        ]];
        let text = csv_text(&["one", "two", "three"], &rows);
        let line = text.lines().nth(1).expect("row");
        assert_eq!(split_csv_line(line), rows[0]);
    }

    #[test]
    fn a_signed_number_that_rounds_to_zero_has_no_sign() {
        assert_eq!(signed(-0.2, 0), "0");
        assert_eq!(signed(-0.004, 2), "0.00");
        assert_eq!(signed(0.6, 0), "+1");
        assert_eq!(signed(-1.25, 1), "-1.2");
    }

    #[test]
    fn nan_is_an_empty_field() {
        assert_eq!(num(f64::NAN, 2), "");
        assert_eq!(num(1.0, 2), "1.00");
    }
}
