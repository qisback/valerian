// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `valerian bench`.

use anyhow::{Result, anyhow, bail};
use valerian_gpu::bench::{Config, Row, run as run_bench};
use valerian_gpu::{AdapterRequest, GpuCompute};

use crate::{AdapterArg, BenchArgs};

/// The table of rows.
#[must_use]
pub fn table(rows: &[Row]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<40} {:>9} {:>9} {:>8} {:>21} {:>10} {:>9}",
        "stage", "CPU ms", "GPU ms", "speed-up", "up / kernel / down ms", "worst diff", "> 1e-4"
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{:<40} {:>9.1} {:>9.1} {:>7.1}x {:>21} {:>10.2e} {:>9.1e}",
            r.name,
            r.cpu * 1000.0,
            r.gpu * 1000.0,
            r.speedup(),
            format!(
                "{:.0} / {:.0} / {:.0}",
                r.phases.0 * 1000.0,
                r.phases.1 * 1000.0,
                r.phases.2 * 1000.0
            ),
            r.worst,
            r.off + r.coverage
        );
    }
    s
}

pub fn run(a: &BenchArgs) -> Result<()> {
    if a.width < 64 || a.height < 64 {
        bail!("--width and --height must be at least 64");
    }
    if !(3..=128).contains(&a.frames) {
        bail!("--frames must be 3 to 128");
    }
    let request = match a.adapter {
        AdapterArg::Hardware => AdapterRequest::Hardware,
        AdapterArg::Software => AdapterRequest::Software,
    };
    let gpu = GpuCompute::new(request).map_err(|e| anyhow!("no {request:?} adapter: {e}"))?;
    println!("adapter: {}", gpu.summary());
    if gpu.is_software() && request == AdapterRequest::Hardware {
        println!("note: {}", valerian_gpu::SOFTWARE_NOTE);
    }
    println!(
        "frames of {} x {}, {} frames in the combine, median of {} runs",
        a.width, a.height, a.frames, a.repeats
    );
    let cfg = Config {
        width: a.width,
        height: a.height,
        frames: a.frames,
        repeats: a.repeats,
    };
    let rows = run_bench(&gpu, &cfg, &mut |line| eprintln!("{line}")).map_err(|e| anyhow!(e))?;
    print!("{}", table(&rows));
    println!(
        "up / kernel / down is the average time of a GPU call spent copying to the device, running the kernels and copying back; \
         worst diff is the largest difference between the CPU and GPU results as a fraction of the data range; \
         > 1e-4 is the fraction of pixels that differ by more than that, or that only one has data for"
    );
    Ok(())
}
