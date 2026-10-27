// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Per-pixel combination of up to MAXN frames with outlier rejection: one thread
// per pixel gathers its samples, rejects outliers by the chosen scheme and
// writes the weighted mean of the rest. The CPU reference is `combine::combine`
// in valerian-core; the scheme code below follows it line for line.
//
// __MAXN__ is replaced by the frame capacity (32, 64 or 128) when the module is
// made. Non-finite samples (NaN: no data) are skipped.

struct Params {
    n: u32,        // frames
    npx: u32,      // pixels in this band
    kind: u32,     // 0 none, 1 min/max, 2 sigma, 3 winsorised, 4 linear fit
    max_iter: u32,
    low: f32,
    high: f32,
    low_n: u32,
    high_n: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> frames: array<f32>;      // n * npx, frame by frame
@group(0) @binding(2) var<storage, read> weights: array<f32>;     // n
@group(0) @binding(3) var<storage, read_write> mean_out: array<f32>;
@group(0) @binding(4) var<storage, read_write> used_out: array<u32>;
@group(0) @binding(5) var<storage, read_write> counters: array<atomic<u32>>; // 3 * n: valid, low, high

const MAXN: u32 = __MAXN__u;
const NAN_BITS: u32 = 0x7fc00000u;
const MIN_POSITIVE: f32 = 1.17549435e-38;

var<workgroup> wg_valid: array<atomic<u32>, MAXN>;
var<workgroup> wg_low: array<atomic<u32>, MAXN>;
var<workgroup> wg_high: array<atomic<u32>, MAXN>;

var<private> sv: array<f32, MAXN>; // sample values
var<private> sw: array<f32, MAXN>; // sample weights
var<private> sf: array<u32, MAXN>; // frame of each sample
var<private> sk: array<u32, MAXN>; // 1 while kept
var<private> st: array<f32, MAXN>; // scratch: kept values, sorted
var<private> su: array<f32, MAXN>; // scratch: winsorised values
var<private> so: array<u32, MAXN>; // sample indices ordered by value
var<private> cnt: u32;

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

// The kept values into st, returning how many.
fn gather_kept() -> u32 {
    var m = 0u;
    for (var i = 0u; i < cnt; i = i + 1u) {
        if (sk[i] == 1u) {
            st[m] = sv[i];
            m = m + 1u;
        }
    }
    return m;
}

fn sort_st(m: u32) {
    for (var a = 1u; a < m; a = a + 1u) {
        let key = st[a];
        var b = a;
        while (b > 0u && st[b - 1u] > key) {
            st[b] = st[b - 1u];
            b = b - 1u;
        }
        st[b] = key;
    }
}

// Median of the sorted st[0..m).
fn median_st(m: u32) -> f32 {
    let mid = m / 2u;
    if ((m & 1u) == 1u) {
        return st[mid];
    }
    return (st[mid - 1u] + st[mid]) * 0.5;
}

fn rms_about(m: u32, centre: f32) -> f32 {
    var s = 0.0;
    for (var i = 0u; i < m; i = i + 1u) {
        let d = st[i] - centre;
        s = s + d * d;
    }
    return sqrt(s / f32(m));
}

// All samples ordered by value into so[0..cnt), ties in frame order.
fn order_all() {
    for (var i = 0u; i < cnt; i = i + 1u) {
        var j = i;
        while (j > 0u && sv[so[j - 1u]] > sv[i]) {
            so[j] = so[j - 1u];
            j = j - 1u;
        }
        so[j] = i;
    }
}

fn clip_sigma() {
    for (var it = 0u; it < p.max_iter; it = it + 1u) {
        let m = gather_kept();
        if (m < 3u) {
            return;
        }
        sort_st(m);
        let centre = median_st(m);
        let sigma = rms_about(m, centre);
        if (sigma <= 0.0) {
            return;
        }
        var changed = false;
        for (var i = 0u; i < cnt; i = i + 1u) {
            if (sk[i] == 1u && (sv[i] < centre - p.low * sigma || sv[i] > centre + p.high * sigma)) {
                sk[i] = 0u;
                changed = true;
            }
        }
        if (!changed) {
            return;
        }
    }
}

// Robust sigma by winsorising at 1.5 sigma until it settles.
fn winsorised_sigma(m: u32, centre: f32, sigma0: f32) -> f32 {
    var sigma = sigma0;
    var mean = centre;
    let n = f32(m);
    for (var it = 0; it < 50; it = it + 1) {
        let lo = mean - 1.5 * sigma;
        let hi = mean + 1.5 * sigma;
        var sum = 0.0;
        for (var i = 0u; i < m; i = i + 1u) {
            su[i] = clamp(st[i], lo, hi);
            sum = sum + su[i];
        }
        let mm = sum / n;
        var v = 0.0;
        for (var i = 0u; i < m; i = i + 1u) {
            let d = su[i] - mm;
            v = v + d * d;
        }
        let next = 1.134 * sqrt(v / max(n - 1.0, 1.0));
        let done = abs(next - sigma) <= 0.0005 * max(sigma, MIN_POSITIVE);
        sigma = next;
        mean = mm;
        if (done || sigma <= 0.0) {
            break;
        }
    }
    return sigma;
}

fn clip_winsorised() {
    for (var it = 0u; it < p.max_iter; it = it + 1u) {
        let m = gather_kept();
        if (m < 4u) {
            return;
        }
        sort_st(m);
        let centre = median_st(m);
        let sigma0 = rms_about(m, centre);
        if (sigma0 <= 0.0) {
            return;
        }
        let sigma = winsorised_sigma(m, centre, sigma0);
        if (sigma <= 0.0) {
            return;
        }
        var changed = false;
        for (var i = 0u; i < cnt; i = i + 1u) {
            if (sk[i] == 1u && (sv[i] < centre - p.low * sigma || sv[i] > centre + p.high * sigma)) {
                sk[i] = 0u;
                changed = true;
            }
        }
        if (!changed) {
            return;
        }
    }
}

fn clip_linear() {
    for (var it = 0u; it < p.max_iter; it = it + 1u) {
        // Kept samples ordered by value into so[0..n).
        var n = 0u;
        for (var i = 0u; i < cnt; i = i + 1u) {
            if (sk[i] == 1u) {
                var j = n;
                while (j > 0u && sv[so[j - 1u]] > sv[i]) {
                    so[j] = so[j - 1u];
                    j = j - 1u;
                }
                so[j] = i;
                n = n + 1u;
            }
        }
        if (n < 5u) {
            return;
        }
        // Least-squares line through (rank, value), with the rank centred.
        let nf = f32(n);
        let xm = (nf - 1.0) * 0.5;
        var sy = 0.0;
        var sxy = 0.0;
        var sxx = 0.0;
        for (var r = 0u; r < n; r = r + 1u) {
            let x = f32(r) - xm;
            let y = sv[so[r]];
            sy = sy + y;
            sxy = sxy + x * y;
            sxx = sxx + x * x;
        }
        let slope = sxy / sxx;
        let icpt = sy / nf;
        var ss = 0.0;
        for (var r = 0u; r < n; r = r + 1u) {
            let d = sv[so[r]] - (icpt + slope * (f32(r) - xm));
            ss = ss + d * d;
        }
        let sigma = sqrt(ss / (nf - 2.0));
        if (sigma <= 0.0) {
            return;
        }
        var dropped = false;
        for (var r = 0u; r < n; r = r + 1u) {
            let d = sv[so[r]] - (icpt + slope * (f32(r) - xm));
            if (d < -p.low * sigma || d > p.high * sigma) {
                sk[so[r]] = 0u;
                dropped = true;
            }
        }
        if (!dropped) {
            return;
        }
    }
}

fn clip_min_max() {
    order_all();
    if (cnt <= p.low_n + p.high_n + 1u) {
        return;
    }
    for (var r = 0u; r < p.low_n; r = r + 1u) {
        sk[so[r]] = 0u;
    }
    for (var r = 0u; r < p.high_n; r = r + 1u) {
        sk[so[cnt - 1u - r]] = 0u;
    }
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    for (var f = lid; f < p.n; f = f + 64u) {
        atomicStore(&wg_valid[f], 0u);
        atomicStore(&wg_low[f], 0u);
        atomicStore(&wg_high[f], 0u);
    }
    workgroupBarrier();

    let i = (wid.y * 65535u + wid.x) * 64u + lid;
    if (i < p.npx) {
        cnt = 0u;
        for (var f = 0u; f < p.n; f = f + 1u) {
            let v = frames[f * p.npx + i];
            if (finite(v)) {
                sv[cnt] = v;
                sw[cnt] = weights[f];
                sf[cnt] = f;
                sk[cnt] = 1u;
                cnt = cnt + 1u;
                atomicAdd(&wg_valid[f], 1u);
            }
        }
        var mean = bitcast<f32>(NAN_BITS);
        var used = 0u;
        if (cnt > 0u) {
            if (p.kind == 1u) {
                clip_min_max();
            } else if (p.kind == 2u) {
                clip_sigma();
            } else if (p.kind == 3u) {
                clip_winsorised();
            } else if (p.kind == 4u) {
                clip_linear();
            }
            var swt = 0.0;
            var swv = 0.0;
            var rejected = false;
            for (var s = 0u; s < cnt; s = s + 1u) {
                if (sk[s] == 1u) {
                    swt = swt + sw[s];
                    swv = swv + sw[s] * sv[s];
                    used = used + 1u;
                } else {
                    rejected = true;
                }
            }
            if (rejected) {
                let m = gather_kept();
                var centre = 0.0;
                if (m > 0u) {
                    sort_st(m);
                    centre = median_st(m);
                }
                for (var s = 0u; s < cnt; s = s + 1u) {
                    if (sk[s] == 0u) {
                        if (sv[s] < centre) {
                            atomicAdd(&wg_low[sf[s]], 1u);
                        } else {
                            atomicAdd(&wg_high[sf[s]], 1u);
                        }
                    }
                }
            }
            if (swt > 0.0) {
                mean = swv / swt;
            } else {
                used = 0u;
            }
        }
        mean_out[i] = mean;
        used_out[i] = used;
    }
    workgroupBarrier();
    for (var f = lid; f < p.n; f = f + 64u) {
        atomicAdd(&counters[f], atomicLoad(&wg_valid[f]));
        atomicAdd(&counters[p.n + f], atomicLoad(&wg_low[f]));
        atomicAdd(&counters[2u * p.n + f], atomicLoad(&wg_high[f]));
    }
}
