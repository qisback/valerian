// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! An image pyramid and the tiling arithmetic a viewer needs to show it.
//!
//! Level 0 is the image itself; level `k` has every block of `2^k x 2^k`
//! pixels averaged into one (the mean of the finite pixels), down to a level
//! that fits in one [`TILE`]. A viewer picks the level from its zoom with
//! [`level_for_zoom`], asks [`visible_tiles`] which [`TILE`]-sized pieces of
//! that level cover the window, cuts them out with [`Pyramid::tile`], and
//! keeps what it made in a [`TileCache`] that is bounded in bytes.
//!
//! Nothing here knows about a window or a graphics library, so the geometry
//! and the cache are tested on their own.
//!
//! # References
//!
//! - Williams, L. (1983), Pyramidal parametrics, ACM SIGGRAPH Computer Graphics 17(3), 1-11, doi:10.1145/964967.801126 (the mip-map).
//! - Burt, P. J. and Adelson, E. H. (1983), The Laplacian pyramid as a compact image code, IEEE Trans. Commun. 31, 532-540, doi:10.1109/TCOM.1983.1095851.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use rayon::prelude::*;

use crate::Frame;
use crate::fits::{Header, Image};

/// Side of a tile, in pixels of its level.
pub const TILE: usize = 512;

/// The levels of an image, finest first.
#[derive(Clone, Debug)]
pub struct Pyramid {
    levels: Vec<Arc<Image>>,
}

/// Which tile: a level and the column and row of the tile in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Level, 0 for the full resolution.
    pub level: usize,
    /// Column of the tile.
    pub tx: usize,
    /// Row of the tile.
    pub ty: usize,
}

/// A pixel rectangle of one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    /// Left column.
    pub x: usize,
    /// Top row.
    pub y: usize,
    /// Width (less than [`TILE`] on the right edge).
    pub w: usize,
    /// Height (less than [`TILE`] on the bottom edge).
    pub h: usize,
}

/// Halve `image` once: each output pixel is the mean of the finite pixels of a
/// 2 x 2 block (fewer at an odd edge), NaN when there are none. Sizes round
/// up, so no pixel is lost. The header is kept.
#[must_use]
pub fn halve(image: &Image) -> Image {
    let planes = image
        .planes
        .iter()
        .map(|p| {
            let (w, h) = (p.width, p.height);
            let (ow, oh) = (w.div_ceil(2), h.div_ceil(2));
            let mut data = vec![0.0_f32; ow * oh];
            data.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
                let rows = (2 * oy)..(2 * oy + 2).min(h);
                for (ox, out) in row.iter_mut().enumerate() {
                    let (mut sum, mut n) = (0.0_f64, 0_u32);
                    for y in rows.clone() {
                        for &v in &p.data[y * w + 2 * ox..y * w + (2 * ox + 2).min(w)] {
                            if v.is_finite() {
                                sum += f64::from(v);
                                n += 1;
                            }
                        }
                    }
                    *out = if n == 0 {
                        f32::NAN
                    } else {
                        (sum / f64::from(n)) as f32
                    };
                }
            });
            Frame {
                width: ow,
                height: oh,
                data,
                bayer: None,
            }
        })
        .collect();
    Image {
        header: image.header.clone(),
        planes,
    }
}

impl Pyramid {
    /// Build the levels of `full`, halving until a level fits in one tile.
    #[must_use]
    pub fn build(full: Arc<Image>) -> Self {
        let mut levels = vec![full];
        while let Some(last) = levels.last() {
            if last.width().max(last.height()) <= TILE {
                break;
            }
            let next = halve(last);
            levels.push(Arc::new(next));
        }
        Self { levels }
    }

    /// Number of levels, at least 1.
    #[must_use]
    pub fn count(&self) -> usize {
        self.levels.len()
    }

    /// The image of `level` (clamped to the coarsest).
    #[must_use]
    pub fn level(&self, level: usize) -> &Image {
        &self.levels[level.min(self.levels.len() - 1)]
    }

    /// Size in pixels of `level`.
    #[must_use]
    pub fn size(&self, level: usize) -> (usize, usize) {
        let l = self.level(level);
        (l.width(), l.height())
    }

    /// Bytes held by the levels beyond the full image, which is shared with
    /// the caller and not counted.
    #[must_use]
    pub fn extra_bytes(&self) -> usize {
        self.levels
            .iter()
            .skip(1)
            .flat_map(|l| l.planes.iter())
            .map(|p| p.data.len() * size_of::<f32>())
            .sum()
    }

