// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Documentation coverage checks, run by `cargo xtask docs --check`.
//!
//! * Every subcommand and every long option of the CLI is mentioned in the
//!   user guide or getting-started pages (not only in the generated reference).
//! * Every row of `reference/documentation-coverage.md` has six cells, a status
//!   of Done, Partial or Missing, and links only pages that exist.
//! * Every book page is in the `zensical.toml` navigation or deliberately excluded.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::CommandFactory;
use valerian_cli::Cli;

const COVERAGE_PAGE: &str = "docs/book/src/reference/documentation-coverage.md";
/// Directories whose pages count as explaining a command line option.
const PROSE_DIRS: [&str; 2] = ["docs/book/src/user-guide", "docs/book/src/getting-started"];

fn prose(root: &Path) -> Result<String> {
    let mut all = String::new();
    for dir in PROSE_DIRS {
        for entry in std::fs::read_dir(root.join(dir))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "md") {
                all.push_str(&std::fs::read_to_string(&path)?.replace("\r\n", "\n"));
                all.push('\n');
            }
        }
    }
    Ok(all)
}

/// True when `needle` occurs in `hay` and is not followed by a word character
/// or a hyphen (so `--low` does not match `--low-water`).
fn mentioned(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        !hay[i + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
    })
}

fn undocumented(text: &str) -> Vec<String> {
    let mut root = Cli::command();
    root.build();
    let mut missing = Vec::new();
    for sub in root.get_subcommands().filter(|s| s.get_name() != "help") {
        let name = sub.get_name();
        if !mentioned(text, &format!("valerian {name}")) {
            missing.push(format!("subcommand `{name}`"));
        }
        for arg in sub.get_arguments() {
            let Some(long) = arg.get_long() else { continue };
            if matches!(long, "help" | "version") {
                continue;
            }
            let flag = format!("--{long}");
            // An option with a short form (`-o`) may be written either way.
            let short = arg.get_short().map(|s| format!(" -{s} "));
            let short_used = short.is_some_and(|s| text.contains(&s));
            if !mentioned(text, &flag) && !short_used {
                missing.push(format!("`valerian {name} {flag}`"));
            }
        }
    }
    missing
}

fn bad_rows(root: &Path, page: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let dir = root.join(COVERAGE_PAGE);
    let dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut rows = 0;
    for (n, line) in page.lines().enumerate() {
        let line = line.trim();
        if !line.starts_with('|') || line.starts_with("|---") || line.starts_with("| Feature") {
            continue;
        }
        rows += 1;
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 6 {
            problems.push(format!("line {}: {} cells, expected 6", n + 1, cells.len()));
            continue;
        }
        if !matches!(cells[4], "Done" | "Partial" | "Missing") {
            problems.push(format!("line {}: status `{}`", n + 1, cells[4]));
        }
        for part in line.split("](").skip(1) {
            let target = part.split(')').next().unwrap_or_default();
            let file = target.split('#').next().unwrap_or_default();
            if file.is_empty() || file.contains("://") {
                continue;
            }
            if !dir.join(file).is_file() {
                problems.push(format!("line {}: links missing page `{file}`", n + 1));
            }
        }
    }
    if rows == 0 {
        problems.push("no feature rows found".to_owned());
    }
    problems
}

/// Every `.md` file under `dir`, recursively.
fn markdown_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            markdown_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Page paths in a Zensical `nav` value (external links are skipped).
fn nav_pages(v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::String(s) if Path::new(s).extension().is_some_and(|e| e == "md") => {
            out.push(s.clone());
        }
        toml::Value::Array(a) => a.iter().for_each(|x| nav_pages(x, out)),
        toml::Value::Table(t) => t.values().for_each(|x| nav_pages(x, out)),
        _ => {}
    }
}

/// Every page under the docs directory is in `nav` or `exclude_docs` of
/// `zensical.toml`, and every `nav` page exists. Zensical itself reports
/// broken links, images and snippets when the site is built with `--strict`.
fn check_nav(root: &Path) -> Result<Vec<String>> {
    let src = root.join("docs/book/src");
    let cfg_path = root.join("zensical.toml");
    let text = std::fs::read_to_string(&cfg_path)
        .with_context(|| format!("cannot read {}", cfg_path.display()))?;
    let cfg: toml::Value = toml::from_str(&text).context("zensical.toml is not valid TOML")?;
    let project = cfg
        .get("project")
        .context("zensical.toml has no [project]")?;
    let mut nav = Vec::new();
    if let Some(n) = project.get("nav") {
        nav_pages(n, &mut nav);
    }
    let excluded: Vec<&str> = project
        .get("exclude_docs")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut problems = Vec::new();
    for page in &nav {
        if !src.join(page).is_file() {
            problems.push(format!(
                "zensical.toml: nav lists `{page}`, which does not exist"
            ));
        }
    }
    let mut files = Vec::new();
    markdown_files(&src, &mut files)?;
    for f in files {
        let rel = f
            .strip_prefix(&src)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        if !nav.contains(&rel) && !excluded.contains(&rel.as_str()) {
            problems.push(format!(
                "zensical.toml: `{rel}` is in neither `nav` nor `exclude_docs`"
            ));
        }
    }
    Ok(problems)
}

/// Fail on an undocumented option or a broken coverage row.
pub fn check(root: &Path) -> Result<()> {
    let mut msg = String::new();
    let missing = undocumented(&prose(root)?);
    if !missing.is_empty() {
        let _ = writeln!(
            msg,
            "not mentioned in the user guide (add them to the page for that command): {}",
            missing.join(", ")
        );
    }
    let page = std::fs::read_to_string(root.join(COVERAGE_PAGE))
        .map_err(|e| anyhow::anyhow!("cannot read {COVERAGE_PAGE}: {e}"))?
        .replace("\r\n", "\n");
    for p in bad_rows(root, &page) {
        let _ = writeln!(msg, "{COVERAGE_PAGE}: {p}");
    }
    for p in check_nav(root)? {
        let _ = writeln!(msg, "{p}");
    }
    if !msg.is_empty() {
        bail!("documentation coverage check failed:\n{msg}");
    }
    println!("documentation coverage check passed");
    Ok(())
}
