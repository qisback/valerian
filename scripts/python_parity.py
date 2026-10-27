#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Run Valerian and the Python reference pipeline on staged real data and fail on drift.

    python python_parity.py --valerian target/release/valerian --work WORKDIR \\
        --ha-root DIR --osc DIR --catalogue gaia-dr3-g11-v2.vcat \\
        [--steps pipeline,stretch,...] [--tolerances FILE] [--update]

Each step runs `valerian` on the staged frames, runs the matching
`compare_python_*.py` script (with `--json`) and checks every number it reports
against a [low, high] range in the tolerance file (python_parity.json, next to
this script). A number outside its range is drift and the step fails.

    pipeline    calibrate, stack and compare with the astroalign/sep pipeline
    stretch     `valerian stretch` against the pipeline's autostretch
    background  `valerian background` against the pipeline's gradient removal (colour stack)
    drizzle     binned colour frames, stack and drizzle against the full-resolution truth
    solve       stack the colour frames, plate-solve and check against Gaia
    photocal    colour calibration against an independent photometry

solve and photocal read the ESA Gaia archive. If it cannot be reached the step
is reported as unavailable (not drift) and the exit status is 3 when nothing
else failed. Exit status 0: everything in range. 1: drift or an error.

--update measures and prints the ranges (the measured value with a margin) for
the tolerance file instead of checking; read them before committing them.

The environment is pinned in python-parity-requirements.txt. Image data is
never committed.

Layout of the data:
    HA_ROOT/calibration/*/{LIGHT,FLAT,DARKFLAT}   mono ASI1600MM frames, H-alpha
    HA_ROOT/deepsky/*/DARK
    OSC/Light_*.fit                               Seestar S50 raw colour frames
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
STEPS = ["pipeline", "stretch", "background", "drizzle", "solve", "photocal"]
NETWORK_STEPS = {"solve", "photocal"}
NETWORK_ERRORS = re.compile(r"URLError|timed out|Temporary failure|Connection|HTTPError|TimeoutError|ssl", re.I)
# How many frames each step uses. Few enough to finish in minutes on the CI host. Of the
# staged H-alpha session only the first five lights are one pointing (the rest sit 2700 px
# away or cannot be registered by astroalign), and the five are what the pipeline step has
# always compared.
HA_LIGHTS = 5
OSC_FRAMES = 20
# Drizzle wants dithered frames and plenty of them (a dozen or more for a 2x grid).
DRIZZLE_FRAMES = 60


class Unavailable(Exception):
    """An external service (the Gaia archive) could not be reached."""


def run(cmd, log, cwd=None, check=True):
    """Run a command, keep its output in log, and return it."""
    t0 = time.time()
    p = subprocess.run([str(c) for c in cmd], cwd=cwd, capture_output=True, text=True)
    with open(log, "w") as f:
        f.write(" ".join(str(c) for c in cmd) + "\n\n" + p.stdout + "\n--- stderr ---\n" + p.stderr)
    print(f"    {pathlib.Path(str(cmd[0])).name} {cmd[1] if len(cmd) > 1 else ''}: {time.time() - t0:.0f} s", flush=True)
    if check and p.returncode != 0:
        tail = "\n".join((p.stdout + p.stderr).strip().splitlines()[-12:])
        if NETWORK_ERRORS.search(p.stderr) and any("compare_python" in str(c) for c in cmd):
            raise Unavailable(tail)
        raise RuntimeError(f"{cmd[0]} failed ({p.returncode}); log {log}\n{tail}")
    return p


def fits_files(d):
    return sorted(str(p) for p in pathlib.Path(d).glob("*") if p.suffix.lower() in (".fit", ".fits"))


def find(root, pattern):
    hits = sorted(pathlib.Path(root).glob(pattern))
    if not hits:
        raise RuntimeError(f"nothing matches {root}/{pattern}")
    return hits[0]


