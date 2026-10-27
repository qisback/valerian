# The grader model contract

The [Seestar sub grader](../user-guide/grading.md) runs a neural network that is trained
outside Valerian. The two sides meet at a small contract, so that a new model can be dropped in
without changing the program. This page is that contract as Valerian implements it (version 1),
and how to put a new model in. The code is `crates/valerian-science/src/grader.rs`.

## How it runs

The model is an ONNX file, run by [`tract`](https://github.com/sonos/tract), a pure-Rust ONNX
runtime (MIT or Apache-2.0). It was chosen over the `ort` binding to the Microsoft runtime
because it adds no native library to build, ship or sign, and gives the same answers. On five
real subs from three targets, the prepared inputs were bit for bit identical to those of the
Python reference code that trained the model, and the scores, defect chances and scalars
agreed to within 5e-7. `tract` is about twice as slow: with one thread on an AMD EPYC 7543,
`tract` 0.23.9 ran the 0.5 MB trained model in 15 ms per sub (the model alone, frame already
prepared), against 7.5 to 8 ms for the Microsoft runtime. A session of a few hundred subs is graded
in seconds, and `grade_all` spreads frames over the cores with rayon.

Operators are limited to those that every ONNX runtime supports in opset 17: `Conv`, `Relu`,
`BatchNormalization` (or folded into the convolutions), `ReduceMean`, `ReduceMax`, `Concat`,
`Gemm`, `Sigmoid` and `Flatten`. A model that needs more may fail to load in `tract`, and
Valerian then says that the runtime could not prepare it.

## Preparing a frame

The input is one raw Seestar S50 light sub, stored in portrait: **1920 rows by 1080 columns**
(`NAXIS2` = 1920, `NAXIS1` = 1080), 16 bits, Bayer pattern GRBG. `BZERO` and `BSCALE` are
applied first, so the values are ADU from 0 to 65535 (Valerian's FITS reader already does
this). Any other size, or any other pattern, is refused before the model is touched.

1. **Superpixel split, no interpolation.** With the array indexed `[row, col]` (row 0 is
   `G R G R ...`, row 1 is `B G B G ...`):
   * `R = raw[0::2, 1::2]`
   * `G = (raw[0::2, 0::2] + raw[1::2, 1::2]) / 2`
   * `B = raw[1::2, 0::2]`

   Each plane is 960 rows by 540 columns of `float32`.
2. **Robust normalisation of each plane `p`.**
   * `m = median(p)` (for an even count, the mean of the two middle values, as numpy does)
   * `s = max(1.4826 * median(|p - m|), 1.0)`
   * `x = clamp((p - m) / s, -20, 2000)`
   * `y = asinh(x / 2)`
3. **Two scalars**, from the green plane before normalisation:
   `[log10(max(m_G, 1)), log10(max(s_G, 1))]`, where `s_G` is the `s` of step 2.

## Inputs

| Name | Type | Shape | Content |
|---|---|---|---|
| `image` | float32 | `[N, 3, 960, 540]` | `y` for R, G, B, in that order |
| `scalars` | float32 | `[N, 2]` | the two scalars |

`N` may be dynamic; Valerian runs one frame at a time. Height and width are fixed.

## Outputs

| Name | Type | Shape | Content |
|---|---|---|---|
| `score` | float32 | `[N, 1]` | quality from 0 (unusable) to 1 (clean), already through a sigmoid |
| `defects` | float32 | `[N, 5]` | independent probabilities (not a softmax), in the class order below |

The five classes, in output order, are `cloud_haze`, `trail`, `blur`, `dew` and `star_poor`.
Valerian clamps the values to 0 to 1 and refuses a result that is not a finite number.

## Metadata

The model carries these keys in its ONNX `metadata_props`:

| Key | Value |
|---|---|
| `contract_version` | `1`. Valerian refuses a model whose major version it does not know. |
| `classes` | a JSON list of the five class names, in output order; must match exactly |
| `input_height`, `input_width` | `960`, `540` |
| `bayer_pattern` | `GRBG` |
| `model_name` | free text, shown in the log |
| `training_commit` | the commit of the training code, or `unknown` |
| `licence` | SPDX identifier of the trained weights |

## What is checked when a model loads

Before a model is used, Valerian checks, and refuses with a plain message if any of these fail:

* `contract_version` is present and its major number is the one Valerian knows;
* `classes` is the list above, in that order;
* `bayer_pattern`, if given, is `GRBG`;
* the graph has exactly the inputs `image` and `scalars` and the outputs `score` and `defects`,
  in that order, all `float32`, with the fixed dimensions above and a batch dimension that is
  dynamic or at least 1.

The tests in `grader.rs` build bad models from the test model by changing its metadata and its
declared shapes, to show each refusal.

## Versioning

Changing the preparation, the shapes or the class order bumps the major version, and then
Valerian needs a matching change. Adding a metadata key does not. A minor version (for
example `1.1`) is accepted.

## Dropping in a new model

1. Train and export a model that follows the contract above.
2. For a one-off test, run it with `--grader-model FILE` (or the Model file box in the Science
   panel). Nothing needs rebuilding.
3. To ship it, replace `crates/valerian-science/assets/seestar-subgrade-v2.onnx`, which
   `grader.rs` embeds with `include_bytes!` (sensible while the file is under about 2 MB). If
   the file name changes, change the path in `grader.rs` and the annotation in `REUSE.toml`:
   the weights are `GPL-3.0-or-later`, copyright 2026 Laurence Horrocks-Barlow, like the
   program. The test `the_built_in_model_follows_the_contract` checks the embedded file loads.
4. Update the measured results in the release notes, and the test that loads the model.

## Testing with real subs

The test `real_subs_grade_to_sane_values` grades three real subs when the environment variable
`VALERIAN_SEESTAR_SUBS` names a folder of them, and skips when it is not set. Real frames are
never committed.

## The test model

`crates/valerian-science/tests/data/untrained-seestar-subgrade.onnx` is a model with random
weights that satisfies the contract. It is a **test fixture** and nothing more: its scores
mean nothing. The tests use it to check loading, the contract checks, the output range and the
plumbing from the command line and the app to the table. The tests of the frame preparation use
synthetic mosaics with known answers, not real images. Real frames are never committed.
