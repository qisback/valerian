// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The full-resolution picture: an image pyramid built on a worker thread and
//! drawn as stretched tiles on top of the 1600 px preview.
//!
//! The preview texture is always drawn first, so the window is never empty
//! while tiles are made. Where the zoom needs more than the preview holds,
//! the tiles of the pyramid level that suits the zoom
//! ([`valerian_core::pyramid::level_for_zoom`]) that cover the window are
//! stretched with the live stretch, tinted when night vision is on, uploaded
//! as textures and kept in a least-recently-used cache bounded in bytes.
//! The geometry and the cache are in `valerian_core::pyramid`.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Context, Painter, Rect, TextureFilter, TextureHandle,
    TextureOptions, pos2, vec2,
};
use valerian_core::fits::Image;
use valerian_core::preview::rgba8;
use valerian_core::pyramid::{
    Pyramid, TileCache, TileKey, level_for_zoom, tile_rect, visible_tiles,
};

use crate::stretch_tool::{Measured, StretchTool};

/// Bytes of tile textures kept (RGBA, so 1 MiB a full tile): about 250 tiles,
/// several screens of 4K.
pub const CACHE_BYTES: usize = 256 << 20;

/// Longest a frame spends making tiles before it draws what it has and asks
/// for another frame; one tile is always made so progress is certain.
const FRAME_BUDGET: Duration = Duration::from_millis(12);

/// What building the pyramid took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildStats {
    /// Levels, the full image included.
    pub levels: usize,
    /// Seconds to build.
    pub secs: f32,
    /// Bytes the coarser levels hold.
    pub extra_bytes: usize,
}

enum Build {
    None,
    Building(Receiver<(Pyramid, f32)>),
    Ready(Arc<Pyramid>),
}

/// The pyramid of the open image and the tiles made from it.
pub struct Tiles {
    build: Build,
    cache: TileCache<TileKey, TextureHandle>,
    night: bool,
    /// Tiles made since the pyramid was ready, for the tests and the figures.
    pub made: usize,
}

impl Default for Tiles {
    fn default() -> Self {
        Self {
            build: Build::None,
            cache: TileCache::new(CACHE_BYTES),
            night: false,
            made: 0,
        }
    }
}

/// Red only, from the luma, so the picture does not spoil dark-adapted eyes.
pub fn night_tint(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let luma =
            (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
                .round() as u8;
        px[0] = luma;
        px[1] = 0;
        px[2] = 0;
    }
}

impl Tiles {
    /// Start the pyramid of `full`. A picture no bigger than `small_side` is
    /// built at once (there is almost nothing to build); a bigger one on a
    /// worker thread.
    pub fn start(&mut self, ctx: &Context, full: &Arc<Image>, small_side: usize) {
        self.cache.clear();
        self.made = 0;
        if full.width().max(full.height()) <= small_side {
            self.build = Build::Ready(Arc::new(Pyramid::build(Arc::clone(full))));
            return;
        }
        let (tx, rx) = channel();
        let (full, ctx) = (Arc::clone(full), ctx.clone());
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let p = Pyramid::build(full);
            let _ = tx.send((p, t0.elapsed().as_secs_f32()));
            ctx.request_repaint();
        });
        self.build = Build::Building(rx);
    }

    /// Forget the pyramid (no picture is open).
    pub fn clear(&mut self) {
        self.build = Build::None;
        self.cache.clear();
    }

    /// Whether the pyramid is being built.
    #[must_use]
    pub fn building(&self) -> bool {
        matches!(self.build, Build::Building(_))
    }

    /// The pyramid, once built.
    #[cfg(test)]
    #[must_use]
    pub fn pyramid(&self) -> Option<&Arc<Pyramid>> {
        match &self.build {
            Build::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// Take a finished build off the worker; its figures, once.
    pub fn poll(&mut self) -> Option<BuildStats> {
        let Build::Building(rx) = &self.build else {
            return None;
        };
        match rx.try_recv() {
            Ok((p, secs)) => {
                let stats = stats_of(&p, secs);
                self.build = Build::Ready(Arc::new(p));
                Some(stats)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.build = Build::None;
                None
            }
        }
    }

    /// Throw away the tiles: the stretch or the palette changed.
    pub fn invalidate(&mut self) {
        self.cache.clear();
    }

    /// Bytes of tile textures held.
    #[cfg(test)]
    #[must_use]
    pub fn cached_bytes(&self) -> usize {
        self.cache.bytes()
    }

    /// Draw the tiles over the picture. Returns whether tiles are still
    /// missing, so the caller can ask for another frame.
    pub fn draw(&mut self, ctx: &Context, painter: &Painter, scene: &Scene<'_>) -> bool {
        let Build::Ready(pyr) = &self.build else {
            return false;
        };
        let pyr = Arc::clone(pyr);
        let Scene {
            rect, clip, full, ..
        } = *scene;
        let scale = rect.width() / full.0 as f32;
        if scale <= scene.preview_ratio * 1.0001 {
            return false;
        }
        if scene.night != self.night {
            self.night = scene.night;
            self.cache.clear();
        }
        let level = level_for_zoom(scale, pyr.count());
        let step = (1_usize << level) as f32;
        let (lw, lh) = pyr.size(level);
        // The visible part of the picture, in pixels of the level.
        let seen = rect.intersect(clip);
        if seen.width() <= 0.0 || seen.height() <= 0.0 {
            return false;
        }
        let to_level = |p: egui::Pos2| ((p - rect.min) / (scale * step)).to_pos2();
        let (lo, hi) = (to_level(seen.min), to_level(seen.max));
        let started = Instant::now();
        let mut missing = false;
        let mut built = 0;
        for key in visible_tiles(level, lw, lh, [lo.x, lo.y, hi.x, hi.y]) {
            if !self.cache.contains(&key) {
                if built > 0 && started.elapsed() > FRAME_BUDGET {
                    missing = true;
                    continue;
                }
                match make(ctx, &pyr, key, scene) {
                    Some((tex, bytes)) => self.cache.insert(key, tex, bytes),
                    None => continue,
                }
                built += 1;
                self.made += 1;
            }
            let Some(tex) = self.cache.get(&key) else {
                continue;
            };
            let r = tile_rect(key, lw, lh);
            // The coarse levels round up, so the last tile can reach past the
            // picture: draw only the part inside it.
            let (x0, y0) = (r.x as f32 * step, r.y as f32 * step);
            let x1 = ((r.x + r.w) as f32 * step).min(full.0 as f32);
            let y1 = ((r.y + r.h) as f32 * step).min(full.1 as f32);
            let dest = Rect::from_min_max(
                rect.min + vec2(x0, y0) * scale,
                rect.min + vec2(x1, y1) * scale,
            );
            let uv = Rect::from_min_max(
                pos2(0.0, 0.0),
                pos2((x1 - x0) / step / r.w as f32, (y1 - y0) / step / r.h as f32),
            );
            painter.image(tex.id(), dest, uv, Color32::WHITE);
        }
        if built > 0 || missing {
            ctx.request_repaint();
        }
        missing
    }
}

/// What a frame draws tiles for.
pub struct Scene<'a> {
    /// Where the whole picture is drawn at the current zoom.
    pub rect: Rect,
    /// The part of the window it is clipped to.
    pub clip: Rect,
    /// Size of the picture in its own pixels.
    pub full: (usize, usize),
    /// The preview's pixels per picture pixel: tiles are only drawn where the
    /// zoom goes beyond what the preview holds.
    pub preview_ratio: f32,
    /// The stretch.
    pub tool: &'a StretchTool,
    /// The levels it was measured at.
    pub measured: &'a Measured,
    /// Night vision is on.
    pub night: bool,
}

