// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Synthetic data with known answers for tests and benchmarks.

/// A small deterministic random number generator (`xoshiro256**`, seeded by
/// `SplitMix64`), so a test's data is the same on every machine.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
    spare: Option<f64>,
}

impl Rng {
    /// A generator for `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Self {
            s: [next(), next(), next(), next()],
            spare: None,
        }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }

    /// A uniform number in `[0, 1)`.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A standard normal deviate (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        let th = std::f64::consts::TAU * u2;
        self.spare = Some(r * th.sin());
        r * th.cos()
    }

    /// A normal deviate with the given mean and standard deviation.
    pub fn gauss(&mut self, mean: f64, sigma: f64) -> f64 {
        mean + sigma * self.normal()
    }
}

/// `erf(x)` to about 1.5e-7 (Abramowitz and Stegun 7.1.26).
#[must_use]
pub fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let y = 1.0 - poly * (-x * x).exp();
    if x >= 0.0 { y } else { -y }
}

/// Flux of a unit Gaussian of width `sigma` centred at `c` that falls in the
/// pixel `[i - 0.5, i + 0.5]`.
fn pixel_fraction(i: usize, c: f64, sigma: f64) -> f64 {
    let s = sigma * std::f64::consts::SQRT_2;
    0.5 * (erf((i as f64 + 0.5 - c) / s) - erf((i as f64 - 0.5 - c) / s))
}

/// A flat sky of `level` with Gaussian stars `(x, y, total_flux, sigma)` added.
/// Each star is integrated over the pixel areas, so its measured centre is
/// exactly `(x, y)` where pixel `(0, 0)` is centred on the coordinate `(0, 0)`.
#[must_use]
pub fn gaussian_field(
    width: usize,
    height: usize,
    level: f32,
    stars: &[(f64, f64, f64, f64)],
) -> helia_core::Frame {
    let wide: Vec<(f64, f64, f64, f64, f64)> =
        stars.iter().map(|&(x, y, f, s)| (x, y, f, s, s)).collect();
    gaussian_field_elliptical(width, height, level, &wide)
}

/// Like [`gaussian_field`] with stars `(x, y, total_flux, sigma_x, sigma_y)`
/// whose axes lie along x and y.
#[must_use]
pub fn gaussian_field_elliptical(
    width: usize,
    height: usize,
    level: f32,
    stars: &[(f64, f64, f64, f64, f64)],
) -> helia_core::Frame {
    let mut data = vec![level; width * height];
    for &(cx, cy, flux, sx, sy) in stars {
        let (rx, ry) = ((5.0 * sx).ceil() as i64, (5.0 * sy).ceil() as i64);
        let (x0, x1) = (
            (cx as i64 - rx).max(0),
            (cx as i64 + rx).min(width as i64 - 1),
        );
        let (y0, y1) = (
            (cy as i64 - ry).max(0),
            (cy as i64 + ry).min(height as i64 - 1),
        );
        for y in y0..=y1 {
            let fy = pixel_fraction(y as usize, cy, sy);
            for x in x0..=x1 {
                data[y as usize * width + x as usize] +=
                    (flux * fy * pixel_fraction(x as usize, cx, sx)) as f32;
            }
        }
    }
    helia_core::Frame {
        width,
        height,
        data,
        bayer: None,
    }
}

/// A sky of stars with known positions, which can be rendered through
/// different pointings, transparencies and seeing.
#[derive(Clone, Debug)]
pub struct Scene {
    /// Width of the reference grid in pixels.
    pub width: usize,
    /// Height of the reference grid in pixels.
    pub height: usize,
    /// Stars as `(x, y, total_flux, sigma)` in reference coordinates.
    pub stars: Vec<(f64, f64, f64, f64)>,
}

impl Scene {
    /// `n` random stars with a heavy-tailed brightness distribution.
    #[must_use]
    pub fn random(n: usize, width: usize, height: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let stars = (0..n)
            .map(|_| {
                let flux = 3000.0 * (1.0 / (0.04 + rng.uniform())).min(60.0);
                (
                    rng.uniform() * width as f64,
                    rng.uniform() * height as f64,
                    flux,
                    1.5 + rng.uniform() * 0.6,
                )
            })
            .collect();
        Self {
            width,
            height,
            stars,
        }
    }

