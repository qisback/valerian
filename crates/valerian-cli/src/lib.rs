// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The `valerian` command-line definition and its handlers.
//!
//! The clap definition lives here so `cargo xtask docs` can generate the CLI
//! reference from it.

// First: its macros shadow print! and println! in the modules after it.
#[macro_use]
mod out;
mod astrometry;
mod backend;
mod background;
mod bench;
mod calibrate;
mod deconvolve;
mod drizzle;
mod files;
mod grade;
mod info;
mod optics;
mod photocal;
mod register;
mod science;
mod solve;
mod stack;
mod starless;
mod stretch;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use valerian_core::calibrate::DarkScaling;
use valerian_core::combine::Rejection;
use valerian_core::debayer::Method;
use valerian_core::stack::{Normalisation, Weights};
use valerian_core::transform::Model;
use valerian_core::warp::Interpolation;
use valerian_gpu::BackendChoice;

/// Deep-sky image processing.
#[derive(Debug, Parser)]
#[command(name = "valerian", version, about = "Deep-sky image processing", long_about = None)]
pub struct Cli {
    /// Keep the observing site (SITELAT, SITELONG, OBSGEO-* ...), device serial numbers and unit
    /// identifiers (the hex part of a Seestar's TELESCOP) in the headers of files written. By
    /// default they are dropped from everything written.
    #[arg(long, global = true)]
    pub keep_location: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Calibrate light frames with bias, dark, flat and dark-flat frames.
    #[command(
        long_about = "Calibrate light frames: subtract the master bias and dark, divide by the \
master flat and repair hot, cold and dead pixels. Each calibration argument is a file or a \
directory of FITS files and may be repeated. Several files are combined into a master; a single \
file whose IMAGETYP says it is a master is used as it is. Each light is written to the output \
directory as a 32-bit float FITS file with `_cal` added to its name. Calibration frames are \
streamed from disk in bands of rows, so memory use follows `--memory` and not the number of \
frames."
    )]
    Calibrate(CalibrateArgs),
    /// Register frames to a reference and write them on its grid.
    #[command(
        long_about = "Find the stars in every frame, register them to the sharpest (or the \
one named), resample each onto the reference grid and write it as a 32-bit float FITS file with \
`_reg` added to its name. Pixels a frame does not cover are NaN. Raw Bayer frames are demosaiced \
first."
    )]
    Register(RegisterArgs),
    /// Register, normalise, weight and combine frames into one image.
    #[command(
        long_about = "Register the frames, match their sky levels and transparency, weight \
them by noise and star width and combine them with per-pixel outlier rejection. The result is \
written as a 32-bit float FITS file with NCOMBINE, EXPTOTAL and the reference in its header. A \
table with one line per frame is printed. Raw Bayer frames are demosaiced first. With --grade, raw Seestar S50 subs are scored by a neural-network grader first, and the ones below --min-grade (or over a --reject-defect limit) are left out with the reason printed; --grade-weights also multiplies each kept frame's weight by its score. Other frames are not graded. The frame limits (--max-fwhm, --max-ellipticity, --min-star-count, --max-noise, --min-snr-weight, --max-residual, --outlier-sigma) leave the frames they reject out of the stack: the frames are registered and judged, then registered and combined again without the rejected ones, and the names and reasons are printed. With --science-report the report still lists every frame."
    )]
    Stack(StackArgs),
    /// Combine dithered frames onto a finer grid with drizzle.
    #[command(
        long_about = "Drizzle (variable-pixel linear reconstruction) for undersampled, dithered frames. The frames are registered as for `stack`, then every input pixel is shrunk to a drop of --pixfrac of its side, mapped onto an output grid --scale times finer and spread over the output pixels it overlaps in proportion to the overlap, weighted by noise and star width. Pixels more than --reject sigma from a first-pass result (cosmic rays, hot pixels, satellite trails) are left out of a second pass. Raw Bayer mosaics are dropped as mosaics, each pixel into its own colour plane with no interpolation, so give the calibrated frames from `valerian calibrate` without --debayer. The result is --scale times as wide and as tall, in the input's units, with output pixels no drop reached left as NaN (--weight-map shows the coverage). It needs a dither of a pixel or more between frames and a dozen or more of them to fill a 2x grid; with too few frames use a larger --pixfrac or a smaller --scale."
    )]
    Drizzle(DrizzleArgs),
    /// Time the GPU kernels against the CPU code and check they agree.
    #[command(
        long_about = "Run calibration, the registration warp and the stack's per-pixel combiner (sigma clipping, winsorised and linear fit) on made-up frames of the size asked for, on the CPU and on the GPU, and print the median times, the speed-up and how far the two results differ (as a fraction of the data range, and in pixels that have data in only one). One run warms each kernel up first, so the times are steady state. --adapter hardware (the default) uses a real GPU, software the fallback adapter (lavapipe, WARP) that CI uses; the software adapter is slower than the CPU code and exists to test the kernels. The same switch, --backend auto|cpu|gpu, is on calibrate, register and stack; auto uses a real GPU only for stack, where the frames stay on the device from the warp to the combine, because for one stage alone the copies across the bus cost more than the kernel saves."
    )]
    Bench(BenchArgs),
    /// Model and remove the sky gradient of an image.
    #[command(
        long_about = "Fit a smooth surface to the sky of a stacked image and take it out. The \
image is cut into tiles, each gives the median of its clear pixels, and a polynomial or a \
thin-plate spline is fitted to those samples, rejecting samples that stars, nebulosity or the \
target lift above the sky. Stars are then masked from the residual and the fit is repeated. By \
default the surface is subtracted and the mean level is put back, so the gradient goes and the sky \
stays where it was. If too little clear sky is left (a target that fills the frame) the plane is \
reported as declined and left unchanged instead of subtracting the target. Colour planes are fitted \
one by one; raw mosaics must be demosaiced first."
    )]
    Background(BackgroundArgs),
    /// Stretch a linear image to a viewable one (MTF, arcsinh or GHS).
    #[command(
        long_about = "Stretch a linear image. The black point is put just below the sky and the white point at a high percentile (or set both with --black and --white); the curve then bends the range onto 0 to 1: the midtone transfer function (the default, which puts the sky at --target), arcsinh, or the generalised hyperbolic stretch. The input is never changed. The output is a 32-bit float FITS file in 0 to 1, and its header records the stretch, so `--invert` can undo it on the data that was not clipped. `--preview` writes the result as a reduced PNG as well (or instead of the FITS file) to look at an automatic stretch before committing to it. Colour planes share one stretch unless --unlinked is given."
    )]
    Stretch(StretchArgs),
    /// Remove or reduce the stars of an image.
    #[command(
        long_about = "Remove the stars from an image with a classical method and no learned model: stars are detected on the luminance, each gets a mask disc sized from its brightness and width, and the masked pixels of every plane are filled by diffusion (Laplace inpainting) from the pixels around them, with noise of the local level added so the patches do not show. The input is never changed. The starless image is written to -o; --stars-out writes what was taken out (original minus starless), which can be added back later; --mask-out writes the mask. --strength below 1 removes only that fraction of every star, reducing them. Work on the linear image or a stretched one; raw mosaics must be demosaiced first. It does not recreate texture a star hides, and diffraction spikes and stars below the detection limit stay."
    )]
    Starless(StarlessArgs),
    /// Sharpen a stack with the stars' own point spread function (Richardson-Lucy).
    #[command(
        long_about = "Sharpen a linear image by deconvolution. Stars are detected, the bright isolated ones are stamped, centred to a fraction of a pixel and combined into a point spread function (PSF) for each tile of the image, so the PSF follows the field's elongation and aberration from the middle to the corners. Each colour plane of each tile is then deconvolved by Richardson-Lucy iterations with total-variation damping (Dey et al. 2006) and the tiles are blended over their overlaps. Run it on the linear stack, after `background` if there is a gradient, and before `stretch`. More iterations are sharper and noisier; the default is a measured middle ground. A Gaussian PSF of a given FWHM can be set with --psf-fwhm when there are too few stars. The input is never changed; the report lists the PSF of every tile."
    )]
    Deconvolve(DeconvolveArgs),
    /// Plate-solve an image and write its world coordinate system (TAN-SIP).
    #[command(
        long_about = "Find where on the sky an image points and write the world coordinate system into a copy of it. Stars are detected, the catalogue stars of the patch of sky the header (or --ra and --dec) names are matched to them by triangle patterns, and a TAN projection with optional SIP distortion is fitted to every star that agrees. The position comes from the RA and DEC (or OBJCTRA and OBJCTDEC) keywords or an existing WCS, the pixel scale from FOCALLEN and XPIXSZ, and either can be given on the command line. With no position at all the whole sky is searched, which needs --scale or --scale-range and takes longer (--blind says so on purpose). The catalogue is Gaia DR3 to G = 11, found beside the program, in VALERIAN_CATALOGUE or with --catalogue; build a deeper one with scripts/build_catalogue.py for fields much narrower than half a degree. At least ten stars must agree or nothing is written. The input is never changed."
    )]
    Solve(SolveArgs),
    /// Calibrate the colour of an image against Gaia star colours.
    #[command(
        long_about = "Set the white balance of a colour image from the stars in it. The stars of the plate solution (the image's own WCS, one from --wcs, or a quick solve from the header) are matched to Gaia stars with a BP-RP colour, each is measured through the same aperture in red, green and blue, and a line of instrumental colour against BP-RP is fitted for red and for blue against green with outlier rejection. The gains make a star of --reference-colour (0.82, the Sun) neutral; green is left alone so the brightness stays. The sky of the three planes is then brought to a common level so the background is neutral grey (--no-neutralise leaves it). The input must be a linear red, green, blue image (not stretched, not a raw mosaic) and the catalogue must carry colours (version 2); calibrate after background extraction and before stretching. The gains and offsets are written to the header as PCCGAINR, PCCGAING, PCCGAINB, PCCOFFR, PCCOFFG and PCCOFFB. Without -o the result is only printed. The input is never changed."
    )]
    Photocal(PhotocalArgs),
    /// Measure every frame and approve or reject it, with plots and a report.
    #[command(
        long_about = "Subframe analysis in the manner of PixInsight's SubframeSelector. The frames are registered to the sharpest and each is measured: stars, FWHM, eccentricity, sky background, noise, an SNR weight (relative to the median, from the noise and the transparency) and the registration residual. Frames are approved or rejected against the limits given (a frame that does not register is always rejected). --report writes subframes.csv and subframes.json (the approve/reject table, with the reason for each rejection), plots of every quantity against frame number and against time as PNG, and report.html, which holds them all and can be opened or mailed on its own. Edit the `approved` column of the CSV by hand if you disagree, then give it to `valerian stack --approve` so the stack uses what you approved. With --grade, raw Seestar S50 subs are also scored by a small neural-network grader (0 unusable to 1 clean, with the chance of cloud or haze, trail, blur and too few stars, and a dew chance that is not reliable); the scores join the table and frames below --min-grade, or over a --reject-defect NAME:P limit, are rejected. The grader is trained on Seestar S50 subs only; other frames are skipped with a note. The input is never changed."
    )]
    Subframes(SubframesArgs),
    /// Draw the plate solution on an image: RA/Dec grid, catalogue objects, Gaia stars, residuals.
    #[command(
        long_about = "Draw a plate solution on its image. The image's own world coordinate system is used (written by `valerian solve`), or the one in --wcs, or one found by a quick solve from the header's position and scale. The picture is the automatically stretched image, reduced to --max-side pixels, with an RA/Dec grid whose spacing is chosen for the field and labelled at the edges, the objects of the bundled NGC, IC and Messier list that lie in the field (an ellipse of the catalogued size and angle, named), and the Gaia stars brighter than --star-mag (labelled with their magnitude when brighter than --label-mag). Each layer can be left out. The residuals of the solution are also drawn: every detected star that matches a Gaia star within 3 pixels gives a vector (detected minus where the solution puts the catalogue star), shown over the frame with exaggerated arrows and against magnitude, with the rms in pixels and arc seconds. --report writes annotated.png, annotated_objects.csv, annotated_stars.csv, wcs_residuals.png and .csv, astrometry.json and report.html. The bundled object list is a subset of OpenNGC (CC BY-SA 4.0). The input is never changed."
    )]
    Annotate(AnnotateArgs),
    /// Aperture photometry of one star across frames: a light curve with comparison stars.
    #[command(
        long_about = "Measure one star in every frame. The frames are registered to the sharpest, the star's position in the reference frame (--star X,Y, or the brightest unsaturated star) is carried to every frame through the registration and refined by a centroid, and its flux is the sum of the pixels in a circle of --aperture times the reference FWHM (with the pixels cut by the edge counted by their fraction) less the clipped median of a sky annulus. Up to --comparison bright, isolated, unsaturated stars of the reference frame are measured the same way; their summed flux is the ensemble, and the star's magnitude against it, minus its median, is the relative magnitude, with an error from the sky noise. The first comparison star is measured against the others as a check star, which should stay flat. --report writes lightcurve.png, lightcurve.csv (time, position, flux, sky, S/N, magnitudes, error, flag per frame), lightcurve.json and report.html. Frames without DATE-OBS are plotted by number."
    )]
    Photometry(PhotometryArgs),
    /// Aberration inspector, FWHM and eccentricity maps, tilt hint and PSF profile of an image.
    #[command(
        long_about = "What the stars of one image say about the telescope. The stars are detected on the luminance and measured (FWHM, eccentricity, orientation); those that are not saturated and have a peak of at least --min-snr times the noise are kept (at least 30 are needed). --report writes: aberration_inspector.png, a 3 by 3 mosaic of crops at the corners, edges and centre, stretched alike, each labelled with the median FWHM and eccentricity of the stars around it; fwhm_map.png and eccentricity_map.png, a polynomial surface of degree --order fitted to the stars with 3-sigma rejection and drawn as an interpolated image with contours and the stars on it (whiskers on the eccentricity map show which way the stars are elongated); tilt.png and a hint in words from the FWHM of the centre and the four corners (curvature, tilt, the softest corner, and whether the outer stars are elongated along or across the radius); and psf.png, the stacked radial profile of the brightest isolated stars with a Moffat and a Gaussian fit. Every plot has its data beside it as CSV or JSON (optics.json holds the tilt, the panels and the fit) and report.html gathers them. The hint says what the pattern is consistent with; it cannot tell which way to move a spacer from one image. The input is never changed."
    )]
    Optics(OpticsArgs),
    /// Draw one science plot (PNG, with its CSV and JSON beside it).
    #[command(
        long_about = "Draw one of the science plots on its own. KIND is subframes: the per-frame plots of `valerian subframes`, from frames (they are measured first) or from a subframes.json written earlier. The PNG is written to -o, with a CSV and a JSON of the plotted data beside it. --x chooses frame number or time on the horizontal axis. Use `--report` of the producing command for a whole session at once."
    )]
    Plot(PlotArgs),
    /// Describe FITS files, or this build and the machine it runs on.
    Info(InfoArgs),
}

