// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Resample a plane through a projective transform: each output pixel looks up
// where it came from, as `warp::warp` in valerian-core does. Output pixels whose
// source is outside the frame (half a pixel of margin) or touches a non-finite
// tap are NaN.
//
// It writes `rows` rows of the output starting at output row `row0` to `dst` at
// element `out_off`, so a stack can fill a band of a many-frame buffer, and can
// match the result to the reference level, `(v - sky) * scale + ref_sky` (flag
// bit 1), or copy the source as it is (flag bit 0: the reference frame).

struct Params {
    src_w: u32,
    src_h: u32,
    out_w: u32,
    rows: u32,
    row0: u32,
    out_off: u32,
    interp: u32, // 0 bilinear, 1 bicubic, 2 Lanczos-3
    flags: u32,
    level: vec4<f32>, // sky, scale, reference sky
    r0: vec4<f32>,    // rows of the inverse transform
    r1: vec4<f32>,
    r2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

const NAN_BITS: u32 = 0x7fc00000u;
const PI: f32 = 3.14159265358979;

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn cubic(x: f32) -> f32 {
    let a = abs(x);
    if (a < 1.0) {
        return 1.5 * a * a * a - 2.5 * a * a + 1.0;
    }
    if (a < 2.0) {
        return -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0;
    }
    return 0.0;
}

fn lanczos(x: f32) -> f32 {
    if (abs(x) < 1e-6) {
        return 1.0;
    }
    if (abs(x) >= 3.0) {
        return 0.0;
    }
    let px = PI * x;
    return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}

var<private> wts: array<f32, 6>;

// Weights of the 2 * reach taps floor(x) - reach + 1 ..= floor(x) + reach.
fn weights(t: f32) {
    if (p.interp == 0u) {
        wts[0] = 1.0 - t;
        wts[1] = t;
    } else if (p.interp == 1u) {
        for (var k = 0u; k < 4u; k = k + 1u) {
            wts[k] = cubic(t - (f32(k) - 1.0));
        }
    } else {
        var sum = 0.0;
        for (var k = 0u; k < 6u; k = k + 1u) {
            wts[k] = lanczos(t - (f32(k) - 2.0));
            sum = sum + wts[k];
        }
        for (var k = 0u; k < 6u; k = k + 1u) {
            wts[k] = wts[k] / sum;
        }
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.out_w || gid.y >= p.rows) {
        return;
    }
    let row = p.row0 + gid.y;
    let o = p.out_off + gid.y * p.out_w + gid.x;
    if ((p.flags & 1u) != 0u) {
        dst[o] = src[row * p.src_w + gid.x];
        return;
    }
    let fx = f32(gid.x);
    let fy = f32(row);
    let w = p.r2.x * fx + p.r2.y * fy + p.r2.z;
    let sx = (p.r0.x * fx + p.r0.y * fy + p.r0.z) / w;
    let sy = (p.r1.x * fx + p.r1.y * fy + p.r1.z) / w;
    let sw = i32(p.src_w);
    let sh = i32(p.src_h);
    if (!(sx >= -0.5 && sy >= -0.5 && sx <= f32(sw) - 0.5 && sy <= f32(sh) - 0.5)) {
        dst[o] = bitcast<f32>(NAN_BITS);
        return;
    }
    let flx = floor(sx);
    let fly = floor(sy);
    var reach = 1;
    if (p.interp == 1u) {
        reach = 2;
    } else if (p.interp == 2u) {
        reach = 3;
    }
    // The x weights are needed for every row, so keep them while the y weights
    // are made.
    var wx: array<f32, 6>;
    weights(sx - flx);
    for (var k = 0; k < 6; k = k + 1) {
        wx[k] = wts[k];
    }
    weights(sy - fly);
    let ix = i32(flx);
    let iy = i32(fly);
    let n = 2 * reach;
    var acc = 0.0;
    var bad = false;
    for (var j = 0; j < n; j = j + 1) {
        let yy = clamp(iy - reach + 1 + j, 0, sh - 1);
        var line = 0.0;
        for (var i = 0; i < n; i = i + 1) {
            let xx = clamp(ix - reach + 1 + i, 0, sw - 1);
            let v = src[u32(yy * sw + xx)];
            if (!finite(v)) {
                bad = true;
            }
            line = line + wx[i] * v;
        }
        acc = acc + wts[j] * line;
    }
    if (bad) {
        dst[o] = bitcast<f32>(NAN_BITS);
    } else if ((p.flags & 2u) != 0u) {
        dst[o] = (acc - p.level.x) * p.level.y + p.level.z;
    } else {
        dst[o] = acc;
    }
}