    /// Render the sky as a `width` by `height` frame seen through `view`
    /// (sky coordinates to frame coordinates): a flat `sky` level, Gaussian
    /// noise of `noise` ADU, stars scaled by `flux_scale` and widened by
    /// `extra_blur` pixels (in quadrature).
    #[must_use]
    pub fn render(
        &self,
        view: &crate::transform::Transform,
        sky: f32,
        noise: f64,
        flux_scale: f64,
        extra_blur: f64,
        seed: u64,
    ) -> helia_core::Frame {
        let stars: Vec<(f64, f64, f64, f64)> = self
            .stars
            .iter()
            .map(|&(x, y, f, s)| {
                let (nx, ny) = view.apply((x, y));
                (nx, ny, f * flux_scale, s.hypot(extra_blur))
            })
            .collect();
        let mut frame = gaussian_field(self.width, self.height, sky, &stars);
        let mut rng = Rng::new(seed);
        for v in &mut frame.data {
            *v += rng.gauss(0.0, noise) as f32;
        }
        frame
    }
}

/// A synthetic deep-sky frame with known truth.
#[derive(Clone, Debug)]
pub struct DeepSky {
    /// Sky, nebulosity, stars and noise.
    pub frame: helia_core::Frame,
    /// The same without the stars (the same noise), what star removal should find.
    pub starless: helia_core::Frame,
    /// The stars as `(x, y, total_flux, sigma)`.
    pub stars: Vec<(f64, f64, f64, f64)>,
}

/// A made-up deep-sky frame: a flat sky, a few broad smooth nebulae, `n_stars`
/// Gaussian stars with a heavy-tailed brightness distribution, and noise.
/// Not a picture of the sky; it has the right structure to test stretching,
/// star removal and plate solving against known truth.
#[must_use]
pub fn deep_sky(width: usize, height: usize, n_stars: usize, seed: u64) -> DeepSky {
    let mut rng = Rng::new(seed);
    let (w, h) = (width as f64, height as f64);
    let blobs: Vec<(f64, f64, f64, f64, f64)> = (0..5)
        .map(|_| {
            let (sx, sy) = (
                w * (0.08 + 0.12 * rng.uniform()),
                h * (0.08 + 0.12 * rng.uniform()),
            );
            let peak = 60.0 + 260.0 * rng.uniform();
            (
                w * (0.15 + 0.7 * rng.uniform()),
                h * (0.15 + 0.7 * rng.uniform()),
                peak * 2.0 * std::f64::consts::PI * sx * sy,
                sx,
                sy,
            )
        })
        .collect();
    let mut starless = gaussian_field_elliptical(width, height, 200.0, &blobs);
    for v in &mut starless.data {
        *v += rng.gauss(0.0, 6.0) as f32;
    }
    let stars = Scene::random(n_stars, width, height, seed ^ 0x5eed).stars;
    let with_stars = gaussian_field(width, height, 0.0, &stars);
    let mut frame = starless.clone();
    for (v, s) in frame.data.iter_mut().zip(&with_stars.data) {
        *v += s;
    }
    DeepSky {
        frame,
        starless,
        stars,
    }
}

/// A synthetic plate-solving scene with known truth.
#[derive(Clone, Debug)]
pub struct PlateScene {
    /// Catalogue of the stars brighter than G = 11.
    pub catalogue: crate::catalogue::Catalogue,
    /// The image: sky, noise and every star to G = 12.5 in the field.
    pub frame: helia_core::Frame,
    /// The true world coordinate system of the image.
    pub truth: crate::wcs::Wcs,
}