    /// The pixels of `key` as an image of its own (the edge tiles are smaller
    /// than [`TILE`]). The header is empty: a tile is for display.
    #[must_use]
    pub fn tile(&self, key: TileKey) -> Image {
        let img = self.level(key.level);
        let r = tile_rect(key, img.width(), img.height());
        crop(img, r)
    }
}

/// The part of `img` inside `r`, with an empty header and no Bayer pattern.
#[must_use]
pub fn crop(img: &Image, r: PixelRect) -> Image {
    let planes = img
        .planes
        .iter()
        .map(|p| {
            let mut data = Vec::with_capacity(r.w * r.h);
            for y in r.y..r.y + r.h {
                data.extend_from_slice(&p.data[y * p.width + r.x..y * p.width + r.x + r.w]);
            }
            Frame {
                width: r.w,
                height: r.h,
                data,
                bayer: None,
            }
        })
        .collect();
    Image {
        header: Header::default(),
        planes,
    }
}

/// The level to draw at `zoom` screen pixels per full-resolution pixel: the
/// coarsest whose pixels are still at least one screen pixel across, so a
/// picture is never drawn from less detail than the screen can show. 1 and
/// above is level 0 (the real pixels); `levels` is the number there are.
#[must_use]
pub fn level_for_zoom(zoom: f32, levels: usize) -> usize {
    if zoom.is_nan() || zoom <= 0.0 || zoom >= 1.0 {
        // Not positive (or NaN) is treated as the finest level.
        return 0;
    }
    // A level is usable while 2^k * zoom is still at least one; the small
    // epsilon keeps an exact 1/2^k zoom on the level it names.
    let k = ((1.0 / zoom).log2() + 1e-4).floor();
    (k as usize).min(levels.saturating_sub(1))
}

/// Number of tile columns and rows of a level of `w x h` pixels.
#[must_use]
pub fn tile_grid(w: usize, h: usize) -> (usize, usize) {
    (w.div_ceil(TILE), h.div_ceil(TILE))
}

/// The pixels `key` covers in a level of `w x h` pixels, clipped to the level.
#[must_use]
pub fn tile_rect(key: TileKey, w: usize, h: usize) -> PixelRect {
    let (x, y) = (key.tx * TILE, key.ty * TILE);
    PixelRect {
        x,
        y,
        w: TILE.min(w.saturating_sub(x)),
        h: TILE.min(h.saturating_sub(y)),
    }
}

/// The tiles of `level` that cover `region` (`[left, top, right, bottom]` in
/// pixels of that level, which may reach outside it), nearest the centre of
/// the region first so the middle of the window fills in before its edges.
#[must_use]
pub fn visible_tiles(level: usize, w: usize, h: usize, region: [f32; 4]) -> Vec<TileKey> {
    let (cols, rows) = tile_grid(w, h);
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let [l, t, r, b] = region;
    if region.iter().any(|v| v.is_nan()) || r <= 0.0 || b <= 0.0 || l >= w as f32 || t >= h as f32 {
        return Vec::new();
    }
    let span = |lo: f32, hi: f32, n: usize| {
        let first = (lo.max(0.0) / TILE as f32).floor() as usize;
        // The right edge is exclusive: a region ending on a tile boundary
        // does not need the next tile.
        let last = (((hi.max(0.0) / TILE as f32).ceil() as usize).max(1) - 1).min(n - 1);
        first.min(n - 1)..=last
    };
    let (cx, cy) = (
        f32::midpoint(l, r) / TILE as f32 - 0.5,
        f32::midpoint(t, b) / TILE as f32 - 0.5,
    );
    let mut keys: Vec<TileKey> = span(t, b, rows)
        .flat_map(|ty| span(l, r, cols).map(move |tx| TileKey { level, tx, ty }))
        .collect();
    keys.sort_by(|a, b| {
        let d = |k: &TileKey| (k.tx as f32 - cx).powi(2) + (k.ty as f32 - cy).powi(2);
        d(a).total_cmp(&d(b)).then(a.cmp(b))
    });
    keys
}

/// A least-recently-used cache bounded by the bytes its entries are said to
/// take. An entry that is itself larger than the bound is kept alone.
#[derive(Debug)]
pub struct TileCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    budget: usize,
    bytes: usize,
    tick: u64,
}

#[derive(Debug)]
struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

