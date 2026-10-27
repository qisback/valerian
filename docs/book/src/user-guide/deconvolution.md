# How deconvolution works

For a numbered walk-through on real data, see
[Tutorial 16: Deconvolution](../tutorials/deconvolution.md).

A stack of 10 second sub-frames from a small alt-azimuth telescope has wider and less round stars
than a long exposure on a tracking mount would give. Seeing, tracking error, field rotation during
each exposure and the lens's aberrations all blur the sub-frames, and averaging cannot undo that. On
six Seestar S50 sessions the stars of a Valerian stack were up to 36 per cent wider than the Seestar's
own stacks of the same frames (issue 125). `valerian deconvolve` sharpens the stack with the blur
measured from its own stars. The code is `valerian_core::deconvolve`.

## The method

1. **Stars.** Stars are detected on the luminance. Those used to measure the blur are bright (peak over
   noise above `--psf-min-snr`), not near the clip level, of normal width and isolated.
2. **A PSF for each tile.** The image is cut into tiles of about 480 pixels (or `--tiles AxB`). For
   each tile the point spread function (PSF) is the per-pixel median of the sub-pixel-centred, unit-sum
   stamps of its nearest good stars, clipped at zero, made point-symmetric and faded to zero in the
   wings by a flat-topped cosine window. The sub-pixel shift is a Catmull-Rom interpolation. A tile with
   fewer than `--min-psf-stars` widens its search, so the PSF follows the field's elongation and
   aberration from the middle to the corners. (Version 0.4.0 faded the wings with a Gaussian, which
   narrowed the core: on synthetic stars of known shape it under-stated the elongation by 10 per cent and
   the width by 4 per cent, so the stars were rounded less than they should have been. The flat-topped
   window brings those to 4 and under 1 per cent.)
3. **Clipped stars.** A saturated star peaks at the clip level whatever its true brightness, so the
   highest peaks of an image with clipped stars pile up at one value per colour plane. Valerian looks
   for that pile (at least five peaks within 4 per cent, flat on top, and three times as many as in the
   band below it) and takes the pixels at the top of it as clipped. They get zero weight in the
   Richardson-Lucy fit, along with their neighbours and any missing data, and the core they hide is
   first filled with the measured PSF scaled to the intact wings and then rebuilt by the iterations. A
   clipped core fed to Richardson-Lucy as if it were data pulls the fit down to its flat top and leaves
   a black core inside a dark ring. `--saturation` gives the level when the pile is not clear (a single
   bright star), or turns this off.
4. **A target.** In the default `round` mode the stars are deconvolved to round stars of one width
   everywhere. The elongation of each tile's PSF is first blurred away along its narrow axis, by the share
   `--roundness` (default 0.5) of the difference between the two axes; the iterations then sharpen with
   a Gaussian kernel of the covariance left once the round target is taken out. The target FWHM is
   `--target-fwhm`, or `--target-ratio` (0.7) of the narrowest PSF axis of any tile. `--mode full`
   deconvolves by the whole measured PSF instead: the sharpest result, which keeps whatever elongation
   the iterations leave.
5. **Richardson-Lucy.** Every colour plane of every tile (with a margin) is deconvolved by
   Richardson-Lucy iterations with the total-variation damping of Dey et al., which keeps noise from
   turning into speckle. The tiles are blended with a linear ramp over their overlap, so no seam shows.
   Missing data (NaN, outside the coverage) stays missing.
6. **Ring guard.** Where a star stands far above the rest of the image, the result is held to the lowest
   value the input takes nearby, so a PSF that is a little too wide in the wings of a bright star cannot
   dig a dark ring into its halo. `--ring-guard` sets the strength; 0 turns it off.

Richardson-Lucy conserves flux and cannot make a value negative, so run it on the linear stack, after
`valerian background` if there is a gradient and before `valerian stretch`.

### Why `round` needs the blur

Deconvolving by the elongated remainder alone (what version 0.4.0 did, and what `--roundness 0` still
does) converges much more slowly along the long axis of the PSF than along the short one. On
synthetic stars with a known elongation, 20 iterations left an ellipticity of 0.13 from an original 0.31 (with the
damping off), 60 left 0.06 and 200 left 0.02; on real stacks the noise stops the iterations well short of that, so the
stars end up almost as elongated as before. Blurring the short axis first makes the kernel round, and a
round kernel converges at the same rate in every direction. The price is sharpness: the iterations have
the blur to take out again.