/// A made-up patch of sky imaged at `scale` arc seconds per pixel, rotated by
/// `rotation` degrees: stars to G = 13 in a cap of six degrees around
/// `centre`, a catalogue to G = 11, and a `width` by `height` image of the
/// stars to G = 12.5 (about 90 stars per square degree). Not the real sky.
#[must_use]
pub fn plate_scene(
    centre: (f64, f64),
    width: usize,
    height: usize,
    scale: f64,
    rotation: f64,
    seed: u64,
) -> PlateScene {
    use crate::catalogue::{CatStar, Catalogue};
    use crate::wcs::{Wcs, deproject};
    let mut rng = Rng::new(seed);
    let cap = 6.0;
    let n = (90.0 * std::f64::consts::PI * cap * cap) as usize;
    let mut all = Vec::with_capacity(n);
    while all.len() < n {
        let (dx, dy) = (
            (rng.uniform() * 2.0 - 1.0) * cap,
            (rng.uniform() * 2.0 - 1.0) * cap,
        );
        if dx.hypot(dy) <= cap {
            let (ra, dec) = deproject(dx, dy, centre.0, centre.1);
            all.push(CatStar {
                ra,
                dec,
                mag: 7.0 + 6.0 * rng.uniform().sqrt() as f32,
                colour: None,
            });
        }
    }
    let bright: Vec<CatStar> = all.iter().copied().filter(|s| s.mag < 11.0).collect();
    let truth = Wcs::simple(
        centre,
        (width as f64 / 2.0 + 0.5, height as f64 / 2.0 + 0.5),
        scale,
        rotation,
        false,
    );
    let specs: Vec<(f64, f64, f64, f64)> = all
        .iter()
        .filter(|s| s.mag <= 12.5)
        .filter_map(|s| {
            let (x, y) = truth.world_to_pixel(s.ra, s.dec)?;
            let flux = 10f64.powf(-0.4 * f64::from(s.mag)) * 4.0e6;
            (x > 4.0 && y > 4.0 && x < width as f64 - 5.0 && y < height as f64 - 5.0)
                .then_some((x, y, flux, 1.8))
        })
        .collect();
    let mut frame = gaussian_field(width, height, 200.0, &specs);
    for v in &mut frame.data {
        *v += rng.gauss(0.0, 5.0) as f32;
    }
    PlateScene {
        catalogue: Catalogue::build(&bright, 11.0),
        frame,
        truth,
    }
}

/// A synthetic colour field with known truth for colour calibration.
#[derive(Clone, Debug)]
pub struct ColourScene {
    /// Catalogue of the stars brighter than G = 11, with BP-RP colours.
    pub catalogue: crate::catalogue::Catalogue,
    /// The red, green and blue image, with a different sky in each plane.
    pub image: crate::fits::Image,
    /// The true world coordinate system.
    pub truth: crate::wcs::Wcs,
    /// The gains that balance the planes (red, green, blue) at BP-RP 0.82.
    pub true_gains: [f64; 3],
    /// The sky level of each plane.
    pub sky: [f32; 3],
}