class Ctx:
    def __init__(self, a):
        self.a = a
        self.work = pathlib.Path(a.work).resolve()
        self.work.mkdir(parents=True, exist_ok=True)
        self.val = str(pathlib.Path(a.valerian).resolve())
        self.py = sys.executable

    def v(self, *args, name):
        return run([self.val, *args], self.work / f"{name}.log")

    def compare(self, script, *args, name):
        out = self.work / f"{name}.json"
        run([self.py, "-I", HERE / script, *args, "--json", out], self.work / f"{name}.log")
        return json.loads(out.read_text())

    # ---- shared products -------------------------------------------------
    def calibrated(self):
        d = self.work / "cal"
        if not any(d.glob("*_cal.fits")):
            r = pathlib.Path(self.a.ha_root)
            lights = fits_files(find(r, "calibration/*/LIGHT"))[:HA_LIGHTS]
            self.v("calibrate", *lights, "--dark", find(r, "deepsky/*/DARK"), "--flat", find(r, "calibration/*/FLAT"),
                   "--dark-flat", find(r, "calibration/*/DARKFLAT"), "-o", d, "--backend", "cpu", name="calibrate")
        return fits_files(d)

    def ha_stack(self):
        out = self.work / "ha_stack.fits"
        if not out.exists():
            self.v("stack", *self.calibrated(), "-o", out, "--reference", "0", "--backend", "cpu",
                   "--report", self.work / "ha_stack.txt", name="stack")
        return out

    def osc_stack(self):
        out = self.work / "osc_stack.fits"
        if not out.exists():
            frames = fits_files(self.a.osc)[:OSC_FRAMES]
            self.v("stack", *frames, "-o", out, "--debayer", "mhc", "--reference", "0", "--backend", "cpu",
                   name="stack_osc")
        return out

    def osc_background(self):
        out = self.work / "osc_flat.fits"
        if not out.exists():
            self.v("background", self.osc_stack(), "-o", out, "--degree", "2", name="background_osc")
        return out

    def osc_solved(self):
        out = self.work / "osc_solved.fits"
        if not out.exists():
            self.v("solve", self.osc_background(), "-o", out, "--catalogue", self.a.catalogue, name="solve_osc")
        return out

    # ---- steps -----------------------------------------------------------
    def pipeline(self):
        stack = self.ha_stack()
        return self.compare("compare_python_pipeline.py", *self.calibrated(), "--valerian", stack, "--reference", "0",
                            name="pipeline")

    def stretch(self):
        stack = self.ha_stack()
        out = self.work / "ha_stretch.fits"
        self.v("stretch", stack, "-o", out, "--target", "0.10", name="stretch")
        return self.compare("compare_python_stretch.py", stack, "--valerian", out, "--target", "0.10", name="stretch_cmp")

    def background(self):
        # The colour stack: the H-alpha stack is mostly nebula, so Valerian rightly declines to fit
        # it (63 % is not clear sky) where the Python code fits regardless, which is no comparison.
        from astropy.io import fits

        stack = self.osc_stack()
        shape = fits.getdata(stack).shape[-2:]
        step = max(48, min(shape) // 40)  # the Python side's tile step
        out, model = self.work / "osc_bg.fits", self.work / "osc_bg_model.fits"
        self.v("background", stack, "-o", out, "--save-model", model, "--degree", "1", "--tile", str(step),
               name="background")
        return self.compare("compare_python_background.py", stack, "--valerian", out, "--model", model,
                            name="background_cmp")

    def drizzle(self):
        # The raw colour frames are already close to undersampled; binning them 2x makes them
        # so, and the unbinned stack is the answer. The H-alpha frames are well sampled.
        frames = fits_files(self.a.osc)[:DRIZZLE_FRAMES]
        raw = self.work / "osc_dir"
        binned = self.work / "binned"
        if not any(binned.glob("*.fits")):
            raw.mkdir(exist_ok=True)
            for i, f in enumerate(frames):
                link = raw / f"frame_{i:03d}.fit"
                if not link.exists():
                    link.symlink_to(f)
            run([self.py, "-I", HERE / "make_binned_frames.py", raw, binned, "--bin", "2"], self.work / "bin.log")
        stack_b, driz, full = self.work / "stack_b.fits", self.work / "drizzle_b.fits", self.work / "full.fits"
        if not stack_b.exists():
            self.v("stack", binned, "-o", stack_b, "--reference", "0", "--backend", "cpu", name="stack_b")
            self.v("drizzle", binned, "-o", driz, "--scale", "2", "--pixfrac", "0.7", "--reference", "0", name="drizzle")
            self.v("stack", raw, "-o", full, "--debayer", "mhc", "--reference", "0", "--backend", "cpu",
                   name="stack_full")
        return self.compare("compare_python_drizzle.py", "--stack", stack_b, "--drizzle", driz, "--truth", full,
                            "--truth-scale", "2", name="drizzle_cmp")

    def solve(self):
        return self.compare("compare_python_solve.py", self.osc_solved(), name="solve_cmp")

    def photocal(self):
        solved = self.osc_solved()
        out = self.work / "osc_pcc.fits"
        if not out.exists():
            self.v("photocal", solved, "-o", out, "--catalogue", self.a.catalogue, name="photocal")
        return self.compare("compare_python_photocal.py", solved, out, name="photocal_cmp")


def check(step, metrics, tol):
    """Return (rows, drift): one row per metric with its verdict."""
    rows, drift = [], False
    ranges = tol.get(step, {})
    for k in sorted(set(metrics) | set(ranges)):
        v = metrics.get(k)
        r = ranges.get(k)
        if v is None:
            rows.append((k, "missing", r, "FAIL (not reported)"))
            drift = True
        elif r is None:
            rows.append((k, v, None, "untracked"))
        else:
            ok = r[0] <= v <= r[1]
            drift |= not ok
            rows.append((k, v, r, "ok" if ok else "DRIFT"))
    return rows, drift


def margin(v):
    """A range around a measured value: 25 per cent, and at least 0.05 absolute for small numbers.

    The runs are deterministic (the CPU backend, one thread count does not change the result), so
    the margin is not noise: it is how far the reference and Valerian may move apart before the
    change is worth a look.
    """
    m = max(abs(v) * 0.25, 0.05)
    return [round(v - m, 4), round(v + m, 4)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--valerian", required=True, help="the valerian binary")
    ap.add_argument("--work", required=True, help="scratch directory (kept; logs and products are written here)")
    ap.add_argument("--ha-root", help="staged H-alpha session")
    ap.add_argument("--osc", help="directory of Seestar Light_*.fit")
    ap.add_argument("--catalogue", help="gaia-dr3-g11-v2.vcat")
    ap.add_argument("--steps", default=",".join(STEPS))
    ap.add_argument("--tolerances", default=str(HERE / "python_parity.json"))
    ap.add_argument("--update", action="store_true", help="print ranges from this run instead of checking")
    a = ap.parse_args()

    steps = a.steps.split(",")
    for s in steps:
        if s not in STEPS:
            ap.error(f"unknown step {s}; choose from {', '.join(STEPS)}")
    need_ha = {"pipeline", "stretch", "background", "drizzle"} & set(steps)
    need_osc = {"solve", "photocal"} & set(steps)
    if need_ha and not a.ha_root:
        ap.error("--ha-root is needed for " + ", ".join(sorted(need_ha)))
    if need_osc and not (a.osc and a.catalogue):
        ap.error("--osc and --catalogue are needed for " + ", ".join(sorted(need_osc)))

    tol = json.loads(pathlib.Path(a.tolerances).read_text()) if not a.update else {}
    ctx = Ctx(a)
    failed, unavailable, measured = [], [], {}
    for s in steps:
        print(f"== {s}", flush=True)
        try:
            metrics = getattr(ctx, s)()
        except Unavailable as e:
            print(f"   UNAVAILABLE (the Gaia archive could not be reached):\n{e}")
            unavailable.append(s)
            continue
        except Exception as e:  # noqa: BLE001
            print(f"   ERROR: {e}")
            failed.append(s)
            continue
        measured[s] = metrics
        if a.update:
            continue
        rows, drift = check(s, metrics, tol)
        for k, v, r, verdict in rows:
            vs = f"{v:.4f}" if isinstance(v, float) else str(v)
            rs = f"[{r[0]}, {r[1]}]" if r else ""
            print(f"   {k:<28} {vs:>12}  {rs:<22} {verdict}")
        if drift:
            failed.append(s)

    if a.update:
        out = {s: {k: margin(float(v)) for k, v in m.items()} for s, m in measured.items()}
        print(json.dumps(out, indent=2, sort_keys=True))
        return 0 if not failed else 1
    print()
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    if unavailable:
        print("OK, but not checked (Gaia archive unreachable): " + ", ".join(unavailable))
        return 3
    print("OK: every number is within its range")
    return 0


if __name__ == "__main__":
    sys.exit(main())