## Settings

* `--iterations` (default 20): more is sharper and noisier.
* `--tv` (default 0.002): the damping. Raise it if the noise grows, lower it if faint stars do not
  sharpen (on a field of faint stars 0.005 stopped the stars short of the target; 0.001 reached it).
  A higher value also calms the halos of bright stars, at the cost of width: on M 34, 0.005 and 0.01
  made the median star 4 and 11 per cent wider.
* `--roundness` (default 0.5, range 0 to 1): how far `round` evens out the elongation before it
  sharpens. 1 gives the roundest stars and the widest, 0 the sharpest and the least round.
* `--target-ratio` (default 0.7): lower is sharper, noisier and more prone to ringing around bright stars.
* `--saturation auto|off|LEVEL` (default `auto`): the pixel value at which stars are clipped. `auto` finds
  it from the pile-up of the brightest peaks; give the number when only one star is clipped, or `off`.
* `--ring-guard STRENGTH` (default 1, range 0 to 1): the guard against dark rings round bright stars.
* `--psf-fwhm PIXELS`: a round Gaussian PSF everywhere, for an image with too few stars to measure one.
* `--psf-radius PIXELS` (default 10, range 3 to 24): the half-width of the PSF kernel, so the kernel is 2r+1 pixels
  across. It is also the size of the stamp cut round each measured star, and a star must have no brighter neighbour
  within r+2 pixels to be used. Raise it when the stars have wide wings (poor seeing, slight defocus), lower it to run
  faster on stars that are already tight; every iteration convolves with the kernel, so the time grows with its area.

## What it does and does not do

It sharpens stars and nebula by the same measure. It does not recover detail the sub-frames never had.
Measured on four Seestar S50 stacks (the tutorial has the table), the default makes the stars 17 to 20
per cent narrower than the stack and rounder on all four (median eccentricity from 0.158 to 0.133 on
IC 1795, from 0.143 to 0.116 on C 39, from 0.236 to 0.114 on M 42; M 34 was already round at 0.104
and goes to 0.065), where `--roundness 0` leaves C 39 and IC 1795 as elongated as the stack. It does not
match the Seestar's own stacks for roundness (0.04 to 0.09 on the same frames): the PSF is one kernel
per tile of about 480 pixels, and the elongation changes faster than that towards the corners.

A faint dark halo remains round the very brightest stars: on the four test stacks the darkest pixel within nine
pixels of the 30 brightest stars was at worst 0 to 2.7 sigmas below the sky (one M 42 star 15), against 16 to 36 with
version 0.4.0. Bright stars also keep a small amount of colour fringing in the rebuilt core, because each colour plane is
rebuilt from its own wings. Very bright stars that are clipped in only one frame of many are not found by
`auto`; use `--saturation`.

## References

* Richardson, W. H. (1972), Bayesian-based iterative method of image restoration, J. Opt. Soc. Am. 62,
  55-59, doi:10.1364/JOSA.62.000055.
* Lucy, L. B. (1974), An iterative technique for the rectification of observed distributions, Astron. J.
  79, 745, doi:10.1086/111605.
* Dey, N. et al. (2006), Richardson-Lucy algorithm with total variation regularization for 3D confocal
  microscope deconvolution, Microsc. Res. Tech. 69, 260-266, doi:10.1002/jemt.20294.
* Hanisch, R. J., White, R. L. and Gilliland, R. L. (1997), Deconvolution of Hubble Space Telescope
  images and spectra, in Deconvolution of Images and Spectra, 2nd ed., Academic Press, 310-356 (bad
  pixels taken out of the likelihood).
* Snyder, D. L., Hammoud, A. M. and White, R. L. (1993), Image recovery from data acquired with a
  charge-coupled-device camera, J. Opt. Soc. Am. A 10, 1014-1023, doi:10.1364/JOSAA.10.001014 (the
  expectation-maximisation form of Richardson-Lucy for CCD data, from which the missing-data update
  follows).
* Catmull, E. and Rom, R. (1974), A class of local interpolating splines, in Computer Aided Geometric
  Design, Academic Press, 317-326.

The convolution kernel type is shared with Helia (the same author's code); the Richardson-Lucy update,
the PSF measurement, the tiling, the clipped-star handling and the round target are written here from
the papers above. No code from other programs was used, and no new dependency was added.
