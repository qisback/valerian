# References

Every method in Valerian was implemented from the published description below (a paper or a
book), or from the plain behaviour that other programs document, such as the names and ranges
of a rejection scheme. No code was copied or ported from another program. Where a method is
closely related to a program's, the entry says so. The same references are in the `# References`
section at the top of each module's documentation (`cargo doc`). A reference marked "to verify"
could not be checked against the publisher's record at the time of writing.

The pages that explain each step ([calibration](../user-guide/calibration.md),
[stacking](../user-guide/stacking.md), [plate solving](../user-guide/plate-solving.md) and so on)
say what Valerian does; this page says where the idea comes from.

## Calibration, statistics and combining

Master frames, robust statistics and per-pixel rejection (`calibrate`, `stats`, `combine`).

* Howell, S. B. (2006), Handbook of CCD Astronomy, 2nd ed., Cambridge University Press, doi:10.1017/CBO9780511807909 (bias, dark and flat calibration).
* Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).
* Rousseeuw, P. J. and Croux, C. (1993), Alternatives to the median absolute deviation, J. Am. Stat. Assoc. 88, 1273-1283, doi:10.1080/01621459.1993.10476408 (the 1.4826 factor)
* Immerkaer, J. (1996), Fast noise variance estimation, Comput. Vis. Image Underst. 64, 300-302, doi:10.1006/cviu.1996.0060 (noise from local differences; here a robust variant on pixel differences).
* Huber, P. J. (1964), Robust estimation of a location parameter, Ann. Math. Statist. 35, 73-101, doi:10.1214/aoms/1177703732.
* Huber, P. J. and Ronchetti, E. M. (2009), Robust Statistics, 2nd ed., Wiley, doi:10.1002/9780470434697 (winsorising, and the 1.134 factor for its bias)
* Sigma clipping, winsorised sigma clipping and linear fit clipping are matched from the behaviour other stackers document (names and ranges), not from their code.

## Stars, registration and resampling

Star detection, star-pattern matching and fitting, and resampling (`stars`, `register`, `transform`, `warp`, `debayer`).

* Bertin, E. and Arnouts, S. (1996), SExtractor: software for source extraction, Astron. Astrophys. Suppl. 117, 393-404, doi:10.1051/aas:1996164 (the background mesh and the windowed centroid, implemented from the paper; no SExtractor code is used).
* Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).
* Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099.
* Valdes, F. G. et al. (1995), FOCAS automatic catalog matching algorithms, Publ. Astron. Soc. Pac. 107, 1119, doi:10.1086/133667.
* Fischler, M. A. and Bolles, R. C. (1981), Random sample consensus, Commun. ACM 24, 381-395, doi:10.1145/358669.358692.
* Beroiz, M., Cabral, J. B. and Sanchez, B. (2020), Astroalign: a Python module for astronomical image registration, Astron. Comput. 32, 100384, doi:10.1016/j.ascom.2020.100384 (the same family of method; used as a test oracle only, no code taken).
* Umeyama, S. (1991), Least-squares estimation of transformation parameters between two point patterns, IEEE Trans. Pattern Anal. Mach. Intell. 13, 376-380, doi:10.1109/34.88573.
* Umeyama, S. (1991), Least-squares estimation of transformation parameters between two point patterns, IEEE Trans. Pattern Anal. Mach. Intell. 13, 376-380, doi:10.1109/34.88573 (the similarity fit)
* Hartley, R. I. (1997), In defense of the eight-point algorithm, IEEE Trans. Pattern Anal. Mach. Intell. 19, 580-593, doi:10.1109/34.601246 (point normalisation for the homography fit).
* Keys, R. G. (1981), Cubic convolution interpolation for digital image processing, IEEE Trans. Acoust. Speech Signal Process. 29, 1153-1160, doi:10.1109/TASSP.1981.1163711 (the bicubic kernel; Catmull-Rom is the a = -0.5 case).
* Duchon, C. E. (1979), Lanczos filtering in one and two dimensions, J. Appl. Meteorol. 18, 1016-1022, `doi:10.1175/1520-0450(1979)018<1016:LFIOAT>2.0.CO;2`.
* Malvar, H. S., He, L.-W. and Cutler, R. (2004), High-quality linear interpolation for demosaicing of Bayer-patterned color images, Proc. IEEE ICASSP 2004, vol. 3, iii-485, doi:10.1109/ICASSP.2004.1326587 (the filter taps).

## Stacking and drizzle

Frame weighting and variable-pixel reconstruction (`stack`, `drizzle`).