fn stats_of(p: &Pyramid, secs: f32) -> BuildStats {
    BuildStats {
        levels: p.count(),
        secs,
        extra_bytes: p.extra_bytes(),
    }
}

/// Stretch, tint and upload one tile; its texture and the bytes it takes.
fn make(
    ctx: &Context,
    pyr: &Pyramid,
    key: TileKey,
    scene: &Scene<'_>,
) -> Option<(TextureHandle, usize)> {
    let raw = pyr.tile(key);
    let (w, h) = (raw.width(), raw.height());
    if w == 0 || h == 0 {
        return None;
    }
    let shown = scene.tool.render(&raw, scene.measured).ok()?;
    let mut rgba = rgba8(&shown);
    if scene.night {
        night_tint(&mut rgba);
    }
    let image = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    // Real pixels stay square when magnified; the coarser levels are smooth.
    let options = if key.level == 0 {
        TextureOptions {
            magnification: TextureFilter::Nearest,
            ..TextureOptions::LINEAR
        }
    } else {
        TextureOptions::LINEAR
    };
    let name = format!("tile-{}-{}-{}", key.level, key.tx, key.ty);
    Some((ctx.load_texture(name, image, options), w * h * 4))
}

/// Tiles that cover a window of `view` screen pixels at most (one more than
/// the exact count each way, for a window that straddles tile edges).
#[cfg(test)]
#[must_use]
pub fn tiles_for_window(view: (f32, f32)) -> usize {
    let n = |s: f32| (s / valerian_core::pyramid::TILE as f32).ceil() as usize + 1;
    n(view.0) * n(view.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_vision_keeps_only_red_from_the_luma() {
        let mut px = [255, 255, 255, 255, 0, 0, 255, 255];
        night_tint(&mut px);
        assert_eq!(px, [255, 0, 0, 255, 18, 0, 0, 255]);
    }

    #[test]
    fn a_tile_is_stretched_like_the_same_pixels_of_the_whole() {
        use valerian_core::fits::{Header, Image};
        use valerian_core::pyramid::{PixelRect, crop};
        use valerian_core::testutil::deep_sky;
        let sky = deep_sky(700, 520, 120, 3);
        let img = Image::mono(sky.frame, Header::new());
        let tool = StretchTool::default();
        let m = tool.measure(&img, None).expect("levels");
        let whole = tool.render(&img, &m).expect("whole");
        let r = PixelRect {
            x: 100,
            y: 37,
            w: 300,
            h: 211,
        };
        let tile = tool.render(&crop(&img, r), &m).expect("tile");
        for y in 0..r.h {
            for x in 0..r.w {
                let a = tile.planes[0].data[y * r.w + x];
                let b = whole.planes[0].data[(r.y + y) * 700 + r.x + x];
                assert!((a - b).abs() < 1e-6, "{x},{y}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn a_window_needs_a_bounded_number_of_tiles() {
        assert_eq!(tiles_for_window((1000.0, 600.0)), 3 * 3);
        assert!((tiles_for_window((3840.0, 2160.0)) << 20) < CACHE_BYTES);
    }
}