/// Rejection scheme.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum RejectionArg {
    /// Winsorised below 15 frames, sigma clipping to 30, linear fit above.
    /// `--low` and `--high` change the limits of the scheme it picks.
    #[default]
    Auto,
    /// Keep every sample.
    None,
    /// Drop the lowest and highest sample.
    Minmax,
    /// Sigma clipping about the median.
    Sigma,
    /// Winsorised sigma clipping: robust with few frames.
    Winsorised,
    /// Linear fit clipping: robust when levels vary.
    LinearFit,
}

impl RejectionArg {
    /// The scheme, with the given limits where it takes them.
    #[must_use]
    pub fn resolve(self, low: Option<f32>, high: Option<f32>, frames: usize) -> Rejection {
        match self {
            Self::Auto => match Rejection::auto(frames) {
                Rejection::Winsorised { low: l, high: h } => Rejection::Winsorised {
                    low: low.unwrap_or(l),
                    high: high.unwrap_or(h),
                },
                Rejection::Sigma { low: l, high: h } => Rejection::Sigma {
                    low: low.unwrap_or(l),
                    high: high.unwrap_or(h),
                },
                Rejection::LinearFit { low: l, high: h } => Rejection::LinearFit {
                    low: low.unwrap_or(l),
                    high: high.unwrap_or(h),
                },
                other => other,
            },
            Self::None => Rejection::None,
            Self::Minmax => Rejection::MinMax { low: 1, high: 1 },
            Self::Sigma => Rejection::Sigma {
                low: low.unwrap_or(3.0),
                high: high.unwrap_or(3.0),
            },
            Self::Winsorised => Rejection::Winsorised {
                low: low.unwrap_or(3.0),
                high: high.unwrap_or(3.0),
            },
            Self::LinearFit => Rejection::LinearFit {
                low: low.unwrap_or(5.0),
                high: high.unwrap_or(2.5),
            },
        }
    }
}