* Naylor, T. (1998), An optimal extraction algorithm for imaging photometry, Mon. Not. R. Astron. Soc. 296, 339-346, doi:10.1046/j.1365-8711.1998.01314.x.
* King, I. R. (1983), Accuracy of measurement of star images on a pixel array, Publ. Astron. Soc. Pac. 95, 163, doi:10.1086/131139 (why a point source is best weighted by 1/(noise^2 FWHM^2)).
* Horne, K. (1986), An optimal extraction algorithm for CCD spectroscopy, Publ. Astron. Soc. Pac. 98, 609, doi:10.1086/131801.
* Fruchter, A. S. and Hook, R. N. (2002), Drizzle: a method for the linear reconstruction of undersampled images, Publ. Astron. Soc. Pac. 114, 144-152, doi:10.1086/338393 (implemented from the paper; none of the STScI drizzle code is used).

## Background, stretch and star removal

Gradient removal, tone curves and star removal (`background`, `stretch`, `starless`).

* Bookstein, F. L. (1989), Principal warps: thin-plate splines and the decomposition of deformations, IEEE Trans. Pattern Anal. Mach. Intell. 11, 567-585, doi:10.1109/34.24792.
* Duchon, J. (1977), Splines minimizing rotation-invariant semi-norms in Sobolev spaces, Lecture Notes in Mathematics 571, 85-100, doi:10.1007/BFb0086566 (the r^2 ln r kernel).
* Bertin, E. and Arnouts, S. (1996), SExtractor: software for source extraction, Astron. Astrophys. Suppl. 117, 393-404, doi:10.1051/aas:1996164 (a smooth background from a mesh of medians).
* Hampel, F. R. (1974), The influence curve and its role in robust estimation, J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median absolute deviation).
* Lupton, R. et al. (2004), Preparing red-green-blue images from CCD data, Publ. Astron. Soc. Pac. 116, 133-137, doi:10.1086/382245 (the arcsinh stretch).
* Payne, D. and Cranfield, M., Generalised hyperbolic stretch, documented at https://ghsastro.co.uk (implemented from the published formulae; no peer-reviewed paper found, to verify; no GHS script code is used).
* The midtone transfer function is a standard rational (Moebius) curve; its use as an automatic stretch is matched from the behaviour PixInsight documents, not from code.
* Moffat, A. F. J. (1969), A theoretical investigation of focal stellar images in the photographic emulsion and application to photographic photometry, Astron. Astrophys. 3, 455 (bibcode 1969A&A.....3..455M; no DOI).
* Shen, J. and Chan, T. F. (2002), Mathematical models for local nontexture inpaintings, SIAM J. Appl. Math. 62, 1019-1043, doi:10.1137/S0036139900368844 (harmonic inpainting).
* Young, D. (1954), Iterative methods for solving partial difference equations of elliptic type, Trans. Am. Math. Soc. 76, 92-111, doi:10.1090/S0002-9947-1954-0059635-7 (successive over-relaxation).

## Plate solving, world coordinates and colour calibration

The solver, the TAN-SIP world coordinate system, the catalogue and the star-colour calibration (`solve`, `wcs`, `catalogue`, `photocal`, `fits`).

