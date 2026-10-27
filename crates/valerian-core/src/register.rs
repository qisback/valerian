// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Star-pattern registration.
//!
//! Given the stars of a reference frame and of a source frame, find the
//! transform that maps source coordinates onto the reference. Matching
//! compares *triangles* of stars by their shape (the ratios of their sides,
//! which do not change under rotation, scale or shift), so no starting guess
//! is needed and a rotation, a meridian flip or a large dither is as easy as
//! none:
//!
//! 1. Triangles are built from the brightest stars and their nearest
//!    neighbours, and matched by side ratios; every matched triangle votes for
//!    the three star correspondences it implies.
//! 2. Hypotheses (a similarity from two voted pairs) are scored by how many
//!    source stars they land on a reference star, and the best is kept
//!    (RANSAC over the voted pairs).
//! 3. The transform is refined by least squares on all stars that match,
//!    using the requested [`Model`], with the match radius tightening each
//!    round and outliers clipped.
//!
//! Mirror-image frames are not matched (a telescope does not mirror between
//! subs; a meridian flip is a rotation by 180 degrees).
//!
//! # References
//!
//! - Groth, E. J. (1986), A pattern-matching algorithm for two-dimensional coordinate lists, Astron. J. 91, 1244, doi:10.1086/114099.
//! - Valdes, F. G. et al. (1995), FOCAS automatic catalog matching algorithms, Publ. Astron. Soc. Pac. 107, 1119, doi:10.1086/133667.
//! - Fischler, M. A. and Bolles, R. C. (1981), Random sample consensus, Commun. ACM 24, 381-395, doi:10.1145/358669.358692.
//! - Beroiz, M., Cabral, J. B. and Sanchez, B. (2020), Astroalign: a Python module for astronomical image registration, Astron. Comput. 32, 100384, doi:10.1016/j.ascom.2020.100384 (the same family of method; used as a test oracle only, no code taken).
//! - Umeyama, S. (1991), Least-squares estimation of transformation parameters between two point patterns, IEEE Trans. Pattern Anal. Mach. Intell. 13, 376-380, doi:10.1109/34.88573.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::stars::Star;
use crate::transform::{Model, Point, Transform, fit};

/// Settings for [`register`].
#[derive(Clone, Debug)]
pub struct RegisterOptions {
    /// The transform family to fit at the end.
    pub model: Model,
    /// Stars used to build triangles (the brightest of each frame).
    pub pattern_stars: usize,
    /// Neighbours per star used to form triangles.
    pub neighbours: usize,
    /// Accepted range of the scale between the frames (source to reference).
    pub scale_range: (f64, f64),
    /// Radius in pixels within which a transformed source star counts as
    /// matching a reference star while searching.
    pub search_radius: f64,
    /// Radius in pixels for the final match.
    pub final_radius: f64,
    /// The fewest matched stars to accept a result.
    pub min_matches: usize,
}

impl Default for RegisterOptions {
    fn default() -> Self {
        Self {
            model: Model::Similarity,
            pattern_stars: 80,
            neighbours: 9,
            scale_range: (0.45, 2.2),
            search_radius: 2.5,
            final_radius: 1.0,
            min_matches: 6,
        }
    }
}

/// The result of [`register`].
#[derive(Clone, Debug)]
pub struct Registration {
    /// Maps source coordinates to reference coordinates.
    pub transform: Transform,
    /// Stars matched in the final fit.
    pub matched: usize,
    /// Root-mean-square distance, in reference pixels, between the mapped
    /// source stars and their matches.
    pub rms: f64,
    /// Matched pairs as `(source index, reference index)`.
    pub pairs: Vec<(usize, usize)>,
}

#[derive(Clone, Copy)]
struct Tri {
    r1: f64,
    r2: f64,
    longest: f64,
    v: [usize; 3],
    ccw: bool,
}

/// Triangles from each star and pairs of its nearest neighbours.
fn triangles(pts: &[Point], k: usize) -> Vec<Tri> {
    let n = pts.len();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 0..n {
        let mut order: Vec<usize> = (0..n).filter(|&j| j != i).collect();
        order.sort_by(|&a, &b| dist(pts[i], pts[a]).total_cmp(&dist(pts[i], pts[b])));
        order.truncate(k);
        for (ai, &a) in order.iter().enumerate() {
            for &b in &order[ai + 1..] {
                let mut key = [i, a, b];
                key.sort_unstable();
                if !seen.insert(key) {
                    continue;
                }
                if let Some(t) = make_tri(pts, [i, a, b]) {
                    out.push(t);
                }
            }
        }
    }
    out
}

