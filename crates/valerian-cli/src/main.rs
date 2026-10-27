// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian`: command-line deep-sky image processing.

use clap::{CommandFactory, FromArgMatches};
use valerian_cli::{Cli, run};

fn main() {
    // `-V` stays one line; `--version` adds the build record, the copyright,
    // the licence, the additional terms and the dedication.
    // clap prints "valerian " itself, so drop ours from the long text.
    static LONG_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let long = LONG_VERSION.get_or_init(|| {
        let text = valerian_core::about::long_version(
            "valerian",
            env!("CARGO_PKG_VERSION"),
            valerian_gpu::WGPU_VERSION,
        );
        text.strip_prefix("valerian ").unwrap_or(&text).to_owned()
    });
    let matches = Cli::command().long_version(long.as_str()).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    if let Err(e) = run(&cli) {
        eprintln!("valerian: {e:#}");
        std::process::exit(1);
    }
}