/// A made-up colour field: stars with Gaia-like G and BP-RP, each plane scaled
/// by a camera response that is linear in colour (red up and blue down with
/// BP-RP) and out of balance by `true_gains`. `density` is stars per square
/// degree to G = 13.
#[must_use]
pub fn colour_scene(
    centre: (f64, f64),
    width: usize,
    height: usize,
    scale: f64,
    density: f64,
    seed: u64,
) -> ColourScene {
    use crate::catalogue::{CatStar, Catalogue};
    use crate::wcs::{Wcs, deproject};
    let mut rng = Rng::new(seed);
    let cap = (width as f64).hypot(height as f64) * scale / 3600.0 / 2.0 * 1.1;
    let n = (density * std::f64::consts::PI * cap * cap) as usize;
    let mut all = Vec::with_capacity(n);
    while all.len() < n {
        let (dx, dy) = (
            (rng.uniform() * 2.0 - 1.0) * cap,
            (rng.uniform() * 2.0 - 1.0) * cap,
        );
        if dx.hypot(dy) <= cap {
            let (ra, dec) = deproject(dx, dy, centre.0, centre.1);
            all.push(CatStar {
                ra,
                dec,
                mag: 7.0 + 6.0 * rng.uniform().sqrt() as f32,
                colour: Some(0.2 + 2.3 * rng.uniform() as f32),
            });
        }
    }
    let bright: Vec<CatStar> = all.iter().copied().filter(|s| s.mag < 11.0).collect();
    let catalogue = Catalogue::build(&bright, 11.0);
    let truth = Wcs::simple(
        centre,
        (width as f64 / 2.0 + 0.5, height as f64 / 2.0 + 0.5),
        scale,
        0.0,
        false,
    );
    // Instrumental colour against green: zero point at BP-RP 0.82 and slope.
    let (zero, slope) = ([0.35, 0.0, -0.2], [-0.55, 0.0, 0.45]);
    let true_gains = [10f64.powf(0.4 * zero[0]), 1.0, 10f64.powf(0.4 * zero[2])];
    let sky = [180.0f32, 240.0, 150.0];
    let planes = (0..3)
        .map(|c| {
            let specs: Vec<(f64, f64, f64, f64)> = all
                .iter()
                .filter(|s| s.mag <= 12.5)
                .filter_map(|s| {
                    let (x, y) = truth.world_to_pixel(s.ra, s.dec)?;
                    let colour = f64::from(s.colour.unwrap_or(0.82)) - 0.82;
                    let ratio = 10f64.powf(-0.4 * (zero[c] + slope[c] * colour));
                    let flux = 10f64.powf(-0.4 * f64::from(s.mag)) * 3.0e7 * ratio;
                    (x > 4.0 && y > 4.0 && x < width as f64 - 5.0 && y < height as f64 - 5.0)
                        .then_some((x, y, flux, 1.8))
                })
                .collect();
            let mut f = gaussian_field(width, height, sky[c], &specs);
            for v in &mut f.data {
                *v += rng.gauss(0.0, 5.0) as f32;
            }
            f
        })
        .collect();
    ColourScene {
        catalogue,
        image: crate::fits::Image {
            header: crate::fits::Header::new(),
            planes,
        },
        truth,
        true_gains,
        sky,
    }
}

/// A model camera sensor with a fixed bias pattern, dark current (a gentle
/// glow plus hot pixels), a flat field (vignetting, dust, colour gain and
/// pixel-to-pixel response), dead pixels and Gaussian read and shot noise.
#[derive(Clone, Debug)]
pub struct Sensor {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// The Bayer pattern, for a colour sensor.
    pub bayer: Option<helia_core::BayerPattern>,
    bias: Vec<f32>,
    dark_rate: Vec<f32>,
    flat: Vec<f32>,
    hot: Vec<usize>,
    dead: Vec<usize>,
    /// Read noise in ADU.
    pub read_noise: f32,
}

impl Sensor {
    /// A sensor for `seed`; `bayer` makes it a colour (RGGB) sensor.
    #[must_use]
    pub fn new(width: usize, height: usize, bayer: bool, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let n = width * height;
        let col: Vec<f32> = (0..width).map(|_| rng.gauss(0.0, 2.0) as f32).collect();
        let bias: Vec<f32> = (0..n)
            .map(|i| 1000.0 + col[i % width] + rng.gauss(0.0, 1.0) as f32)
            .collect();
        let pattern = bayer.then_some(helia_core::BayerPattern::Rggb);
        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let rmax2 = cx * cx + cy * cy;
        let dust = [
            (width as f32 * 0.3, height as f32 * 0.4, 6.0_f32),
            (width as f32 * 0.7, height as f32 * 0.7, 9.0),
            (width as f32 * 0.55, height as f32 * 0.2, 4.0),
        ];
        let mut flat = Vec::with_capacity(n);
        let mut dark_rate = Vec::with_capacity(n);
        for i in 0..n {
            let (x, y) = ((i % width) as f32, (i / width) as f32);
            let r2 = ((x - cx).powi(2) + (y - cy).powi(2)) / rmax2;
            let mut f = 1.0 - 0.35 * r2;
            for &(dx, dy, rad) in &dust {
                let d = ((x - dx).powi(2) + (y - dy).powi(2)).sqrt();
                if d < rad {
                    f *= 0.85 + 0.15 * (d / rad);
                }
            }
            if let Some(p) = pattern {
                f *= [0.75, 1.0, 0.6][p.channel_at(i % width, i / width)];
            }
            f *= 1.0 + 0.004 * rng.normal() as f32;
            flat.push(f);
            let glow = 0.05 * (1.0 - x / width as f32).powi(3) * (1.0 - y / height as f32).powi(3);
            dark_rate.push(0.02 + glow);
        }
        let mut hot = Vec::new();
        let mut dead = Vec::new();
        let mut taken = std::collections::HashSet::new();
        let mut pick = |rng: &mut Rng| loop {
            let i = (rng.uniform() * n as f64) as usize;
            // Keep defects apart so repairs have clean neighbours.
            let (x, y) = (i % width, i / width);
            if x > 3 && y > 3 && x + 4 < width && y + 4 < height && taken.insert((x / 6, y / 6)) {
                return i;
            }
        };
        for _ in 0..25 {
            let i = pick(&mut rng);
            dark_rate[i] += 0.3 + 2.7 * rng.uniform() as f32;
            hot.push(i);
        }
        for _ in 0..5 {
            let i = pick(&mut rng);
            flat[i] = 0.0;
            dark_rate[i] = 0.0;
            dead.push(i);
        }
        Self {
            width,
            height,
            bayer: pattern,
            bias,
            dark_rate,
            flat,
            hot,
            dead,
            read_noise: 3.0,
        }
    }