fn dist(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn make_tri(pts: &[Point], v: [usize; 3]) -> Option<Tri> {
    let p = [pts[v[0]], pts[v[1]], pts[v[2]]];
    // Side opposite vertex i.
    let side = [dist(p[1], p[2]), dist(p[2], p[0]), dist(p[0], p[1])];
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| side[b].total_cmp(&side[a]));
    let (l1, l2, l3) = (side[idx[0]], side[idx[1]], side[idx[2]]);
    if l1 < 10.0 || l3 / l1 < 0.1 {
        return None;
    }
    let (a, b, c) = (p[idx[0]], p[idx[1]], p[idx[2]]);
    let cross = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    // Too nearly a straight line: the shape is not stable.
    if (cross.abs() / (l1 * l1)) < 0.03 {
        return None;
    }
    Some(Tri {
        r1: l2 / l1,
        r2: l3 / l1,
        longest: l1,
        v: [v[idx[0]], v[idx[1]], v[idx[2]]],
        ccw: cross > 0.0,
    })
}

/// A uniform grid for nearest-neighbour queries within a radius.
struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(pts: &[Point], cell: f64) -> Self {
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, p) in pts.iter().enumerate() {
            cells
                .entry(((p.0 / cell).floor() as i64, (p.1 / cell).floor() as i64))
                .or_default()
                .push(i);
        }
        Self { cell, cells }
    }

    /// The nearest point within `radius` of `q`.
    fn nearest(&self, pts: &[Point], q: Point, radius: f64) -> Option<(usize, f64)> {
        let (cx, cy) = (
            (q.0 / self.cell).floor() as i64,
            (q.1 / self.cell).floor() as i64,
        );
        let reach = (radius / self.cell).ceil() as i64;
        let mut best: Option<(usize, f64)> = None;
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                if let Some(list) = self.cells.get(&(gx, gy)) {
                    for &i in list {
                        let d = dist(pts[i], q);
                        if d <= radius && best.is_none_or(|b| d < b.1) {
                            best = Some((i, d));
                        }
                    }
                }
            }
        }
        best
    }
}

/// Similarity from two point pairs.
fn similarity_from_two(a: (Point, Point), b: (Point, Point)) -> Option<Transform> {
    let (s1, r1) = a;
    let (s2, r2) = b;
    let (ds, dr) = ((s2.0 - s1.0, s2.1 - s1.1), (r2.0 - r1.0, r2.1 - r1.1));
    let den = ds.0 * ds.0 + ds.1 * ds.1;
    if den < 1e-9 {
        return None;
    }
    let ar = (ds.0 * dr.0 + ds.1 * dr.1) / den;
    let ai = (ds.0 * dr.1 - ds.1 * dr.0) / den;
    Some(Transform {
        m: [
            [ar, -ai, r1.0 - ar * s1.0 + ai * s1.1],
            [ai, ar, r1.1 - ai * s1.0 - ar * s1.1],
            [0.0, 0.0, 1.0],
        ],
    })
}

/// Pairs `(source, reference)` where the transformed source star has a
/// reference star within `radius`; each reference star is used once.
fn match_all(
    t: &Transform,
    src: &[Point],
    rf: &[Point],
    grid: &Grid,
    radius: f64,
) -> Vec<(usize, usize, f64)> {
    let mut best: HashMap<usize, (usize, f64)> = HashMap::new();
    for (i, &p) in src.iter().enumerate() {
        let q = t.apply(p);
        if let Some((j, d)) = grid.nearest(rf, q, radius) {
            let e = best.entry(j).or_insert((i, d));
            if d < e.1 {
                *e = (i, d);
            }
        }
    }
    let mut v: Vec<(usize, usize, f64)> = best.into_iter().map(|(j, (i, d))| (i, j, d)).collect();
    v.sort_by_key(|p| p.0);
    v
}