/// Demosaicing.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum DebayerArg {
    /// Keep a raw mosaic as it is (calibrate) or demosaic with Malvar-He-Cutler
    /// if the frames are Bayer data (register and stack).
    #[default]
    Auto,
    /// Never demosaic.
    None,
    /// Malvar-He-Cutler gradient-corrected interpolation.
    Mhc,
    /// Bilinear interpolation.
    Bilinear,
}

impl DebayerArg {
    /// The method, if frames are to be demosaiced; `auto_on` says whether
    /// `auto` means demosaic.
    #[must_use]
    pub fn method(self, auto_on: bool) -> Option<Method> {
        match self {
            Self::Auto => auto_on.then_some(Method::Mhc),
            Self::None => None,
            Self::Mhc => Some(Method::Mhc),
            Self::Bilinear => Some(Method::Bilinear),
        }
    }
}

/// Dark scaling.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ScaleArg {
    /// Subtract the dark as it is.
    #[default]
    Off,
    /// Multiply by the ratio of exposure times.
    Exposure,
    /// Fit the multiplier to the light from its hot pixels.
    Optimise,
}

impl From<ScaleArg> for DarkScaling {
    fn from(a: ScaleArg) -> Self {
        match a {
            ScaleArg::Off => Self::Off,
            ScaleArg::Exposure => Self::Exposure,
            ScaleArg::Optimise => Self::Optimise,
        }
    }
}

/// Transform family.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ModelArg {
    /// Rotation, uniform scale and shift.
    #[default]
    Similarity,
    /// Adds shear and unequal scale.
    Affine,
    /// Full perspective; needs many stars across the frame.
    Homography,
}

impl From<ModelArg> for Model {
    fn from(a: ModelArg) -> Self {
        match a {
            ModelArg::Similarity => Self::Similarity,
            ModelArg::Affine => Self::Affine,
            ModelArg::Homography => Self::Homography,
        }
    }
}

/// What `valerian deconvolve` sharpens the stars to.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum DeconvModeArg {
    /// Round stars of one width everywhere: undoes the elongation and its variation over the field.
    #[default]
    Round,
    /// Deconvolve by the whole measured PSF: the sharpest, keeps any elongation that survives.
    Full,
}

impl From<DeconvModeArg> for valerian_core::deconvolve::Mode {
    fn from(a: DeconvModeArg) -> Self {
        match a {
            DeconvModeArg::Round => Self::Round,
            DeconvModeArg::Full => Self::Full,
        }
    }
}

/// Resampling kernel.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum InterpArg {
    /// 2x2 bilinear: smooth, no ringing.
    Bilinear,
    /// 4x4 bicubic (Catmull-Rom).
    #[default]
    Bicubic,
    /// 6x6 Lanczos: sharpest, can ring around bright stars.
    Lanczos3,
}

impl From<InterpArg> for Interpolation {
    fn from(a: InterpArg) -> Self {
        match a {
            InterpArg::Bilinear => Self::Bilinear,
            InterpArg::Bicubic => Self::Bicubic,
            InterpArg::Lanczos3 => Self::Lanczos3,
        }
    }
}

/// Frame weights.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum WeightArg {
    /// Every frame counts the same.
    Equal,
    /// One over the noise squared.
    Noise,
    /// One over the star FWHM squared.
    Fwhm,
    /// One over noise squared times FWHM squared: best for point sources.
    #[default]
    NoiseFwhm,
}

impl From<WeightArg> for Weights {
    fn from(a: WeightArg) -> Self {
        match a {
            WeightArg::Equal => Self::Equal,
            WeightArg::Noise => Self::Noise,
            WeightArg::Fwhm => Self::Fwhm,
            WeightArg::NoiseFwhm => Self::NoiseFwhm,
        }
    }
}

/// Level matching.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum NormArg {
    /// Leave levels as they are.
    None,
    /// Match the sky level to the reference.
    Additive,
    /// Match the sky level and scale by the matched stars' flux ratio.
    #[default]
    AdditiveScaling,
}

impl From<NormArg> for Normalisation {
    fn from(a: NormArg) -> Self {
        match a {
            NormArg::None => Self::None,
            NormArg::Additive => Self::Additive,
            NormArg::AdditiveScaling => Self::AdditiveScaling,
        }
    }
}

/// Arguments of `valerian calibrate`.
#[derive(Debug, Args)]
pub struct CalibrateArgs {
    /// Light frames: FITS files or directories of them.
    #[arg(required = true, value_name = "LIGHTS")]
    pub lights: Vec<PathBuf>,
    /// Bias frames, or a master bias (a file or directory; repeatable).
    #[arg(long, value_name = "PATH")]
    pub bias: Vec<PathBuf>,
    /// Dark frames, or a master dark (a file or directory; repeatable).
    #[arg(long, value_name = "PATH")]
    pub dark: Vec<PathBuf>,
    /// Flat frames, or a master flat (a file or directory; repeatable).
    #[arg(long, value_name = "PATH")]
    pub flat: Vec<PathBuf>,
    /// Dark-flat frames, subtracted from the flats instead of the bias
    /// (a file or directory; repeatable).
    #[arg(long = "dark-flat", value_name = "PATH")]
    pub dark_flat: Vec<PathBuf>,
    /// Directory for the calibrated frames.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
    /// Also write the master frames to this directory.
    #[arg(long, value_name = "DIR")]
    pub masters: Option<PathBuf>,
    /// How the dark is scaled to each light's exposure.
    #[arg(long, value_enum, default_value_t)]
    pub scale_dark: ScaleArg,
    /// Rejection used to combine the calibration frames.
    #[arg(long, value_enum, default_value_t)]
    pub rejection: RejectionArg,
    /// Memory for combining calibration frames, in MiB. The frames are read a
    /// band of rows at a time, so the master of any number of frames fits.
    #[arg(long, value_name = "MIB", default_value_t = 1024)]
    pub memory: usize,
    /// Hot-pixel limit: sigmas above the neighbours in the master dark.
    #[arg(long, value_name = "SIGMA", default_value_t = 40.0)]
    pub hot_sigma: f32,
    /// Cold-pixel limit: sigmas below the neighbours in the master dark.
    #[arg(long, value_name = "SIGMA", default_value_t = 12.0)]
    pub cold_sigma: f32,
    /// Without a dark, find isolated hot and cold pixels in each light itself.
    #[arg(long)]
    pub auto_cosmetic: bool,
    /// Normalise the flat per Bayer colour, so it corrects vignetting and
    /// dust but not colour balance.
    #[arg(long)]
    pub flat_per_channel: bool,
    /// Demosaic the calibrated frames to three colour planes.
    #[arg(long, value_enum, default_value_t = DebayerArg::None)]
    pub debayer: DebayerArg,
    /// The engine for the per-pixel stages: auto (the CPU, as one stage alone is slower on the GPU), cpu or gpu.
    #[arg(long, value_enum, default_value_t)]
    pub backend: BackendArg,
}

