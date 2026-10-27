// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Bias, dark and flat arithmetic of one plane:
//   v = light - bias; v = v - k * dark; v = v / flat where flat > floor.
// The CPU reference is `Calibrator::calibrate` in valerian-core.

struct Params {
    count: u32,
    flags: u32, // bit 0 bias, bit 1 dark, bit 2 flat
    k: f32,
    floor: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> light: array<f32>;
@group(0) @binding(2) var<storage, read> bias: array<f32>;
@group(0) @binding(3) var<storage, read> dark: array<f32>;
@group(0) @binding(4) var<storage, read> flat_: array<f32>;
@group(0) @binding(5) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = (wid.y * 65535u + wid.x) * 256u + lid;
    if (i >= p.count) {
        return;
    }
    var v = light[i];
    if ((p.flags & 1u) != 0u) {
        v = v - bias[i];
    }
    if ((p.flags & 2u) != 0u) {
        v = v - p.k * dark[i];
    }
    if ((p.flags & 4u) != 0u) {
        let f = flat_[i];
        if (f > p.floor) {
            v = v / f;
        }
    }
    out[i] = v;
}
