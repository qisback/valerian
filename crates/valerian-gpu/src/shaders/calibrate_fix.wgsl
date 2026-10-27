// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Cosmetic correction: a flagged pixel becomes the median of its unflagged,
// finite same-colour neighbours, searching one to three rings out (the ring
// spacing is `step`, 2 for a Bayer mosaic) until at least three are found.
// Unflagged pixels are copied. The CPU reference is `PixelMap::correct`.

struct Params {
    width: u32,
    height: u32,
    step: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> flags: array<u32>; // four pixels per word
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;
@group(0) @binding(4) var<storage, read_write> repaired: array<atomic<u32>>; // one word

fn flag_at(i: u32) -> u32 {
    return (flags[i >> 2u] >> ((i & 3u) * 8u)) & 255u;
}

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = (wid.y * 65535u + wid.x) * 256u + lid;
    if (i >= p.count) {
        return;
    }
    if (flag_at(i) == 0u) {
        dst[i] = src[i];
        return;
    }
    let x = i32(i % p.width);
    let y = i32(i / p.width);
    let w = i32(p.width);
    let h = i32(p.height);
    let step = i32(p.step);
    var vals: array<f32, 24>;
    var found = false;
    var result = src[i];
    for (var ring = 1; ring <= 3; ring = ring + 1) {
        let r = ring * step;
        var n = 0u;
        for (var dy = -r; dy <= r; dy = dy + step) {
            for (var dx = -r; dx <= r; dx = dx + step) {
                if (max(abs(dx), abs(dy)) != r) {
                    continue;
                }
                let xx = x + dx;
                let yy = y + dy;
                if (xx < 0 || yy < 0 || xx >= w || yy >= h) {
                    continue;
                }
                let j = u32(yy * w + xx);
                let v = src[j];
                if (flag_at(j) == 0u && finite(v)) {
                    vals[n] = v;
                    n = n + 1u;
                }
            }
        }
        if (n >= 3u) {
            // Insertion sort, then the median as the CPU defines it.
            for (var a = 1u; a < n; a = a + 1u) {
                let key = vals[a];
                var b = a;
                while (b > 0u && vals[b - 1u] > key) {
                    vals[b] = vals[b - 1u];
                    b = b - 1u;
                }
                vals[b] = key;
            }
            let mid = n / 2u;
            if ((n & 1u) == 1u) {
                result = vals[mid];
            } else {
                result = (vals[mid - 1u] + vals[mid]) * 0.5;
            }
            found = true;
            break;
        }
    }
    dst[i] = result;
    if (found) {
        atomicAdd(&repaired[0], 1u);
    }
}
