// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

use super::*;
use crate::fits::Header;
use crate::stack::{Reference, stack};
use crate::testutil::{Rng, Scene};
use crate::warp::Interpolation;

fn silent(_: &str) {}

/// A sky of `n` stars of the given Gaussian `sigma` (small is undersampled).
fn scene(n: usize, w: usize, h: usize, sigma: f64, seed: u64) -> Scene {
    let mut rng = Rng::new(seed);
    Scene {
        width: w,
        height: h,
        stars: (0..n)
            .map(|_| {
                (
                    8.0 + rng.uniform() * (w as f64 - 16.0),
                    8.0 + rng.uniform() * (h as f64 - 16.0),
                    6000.0 + 30000.0 * rng.uniform(),
                    sigma,
                )
            })
            .collect(),
    }
}

fn mono(f: Frame) -> Image {
    let mut img = Image::mono(f, Header::new());
    img.header.set("EXPTIME", Value::Float(60.0), "");
    img
}

/// `n` renderings with random sub-pixel pointing changes of up to `reach`
/// pixels; frame 0 is exactly the scene.
fn dithered(sc: &Scene, n: usize, reach: f64, noise: f64, seed: u64) -> Vec<Image> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|k| {
            let (dx, dy) = if k == 0 {
                (0.0, 0.0)
            } else {
                (
                    (rng.uniform() - 0.5) * 2.0 * reach,
                    (rng.uniform() - 0.5) * 2.0 * reach,
                )
            };
            let view = Transform::similarity(1.0, 0.0, dx, dy);
            mono(sc.render(&view, 300.0, noise, 1.0, 0.0, seed * 1000 + k as u64))
        })
        .collect()
}

fn stack_opts() -> StackOptions {
    StackOptions {
        reference: Reference::Index(0),
        ..StackOptions::default()
    }
}

/// The brightest stars with no other star within `gap` pixels and clear of the edge.
fn isolated(sc: &Scene, gap: f64, n: usize) -> Vec<(f64, f64, f64)> {
    let mut v: Vec<(f64, f64, f64)> = sc
        .stars
        .iter()
        .enumerate()
        .filter(|&(i, s)| {
            s.0 > 20.0
                && s.1 > 20.0
                && s.0 < sc.width as f64 - 20.0
                && s.1 < sc.height as f64 - 20.0
                && sc
                    .stars
                    .iter()
                    .enumerate()
                    .all(|(j, t)| i == j || (s.0 - t.0).hypot(s.1 - t.1) > gap)
        })
        .map(|(_, s)| (s.0, s.1, s.2))
        .collect();
    v.sort_by(|a, b| b.2.total_cmp(&a.2));
    v.truncate(n);
    v
}

/// Sky-subtracted flux in a box of half-width `r` around `(x, y)`, with pixels
/// that have no data (NaN) taken to be the mean of the others.
fn box_flux(f: &Frame, x: f64, y: f64, r: i64, sky: f32) -> f64 {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    let (mut s, mut n, mut all) = (0.0, 0usize, 0usize);
    for j in cy - r..=cy + r {
        for i in cx - r..=cx + r {
            all += 1;
            let v = f.data[j as usize * f.width + i as usize];
            if v.is_finite() {
                s += f64::from(v - sky);
                n += 1;
            }
        }
    }
    s * all as f64 / n.max(1) as f64
}