/// Count the star correspondences implied by matching triangles, most voted first.
fn vote(tr: &[Tri], ts: &[Tri]) -> Vec<((usize, usize), u32)> {
    let mut votes: HashMap<(usize, usize), u32> = HashMap::new();
    for t in tr {
        let tol = 0.008 + 0.6 / t.longest;
        let lo = ts.partition_point(|s| s.r1 < t.r1 - 2.0 * tol);
        for s in ts[lo..].iter().take_while(|s| s.r1 <= t.r1 + 2.0 * tol) {
            let tol = tol.min(0.008 + 0.6 / s.longest).max(0.008);
            if (s.r1 - t.r1).abs() <= tol && (s.r2 - t.r2).abs() <= tol && s.ccw == t.ccw {
                for k in 0..3 {
                    *votes.entry((s.v[k], t.v[k])).or_insert(0) += 1;
                }
            }
        }
    }
    let mut cand: Vec<((usize, usize), u32)> = votes.into_iter().filter(|c| c.1 >= 2).collect();
    cand.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    cand.truncate(30);
    cand
}

/// The pattern stars of both frames and what the hypothesis search needs.
struct Search<'a> {
    sr_pts: &'a [Point],
    rf_pts: &'a [Point],
    rf_all: &'a [Point],
    grid: &'a Grid,
    opts: &'a RegisterOptions,
}

/// The best similarity from two voted pairs, scored by matched stars.
fn best_hypothesis(cand: &[((usize, usize), u32)], q: &Search<'_>) -> Option<(usize, Transform)> {
    let (smin, smax) = q.opts.scale_range;
    let mut best: Option<(usize, f64, Transform)> = None;
    for a in 0..cand.len() {
        for b in a + 1..cand.len() {
            let ((sa, ra), (sb, rb)) = (cand[a].0, cand[b].0);
            if sa == sb || ra == rb {
                continue;
            }
            let pa = (q.sr_pts[sa], q.rf_pts[ra]);
            let pb = (q.sr_pts[sb], q.rf_pts[rb]);
            let Some(t) = similarity_from_two(pa, pb) else {
                continue;
            };
            let scale = t.m[0][0].hypot(t.m[1][0]);
            if scale < smin || scale > smax {
                continue;
            }
            let m = match_all(&t, q.sr_pts, q.rf_all, q.grid, q.opts.search_radius);
            let score = m.len();
            let rms = (m.iter().map(|p| p.2 * p.2).sum::<f64>() / score.max(1) as f64).sqrt();
            if best
                .as_ref()
                .is_none_or(|b| score > b.0 || (score == b.0 && rms < b.1))
            {
                best = Some((score, rms, t));
            }
        }
    }
    best.map(|(score, _, t)| (score, t))
}

fn pairs_of(m: &[(usize, usize, f64)], src: &[Point], rf: &[Point]) -> Vec<(Point, Point)> {
    m.iter().map(|&(i, j, _)| (src[i], rf[j])).collect()
}

fn rms_of(m: &[(usize, usize, f64)]) -> f64 {
    (m.iter().map(|p| p.2 * p.2).sum::<f64>() / m.len().max(1) as f64).sqrt()
}