impl<K: Eq + Hash + Copy, V> TileCache<K, V> {
    /// An empty cache that holds up to `budget` bytes.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            bytes: 0,
            tick: 0,
        }
    }

    /// The entry for `key`, marking it as just used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|e| {
            e.used = tick;
            &e.value
        })
    }

    /// Whether `key` is held, without marking it used.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Add `value`, which takes `bytes`, then drop the least recently used
    /// entries until the cache is within its budget again (never the entry
    /// just added).
    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.tick += 1;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                used: self.tick,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.bytes -= gone.bytes;
            }
        }
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Bytes held.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Image {
        let data = (0..w * h).map(|i| i as f32).collect();
        Image::mono(
            Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            },
            Header::default(),
        )
    }

    #[test]
    fn the_level_follows_the_zoom() {
        assert_eq!(level_for_zoom(8.0, 5), 0);
        assert_eq!(level_for_zoom(1.0, 5), 0);
        assert_eq!(level_for_zoom(0.75, 5), 0);
        assert_eq!(level_for_zoom(0.5, 5), 1);
        assert_eq!(level_for_zoom(0.3, 5), 1);
        assert_eq!(level_for_zoom(0.25, 5), 2);
        assert_eq!(level_for_zoom(0.13, 5), 2);
        assert_eq!(level_for_zoom(0.125, 5), 3);
        // Past the coarsest level it stays on it; bad zooms use the finest.
        assert_eq!(level_for_zoom(0.001, 5), 4);
        assert_eq!(level_for_zoom(0.0, 5), 0);
        assert_eq!(level_for_zoom(f32::NAN, 5), 0);
        assert_eq!(level_for_zoom(0.1, 1), 0);
    }

    #[test]
    fn the_levels_halve_until_one_tile_holds_them() {
        let p = Pyramid::build(Arc::new(ramp(4000, 3000)));
        let sizes: Vec<_> = (0..p.count()).map(|k| p.size(k)).collect();
        assert_eq!(sizes, [(4000, 3000), (2000, 1500), (1000, 750), (500, 375)]);
        assert_eq!(Pyramid::build(Arc::new(ramp(512, 100))).count(), 1);
        assert_eq!(Pyramid::build(Arc::new(ramp(513, 1))).count(), 2);
    }

    #[test]
    fn odd_sizes_round_up_and_lose_no_pixel() {
        let h = halve(&ramp(5, 3));
        assert_eq!((h.width(), h.height()), (3, 2));
        let d = &h.planes[0].data;
        // First block is pixels 0, 1, 5, 6.
        assert!((d[0] - (0.0 + 1.0 + 5.0 + 6.0) / 4.0).abs() < 1e-6);
        // The last column has one wide block, the last row one high: pixel 14.
        assert!((d[5] - 14.0).abs() < 1e-6);
        // Right edge of the first row: pixels 4 and 9.
        assert!((d[2] - 6.5).abs() < 1e-6);
    }

    #[test]
    fn halving_skips_unusable_pixels() {
        let mut img = ramp(2, 2);
        img.planes[0].data = vec![f32::NAN, 2.0, 4.0, f32::INFINITY];
        assert!((halve(&img).planes[0].data[0] - 3.0).abs() < 1e-6);
        img.planes[0].data = vec![f32::NAN; 4];
        assert!(halve(&img).planes[0].data[0].is_nan());
    }

    #[test]
    fn every_pixel_is_in_exactly_one_tile() {
        let (w, h) = (1300, 1030);
        let (cols, rows) = tile_grid(w, h);
        assert_eq!((cols, rows), (3, 3));
        let mut seen = vec![0_u8; w * h];
        for ty in 0..rows {
            for tx in 0..cols {
                let r = tile_rect(TileKey { level: 0, tx, ty }, w, h);
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        seen[y * w + x] += 1;
                    }
                }
            }
        }
        assert!(seen.iter().all(|&n| n == 1));
    }

    #[test]
    fn edge_tiles_are_smaller() {
        let r = tile_rect(
            TileKey {
                level: 0,
                tx: 2,
                ty: 2,
            },
            1300,
            1030,
        );
        assert_eq!(
            r,
            PixelRect {
                x: 1024,
                y: 1024,
                w: 276,
                h: 6
            }
        );
        let full = tile_rect(
            TileKey {
                level: 0,
                tx: 0,
                ty: 0,
            },
            1300,
            1030,
        );
        assert_eq!((full.w, full.h), (TILE, TILE));
        // A tile outside the level is empty rather than underflowing.
        let out = tile_rect(
            TileKey {
                level: 0,
                tx: 9,
                ty: 0,
            },
            1300,
            1030,
        );
        assert_eq!(out.w, 0);
    }

    #[test]
    fn a_tile_holds_the_right_pixels() {
        let p = Pyramid::build(Arc::new(ramp(700, 600)));
        let t = p.tile(TileKey {
            level: 0,
            tx: 1,
            ty: 1,
        });
        assert_eq!((t.width(), t.height()), (188, 88));
        assert!((t.planes[0].data[0] - (512 * 700 + 512) as f32).abs() < 1e-3);
        let last = t.planes[0].data.len() - 1;
        assert!((t.planes[0].data[last] - (700 * 600 - 1) as f32).abs() < 1e-3);
    }

    #[test]
    fn the_visible_tiles_cover_the_window_and_nothing_more() {
        let (w, h) = (2000, 1500);
        // Inside one tile.
        let one = visible_tiles(0, w, h, [10.0, 10.0, 100.0, 100.0]);
        assert_eq!(
            one,
            [TileKey {
                level: 0,
                tx: 0,
                ty: 0
            }]
        );
        // A region ending exactly on a boundary does not take the next tile.
        let exact = visible_tiles(0, w, h, [0.0, 0.0, 512.0, 512.0]);
        assert_eq!(exact.len(), 1);
        let across = visible_tiles(0, w, h, [500.0, 500.0, 520.0, 520.0]);
        assert_eq!(across.len(), 4);
        // A region larger than the level is every tile.
        let all = visible_tiles(0, w, h, [-100.0, -100.0, 5000.0, 5000.0]);
        assert_eq!(all.len(), 4 * 3);
        // Entirely outside is none.
        assert_eq!(visible_tiles(0, w, h, [-50.0, 0.0, -1.0, 100.0]).len(), 0);
        assert_eq!(
            visible_tiles(0, w, h, [2000.0, 0.0, 2100.0, 100.0]).len(),
            0
        );
        assert_eq!(visible_tiles(0, 0, 0, [0.0, 0.0, 10.0, 10.0]).len(), 0);
    }

    #[test]
    fn the_middle_of_the_window_comes_first() {
        let keys = visible_tiles(0, 2000, 1500, [0.0, 0.0, 2000.0, 1500.0]);
        let first = keys[0];
        assert!(matches!((first.tx, first.ty), (1 | 2, 1)), "{first:?}");
        let corner = keys.last().copied().unwrap_or(first);
        assert!(corner.tx != 1 && corner.tx != 2);
    }

    #[test]
    fn the_cache_stays_within_its_budget_and_drops_the_oldest() {
        let mut c: TileCache<u32, &str> = TileCache::new(100);
        c.insert(1, "a", 40);
        c.insert(2, "b", 40);
        // Touch 1 so that 2 is the oldest.
        assert!(c.get(&1).is_some());
        c.insert(3, "c", 40);
        assert_eq!(c.bytes(), 80);
        assert!(c.contains(&1) && !c.contains(&2) && c.contains(&3));
        // Never over budget after an insert, whatever the sizes.
        for k in 10..60 {
            c.insert(k, "x", 30);
            assert!(c.bytes() <= 100, "{}", c.bytes());
        }
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn replacing_an_entry_does_not_double_count_it() {
        let mut c: TileCache<u32, u8> = TileCache::new(1000);
        c.insert(1, 1, 300);
        c.insert(1, 2, 200);
        assert_eq!((c.len(), c.bytes()), (1, 200));
        assert_eq!(c.get(&1), Some(&2));
        c.clear();
        assert!(c.is_empty() && c.bytes() == 0);
    }

    #[test]
    fn an_entry_bigger_than_the_budget_is_kept_alone() {
        let mut c: TileCache<u32, u8> = TileCache::new(10);
        c.insert(1, 1, 5);
        c.insert(2, 2, 50);
        assert_eq!(c.len(), 1);
        assert!(c.contains(&2));
    }

    #[test]
    fn the_extra_memory_is_about_a_third_of_the_image() {
        let p = Pyramid::build(Arc::new(ramp(4096, 4096)));
        let base = 4096 * 4096 * 4;
        let extra = p.extra_bytes();
        assert!(extra > base / 4 && extra < base / 3 + base / 50, "{extra}");
    }
}