/// FWHM from the flux-weighted second moments of the star at `(x, y)` (grid
/// coordinates) within `r` pixels, along x and y, in grid pixels.
fn moment_fwhm(f: &Frame, x: f64, y: f64, r: i64, sky: f32) -> f64 {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    let (mut s, mut sx, mut sy, mut sxx, mut syy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for j in cy - r..=cy + r {
        for i in cx - r..=cx + r {
            let v = f64::from(f.data[j as usize * f.width + i as usize] - sky).max(0.0);
            s += v;
            sx += v * i as f64;
            sy += v * j as f64;
            sxx += v * (i * i) as f64;
            syy += v * (j * j) as f64;
        }
    }
    let (mx, my) = (sx / s, sy / s);
    let var = f64::midpoint(sxx / s - mx * mx, syy / s - my * my);
    2.354_82 * var.sqrt()
}

#[test]
fn the_overlap_of_a_square_with_pixels_is_exact() {
    let unit = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
    assert!((overlap(&unit, -0.5, 0.5, -0.5, 0.5) - 0.09).abs() < 1e-12);
    assert!((overlap(&unit, -2.0, 2.0, -2.0, 2.0) - 0.36).abs() < 1e-12);
    assert!((overlap(&unit, 1.0, 2.0, -0.5, 0.5)).abs() < 1e-12);
    // Straddling a pixel edge: split in proportion.
    let q = [(0.3, 0.0), (0.7, 0.0), (0.7, 0.4), (0.3, 0.4)];
    let (a, b) = (
        overlap(&q, -0.5, 0.5, -0.5, 0.5),
        overlap(&q, 0.5, 1.5, -0.5, 0.5),
    );
    assert!((a - 0.2 * 0.4).abs() < 1e-12 && (b - 0.2 * 0.4).abs() < 1e-12);
    // A rotated square's overlaps add up to its area.
    let (c, s) = (0.5f64.cos(), 0.5f64.sin());
    let rot: [Pt; 4] = [(-0.7, -0.7), (0.7, -0.7), (0.7, 0.7), (-0.7, 0.7)]
        .map(|(x, y)| (2.3 + c * x - s * y, 3.1 + s * x + c * y));
    let mut total = 0.0;
    for j in 0..6 {
        for i in 0..6 {
            total += overlap(
                &rot,
                f64::from(i) - 0.5,
                f64::from(i) + 0.5,
                f64::from(j) - 0.5,
                f64::from(j) + 0.5,
            );
        }
    }
    assert!((total - 1.96).abs() < 1e-9, "{total}");
}

#[test]
fn scale_one_and_full_drops_are_shift_and_add() {
    let sc = scene(120, 600, 450, 1.8, 3);
    let frames: Vec<Image> = {
        let mut rng = Rng::new(9);
        (0..8)
            .map(|k| {
                let view = Transform::similarity(
                    1.0,
                    0.0,
                    (rng.uniform() * 10.0 - 5.0).round() * f64::from(u8::from(k > 0)),
                    (rng.uniform() * 10.0 - 5.0).round() * f64::from(u8::from(k > 0)),
                );
                mono(sc.render(&view, 300.0, 10.0, 1.0, 0.0, 70 + k))
            })
            .collect()
    };
    let d = drizzle(
        &frames,
        &stack_opts(),
        &Options {
            scale: 1.0,
            pixfrac: 1.0,
            reject: None,
        },
        &mut silent,
    )
    .expect("drizzle");
    let s = stack(frames, &stack_opts(), &mut silent).expect("stack");
    assert_eq!(d.image.planes[0].width, 600);
    // Same noise in a clean patch, and the same stars where both have data.
    let patch = |f: &Frame| -> f32 {
        let mut v = Vec::new();
        for y in 150..250 {
            for x in 150..250 {
                v.push(f.data[y * f.width + x]);
            }
        }
        stats::median_sigma(&v).1
    };
    let (nd, ns) = (patch(&d.image.planes[0]), patch(&s.image.planes[0]));
    assert!((nd / ns - 1.0).abs() < 0.15, "noise {nd} against {ns}");
    let mut diff = Vec::new();
    for y in 40..400 {
        for x in 40..550 {
            let i = y * 600 + x;
            let (a, b) = (d.image.planes[0].data[i], s.image.planes[0].data[i]);
            if a.is_finite() && b.is_finite() {
                diff.push(a - b);
            }
        }
    }
    let rms = (diff.iter().map(|v| v * v).sum::<f32>() / diff.len() as f32).sqrt();
    assert!(rms < 0.6 * ns, "rms difference {rms}, stack noise {ns}");
}

#[test]
fn flux_is_conserved_on_the_finer_grid() {
    let sc = scene(60, 500, 400, 1.2, 4);
    let frames = dithered(&sc, 12, 2.0, 4.0, 5);
    let d = drizzle(&frames, &stack_opts(), &Options::default(), &mut silent).expect("drizzle");
    let out = &d.image.planes[0];
    assert_eq!((out.width, out.height), (1000, 800));
    // The brightest isolated star: its flux in output pixels is scale^2 times
    // the number of input pixels it covers, each pixel being surface brightness.
    let (x, y, flux) = isolated(&sc, 20.0, 1)[0];
    let (ux, uy) = ((x + 0.5) * 2.0 - 0.5, (y + 0.5) * 2.0 - 0.5);
    let got = box_flux(out, ux, uy, 14, 300.0) / 4.0;
    let raw = box_flux(&frames[0].planes[0], x, y, 7, 300.0);
    eprintln!("flux: drizzle {got:.0}, reference frame {raw:.0}, true {flux:.0}");
    assert!((got / flux - 1.0).abs() < 0.04, "flux {got}, true {flux}");
}

#[test]
fn dithered_undersampled_frames_are_sharper_than_a_softer_stack() {
    // A Gaussian star of sigma 0.65 px has a FWHM of 1.5 px: undersampled.
    let sc = scene(150, 700, 500, 0.65, 11);
    let frames = dithered(&sc, 24, 3.0, 0.2, 12);
    let d = drizzle(
        &frames,
        &stack_opts(),
        &Options {
            scale: 2.0,
            pixfrac: 0.6,
            reject: Some(4.0),
        },
        &mut silent,
    )
    .expect("drizzle");
    let pts: Vec<(f64, f64)> = isolated(&sc, 25.0, 12).iter().map(|s| (s.0, s.1)).collect();
    assert!(pts.len() >= 8);
    let mean = |f: &Frame, scale: f64, r: i64| -> f64 {
        pts.iter()
            .map(|&(x, y)| {
                moment_fwhm(
                    f,
                    (x + 0.5) * scale - 0.5,
                    (y + 0.5) * scale - 0.5,
                    r,
                    300.0,
                ) / scale
            })
            .sum::<f64>()
            / pts.len() as f64
    };
    let fd = mean(&d.image.planes[0], 2.0, 10);
    assert!(d.holes() < 0.001, "{} holes", d.holes());
    // The same frames stacked with each interpolation kernel.
    let mut seen = Vec::new();
    for interp in [
        Interpolation::Bilinear,
        Interpolation::Bicubic,
        Interpolation::Lanczos3,
    ] {
        let s = stack(
            frames.clone(),
            &StackOptions {
                interpolation: interp,
                ..stack_opts()
            },
            &mut silent,
        )
        .expect("stack");
        seen.push((interp, mean(&s.image.planes[0], 1.0, 5)));
    }
    eprintln!(
        "FWHM in input pixels: drizzle {fd:.3}, stacks {seen:?} (the star is 1.531 before the pixel)"
    );
    // Drizzle does not interpolate, so it is sharper than the softest kernel, and
    // close to the sharper ones, which ring.
    assert!(
        fd < 0.97 * seen[0].1,
        "drizzle {fd}, bilinear {}",
        seen[0].1
    );
    assert!(fd < 1.06 * seen[1].1, "drizzle {fd}, bicubic {}", seen[1].1);
}

#[test]
fn a_close_pair_is_resolved_better_by_drizzle() {
    // Two stars 2 px apart, each sigma 0.6: blended in one pixel grid.
    let mut sc = scene(40, 300, 300, 0.6, 1);
    sc.stars.retain(|s| (s.0 - 150.0).hypot(s.1 - 150.0) > 25.0);
    sc.stars.push((150.0, 150.0, 30000.0, 0.6));
    sc.stars.push((152.0, 150.0, 30000.0, 0.6));
    let frames = dithered(&sc, 30, 3.0, 2.0, 21);
    let d = drizzle(
        &frames,
        &stack_opts(),
        &Options {
            scale: 3.0,
            pixfrac: 0.5,
            reject: None,
        },
        &mut silent,
    )
    .expect("drizzle");
    let s = stack(frames, &stack_opts(), &mut silent).expect("stack");
    // Dip between the two stars as a fraction of the lower peak, along the row.
    let dip = |f: &Frame, scale: f64| -> f32 {
        let at = |x: f64| -> f32 {
            let (ux, uy) = ((x + 0.5) * scale - 0.5, (150.0 + 0.5) * scale - 0.5);
            let (i, j) = (ux.round() as usize, uy.round() as usize);
            f.data[j * f.width + i] - 300.0
        };
        let (a, b, mid) = (at(150.0), at(152.0), at(151.0));
        mid / a.min(b)
    };
    let (dd, ds) = (dip(&d.image.planes[0], 3.0), dip(&s.image.planes[0], 1.0));
    eprintln!("valley over lower peak: drizzle {dd:.3}, plain stack {ds:.3}");
    assert!(dd < ds - 0.05, "drizzle {dd}, stack {ds}");
}

#[test]
fn an_outlier_is_rejected_by_the_second_pass() {
    let sc = scene(80, 400, 300, 1.4, 6);
    let mut frames = dithered(&sc, 14, 2.5, 4.0, 7);
    // A cosmic ray in one frame and a hot pixel in another.
    frames[4].planes[0].data[150 * 400 + 200] += 6000.0;
    frames[9].planes[0].data[60 * 400 + 90] += 4000.0;
    let on = drizzle(&frames, &stack_opts(), &Options::default(), &mut silent).expect("on");
    let off = drizzle(
        &frames,
        &stack_opts(),
        &Options {
            reject: None,
            ..Options::default()
        },
        &mut silent,
    )
    .expect("off");
    let peak = |d: &Drizzled, x: usize, y: usize| -> f32 {
        let f = &d.image.planes[0];
        let mut m = f32::MIN;
        for j in 2 * y - 3..2 * y + 5 {
            for i in 2 * x - 3..2 * x + 5 {
                m = m.max(f.data[j * f.width + i] - 300.0);
            }
        }
        m
    };
    // Place the ray where the reference sees it (the other frames are dithered).
    let before = peak(&off, 200, 150).max(peak(&off, 90, 60));
    let after = peak(&on, 200, 150).max(peak(&on, 90, 60));
    assert!(before > 150.0, "the unrejected ray is {before}");
    assert!(after < 60.0, "the ray survives rejection at {after}");
    assert!(on.rejected > 0.0 && on.rejected < 0.01, "{}", on.rejected);
}

#[test]
fn a_raw_mosaic_drops_each_pixel_into_its_own_colour() {
    let sc = scene(120, 400, 300, 0.8, 13);
    let gains = [1.0f64, 1.6, 0.7];
    let pat = BayerPattern::Rggb;
    let mut rng = Rng::new(30);
    let frames: Vec<Image> = (0..20)
        .map(|k| {
            let (dx, dy) = if k == 0 {
                (0.0, 0.0)
            } else {
                ((rng.uniform() - 0.5) * 6.0, (rng.uniform() - 0.5) * 6.0)
            };
            let view = Transform::similarity(1.0, 0.0, dx, dy);
            let colours: Vec<Frame> = gains
                .iter()
                .enumerate()
                .map(|(c, &g)| sc.render(&view, 300.0, 3.0, g, 0.0, 500 + 3 * k + c as u64))
                .collect();
            let mut mosaic = colours[0].clone();
            for y in 0..300 {
                for x in 0..400 {
                    mosaic.data[y * 400 + x] = colours[pat.channel_at(x, y)].data[y * 400 + x];
                }
            }
            mosaic.bayer = Some(pat);
            let mut img = Image::mono(mosaic, Header::new());
            img.header.set("EXPTIME", Value::Float(60.0), "");
            img
        })
        .collect();
    let d = drizzle(
        &frames,
        &stack_opts(),
        &Options {
            scale: 2.0,
            pixfrac: 1.0,
            reject: Some(4.0),
        },
        &mut silent,
    )
    .expect("drizzle");
    assert_eq!(d.image.planes.len(), 3);
    assert!(d.image.header.get("BAYERPAT").is_none());
    let (x, y, flux) = isolated(&sc, 20.0, 1)[0];
    let (ux, uy) = ((x + 0.5) * 2.0 - 0.5, (y + 0.5) * 2.0 - 0.5);
    for (c, &g) in gains.iter().enumerate() {
        let got = box_flux(&d.image.planes[c], ux, uy, 12, 300.0) / 4.0;
        assert!(
            (got / (flux * g) - 1.0).abs() < 0.06,
            "plane {c}: flux {got}, true {}",
            flux * g
        );
    }
}

#[test]
fn a_frame_of_another_field_is_left_out_and_said_so() {
    let sc = scene(100, 400, 300, 1.4, 8);
    let mut frames = dithered(&sc, 8, 2.0, 4.0, 9);
    let other = scene(100, 400, 300, 1.4, 99);
    frames[5] = mono(other.render(&Transform::identity(), 300.0, 4.0, 1.0, 0.0, 77));
    let d = drizzle(&frames, &stack_opts(), &Options::default(), &mut silent).expect("drizzle");
    assert_eq!(d.used(), 7);
    assert!(matches!(d.frames[5].status, Status::Skipped(_)));
    assert_eq!(d.image.header.get("NCOMBINE"), Some(&Value::Int(7)));
    assert_eq!(d.image.header.get_f64("DRIZSCL"), Some(2.0));
}

#[test]
fn too_few_drops_leave_holes_that_the_weight_map_shows() {
    let sc = scene(80, 300, 220, 1.4, 14);
    let few = dithered(&sc, 3, 2.0, 3.0, 15);
    let d = drizzle(
        &few,
        &stack_opts(),
        &Options {
            scale: 3.0,
            pixfrac: 0.3,
            reject: None,
        },
        &mut silent,
    )
    .expect("drizzle");
    assert!(d.holes() > 0.2, "{} holes", d.holes());
    let zero = d.weight.data.iter().filter(|&&w| w <= 0.0).count();
    assert!(zero > 0);
    let many = dithered(&sc, 30, 3.0, 3.0, 16);
    let full = drizzle(
        &many,
        &stack_opts(),
        &Options {
            scale: 3.0,
            pixfrac: 0.3,
            reject: None,
        },
        &mut silent,
    )
    .expect("drizzle");
    assert!(
        full.holes() < d.holes() / 4.0,
        "{} then {}",
        d.holes(),
        full.holes()
    );
}

#[test]
fn bad_settings_and_inputs_are_refused() {
    let sc = scene(60, 200, 150, 1.4, 17);
    let frames = dithered(&sc, 4, 1.0, 3.0, 18);
    let bad = |o: Options| {
        drizzle(&frames, &stack_opts(), &o, &mut silent)
            .expect_err("refused")
            .to_string()
    };
    assert!(
        bad(Options {
            scale: 0.0,
            ..Options::default()
        })
        .contains("scale")
    );
    assert!(
        bad(Options {
            pixfrac: 0.0,
            ..Options::default()
        })
        .contains("pixfrac")
    );
    assert!(
        bad(Options {
            pixfrac: 1.5,
            ..Options::default()
        })
        .contains("pixfrac")
    );
    assert!(
        bad(Options {
            reject: Some(0.0),
            ..Options::default()
        })
        .contains("rejection")
    );
    assert!(
        drizzle(&[], &stack_opts(), &Options::default(), &mut silent)
            .expect_err("empty")
            .to_string()
            .contains("no frames")
    );
    // A mosaic among colour frames.
    let mut mixed = frames.clone();
    mixed[1].planes[0].bayer = Some(BayerPattern::Rggb);
    assert!(
        drizzle(&mixed, &stack_opts(), &Options::default(), &mut silent)
            .expect_err("mixed")
            .to_string()
            .contains("Bayer pattern")
    );
}

#[test]
fn a_cancelled_token_stops_the_drizzle() {
    let sc = scene(60, 200, 160, 1.2, 3);
    let frames = dithered(&sc, 4, 3.0, 0.0, 5);
    let stack = StackOptions {
        cancel: CancelToken::new(),
        ..stack_opts()
    };
    let token = stack.cancel.clone();
    // Cancel once registration is done, so the drops are what stops.
    let r = drizzle(&frames, &stack, &Options::default(), &mut |l: &str| {
        if l.starts_with("drizzling") {
            token.cancel();
        }
    });
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    let done = drizzle(&frames, &stack_opts(), &Options::default(), &mut silent);
    assert!(done.is_ok(), "{done:?}");
}
