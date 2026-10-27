// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `cargo xtask`: repository automation.
//!
//! `cargo xtask docs` regenerates the CLI reference page of the book from the
//! clap definition in `valerian-cli`. `cargo xtask docs --check` fails when the
//! committed file differs, so the reference cannot go stale.
//!
//! `docs --check` also runs the coverage check: every subcommand and long option
//! must be mentioned in the user guide, and the links in the coverage table of
//! the book must resolve.
//!
//! `cargo xtask reuse-check` fails when a file lacks licensing information.

mod clidoc;
mod coverage;
mod reuse;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Valerian repository automation")]
struct Xtask {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the generated pages of the book.
    Docs {
        /// Do not write; fail if a committed generated file is out of date.
        #[arg(long)]
        check: bool,
    },
    /// Fail if a file lacks an SPDX header or a REUSE.toml annotation.
    ReuseCheck,
}

/// Directory of the generated reference pages, relative to the repository root.
const REFERENCE_DIR: &str = "docs/book/src/reference";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Every generated file: repository-relative path and exact content.
fn generated() -> Vec<(String, String)> {
    vec![(format!("{REFERENCE_DIR}/cli.md"), clidoc::render())]
}

/// Line endings differ between checkouts on Windows; content must not.
fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Render the book's measured-result charts from their data files, or check that they are
/// current and quote only numbers their page carries.
fn charts(root: &Path, check: bool) -> Result<()> {
    let script = root.join("tools/docs-graphs/make_graphs.py");
    let mut failure = String::new();
    for py in ["python3", "python"] {
        let mut cmd = std::process::Command::new(py);
        cmd.arg(&script).arg("--root").arg(root);
        if check {
            cmd.arg("--check");
        }
        match cmd.status() {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => failure = format!("`{py}` exited with {s}"),
            Err(e) => failure = format!("cannot run `{py}`: {e}"),
        }
    }
    bail!(
        "the chart generator failed ({failure}); run `python3 tools/docs-graphs/make_graphs.py` and commit the result"
    )
}

fn docs(check: bool) -> Result<()> {
    let root = repo_root();
    let mut stale = Vec::new();
    for (rel, content) in generated() {
        let path = root.join(&rel);
        if check {
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            if normalise(&on_disk) != content {
                stale.push(rel);
            }
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &content)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("wrote {rel}");
        }
    }
    if !stale.is_empty() {
        bail!(
            "generated documentation is out of date: {}\nrun `cargo xtask docs` and commit the result",
            stale.join(", ")
        );
    }
    charts(&root, check)?;
    if check {
        println!("generated documentation is up to date");
        coverage::check(&root)?;
    }
    Ok(())
}

fn main() {
    let result = match Xtask::parse().command {
        Cmd::Docs { check } => docs(check),
        Cmd::ReuseCheck => reuse::check(&repo_root()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e:#}");
        std::process::exit(1);
    }
}