/// Find the transform from `source` stars to `reference` stars.
///
/// # Errors
/// [`Error::NoMatch`] when too few stars are given or no consistent
/// transform is found.
pub fn register(
    reference: &[Star],
    source: &[Star],
    opts: &RegisterOptions,
) -> Result<Registration> {
    let need = opts.min_matches.max(opts.model.min_pairs()).max(4);
    if reference.len() < need || source.len() < need {
        return Err(Error::NoMatch(format!(
            "too few stars to register: {} in the reference, {} in the source (need {need})",
            reference.len(),
            source.len()
        )));
    }
    let rf_all: Vec<Point> = reference.iter().map(|s| (s.x, s.y)).collect();
    let sr_all: Vec<Point> = source.iter().map(|s| (s.x, s.y)).collect();
    // Stars arrive brightest first from `detect`; do not rely on it.
    let top = |stars: &[Star], n: usize| -> Vec<usize> {
        let mut i: Vec<usize> = (0..stars.len()).collect();
        i.sort_by(|&a, &b| stars[b].flux.total_cmp(&stars[a].flux));
        i.truncate(n);
        i
    };
    let rf_pts: Vec<Point> = top(reference, opts.pattern_stars)
        .iter()
        .map(|&i| rf_all[i])
        .collect();
    let sr_pts: Vec<Point> = top(source, opts.pattern_stars)
        .iter()
        .map(|&i| sr_all[i])
        .collect();

    let tr = triangles(&rf_pts, opts.neighbours);
    let mut ts = triangles(&sr_pts, opts.neighbours);
    ts.sort_by(|a, b| a.r1.total_cmp(&b.r1));
    let cand = vote(&tr, &ts);
    if cand.len() < 2 {
        return Err(Error::NoMatch("no star patterns in common".into()));
    }

    let grid = Grid::new(&rf_all, 8.0);
    let search = Search {
        sr_pts: &sr_pts,
        rf_pts: &rf_pts,
        rf_all: &rf_all,
        grid: &grid,
        opts,
    };
    let Some((score, mut t)) = best_hypothesis(&cand, &search) else {
        return Err(Error::NoMatch("no consistent transform".into()));
    };
    if score < opts.min_matches {
        return Err(Error::NoMatch(format!(
            "best hypothesis matches only {score} stars (need {})",
            opts.min_matches
        )));
    }
    let too_few =
        |n: usize, when: &str| Error::NoMatch(format!("only {n} stars match {when} (need {need})"));

    // Refine on all stars, tightening the radius; the last rounds use the
    // requested model.
    let mut radius = opts.search_radius;
    let rounds = 6;
    for round in 0..rounds {
        let m = match_all(&t, &sr_all, &rf_all, &grid, radius);
        if m.len() < need {
            return Err(too_few(m.len(), "after refinement"));
        }
        let model = if round + 2 >= rounds {
            opts.model
        } else {
            Model::Similarity
        };
        t = fit(model, &pairs_of(&m, &sr_all, &rf_all))?;
        radius = (radius * 0.7).max(opts.final_radius.min(opts.search_radius));
    }
    // Final match, clip outliers at three times the scatter, and refit.
    let final_radius = opts.final_radius.max(0.5);
    for _ in 0..3 {
        let mut m = match_all(&t, &sr_all, &rf_all, &grid, final_radius);
        let limit = (3.0 * rms_of(&m)).max(0.15);
        m.retain(|p| p.2 <= limit);
        if m.len() < need {
            return Err(too_few(m.len(), "in the final fit"));
        }
        t = fit(opts.model, &pairs_of(&m, &sr_all, &rf_all))?;
    }
    let m = match_all(&t, &sr_all, &rf_all, &grid, final_radius);
    if m.len() < opts.min_matches {
        return Err(too_few(m.len(), "in the end"));
    }
    Ok(Registration {
        transform: t,
        matched: m.len(),
        rms: rms_of(&m),
        pairs: m.iter().map(|p| (p.0, p.1)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stars::{DetectOptions, detect};
    use crate::testutil::{Rng, gaussian_field};

    type Spec = (f64, f64, f64, f64);

    /// A field of random stars in `w` by `h`.
    fn field(n: usize, w: usize, h: usize, seed: u64) -> Vec<Spec> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| {
                let flux = 3000.0 * (1.0 / (0.04 + rng.uniform())).min(60.0);
                (
                    rng.uniform() * w as f64,
                    rng.uniform() * h as f64,
                    flux,
                    1.5 + rng.uniform() * 0.6,
                )
            })
            .collect()
    }

    fn render(stars: &[Spec], w: usize, h: usize, noise: f64, seed: u64) -> helia_core::Frame {
        let mut f = gaussian_field(w, h, 400.0, stars);
        let mut rng = Rng::new(seed);
        for v in &mut f.data {
            *v += rng.gauss(0.0, noise) as f32;
        }
        f
    }

    /// The truth star list seen through `t` (reference coords to source coords).
    fn transformed(stars: &[Spec], t: &Transform) -> Vec<Spec> {
        stars
            .iter()
            .map(|&(x, y, f, s)| {
                let (nx, ny) = t.apply((x, y));
                (nx, ny, f, s)
            })
            .collect()
    }

    fn stars_of(f: &helia_core::Frame) -> Vec<Star> {
        detect(f, &DetectOptions::default()).stars
    }

    /// RMS distance between where the fitted transform sends the true source
    /// star positions and where those stars really are in the reference.
    fn truth_error(
        reg: &Registration,
        truth_ref: &[Spec],
        to_source: &Transform,
        w: f64,
        h: f64,
    ) -> (f64, f64) {
        let (mut sq, mut worst, mut n) = (0.0, 0.0_f64, 0.0);
        for &(x, y, _, _) in truth_ref {
            let src = to_source.apply((x, y));
            if src.0 < 30.0 || src.1 < 30.0 || src.0 > w - 30.0 || src.1 > h - 30.0 {
                continue;
            }
            let back = reg.transform.apply(src);
            let d = (back.0 - x).hypot(back.1 - y);
            sq += d * d;
            worst = worst.max(d);
            n += 1.0;
        }
        ((sq / n).sqrt(), worst)
    }

    #[test]
    fn recovers_a_shift_and_rotation_to_a_few_hundredths_of_a_pixel() {
        let (w, h) = (1200usize, 900usize);
        let truth = field(180, w, h, 1);
        // The source frame is the sky rotated by 0.8 degrees and shifted.
        let to_source = Transform::rotation_about(600.0, 450.0, 0.8_f64.to_radians(), 13.37, -8.21);
        let reference = render(&truth, w, h, 8.0, 2);
        let source = render(&transformed(&truth, &to_source), w, h, 8.0, 3);
        let reg = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("register");
        let (rms, worst) = truth_error(&reg, &truth, &to_source, w as f64, h as f64);
        assert!(reg.matched >= 60, "matched {}", reg.matched);
        assert!(
            rms < 0.03,
            "registration error rms {rms:.4} px (worst {worst:.4})"
        );
        assert!(worst < 0.1, "worst {worst}");
        assert!(reg.rms < 0.1, "fit rms {}", reg.rms);
    }

    #[test]
    fn warped_stars_land_on_the_reference_stars_to_a_few_hundredths_of_a_pixel() {
        // The end-to-end check: register, resample, then measure the stars again.
        let (w, h) = (1000usize, 800usize);
        let truth = field(160, w, h, 61);
        let to_source =
            Transform::rotation_about(500.0, 400.0, 0.35_f64.to_radians(), 9.41, -17.73);
        let reference = render(&truth, w, h, 8.0, 62);
        let source = render(&transformed(&truth, &to_source), w, h, 8.0, 63);
        let ref_stars = stars_of(&reference);
        let reg = register(&ref_stars, &stars_of(&source), &RegisterOptions::default())
            .expect("register");
        let warped = crate::warp::warp(
            &source,
            &reg.transform,
            w,
            h,
            crate::warp::Interpolation::Lanczos3,
        )
        .expect("warp");
        let now = stars_of(&warped);
        let (mut sq, mut n) = (0.0, 0.0);
        for s in ref_stars
            .iter()
            .filter(|s| s.snr > 30.0 && s.x > 40.0 && s.y > 40.0 && s.x < 960.0 && s.y < 760.0)
        {
            if let Some(m) = now.iter().find(|m| (m.x - s.x).hypot(m.y - s.y) < 2.0) {
                sq += (m.x - s.x).powi(2) + (m.y - s.y).powi(2);
                n += 1.0;
            }
        }
        assert!(n >= 40.0, "only {n} bright stars compared");
        let rms = (sq / n).sqrt();
        assert!(
            rms < 0.05,
            "stars in the warped frame are {rms:.4} px from the reference"
        );
    }

    #[test]
    fn a_meridian_flip_is_just_a_rotation_by_180_degrees() {
        let (w, h) = (900usize, 700usize);
        let truth = field(140, w, h, 7);
        let to_source = Transform::rotation_about(450.0, 350.0, std::f64::consts::PI, 5.5, 3.25);
        let reference = render(&truth, w, h, 8.0, 8);
        let source = render(&transformed(&truth, &to_source), w, h, 8.0, 9);
        let reg = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("register");
        let (rms, _) = truth_error(&reg, &truth, &to_source, w as f64, h as f64);
        assert!(rms < 0.03, "{rms}");
        let (_, angle, _) = reg.transform.decompose();
        assert!((angle.abs() - std::f64::consts::PI).abs() < 1e-3, "{angle}");
    }

    #[test]
    fn a_large_dither_with_partial_overlap_still_registers() {
        let (w, h) = (1000usize, 800usize);
        let big = field(380, w + 500, h + 400, 11);
        // Reference sees the top-left of the sky, the source a window moved by (310, 190).
        let in_ref = |s: &&Spec| s.0 < w as f64 && s.1 < h as f64;
        let ref_truth: Vec<Spec> = big.iter().filter(in_ref).copied().collect();
        let to_source = Transform::similarity(1.0, 0.0, -310.0, -190.0);
        let src_truth: Vec<Spec> = transformed(&big, &to_source)
            .into_iter()
            .filter(|s| s.0 >= 0.0 && s.1 >= 0.0 && s.0 < w as f64 && s.1 < h as f64)
            .collect();
        let reference = render(&ref_truth, w, h, 8.0, 12);
        let source = render(&src_truth, w, h, 8.0, 13);
        let reg = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("register");
        let (rms, _) = truth_error(&reg, &ref_truth, &to_source, w as f64, h as f64);
        assert!(rms < 0.04, "{rms}");
        let (_, _, (tx, ty)) = reg.transform.decompose();
        assert!(
            (tx - 310.0).abs() < 0.1 && (ty - 190.0).abs() < 0.1,
            "{tx} {ty}"
        );
    }

    #[test]
    fn spurious_and_missing_stars_do_not_fool_it() {
        let (w, h) = (900usize, 700usize);
        let truth = field(160, w, h, 21);
        let to_source = Transform::rotation_about(450.0, 350.0, -0.4_f64.to_radians(), -22.7, 14.1);
        let mut src = transformed(&truth, &to_source);
        // Drop a fifth of the stars and add a third as many that do not exist in the reference.
        let mut rng = Rng::new(22);
        src.retain(|_| rng.uniform() > 0.2);
        src.extend(field(50, w, h, 23));
        let reference = render(&truth, w, h, 8.0, 24);
        let source = render(&src, w, h, 8.0, 25);
        let reg = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("register");
        let (rms, _) = truth_error(&reg, &truth, &to_source, w as f64, h as f64);
        assert!(rms < 0.04, "{rms}");
    }

    #[test]
    fn a_frame_at_half_scale_registers_with_the_scale_recovered() {
        let (w, h) = (1000usize, 800usize);
        let truth = field(200, w, h, 31);
        // A 2x2-binned frame: half the size, stars half as wide.
        let to_source = Transform::similarity(0.5, 0.0, 0.0, 0.0);
        let src: Vec<Spec> = transformed(&truth, &to_source)
            .into_iter()
            .map(|(x, y, f, s)| (x, y, f, (s / 2.0).max(1.1)))
            .collect();
        let reference = render(&truth, w, h, 8.0, 32);
        let source = render(&src, w / 2, h / 2, 8.0, 33);
        let reg = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("register");
        let (scale, _, _) = reg.transform.decompose();
        assert!((scale - 2.0).abs() < 0.01, "scale {scale}");
    }

    #[test]
    fn affine_model_recovers_a_small_shear() {
        let (w, h) = (900usize, 700usize);
        let truth = field(200, w, h, 41);
        let to_source = Transform {
            m: [
                [1.0005, 0.0012, 4.2],
                [-0.0008, 0.9996, -6.6],
                [0.0, 0.0, 1.0],
            ],
        };
        let reference = render(&truth, w, h, 6.0, 42);
        let source = render(&transformed(&truth, &to_source), w, h, 6.0, 43);
        let sim = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions::default(),
        )
        .expect("similarity");
        let aff = register(
            &stars_of(&reference),
            &stars_of(&source),
            &RegisterOptions {
                model: Model::Affine,
                ..RegisterOptions::default()
            },
        )
        .expect("affine");
        let (e_sim, _) = truth_error(&sim, &truth, &to_source, w as f64, h as f64);
        let (e_aff, _) = truth_error(&aff, &truth, &to_source, w as f64, h as f64);
        assert!(e_aff < 0.04, "affine error {e_aff}");
        assert!(
            e_aff < e_sim,
            "affine {e_aff} should beat similarity {e_sim} on a sheared frame"
        );
    }

    #[test]
    fn unrelated_fields_do_not_register() {
        let (w, h) = (700usize, 500usize);
        let a = render(&field(120, w, h, 51), w, h, 8.0, 52);
        let b = render(&field(120, w, h, 53), w, h, 8.0, 54);
        let r = register(&stars_of(&a), &stars_of(&b), &RegisterOptions::default());
        assert!(matches!(r, Err(Error::NoMatch(_))), "{r:?}");
    }

    #[test]
    fn too_few_stars_is_an_error() {
        let few: Vec<Star> = Vec::new();
        assert!(matches!(
            register(&few, &few, &RegisterOptions::default()),
            Err(Error::NoMatch(_))
        ));
    }
}