/// Which engine runs the heavy stages.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum BackendArg {
    /// A real GPU if there is one, otherwise the CPU.
    #[default]
    Auto,
    /// Always the CPU.
    Cpu,
    /// A GPU (the software adapter if there is no real one), or an error.
    Gpu,
}

impl From<BackendArg> for BackendChoice {
    fn from(b: BackendArg) -> Self {
        match b {
            BackendArg::Auto => Self::Auto,
            BackendArg::Cpu => Self::Cpu,
            BackendArg::Gpu => Self::Gpu,
        }
    }
}

/// The adapter `bench` opens.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum AdapterArg {
    /// A real GPU.
    #[default]
    Hardware,
    /// The software adapter (lavapipe, WARP).
    Software,
}

/// Arguments of `valerian bench`.
#[derive(Debug, Args)]
pub struct BenchArgs {
    /// The adapter to open.
    #[arg(long, value_enum, default_value_t)]
    pub adapter: AdapterArg,
    /// Frame width in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 4096)]
    pub width: usize,
    /// Frame height in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 3072)]
    pub height: usize,
    /// Frames in the combine (3 to 128).
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub frames: usize,
    /// Timed repeats of each stage (the median is shown).
    #[arg(long, value_name = "N", default_value_t = 3)]
    pub repeats: usize,
}

/// Arguments of `valerian register`.
#[derive(Debug, Args)]
pub struct RegisterArgs {
    /// Frames: FITS files or directories of them.
    #[arg(required = true, value_name = "FRAMES")]
    pub frames: Vec<PathBuf>,
    /// Directory for the registered frames.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
    /// Index (from 0, in sorted order) of the reference frame; default the sharpest.
    #[arg(long, value_name = "N")]
    pub reference: Option<usize>,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: ModelArg,
    /// Resampling kernel.
    #[arg(long, value_enum, default_value_t)]
    pub interpolation: InterpArg,
    /// Demosaic raw Bayer frames.
    #[arg(long, value_enum, default_value_t)]
    pub debayer: DebayerArg,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// The engine for the resampling: auto (the CPU, as one stage alone is slower on the GPU), cpu or gpu.
    #[arg(long, value_enum, default_value_t)]
    pub backend: BackendArg,
}

/// Arguments of `valerian stack`.
#[derive(Debug, Args)]
pub struct StackArgs {
    /// Frames: FITS files or directories of them.
    #[arg(required = true, value_name = "FRAMES")]
    pub frames: Vec<PathBuf>,
    /// The stacked FITS file to write.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Also write the per-pixel coverage (frames used) to this FITS file.
    #[arg(long, value_name = "FILE")]
    pub coverage: Option<PathBuf>,
    /// Also write the per-frame table to this text file.
    #[arg(long, value_name = "FILE")]
    pub report: Option<PathBuf>,
    /// Leave out the frames this approve/reject table (CSV or JSON from
    /// `valerian subframes`) marks as rejected. Frames it does not name are kept.
    #[arg(long, value_name = "FILE")]
    pub approve: Option<PathBuf>,
    /// Grade the raw Seestar subs and leave out the ones that fail.
    #[command(flatten)]
    pub grade: GradeArgs,
    /// With --grade: also multiply each kept frame's stack weight by its grade
    /// score, so cleaner frames count for more.
    #[arg(long, requires = "grade")]
    pub grade_weights: bool,
    /// Write the subframe analysis of this stack (plots, CSV, JSON, report.html) to this directory.
    #[arg(long, value_name = "DIR")]
    pub science_report: Option<PathBuf>,
    /// Limits for the analysis written by `--science-report`.
    #[command(flatten)]
    pub limits: LimitArgs,
    /// Rejection scheme.
    #[arg(long, value_enum, default_value_t)]
    pub rejection: RejectionArg,
    /// Lower rejection limit in sigmas, where the scheme has one. With
    /// `--rejection auto` it changes the limit of the scheme auto picks.
    #[arg(long, value_name = "SIGMA")]
    pub low: Option<f32>,
    /// Upper rejection limit in sigmas, where the scheme has one. With
    /// `--rejection auto` it changes the limit of the scheme auto picks.
    #[arg(long, value_name = "SIGMA")]
    pub high: Option<f32>,
    /// Frame weights.
    #[arg(long, value_enum, default_value_t)]
    pub weights: WeightArg,
    /// Level matching.
    #[arg(long, value_enum, default_value_t)]
    pub normalisation: NormArg,
    /// Index (from 0, in sorted order) of the reference frame; default the sharpest.
    #[arg(long, value_name = "N")]
    pub reference: Option<usize>,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: ModelArg,
    /// Resampling kernel.
    #[arg(long, value_enum, default_value_t)]
    pub interpolation: InterpArg,
    /// Demosaic raw Bayer frames.
    #[arg(long, value_enum, default_value_t)]
    pub debayer: DebayerArg,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// Leave out frames with fewer stars than this.
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub min_stars: usize,
    /// Leave out frames whose FWHM is more than this multiple of the reference's.
    #[arg(long, value_name = "RATIO")]
    pub max_fwhm_ratio: Option<f32>,
    /// The engine for the resampling and the combine: auto (a real GPU if there is one), cpu or gpu.
    #[arg(long, value_enum, default_value_t)]
    pub backend: BackendArg,
    /// Memory for the stack, in MiB. Frames are read one at a time, resampled
    /// into a staging file and combined a band of rows at a time, so the peak
    /// follows this and not the number of frames. Ignored with `--science-report`,
    /// which holds every frame.
    #[arg(long, value_name = "MIB", default_value_t = 4096)]
    pub memory: usize,
    /// Directory for the staging file (frames x width x height x planes x 4
    /// bytes), used when the stack does not fit `--memory`; the system
    /// temporary directory by default. The file is removed when the run ends.
    #[arg(long, value_name = "DIR")]
    pub scratch_dir: Option<PathBuf>,
    /// Stack a mosaic: register every pane onto one larger canvas instead of
    /// keeping the reference frame's grid. Frames that do not overlap the
    /// reference are matched to the panes already placed. Seams are feathered
    /// (see --mosaic-feather) and --coverage shows how many frames reached
    /// each pixel. Not with --science-report.
    #[arg(long, conflicts_with = "science_report")]
    pub mosaic: bool,
    /// With --mosaic: width in pixels of the ramp at each frame's edge, over
    /// which its weight rises from nearly zero to full, so overlapping panes
    /// cross-fade. 0 gives hard edges.
    #[arg(
        long,
        value_name = "PIXELS",
        default_value_t = 120.0,
        requires = "mosaic"
    )]
    pub mosaic_feather: f32,
    /// With --mosaic: do not fit a level per frame from the overlaps; every
    /// frame keeps the sky level the usual normalisation gives it.
    #[arg(long, requires = "mosaic")]
    pub no_mosaic_level_match: bool,
    /// With --mosaic: keep only the part of the canvas that at least this many
    /// frames reach, and cut the canvas to it. Thin edges (a frame or two) are
    /// noisy; the Seestar's own mosaic keeps only the well-covered middle.
    #[arg(long, value_name = "FRAMES", default_value_t = 1, requires = "mosaic")]
    pub mosaic_min_coverage: u16,
}

/// The Seestar sub-frame grader: a small neural network that scores raw
/// Seestar S50 subs and spots clouds, trails, blur and star-poor frames.
#[derive(Debug, Args, Clone)]
pub struct GradeArgs {
    /// Optional, off by default: grade each raw Seestar S50 sub with the
    /// neural-network grader, before anything else. Other frames are left
    /// alone, with a note.
    #[arg(long)]
    pub grade: bool,
    /// With --grade: reject frames that score below this, 0 (unusable) to 1 (clean).
    #[arg(long, value_name = "SCORE", default_value_t = 0.5)]
    pub min_grade: f32,
    /// With --grade: also reject frames whose chance of a defect is at or above
    /// P. NAME is `cloud_haze`, `trail`, `blur` or `star_poor`; the dew output is not reliable. Repeat for several.
    #[arg(long, value_name = "NAME:P", value_parser = valerian_science::grader::parse_defect_limit)]
    pub reject_defect: Vec<(valerian_science::grader::Defect, f32)>,
    /// With --grade: use this ONNX grader model instead of the built-in one.
    #[arg(long, value_name = "FILE")]
    pub grader_model: Option<PathBuf>,
}