* Lang, D., Hogg, D. W., Mierle, K., Blanton, M. and Roweis, S. (2010), Astrometry.net: blind astrometric calibration of arbitrary astronomical images, Astron. J. 139, 1782-1800, doi:10.1088/0004-6256/139/5/1782 (the idea of blind solving by matching star patterns; Valerian matches triangles, not quads, and uses none of that project's code).
* Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099.
* Valdes, F. G. et al. (1995), FOCAS automatic catalog matching algorithms, Publ. Astron. Soc. Pac. 107, 1119, doi:10.1086/133667.
* Calabretta, M. R. and Greisen, E. W. (2002), Representations of celestial coordinates in FITS, Astron. Astrophys. 395, 1077-1122, doi:10.1051/0004-6361:20021327 (the TAN projection).
* Shupe, D. L. et al. (2005), The SIP convention for representing distortion in FITS image headers, ASP Conf. Ser. 347, 491 (no DOI; to verify).
* Shupe, D. L. et al. (2012), More flexibility in representing geometric distortion in astronomical images, Proc. SPIE 8451, 84511M, doi:10.1117/12.925460.
* Gaia Collaboration, Prusti, T. et al. (2016), The Gaia mission, Astron. Astrophys. 595, A1, doi:10.1051/0004-6361/201629272.
* Gaia Collaboration, Vallenari, A. et al. (2023), Gaia Data Release 3: summary of the content and survey properties, Astron. Astrophys. 674, A1, doi:10.1051/0004-6361/202243940.
* Greisen, E. W. and Calabretta, M. R. (2002), Representations of world coordinates in FITS, Astron. Astrophys. 395, 1061-1075, doi:10.1051/0004-6361:20021326.
* Pence, W. D. et al. (2010), Definition of the Flexible Image Transport System (FITS), version 3.0, Astron. Astrophys. 524, A42, doi:10.1051/0004-6361/201015362.
* Lindegren, L. et al. (2021), Gaia Early Data Release 3: the astrometric solution, Astron. Astrophys. 649, A2, doi:10.1051/0004-6361/202039709 (Gaia DR3 astrometry is that of EDR3).
* Data: ESA/Gaia/DPAC. ESA states the licence is CC BY-NC 3.0 IGO (https://www.cosmos.esa.int/web/gaia-users/license): see docs/LEGAL-AUDIT.md.
* Riello, M. et al. (2021), Gaia Early Data Release 3: photometric content and validation, Astron. Astrophys. 649, A3, doi:10.1051/0004-6361/202039587 (the BP and RP photometry; DR3 photometry is that of EDR3).
* Casagrande, L. and VandenBerg, D. A. (2018), On the use of Gaia magnitudes and new tables of bolometric corrections, Mon. Not. R. Astron. Soc. 479, L102-L107, doi:10.1093/mnrasl/sly104 (the solar BP-RP colour of 0.82; value to verify against the paper).
* Theil, H. (1950), reprinted in Henri Theil's Contributions to Economics and Econometrics, Springer (1992), doi:10.1007/978-94-011-2546-8_20, and Sen, P. K. (1968), Estimates of the regression coefficient based on Kendall's tau, J. Am. Stat. Assoc. 63, 1379-1389, doi:10.1080/01621459.1968.10480934 (the Theil-Sen line).
* Huber, P. J. (1964), Robust estimation of a location parameter, Ann. Math. Statist. 35, 73-101, doi:10.1214/aoms/1177703732.
* Stetson, P. B. (2000), Homogeneous photometry for star clusters and resolved galaxies. II. Photometric standard stars, Publ. Astron. Soc. Pac. 112, 925-931, doi:10.1086/316595 (DOI to verify; the colour equation behind the zero point and colour term of `photocal --report` and `plot zeropoint`).
* Howell, S. B. (1989), Two-dimensional aperture photometry: signal-to-noise ratio of point-source observations and optimal data-extraction techniques, Publ. Astron. Soc. Pac. 101, 616-622, doi:10.1086/132477 (DOI to verify; the aperture signal-to-noise and magnitude error of the calibrated star catalogue).
* Merline, W. J. and Howell, S. B. (1995), A realistic model for point-sources imaged on array detectors: the model and initial results, Exp. Astron. 6, 163-210, doi:10.1007/BF00421131 (DOI to verify; the same).
* Newberry, M. V. (1991), Signal-to-noise considerations for sky-subtracted CCD data, Publ. Astron. Soc. Pac. 103, 122-130, doi:10.1086/132802 (DOI to verify; the point-source limiting magnitude at a signal-to-noise ratio).

## Mosaics

Placing panes on one canvas, matching their levels and blending the seams (`mosaic`).

* Burt, P. J. and Adelson, E. H. (1983), A multiresolution spline with application to image mosaics, ACM Trans. Graph. 2, 217-236, doi:10.1145/245.247.
* Szeliski, R. (2006), Image alignment and stitching: a tutorial, Found. Trends Comput. Graph. Vis. 2, 1-104, doi:10.1561/0600000009.
* Brown, M. and Lowe, D. G. (2007), Automatic panoramic image stitching using invariant features, Int. J. Comput. Vis. 74, 59-73, doi:10.1007/s11263-006-0002-3.

## Viewing

The image pyramid the viewer draws from (`pyramid`).

* Williams, L. (1983), Pyramidal parametrics, ACM SIGGRAPH Computer Graphics 17(3), 1-11, doi:10.1145/964967.801126 (the mip-map).
* Burt, P. J. and Adelson, E. H. (1983), The Laplacian pyramid as a compact image code, IEEE Trans. Commun. 31, 532-540, doi:10.1109/TCOM.1983.1095851.

## Data

* Gaia Data Release 3 star positions, G magnitudes and BP-RP colours (`gaia-dr3-g11.vcat`):
  (c) ESA/Gaia/DPAC. See [Credits and trademarks](../credits.md) for the acknowledgement and the licence.

## Programs used only to check Valerian

The parity scripts in `scripts/` run these Python libraries as independent references on the
developer's machine. They are not part of Valerian, not shipped and not linked: numpy, scipy,
astropy, photutils, scikit-image, `sep` and astroalign (see the list in
[Credits and trademarks](../credits.md)).
