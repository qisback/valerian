# How registration works

For a numbered walk-through on real data, with the settings and troubleshooting, see
[Tutorial 3: Register](../tutorials/register.md).

Registration lines up every frame with a reference so that the same star falls
on the same pixel in all of them. Valerian does it from the stars alone, with
no starting guess, so a dither, a rotation or a meridian flip is as easy as no
movement at all. The functions live in `valerian_core::stars`,
`valerian_core::register`, `valerian_core::transform` and
`valerian_core::warp`; the command is `valerian register`.

## Finding the stars

1. **Background.** The sky level and its noise are measured in tiles (a
   sigma-clipped median, so stars do not lift it), the tile grid is
   median-filtered so that a tile on a big galaxy does not make a bump, and the
   grid is interpolated to every pixel.
2. **Detection.** The background-subtracted frame is smoothed with a Gaussian
   and every local maximum above a threshold, in units of the noise of the
   *smoothed* frame, becomes a candidate. The noise is measured on the smoothed
   frame itself, so frames whose noise is correlated (resampled or stacked
   ones) do not produce false stars.
3. **Centroid and shape.** Each candidate is refined by an iterative,
   Gaussian-weighted centroid, which reaches a few hundredths of a pixel on a
   good star. The same weighted second moments, corrected for the width of the
   window, give the FWHM, the ellipticity and the orientation of the star.
   Candidates narrower than a real star (hot pixels, cosmic rays) are dropped,
   and a star at or above the saturation level is flagged.

On the five-minute H-alpha frames used for testing (ASI1600MM, 4656 by 3520
pixels) detection takes about 0.4 s per frame and finds the 1000 brightest
stars with a median FWHM near 8 pixels.

## Matching

Triangles of stars are compared by the ratios of their sides, which do not
change with rotation, scale or shift. Each star is combined with pairs of its
nearest neighbours among the brightest eighty, and every matched triangle votes
for the three star pairings it implies. The most voted pairings are then tried
two at a time as a similarity transform, and the one that lands the most source
stars on reference stars wins (RANSAC). That is refined by least squares on
every star that matches, with the match radius shrinking each round and
outliers beyond three times the scatter clipped.

The fitted model is your choice:

| Model | Parameters | Use it for |
|---|---|---|
| Similarity | 4 | Rotation, uniform scale and shift: the normal case. |
| Affine | 6 | Slight shear or unequal scale, as with some field rotation or atmospheric refraction. |
| Homography | 8 | Wide fields and perspective. Needs many stars across the whole frame. |

Frames at a different scale (2x2 binned against unbinned) work within the
scale range (0.45 to 2.2 by default). Mirror images are not matched: a
telescope does not mirror between frames, and a meridian flip is a rotation by
180 degrees. Distortion-aware warps are not implemented yet.

If too few stars are found, no common pattern exists, or no consistent
transform is found, registration fails with a clear message rather than
returning a poor fit.

## Resampling

`warp` resamples a frame onto the reference grid with bilinear, bicubic
(Catmull-Rom, the default) or Lanczos-3 interpolation. Where the source does
not cover an output pixel the result is NaN, which the stacker reads as "no
data", so a rotated or shifted frame needs no separate mask.

## What the tests show

Registration is tested on synthetic star fields with known rotations, shifts and
scale: the fitted transform maps the true star positions to within 0.03 pixel,
and stars measured again after warping land within 0.05 pixel of the
reference's. A 180 degree rotation, a 310 by 190 pixel dither with only part of
the field in common, a fifth of the stars missing and a third as many
spurious ones, a half-scale frame and a sheared frame all register. Unrelated
star fields are refused.

On the real session, five frames registered to the first matched about 790
stars each and landed with a mean offset under 0.05 pixel; the scatter of
single bright stars about that mean is 0.1 pixel, which is the centroid noise of
a star with an 8 pixel FWHM.