    fn frame(&self, data: Vec<f32>) -> helia_core::Frame {
        helia_core::Frame {
            width: self.width,
            height: self.height,
            data,
            bayer: self.bayer,
        }
    }

    /// The true bias level of every pixel.
    #[must_use]
    pub fn bias_frame(&self) -> helia_core::Frame {
        self.frame(self.bias.clone())
    }

    /// The true dark current (ADU) accumulated in `seconds`.
    #[must_use]
    pub fn dark_current_frame(&self, seconds: f32) -> helia_core::Frame {
        self.frame(self.dark_rate.iter().map(|r| r * seconds).collect())
    }

    /// The true relative response of every pixel (not normalised).
    #[must_use]
    pub fn flat_frame(&self) -> helia_core::Frame {
        self.frame(self.flat.clone())
    }

    /// Mean of the true flat, which is what a calibrated frame is scaled by.
    #[must_use]
    pub fn mean_flat(&self) -> f32 {
        self.flat.iter().sum::<f32>() / self.flat.len() as f32
    }

    /// Indices of the hot and dead pixels.
    #[must_use]
    pub fn defect_indices(&self) -> Vec<usize> {
        self.hot.iter().chain(&self.dead).copied().collect()
    }

    /// Expose `signal` (counts that would arrive on a perfect pixel) for
    /// `seconds`, returning the raw frame with a FITS-like header.
    #[must_use]
    pub fn expose(
        &self,
        signal: &helia_core::Frame,
        seconds: f32,
        rng: &mut Rng,
    ) -> crate::fits::Image {
        use crate::fits::{Header, Image, Value};
        let data = (0..self.width * self.height)
            .map(|i| {
                let dark = self.dark_rate[i] * seconds;
                let sig = signal.data[i] * self.flat[i];
                let var = self.read_noise.powi(2) + (dark + sig).max(0.0);
                self.bias[i] + dark + sig + (f64::from(var).sqrt() * rng.normal()) as f32
            })
            .collect();
        let mut header = Header::new();
        header.set("EXPTIME", Value::Float(f64::from(seconds)), "[s]");
        header.set("CCD-TEMP", Value::Float(-10.0), "[degC]");
        if self.bayer.is_some() {
            header.set("BAYERPAT", Value::Str("RGGB".into()), "");
        }
        Image::mono(self.frame(data), header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_normal() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
        let n = 200_000;
        let v: Vec<f64> = (0..n).map(|_| a.normal()).collect();
        let mean = v.iter().sum::<f64>() / f64::from(n);
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / f64::from(n);
        assert!(mean.abs() < 0.01, "{mean}");
        assert!((var - 1.0).abs() < 0.02, "{var}");
    }
}
