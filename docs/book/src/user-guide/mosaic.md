# How mosaics work

For a numbered walk-through on real data see [Tutorial 17: Stack a mosaic](../tutorials/mosaic.md).

A mosaic session points the camera at several overlapping parts of a target: the Seestar's
"mosaic mode" folders, or any run where the mount was moved between groups of frames. A plain
`valerian stack` keeps the grid of one reference frame, so only the part of the target inside
that frame survives. `valerian stack --mosaic` builds one larger canvas instead. The code is
`valerian_core::mosaic` and the mosaic branch of `valerian_core::stack_stream`.

## The steps

1. **Register jointly.** Every frame is first matched to the reference by star patterns, as for
   any stack ([How registration works](registration.md)). Frames that see a different part of
   the sky fail that match. They are matched to *anchors* instead: one already-placed frame for
   each pointing found so far. A frame that matches an anchor gets the anchor's transform to the
   reference composed with its own, and becomes an anchor itself if it sits at a new pointing,
   so panes that only overlap their neighbour are reached in later rounds (at most twelve).
   This is the chain of pairwise registrations of Brown and Lowe (2007) with the star pattern
   matcher in place of image features.
2. **Check the matches.** A plain stack accepts a match of six stars. A wrong match there only
   drops one frame; in a mosaic it would put a frame in the wrong place. Mosaic registration
   therefore needs **10 matched stars** and a scale within 2 per cent of 1 (one camera and
   lens). Frames that fail are skipped and listed with the reason.
3. **Grow the canvas** to the bounding box of every placed frame's corners, in the reference
   frame's pixels. Pixels no frame reaches hold no data (NaN, as at the edge of any stack). A
   canvas more than 12 times the area of one frame is refused as a sign of a wrong registration.
4. **Match levels.** Each pane has its own sky level and gradient, and a step between panes is
   the most visible flaw of a mosaic. Valerian fits one additive level per frame and colour
   plane from the overlaps, on copies binned 8 by 8: it forms the weighted mean of all frames,
   moves each frame by the median of its difference from that mean over the pixels at least two
   frames cover (the median ignores stars and the target, which all frames see alike), and
   repeats until the largest move is under 0.02 ADU. The reference stays at zero. This is the
   gain compensation of Brown and Lowe, additive because the sky is an offset (transparency is
   already a scale per frame). Turn it off with `--no-mosaic-level-match`.
5. **Feather and combine.** Each frame's samples are weighted by a ramp that rises linearly
   from nearly zero at its edge to one `--mosaic-feather` pixels inside it (default 120).
   Where panes overlap, each contributes in proportion to its distance from its own edge, so
   the seam is a cross-fade (Szeliski 2006; the wide-zone reasoning is Burt and Adelson 1983).
   Inside a pane, where many dithered frames overlap, the ramp changes almost nothing. Outlier
   rejection runs on the samples as usual; the weighted mean uses frame weight times ramp. The
   combine runs on the processor (the per-sample ramp is not in the graphics-card combiner);
   the resampling still uses the graphics card.
6. **Cut to depth (optional).** `--mosaic-min-coverage N` keeps only the part of the canvas that
   at least N frames reach and cuts the canvas to it. A mosaic is deep in the middle and thin
   at the edges (M 33 below runs from 144 frames to one); the thin rim is noisy.

The output keeps the reference header, adds `MOSAIC`, `MOSFEATH` (and `MOSMINCV`) and moves
`CRPIX1` and `CRPIX2` to the canvas when the header has a world coordinate system. The
position hint (`RA`, `DEC`) is still the reference frame's, which is inside the canvas, so
`valerian solve` finds the field from it.

## What to expect

On the 167 raw Seestar S50 frames of the M 33 mosaic folder, the plain stack placed 140 frames in
one 1080 x 1920 pane (27 failed to register) and cut M 33 off at the corner. With `--mosaic` all
167 were placed on a 2568 x 4390 canvas; `--mosaic-min-coverage 12` cut it to 2118 x 3627. The
plate solution of the result fits 0.19 pixel rms over the whole canvas, so the frames agree
across the canvas to a fraction of a pixel. The Seestar's own stack of the same folder is a
1296 x 2304 crop of the well-covered middle.

## Limits

* Levels are matched per frame, not per pixel: a gradient that differs between panes (light
  pollution, dusk) is not removed by this step. Run `valerian background` on the result.
* Registration is similarity (shift, rotation, scale 1). Lens distortion at the corners, which
  differs between panes, is not modelled.
* The staging file holds the whole canvas for every frame: `frames x canvas width x canvas
  height x (planes + 1) x 4` bytes (about 30 GB for the M 33 run). Point `--scratch-dir` at a disk with
  room.
* `--mosaic` does not combine with `--science-report`.
* Pane-to-pane registration chains can accumulate error in a long, thin chain of panes.

## References

* Burt, P. J. and Adelson, E. H. (1983), A multiresolution spline with application to image mosaics, ACM Trans. Graph. 2, 217-236, doi:10.1145/245.247.
* Szeliski, R. (2006), Image alignment and stitching: a tutorial, Found. Trends Comput. Graph. Vis. 2, 1-104, doi:10.1561/0600000009.
* Brown, M. and Lowe, D. G. (2007), Automatic panoramic image stitching using invariant features, Int. J. Comput. Vis. 74, 59-73, doi:10.1007/s11263-006-0002-3.
* Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099.
