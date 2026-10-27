# How background extraction works

For a numbered walk-through on real data, with the settings and troubleshooting, see
[Tutorial 5: Background extraction](../tutorials/background.md).

Every real sky has a glow under the stars: light pollution, moonlight, airglow,
or vignetting a flat did not fully remove. It is smooth and slowly varying,
and it hides faint nebulosity, so it is modelled and taken out before the
image is stretched. The functions live in `valerian_core::background`; the
command is `valerian background`.

## What it does

1. **Tiles.** The image is cut into tiles (a thirty-second of the short side,
   at least 32 pixels, or `--tile`). Each tile gives one *sample*: the median of
   its pixels that are not masked and not missing (NaN). A tile with fewer than a
   quarter of its pixels usable gives none.
2. **Surface.** A smooth surface is fitted to the samples:
   * `--model polynomial` (the default) fits a polynomial of `--degree` 0 to 6
     in coordinates running from -1 to 1 on each axis. Degree 1 is a plane (tilt
     only, the safest), 2 follows a bowl, higher degrees follow an uneven glow
     and need clean sky across the frame.
   * `--model spline` fits a smoothing thin-plate spline through the samples, the
     kind of surface a manual "dynamic background extraction" draws. It follows
     any smooth gradient. `--smoothing` trades following the samples against
     averaging their noise: about 0.001 to 0.01 for a hundred or so tiles, and
     larger values tend to a plane.
3. **Rejection.** Samples far above the surface (a nebula, a galaxy's halo, a
   star-rich tile) or far below it are dropped and the surface is fitted again.
   The limits are `--clip-high` (2 sigma, tighter because light only adds) and
   `--clip-low` (3). The spread used is never less than the noise of a tile
   median, so a flexible surface that follows its samples closely does not
   mistake ordinary noise for outliers.
4. **Masks.** The residual of the image against the surface marks the stars:
   pixels more than `--star-sigma` robust sigmas above it, grown by
   `--mask-grow` pixels. Tiles dropped as high, and their neighbours, are
   treated as nebula and left out. The fit is repeated with these masks, for
   `--rounds` rounds, so the samples come from sky and not from the target.
5. **Apply.** The surface is evaluated every 8 pixels and interpolated, and
   subtracted from (or divided into) every pixel. Pixels without data stay NaN.

## Subtract or divide, and the level

* `--mode subtract` (the default) is for added light: gradients from light
  pollution or the moon.
* `--mode divide` is for multiplicative falloff: vignetting, or a flat that did
  not match.
* `--level preserve` (the default) takes out the gradient and puts the mean of
  the surface back, so the sky stays at the brightness it had. It removes tilt,
  not level: rebasing the sky is the stretch's job, and doing it here as well
  moves the nebula with it. `--level zero` removes the whole surface, so the sky
  goes to about zero (subtract) or one (divide).

## When there is no sky to measure

A plane fitted to a frame that a big target fills absorbs the target's outer
light. On Centaurus A the owner's pipeline measured 17.9 ADU per pixel of real
halo being quietly subtracted away. Valerian guards against it in three ways:

* `--exclude-target` leaves out an ellipse over the bright target, grown until it
  holds 45% of the flux above the sky (the pipeline's `target_mask`).
* The fit is **declined**, and the plane left exactly as it was, with the reason
  printed, when more than 55% of the image is masked (star, nebula or target),
  when fewer clean tiles remain than the model needs (at least 12), or when the
  samples scatter about the surface by more than twice the gradient it
  describes. In that last case what was measured is structure, not sky.
* A raw one-shot-colour mosaic is refused: demosaic it first
  (`valerian calibrate --debayer`), because the colours have different levels.

A declined plane is a result, not a failure: the command succeeds, says which
plane and why, and the other planes are still corrected.

## Colour, and looking at the model

Colour planes are fitted one at a time, so each colour gets its own gradient.
`--save-model` writes the fitted surface (one plane per input plane) so you can
look at what was removed: it should be smooth and should show no stars or
nebula.

## How it compares with the Python pipeline

`scripts/compare_python_background.py` restates the pipeline's `target_mask`
and `remove_gradient` (a plane fitted to tile medians outside the target,
with its mean put back) and compares the fitted gradient with Valerian's.

* On the Seestar IC 5070 stack (three planes, 1080 by 1920, a frame the nebula
  fills): the pipeline's plane has a tilt across the frame of 10.4, 6.7 and 7.6
  ADU in red, green and blue; Valerian's plane (no target exclusion, 88-pixel
  tiles) has 11.0, 6.7 and 7.7, and the models differ by 0.32, 0.03 and 0.03
  ADU rms (largest 0.7, 0.06 and 0.06). With `--exclude-target` Valerian
  declines all three planes, because 62% to 64% of the image is then masked,
  where the pipeline's script
  still fits a plane to the tiles that remain (the pipeline's own `assemble` makes
  the same call with its `fills_frame` test, at 55%).
* On the 25-frame H-alpha stack of the California Nebula (4656 by 3520 pixels),
  Valerian fits a plane (88-pixel tiles) through 626 of 1033 tiles, with a tilt
  of 68.1 ADU across the frame; the pipeline fits one to the 29% of the frame
  outside its target mask and gets 85.6 ADU, and the two models differ by 5.3
  ADU rms. With the target excluded Valerian declines (77% masked). With
  110-pixel tiles a degree 2 surface removes 109 ADU peak to peak, degree 4
  160, and the spline 141; on this frame the extra freedom has little clean sky
  to be tested against, so a plane is the safe choice here.

On a 24-core machine without a GPU, a 16-megapixel plane takes 1.0 s and
300 MiB with a polynomial, and 5.1 s with a spline (400 samples at most).

*Product and project names in this manual are trademarks of their owners and are used only to
identify them; Valerian is not affiliated with them. See [Credits and trademarks](../credits.md).*

## Not done yet

* Interactive placement of samples, as manual dynamic background extraction
  offers; the samples are a regular grid with automatic rejection.
* A background model that is shared by all channels of a mosaic of panels.
* Extraction on the GPU; it runs on the CPU, in parallel over rows.