/// Limits a frame is judged against in the subframe analysis.
#[derive(Debug, Args, Clone, Default)]
pub struct LimitArgs {
    /// Reject frames with a larger FWHM, in pixels.
    #[arg(long, value_name = "PIXELS")]
    pub max_fwhm: Option<f64>,
    /// Reject frames with a larger eccentricity (1 - b/a, 0 to 1).
    #[arg(long, value_name = "E")]
    pub max_ellipticity: Option<f64>,
    /// Reject frames with fewer detected stars.
    #[arg(long, value_name = "N")]
    pub min_star_count: Option<usize>,
    /// Reject frames with more noise, in ADU.
    #[arg(long, value_name = "ADU")]
    pub max_noise: Option<f64>,
    /// Reject frames with a lower SNR weight (1 is the median frame).
    #[arg(long, value_name = "W")]
    pub min_snr_weight: Option<f64>,
    /// Reject frames with a larger registration residual, in pixels.
    #[arg(long, value_name = "PIXELS")]
    pub max_residual: Option<f64>,
    /// Reject frames this many robust sigmas (1.4826 MAD) from the median in
    /// the bad direction of any measurement.
    #[arg(long, value_name = "SIGMA")]
    pub outlier_sigma: Option<f64>,
}

/// Arguments of `valerian subframes`.
#[derive(Debug, Args)]
pub struct SubframesArgs {
    /// Frames: FITS files or directories of them.
    #[arg(required = true, value_name = "FRAMES")]
    pub frames: Vec<PathBuf>,
    /// Directory for the plots, CSV, JSON and report.html.
    #[arg(long, value_name = "DIR", default_value = "subframes-report")]
    pub report: PathBuf,
    /// The limits.
    #[command(flatten)]
    pub limits: LimitArgs,
    /// Index (from 0, in sorted order) of the reference frame; default the sharpest.
    #[arg(long, value_name = "N")]
    pub reference: Option<usize>,
    /// Grade the raw Seestar subs, add the scores to the table and reject the ones that fail.
    #[command(flatten)]
    pub grade: GradeArgs,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: ModelArg,
    /// Demosaic raw Bayer frames.
    #[arg(long, value_enum, default_value_t)]
    pub debayer: DebayerArg,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
}

/// What `valerian plot` draws.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum PlotKind {
    /// Per-frame FWHM, eccentricity, star count, background, noise, SNR weight and registration residual.
    Subframes,
    /// The aberration inspector: crops at the corners, edges and centre (an image).
    Aberration,
    /// The FWHM map across the field (an image).
    FwhmMap,
    /// The eccentricity map across the field, with whiskers (an image).
    EccentricityMap,
    /// The FWHM of the centre and the four corners (an image).
    Tilt,
    /// The stacked radial profile with a Moffat fit (an image).
    Psf,
    /// Samples rejected below the centre, per pixel (frames, stacked on the CPU).
    RejectionLow,
    /// Samples rejected above the centre, per pixel (frames, stacked on the CPU).
    RejectionHigh,
    /// Summed weight of the samples used, per pixel (frames, stacked on the CPU).
    WeightMap,
    /// Samples used, per pixel (frames, stacked on the CPU).
    CountMap,
    /// Noise against the number of frames stacked, with the square-root law (frames).
    Noise,
    /// Histogram of each channel, linear and stretched (an image).
    Histogram,
    /// The plate solution drawn on the image: grid, objects, Gaia stars (a solved image).
    Annotate,
    /// Residual vectors of the plate solution (a solved image).
    Residuals,
    /// Photometric colour calibration scatter against Gaia BP-RP (a solved colour image).
    Pcc,
    /// Light curve of one star across frames (frames, with --star).
    Lightcurve,
    /// Zero point and colour term of each colour plane against Gaia (a solved colour image).
    Zeropoint,
    /// Limiting magnitude: signal-to-noise and star counts against magnitude (a solved colour image).
    LimitingMag,
    /// Astrometric residual map: mean residual per cell, radial and tangential terms (a solved image).
    ResidualMap,
}

/// Horizontal axis of the per-frame plots.
#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq, Eq)]
pub enum XAxisArg {
    /// Frame number.
    #[default]
    Index,
    /// Hours since the first frame (needs DATE-OBS in every frame).
    Time,
}

/// Arguments of `valerian plot`.
#[derive(Debug, Args)]
pub struct PlotArgs {
    /// Which plot.
    #[arg(value_enum)]
    pub kind: PlotKind,
    /// Frames (FITS files or directories of them) to measure or stack, or one
    /// subframes.json written by `valerian subframes`; for the optics and
    /// histogram plots, one FITS image.
    #[arg(required = true, value_name = "INPUT")]
    pub input: Vec<PathBuf>,
    /// The PNG to write; the CSV and JSON of the plotted data go beside it.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Horizontal axis.
    #[arg(long, value_enum, default_value_t)]
    pub x: XAxisArg,
    /// The limits, when frames are measured.
    #[command(flatten)]
    pub limits: LimitArgs,
    /// Detection threshold in sigmas of the smoothed noise, when frames or stars are measured.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// Optics plots: the smallest peak-to-noise ratio of a star that is measured.
    #[arg(long, value_name = "RATIO", default_value_t = 10.0)]
    pub min_snr: f64,
    /// Aberration inspector: side of a crop in pixels (32 to 512, even).
    #[arg(long, value_name = "PIXELS", default_value_t = 128)]
    pub crop: usize,
    /// Aberration inspector: enlargement of a crop (1 to 6).
    #[arg(long, value_name = "FACTOR", default_value_t = 2)]
    pub zoom: usize,
    /// Maps: degree of the polynomial surface fitted to the stars (1 to 4).
    #[arg(long, value_name = "DEGREE", default_value_t = 3)]
    pub order: usize,
    /// PSF profile: how many of the brightest isolated stars to stack.
    #[arg(long, value_name = "N", default_value_t = 50)]
    pub psf_stars: usize,
    /// Annotate, residuals and pcc: take the plate solution from this FITS file's header.
    #[arg(long, value_name = "FILE")]
    pub wcs: Option<PathBuf>,
    /// Annotate, residuals and pcc: the star catalogue (default: the one beside the program).
    #[arg(long, value_name = "FILE")]
    pub catalogue: Option<PathBuf>,
    /// Annotate: faintest Gaia star drawn, G magnitude.
    #[arg(long, value_name = "MAG", default_value_t = 10.0)]
    pub star_mag: f32,
    /// Lightcurve: the star as `X,Y` pixels in the reference frame (default: the brightest unsaturated star).
    #[arg(long, value_name = "X,Y")]
    pub star: Option<String>,
    /// Lightcurve: aperture radius as a multiple of the reference FWHM.
    #[arg(long, value_name = "FWHM", default_value_t = 2.0)]
    pub aperture: f64,
    /// Lightcurve: comparison stars to use at most.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub comparison: usize,
    /// Zeropoint and limiting-mag: the signal-to-noise ratio that defines the limit.
    #[arg(long, value_name = "SNR", default_value_t = 5.0)]
    pub limit_snr: f64,
    /// Residual-map: cells per side of the grid (2 to 24).
    #[arg(long, value_name = "N", default_value_t = 6)]
    pub grid: usize,
}

