// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Choosing the engine for `--backend`.

use anyhow::{Result, anyhow};
use valerian_gpu::{Selected, Stats, select};

use crate::BackendArg;

/// Open the engine `arg` asks for and say which it is. `gpu_pays` is whether
/// `auto` should use a GPU for this command (see [`valerian_gpu::select`]).
///
/// # Errors
/// For `gpu` when no adapter opens.
pub fn open(arg: BackendArg, gpu_pays: bool) -> Result<Selected> {
    let s = select(arg.into(), gpu_pays).map_err(|e| anyhow!("--backend gpu: {e}"))?;
    eprintln!("compute: {}", s.description);
    Ok(s)
}

/// A line about the GPU calls and fallbacks of a finished run, if the GPU was used.
pub fn report(s: &Selected) {
    if let Some(g) = &s.gpu {
        let Stats {
            gpu_calls,
            fallbacks,
            last_fallback,
            ..
        } = g.stats();
        eprintln!(
            "GPU: {gpu_calls} calls ran on the device{}",
            if fallbacks > 0 {
                format!(", {fallbacks} fell back to the CPU ({last_fallback})")
            } else {
                String::new()
            }
        );
    }
}
