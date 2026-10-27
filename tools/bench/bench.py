#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Reproducible `valerian stack` and `valerian bench` benchmark: one JSON file per machine.

    bench.py --bin target/release/valerian --lights DIR_OF_FITS --out results/x86.json \
        --modes cpu,sw --reps 3 --threads 1,2,4,8 --scaling --kernels

* `captures`: the whole stack of DIR, per mode, with the wall time of each stage
  taken from when the program prints its progress lines (read, engine start,
  star detection, registration and reference choice, combine, write).
* `threads`: the same stack on 1..N cores (taskset plus RAYON_NUM_THREADS).
* `frames`: the stack of a quarter, a half and all of the frames, and of 2x and 4x
  the frames (the same files listed again under new names, so the pixel data
  repeats; it shows how the time grows, not how the stars register).
* `--kernels`: `valerian bench`, the CPU and GPU time of calibration, the
  registration warp and the combiner on made-up frames, with the copy up, the
  kernel and the copy down.

Modes: `cpu` (--backend cpu), `gpu` (--backend gpu, first real adapter), `sw`
(--backend gpu on the Mesa lavapipe software adapter). Runs on a machine with
more than half a core of foreign load are marked loaded and left out of the plots;
a GPU run waits until the load average is under 8.
"""
import argparse
import json
import os
import re
import subprocess
import tempfile
import time

from benchlib import LVP, collect, env_for, gate, machine, run_timed

FRAMES_RE = re.compile(r"(\d+) of (\d+) frames stacked")
ROW_RE = re.compile(
    r"^(\S.*?)\s{2,}([\d.]+)\s+([\d.]+)\s+([\d.]+)x\s+([\d.]+) / ([\d.]+) / ([\d.]+)\s+(\S+)\s+(\S+)\s*$")


def stage_times(lines):
    """Split the wall time at the progress lines the program prints."""
    t, mark, last_frame = {}, {}, 0.0
    for ts, l in lines:
        if l.startswith("read ") and "frames in" in l:
            mark["read"] = ts
        elif l.startswith("compute:"):
            mark["engine"] = ts
        elif l.startswith("frame ") and ":" in l[:12]:
            last_frame = ts
        elif l.startswith("combining"):
            mark["combining"] = ts
        elif FRAMES_RE.search(l):
            mark["stacked"] = ts
    end = lines[-1][0] if lines else 0
    if "read" in mark:
        t["read frames"] = mark["read"]
        if "engine" in mark:
            t["start engine"] = mark["engine"] - mark["read"]
            if last_frame:
                t["detect stars"] = last_frame - mark["engine"]
        if "combining" in mark and last_frame:
            t["register"] = mark["combining"] - last_frame
        if "stacked" in mark and "combining" in mark:
            t["warp and combine"] = mark["stacked"] - mark["combining"]
        if "stacked" in mark:
            t["write"] = end - mark["stacked"]
    return {k: round(v, 3) for k, v in t.items()}


def subset(dirpath, n, scratch, repeat=1):
    """A directory of symlinks to the first n frames, listed `repeat` times."""
    files = sorted(f for f in os.listdir(dirpath) if f.lower().endswith((".fit", ".fits", ".fts")))[:n]
    d = os.path.join(scratch, f"set-{len(files)}x{repeat}")
    os.makedirs(d, exist_ok=True)
    for r in range(repeat):
        for f in files:
            dst = os.path.join(d, f"r{r}-{f}")
            if not os.path.lexists(dst):
                os.symlink(os.path.abspath(os.path.join(dirpath, f)), dst)
    size = sum(os.path.getsize(os.path.join(dirpath, f)) for f in files) * repeat
    return d, len(files) * repeat, size


def stack_once(binary, d, mode, threads, gpu_index):
    out = os.path.join(tempfile.gettempdir(), "valerian-bench-out.fits")
    cmd = [binary, "stack", d, "-o", out, "--backend", "cpu" if mode == "cpu" else "gpu"]
    r = run_timed(cmd, env_for(mode, gpu_index), threads, gpu_index, vram=mode == "gpu")
    lines = r.pop("lines")
    txt = [l for _, l in lines]
    m = next((FRAMES_RE.search(l) for l in txt if FRAMES_RE.search(l)), None)
    r.update(stages=stage_times(lines), frames=int(m.group(2)) if m else None,
             tail=None if r["ok"] else chr(10).join(txt[-5:]))
    r["stages"]["total"] = r["wall"]
    r["ok"] = r["ok"] and m is not None
    return r


def kernels(binary, mode, gpu_index, width, height, frames, repeats):
    env = env_for(mode, gpu_index)
    adapter = "hardware" if mode == "gpu" else "software"
    t = time.perf_counter()
    p = subprocess.run([binary, "bench", "--adapter", adapter, "--width", str(width), "--height", str(height),
                        "--frames", str(frames), "--repeats", str(repeats)], capture_output=True, text=True, env=env)
    rows = []
    for l in p.stdout.splitlines():
        m = ROW_RE.match(l)
        if m:
            rows.append(dict(stage=m.group(1).strip(), cpu_ms=float(m.group(2)), gpu_ms=float(m.group(3)),
                             speedup=float(m.group(4)), up_ms=float(m.group(5)), kernel_ms=float(m.group(6)),
                             down_ms=float(m.group(7)), worst_diff=m.group(8)))
    adapter_line = next((l for l in p.stdout.splitlines() if l.startswith("adapter:")), "")
    return dict(mode=mode, adapter=adapter_line[8:].strip(), width=width, height=height, frames=frames,
                repeats=repeats, rows=rows, load1=float(open("/proc/loadavg").read().split()[0]),
                seconds=round(time.perf_counter() - t, 1), ok=p.returncode == 0 and bool(rows))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--lights", required=True, help="directory of FITS frames")
    ap.add_argument("--out", required=True)
    ap.add_argument("--modes", default="cpu")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--threads", default="")
    ap.add_argument("--scaling", action="store_true")
    ap.add_argument("--kernels", action="store_true", help="also run `valerian bench`")
    ap.add_argument("--gpu-index", type=int, default=None)
    ap.add_argument("--commit", default="")
    ap.add_argument("--label", default="")
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    modes = a.modes.split(",")
    scratch = tempfile.mkdtemp(prefix="valerian-bench-")
    full, nfull, size = subset(a.lights, 10**6, scratch)
    jobs = [("captures", f"stack-{nfull}", full, m, 0, size) for m in modes]
    for t in [int(x) for x in a.threads.split(",") if x]:
        jobs += [("threads", f"stack-{nfull}", full, m, t, size) for m in modes if m == "cpu"]
    if a.scaling:
        for n, rep in ((nfull // 4, 1), (nfull // 2, 1), (nfull, 1), (nfull, 2), (nfull, 4)):
            d, k, sz = subset(a.lights, n, scratch, rep)
            jobs += [("frames", f"stack-{k}", d, m, 0, sz) for m in modes if m != "sw"]
    runs = []
    for group, case, d, mode, threads, size in jobs:
        l1 = gate(mode, a.force)
        for r, res in enumerate(collect(lambda: stack_once(a.bin, d, mode, threads, a.gpu_index), a.reps)):
            res.update(group=group, case=case, mode=mode, threads=threads, rep=r, load1_before=l1, bytes=size)
            runs.append(res)
            print(json.dumps({k: res[k] for k in ("group", "case", "mode", "threads", "rep", "wall", "rss_mb",
                                                  "other_cores", "ok")}), flush=True)
    kern = []
    if a.kernels:
        for m in modes:
            if m in ("gpu", "sw"):
                gate(m, a.force)
                kern.append(kernels(a.bin, m, a.gpu_index, 4096, 3072, 20, 5))
    meta = machine()
    meta.update(label=a.label or os.uname().nodename, commit=a.commit, bin=a.bin, date=time.strftime("%Y-%m-%d %H:%M"),
                version=subprocess.run([a.bin, "--version"], capture_output=True, text=True).stdout.splitlines()[0].strip(),
                reps=a.reps, gpu_index=a.gpu_index, lavapipe=os.path.exists(LVP))
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    with open(a.out, "w") as f:
        json.dump({"app": "valerian", "meta": meta, "runs": runs, "kernels": kern}, f, indent=1)


if __name__ == "__main__":
    main()