/// Arguments of `valerian annotate`.
#[derive(Debug, Args)]
pub struct AnnotateArgs {
    /// The solved FITS image (linear or stretched).
    #[arg(value_name = "IMAGE")]
    pub image: PathBuf,
    /// Directory for the picture, lists, residuals and report.html.
    #[arg(long, value_name = "DIR", default_value = "astrometry-report")]
    pub report: PathBuf,
    /// Take the plate solution from this FITS file's header instead of the image's.
    #[arg(long, value_name = "FILE")]
    pub wcs: Option<PathBuf>,
    /// The star catalogue (default: `gaia-dr3-g11.vcat` beside the program or in `VALERIAN_CATALOGUE`).
    #[arg(long, value_name = "FILE")]
    pub catalogue: Option<PathBuf>,
    /// Leave the RA/Dec grid out.
    #[arg(long)]
    pub no_grid: bool,
    /// Leave the catalogue objects out.
    #[arg(long)]
    pub no_objects: bool,
    /// Leave the Gaia stars out.
    #[arg(long)]
    pub no_stars: bool,
    /// Faintest Gaia star drawn, G magnitude.
    #[arg(long, value_name = "MAG", default_value_t = 10.0)]
    pub star_mag: f32,
    /// Gaia stars brighter than this, G magnitude, are labelled.
    #[arg(long, value_name = "MAG", default_value_t = 8.0)]
    pub label_mag: f32,
    /// Longest side of the picture in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 1400)]
    pub max_side: usize,
    /// Cells per side of the astrometric residual map (2 to 24).
    #[arg(long, value_name = "N", default_value_t = 6)]
    pub map_grid: usize,
}

/// Arguments of `valerian photometry`.
#[derive(Debug, Args)]
pub struct PhotometryArgs {
    /// Frames: FITS files or directories of them.
    #[arg(required = true, value_name = "FRAMES")]
    pub frames: Vec<PathBuf>,
    /// Directory for the light curve, its data and report.html.
    #[arg(long, value_name = "DIR", default_value = "photometry-report")]
    pub report: PathBuf,
    /// The star as `X,Y` pixels in the reference frame (default: the brightest unsaturated star).
    #[arg(long, value_name = "X,Y")]
    pub star: Option<String>,
    /// Aperture radius as a multiple of the reference frame's FWHM.
    #[arg(long, value_name = "FWHM", default_value_t = 2.0)]
    pub aperture: f64,
    /// Comparison stars to use at most.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub comparison: usize,
    /// Index (from 0, in sorted order) of the reference frame; default the sharpest.
    #[arg(long, value_name = "N")]
    pub reference: Option<usize>,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: ModelArg,
    /// Demosaic raw Bayer frames.
    #[arg(long, value_enum, default_value_t)]
    pub debayer: DebayerArg,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
}

/// Arguments of `valerian optics`.
#[derive(Debug, Args)]
pub struct OpticsArgs {
    /// The FITS image (a single frame or a stack; colour images use the luminance).
    #[arg(value_name = "IMAGE")]
    pub image: PathBuf,
    /// Directory for the plots, CSV, JSON and report.html.
    #[arg(long, value_name = "DIR", default_value = "optics-report")]
    pub report: PathBuf,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// The smallest peak-to-noise ratio of a star that is measured.
    #[arg(long, value_name = "RATIO", default_value_t = 10.0)]
    pub min_snr: f64,
    /// Aberration inspector: side of a crop in pixels (32 to 512, even).
    #[arg(long, value_name = "PIXELS", default_value_t = 128)]
    pub crop: usize,
    /// Aberration inspector: enlargement of a crop (1 to 6).
    #[arg(long, value_name = "FACTOR", default_value_t = 2)]
    pub zoom: usize,
    /// Maps: degree of the polynomial surface fitted to the stars (1 to 4).
    #[arg(long, value_name = "DEGREE", default_value_t = 3)]
    pub order: usize,
    /// PSF profile: how many of the brightest isolated stars to stack.
    #[arg(long, value_name = "N", default_value_t = 50)]
    pub psf_stars: usize,
}

/// Arguments of `valerian drizzle`.
#[derive(Debug, Args)]
pub struct DrizzleArgs {
    /// Frames: FITS files or directories of them (calibrated; raw mosaics are fine).
    #[arg(required = true, value_name = "FRAMES")]
    pub frames: Vec<PathBuf>,
    /// The drizzled FITS file to write.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Output pixels per input pixel, linear.
    #[arg(long, value_name = "FACTOR", default_value_t = 2.0)]
    pub scale: f64,
    /// Side of a drop as a fraction of an input pixel, above 0 and at most 1.
    #[arg(long, value_name = "FRACTION", default_value_t = 0.6)]
    pub pixfrac: f64,
    /// Reject input pixels this many sigma from the first-pass result.
    #[arg(
        long,
        value_name = "SIGMA",
        default_value_t = 4.0,
        conflicts_with = "no_reject"
    )]
    pub reject: f64,
    /// Drizzle every pixel, with no rejection pass.
    #[arg(long)]
    pub no_reject: bool,
    /// Also write the weight map (coverage of the output) to this FITS file.
    #[arg(long, value_name = "FILE")]
    pub weight_map: Option<PathBuf>,
    /// Write the integration diagnostics of this drizzle (weight, pixel-count and
    /// outlier-rejection maps, histograms; plots, CSV, JSON, report.html) to this directory.
    #[arg(long, value_name = "DIR")]
    pub science_report: Option<PathBuf>,
    /// Also write the per-frame table to this text file.
    #[arg(long, value_name = "FILE")]
    pub report: Option<PathBuf>,
    /// Frame weights.
    #[arg(long, value_enum, default_value_t)]
    pub weights: WeightArg,
    /// Level matching.
    #[arg(long, value_enum, default_value_t)]
    pub normalisation: NormArg,
    /// Index (from 0, in sorted order) of the reference frame; default the sharpest.
    #[arg(long, value_name = "N")]
    pub reference: Option<usize>,
    /// Transform to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: ModelArg,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// Leave out frames with fewer stars than this.
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub min_stars: usize,
    /// Leave out frames whose FWHM is more than this multiple of the reference's.
    #[arg(long, value_name = "RATIO")]
    pub max_fwhm_ratio: Option<f32>,
}

/// Background surface.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum BackgroundModelArg {
    /// A polynomial of `--degree`: degree 1 is a plane.
    #[default]
    Polynomial,
    /// A thin-plate spline with `--smoothing`: follows any smooth gradient.
    Spline,
}

/// How the background is applied.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ModeArg {
    /// Subtract: light pollution and other added light.
    #[default]
    Subtract,
    /// Divide: vignetting and other falloff.
    Divide,
}

/// What happens to the overall level.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum LevelArg {
    /// Remove the gradient but keep the mean level.
    #[default]
    Preserve,
    /// Remove the whole surface: the sky goes to about zero (subtract) or one (divide).
    Zero,
}

/// Arguments of `valerian background`.
#[derive(Debug, Args)]
pub struct BackgroundArgs {
    /// The stacked FITS image (mono or colour planes, not a raw mosaic).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// The corrected FITS file to write.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Also write the fitted background (one plane per input plane) to this FITS file.
    #[arg(long, value_name = "FILE")]
    pub save_model: Option<PathBuf>,
    /// The surface to fit.
    #[arg(long, value_enum, default_value_t)]
    pub model: BackgroundModelArg,
    /// Polynomial degree, 0 to 6.
    #[arg(long, value_name = "N", default_value_t = 2)]
    pub degree: usize,
    /// Spline smoothing: about 0.001 to 0.01 for a hundred tiles; larger tends to a plane.
    #[arg(long, value_name = "S", default_value_t = 0.005)]
    pub smoothing: f64,
    /// Subtract the background or divide by it.
    #[arg(long, value_enum, default_value_t)]
    pub mode: ModeArg,
    /// Keep the mean level or remove the whole surface.
    #[arg(long, value_enum, default_value_t)]
    pub level: LevelArg,
    /// Tile size in pixels; default a thirty-second of the short side (at least 32).
    #[arg(long, value_name = "PX", default_value_t = 0)]
    pub tile: usize,
    /// Mask pixels this many robust sigmas above the surface as stars.
    #[arg(long, value_name = "SIGMA", default_value_t = 3.0)]
    pub star_sigma: f32,
    /// Grow the star mask by this many pixels.
    #[arg(long, value_name = "PX", default_value_t = 3)]
    pub mask_grow: usize,
    /// Reject samples this many sigmas below the fit.
    #[arg(long, value_name = "SIGMA", default_value_t = 3.0)]
    pub clip_low: f32,
    /// Reject samples this many sigmas above the fit.
    #[arg(long, value_name = "SIGMA", default_value_t = 2.0)]
    pub clip_high: f32,
    /// Rounds of mask refinement after the first fit.
    #[arg(long, value_name = "N", default_value_t = 3)]
    pub rounds: usize,
    /// Leave out the bright central target, sized from the data (for a large
    /// galaxy or nebula that fills much of the frame).
    #[arg(long)]
    pub exclude_target: bool,
}

/// Tone curve.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum CurveArg {
    /// A midtone transfer function that puts the sky at `--target`.
    #[default]
    Auto,
    /// Midtone transfer function with the given `--midtone` (or solved for `--target`).
    Mtf,
    /// asinh(beta x) / asinh(beta), with `--beta`.
    Arcsinh,
    /// Generalised hyperbolic stretch, with the `--ghs-*` parameters.
    Ghs,
}

/// Arguments of `valerian stretch`.
#[derive(Debug, Args)]
pub struct StretchArgs {
    /// The linear FITS image to stretch.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// The stretched FITS file to write (optional with `--preview`).
    #[arg(short, long, value_name = "FILE", required_unless_present = "preview")]
    pub output: Option<PathBuf>,
    /// Also write the stretched image as an 8-bit PNG, reduced to `--preview-size`.
    #[arg(long, value_name = "PNG")]
    pub preview: Option<PathBuf>,
    /// Longest side of the `--preview` PNG, in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 1600)]
    pub preview_size: usize,
    /// Undo the stretch recorded in the input's header instead of stretching.
    #[arg(long)]
    pub invert: bool,
    /// Tone curve.
    #[arg(long, value_enum, default_value_t)]
    pub curve: CurveArg,
    /// Sky level after the stretch, 0 to 1 (auto, and mtf without `--midtone`).
    #[arg(long, value_name = "LEVEL", default_value_t = 0.10)]
    pub target: f64,
    /// Midtone balance of the MTF, 0 to 1: below 0.5 brightens.
    #[arg(long, value_name = "M")]
    pub midtone: Option<f64>,
    /// Strength of the arcsinh stretch.
    #[arg(long, value_name = "BETA", default_value_t = 50.0)]
    pub beta: f64,
    /// GHS stretch factor: 0 changes nothing; D is e to this power minus one.
    #[arg(long, value_name = "S", default_value_t = 3.0)]
    pub ghs_stretch: f64,
    /// GHS local intensity b: -1 logarithmic, 0 exponential, above 0 hyperbolic.
    #[arg(
        long,
        value_name = "B",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    pub ghs_b: f64,
    /// GHS symmetry point, 0 to 1: the brightness that is stretched hardest.
    #[arg(long, value_name = "SP", default_value_t = 0.0)]
    pub ghs_sp: f64,
    /// GHS shadow protection point (at most the symmetry point).
    #[arg(long, value_name = "LP", default_value_t = 0.0)]
    pub ghs_lp: f64,
    /// GHS highlight protection point (at least the symmetry point).
    #[arg(long, value_name = "HP", default_value_t = 1.0)]
    pub ghs_hp: f64,
    /// Black point, in the image's own units (with `--white`).
    #[arg(long, value_name = "ADU", allow_negative_numbers = true)]
    pub black: Option<f32>,
    /// White point, in the image's own units (with `--black`).
    #[arg(long, value_name = "ADU", allow_negative_numbers = true)]
    pub white: Option<f32>,
    /// Automatic black point: this many robust sigmas below the sky.
    #[arg(long, value_name = "SIGMA", default_value_t = 2.8)]
    pub shadow_sigma: f32,
    /// Automatic white point: this percentile of the pixels.
    #[arg(long, value_name = "PERCENT", default_value_t = 99.995)]
    pub white_percentile: f32,
    /// Stretch each colour plane on its own instead of all alike.
    #[arg(long)]
    pub unlinked: bool,
}

/// Arguments of `valerian starless`.
#[derive(Debug, Args)]
pub struct StarlessArgs {
    /// The FITS image (mono or colour planes, not a raw mosaic).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// The starless FITS file to write (optional with `--preview`).
    #[arg(short, long, value_name = "FILE", required_unless_present = "preview")]
    pub output: Option<PathBuf>,
    /// Also write what was removed (original minus starless) to this FITS file.
    #[arg(long, value_name = "FILE")]
    pub stars_out: Option<PathBuf>,
    /// Also write the star mask (1 where pixels were filled) to this FITS file.
    #[arg(long, value_name = "FILE")]
    pub mask_out: Option<PathBuf>,
    /// Also write a PNG of the original and the starless image side by side,
    /// both on the automatic stretch of the original and reduced to `--preview-size`.
    #[arg(long, value_name = "PNG")]
    pub preview: Option<PathBuf>,
    /// Longest side of each half of the `--preview` PNG, in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 900)]
    pub preview_size: usize,
    /// Fraction of each star to remove, 0 to 1: below 1 reduces stars instead.
    #[arg(long, value_name = "FRACTION", default_value_t = 1.0)]
    pub strength: f32,
    /// Detection threshold in sigmas of the smoothed noise: lower finds fainter stars.
    #[arg(long, value_name = "SIGMA", default_value_t = 4.0)]
    pub threshold: f32,
    /// Multiplier of every mask radius.
    #[arg(long, value_name = "FACTOR", default_value_t = 1.0)]
    pub mask_scale: f32,
    /// Pixels added to every mask radius.
    #[arg(long, value_name = "PIXELS", default_value_t = 1.5)]
    pub grow: f32,
    /// Largest mask radius in pixels.
    #[arg(long, value_name = "PIXELS", default_value_t = 60.0)]
    pub max_radius: f32,
    /// Fill with the smooth diffusion result and add no noise.
    #[arg(long)]
    pub no_noise: bool,
    /// Seed of the added noise.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub seed: u64,
}

/// Arguments of `valerian deconvolve`.
#[derive(Debug, Args)]
pub struct DeconvolveArgs {
    /// The FITS image (mono or colour planes, linear, not a raw mosaic).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// The deconvolved FITS file to write.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// What the stars become: `round` (one round width everywhere) or `full` (the sharpest).
    #[arg(long, value_enum, default_value_t)]
    pub mode: DeconvModeArg,
    /// Star FWHM in pixels that `round` aims for. Default: `--target-ratio` of the narrowest PSF axis of any tile.
    #[arg(long, value_name = "PIXELS")]
    pub target_fwhm: Option<f32>,
    /// The automatic `round` target as a share of the narrowest PSF axis, 0.3 to 1: lower is sharper and noisier.
    #[arg(long, value_name = "RATIO", default_value_t = 0.7)]
    pub target_ratio: f32,
    /// Richardson-Lucy iterations, 1 to 200: more is sharper and noisier.
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub iterations: u32,
    /// Total-variation damping, 0 to 1 (0 is plain Richardson-Lucy): higher keeps the noise smoother.
    #[arg(long, value_name = "LAMBDA", default_value_t = 0.002)]
    pub tv: f32,
    /// How far `round` evens out the stars' elongation before it sharpens, 0 to 1: 1 gives the roundest stars, 0 the sharpest.
    #[arg(long, value_name = "SHARE", default_value_t = 0.5)]
    pub roundness: f32,
    /// Pixel value at which stars are clipped: `auto` (found from the image), `off`, or a number in the image's units.
    #[arg(long, value_name = "LEVEL", default_value = "auto")]
    pub saturation: String,
    /// Strength of the guard that stops a dark ring forming around bright stars, 0 to 1 (0 is off).
    #[arg(long, value_name = "STRENGTH", default_value_t = 1.0)]
    pub ring_guard: f32,
    /// Use a round Gaussian PSF of this FWHM in pixels everywhere instead of measuring one from the stars.
    #[arg(long, value_name = "PIXELS")]
    pub psf_fwhm: Option<f32>,
    /// Tiles across and down, as `ACROSSxDOWN` (for example `3x4`); the default cuts tiles of about 480 pixels.
    #[arg(long, value_name = "AxB")]
    pub tiles: Option<String>,
    /// Half-width of the PSF kernel in pixels, 3 to 24.
    #[arg(long, value_name = "PIXELS", default_value_t = 10)]
    pub psf_radius: usize,
    /// Most stars used for the PSF of one tile.
    #[arg(long, value_name = "N", default_value_t = 40)]
    pub psf_stars: usize,
    /// Fewest stars a tile's PSF may be measured from; a tile with fewer widens its search.
    #[arg(long, value_name = "N", default_value_t = 8)]
    pub min_psf_stars: usize,
    /// Faintest star used for a PSF, as peak over pixel noise.
    #[arg(long, value_name = "SNR", default_value_t = 25.0)]
    pub psf_min_snr: f32,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub threshold: f32,
    /// Also write the PSF of every tile (position, stars, FWHM, ellipticity) to this CSV file.
    #[arg(long, value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Arguments of `valerian solve`.
#[derive(Debug, Args)]
pub struct SolveArgs {
    /// The FITS image (mono or colour planes, not a raw mosaic).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// Write a copy of the image with the WCS in its header (without it the solution is only printed).
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// The star catalogue (default: `gaia-dr3-g11.vcat` beside the program or in `VALERIAN_CATALOGUE`).
    #[arg(long, value_name = "FILE")]
    pub catalogue: Option<PathBuf>,
    /// Right ascension of the field centre: degrees, or hours as hh:mm:ss.
    #[arg(long, value_name = "RA", requires = "dec")]
    pub ra: Option<String>,
    /// Declination of the field centre: degrees, or dd:mm:ss.
    #[arg(
        long,
        value_name = "DEC",
        requires = "ra",
        allow_negative_numbers = true
    )]
    pub dec: Option<String>,
    /// How far the true centre may be from the position, in degrees.
    #[arg(long, value_name = "DEGREES", default_value_t = 1.0)]
    pub radius: f64,
    /// Pixel scale in arc seconds per pixel (searched within 15 per cent either way).
    #[arg(long, value_name = "ARCSEC", conflicts_with = "scale_range")]
    pub scale: Option<f64>,
    /// Range of the pixel scale in arc seconds per pixel.
    #[arg(long, value_names = ["LOW", "HIGH"], num_args = 2)]
    pub scale_range: Option<Vec<f64>>,
    /// Search the whole sky even if the header names a position.
    #[arg(long)]
    pub blind: bool,
    /// SIP distortion order, 0 for none, or auto to use it when it clearly helps.
    #[arg(long, value_name = "ORDER", default_value = "auto")]
    pub sip_order: String,
    /// The fewest stars that must agree.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub min_matches: usize,
    /// Detection threshold in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 6.0)]
    pub threshold: f32,
}

/// Arguments of `valerian photocal`.
#[derive(Debug, Args)]
pub struct PhotocalArgs {
    /// The linear colour FITS image (three planes).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// Write the calibrated image here (without it the result is only printed).
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Take the plate solution from this FITS file's header instead of the image's.
    #[arg(long, value_name = "FILE")]
    pub wcs: Option<PathBuf>,
    /// The star catalogue (default: `gaia-dr3-g11.vcat` beside the program or in `VALERIAN_CATALOGUE`).
    #[arg(long, value_name = "FILE")]
    pub catalogue: Option<PathBuf>,
    /// Write every star measured (position, G, BP-RP, fluxes, used) to this CSV file.
    #[arg(long, value_name = "FILE")]
    pub stars_out: Option<PathBuf>,
    /// The BP-RP colour to make neutral; 0.82 is a G2V star like the Sun.
    #[arg(
        long,
        value_name = "BP-RP",
        default_value_t = 0.82,
        allow_negative_numbers = true
    )]
    pub reference_colour: f32,
    /// Brightest star used, Gaia G magnitude.
    #[arg(long, value_name = "MAG", default_value_t = 6.0)]
    pub min_mag: f32,
    /// Faintest star used, Gaia G magnitude.
    #[arg(long, value_name = "MAG", default_value_t = 11.0)]
    pub max_mag: f32,
    /// Bluest BP-RP used in the fit.
    #[arg(
        long,
        value_name = "BP-RP",
        default_value_t = 0.3,
        allow_negative_numbers = true
    )]
    pub colour_min: f32,
    /// Reddest BP-RP used in the fit.
    #[arg(long, value_name = "BP-RP", default_value_t = 2.5)]
    pub colour_max: f32,
    /// Aperture radius as a multiple of the median FWHM.
    #[arg(long, value_name = "FWHM", default_value_t = 2.0)]
    pub aperture: f32,
    /// Least signal-to-noise of a star in each colour.
    #[arg(long, value_name = "SNR", default_value_t = 15.0)]
    pub min_snr: f64,
    /// The fewest stars the fit may rest on.
    #[arg(long, value_name = "N", default_value_t = 15)]
    pub min_stars: usize,
    /// Leave the background levels of the planes as they are.
    #[arg(long)]
    pub no_neutralise: bool,
    /// Write the colour-calibration scatter (instrumental colour against Gaia BP-RP, with the
    /// fitted lines), the zero points and colour terms, the limiting magnitude and the calibrated
    /// star catalogue (PNG, CSV, JSON, report.html) to this directory.
    #[arg(long, value_name = "DIR")]
    pub report: Option<PathBuf>,
    /// Write the calibrated star catalogue (every detected star with Gaia-G equivalent
    /// magnitudes and errors in red, green and blue) to this CSV file.
    #[arg(long, value_name = "FILE")]
    pub calibrated_stars: Option<PathBuf>,
    /// The signal-to-noise ratio that defines the limiting magnitude.
    #[arg(long, value_name = "SNR", default_value_t = 5.0)]
    pub limit_snr: f64,
    /// Detection threshold of the catalogue stars, in sigmas of the smoothed noise.
    #[arg(long, value_name = "SIGMA", default_value_t = 5.0)]
    pub field_threshold: f32,
    /// The most stars measured for the catalogue (the brightest).
    #[arg(long, value_name = "N", default_value_t = 5000)]
    pub max_field_stars: usize,
}

/// Arguments of `valerian info`.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// FITS files to describe; with none, describe this build.
    pub files: Vec<PathBuf>,
    /// For each file, also measure the sky, noise and stars.
    #[arg(long)]
    pub stats: bool,
    /// Open the GPU adapter and report it.
    #[arg(long)]
    pub gpu: bool,
    /// With `--gpu`, use the software (fallback) adapter instead of a real GPU.
    #[arg(long, requires = "gpu")]
    pub software: bool,
}

/// Run a parsed command line, writing to standard output.
///
/// # Errors
/// Returns the first error of the chosen subcommand.
pub fn run(cli: &Cli) -> Result<()> {
    let env = std::env::var("VALERIAN_KEEP_LOCATION")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    valerian_core::fits::set_keep_location(cli.keep_location || env);
    match &cli.command {
        Command::Calibrate(a) => calibrate::run(a),
        Command::Register(a) => register::run(a),
        Command::Stack(a) => stack::run(a),
        Command::Drizzle(a) => drizzle::run(a),
        Command::Bench(a) => bench::run(a),
        Command::Background(a) => background::run(a),
        Command::Stretch(a) => stretch::run(a),
        Command::Starless(a) => starless::run(a),
        Command::Deconvolve(a) => deconvolve::run(a),
        Command::Solve(a) => solve::run(a),
        Command::Photocal(a) => photocal::run(a),
        Command::Subframes(a) => science::subframes(a),
        Command::Annotate(a) => astrometry::annotate(a),
        Command::Photometry(a) => astrometry::photometry(a),
        Command::Optics(a) => optics::run(a),
        Command::Plot(a) => science::plot(a),
        Command::Info(a) => info::run(a),
    }
}
