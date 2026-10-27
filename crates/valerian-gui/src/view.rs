// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The image view: zoom, pan and the histogram of what is shown.
//!
//! Zoom is in screen pixels per pixel of the open image (100 % is one image
//! pixel per screen pixel, so 1:1 shows the real pixels; the pyramid in
//! `tiles` supplies them). The wheel zooms about the pointer, a drag pans, the
//! 1 key zooms to 1:1, a double click or the Fit button fits
//! the picture to the window.

use eframe::egui::{
    self, Align2, Color32, ColorImage, Mesh, Pos2, Rect, Sense, Shape, Stroke, TextureId, Ui, Vec2,
    pos2, vec2,
};

use crate::theme::{is_night, mono, palette, semibold};

/// Smallest and largest zoom.
pub const ZOOM_RANGE: (f32, f32) = (0.02, 32.0);

/// Zoom and pan of the picture.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Screen pixels per preview pixel; `None` fits the picture.
    pub zoom: Option<f32>,
    /// Offset of the picture's centre from the centre of the view, in screen
    /// pixels.
    pub pan: Vec2,
}

impl Default for View {
    fn default() -> Self {
        Self::FIT
    }
}

impl View {
    /// The picture fitted to the window.
    pub const FIT: Self = Self {
        zoom: None,
        pan: Vec2::ZERO,
    };

    /// The zoom in force for a picture of `tex` in a view of `avail`.
    #[must_use]
    pub fn scale(&self, tex: Vec2, avail: Vec2) -> f32 {
        self.zoom.unwrap_or_else(|| fit_scale(tex, avail))
    }

    /// Zoom by `factor` about `anchor` (a point of the view, from its centre).
    pub fn zoom_by(&mut self, factor: f32, anchor: Vec2, tex: Vec2, avail: Vec2) {
        let old = self.scale(tex, avail);
        let new = (old * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        // Keep the picture point under the anchor where it is.
        self.pan = anchor + (self.pan - anchor) * (new / old);
        self.zoom = Some(new);
    }

    #[cfg(test)]
    /// Zoom to `zoom` with the image pixel `(x, y)` at the middle of the view.
    pub fn centre_on(&mut self, zoom: f32, x: f32, y: f32, tex: Vec2) {
        let zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        self.zoom = Some(zoom);
        self.pan = vec2(tex.x / 2.0 - x, tex.y / 2.0 - y) * zoom;
    }

    /// Fit the picture to the window again.
    pub fn fit(&mut self) {
        *self = Self::FIT;
    }
}

/// The scale at which `tex` just fits `avail`, never above 1.
#[must_use]
pub fn fit_scale(tex: Vec2, avail: Vec2) -> f32 {
    (avail.x / tex.x).min(avail.y / tex.y).clamp(0.01, 1.0)
}

/// The zoom buttons and the percentage; returns what was pressed.
pub fn toolbar_buttons(ui: &mut Ui, view: &mut View, tex: Vec2, avail: Vec2) {
    let p = palette();
    let pct = view.scale(tex, avail) * 100.0;
    let fitted = view.zoom.is_none();
    let out = |ui: &mut Ui, text: &str, tip: &str, on: bool| {
        let r = ui.add_sized(
            [if text.len() > 2 { 60.0 } else { 34.0 }, 32.0],
            egui::Button::new(egui::RichText::new(text).font(semibold(14.0)).color(if on {
                p.accent_hover
            } else {
                p.text
            }))
            .fill(if on { p.selected_fill } else { p.raised })
            .stroke(Stroke::new(
                1.0,
                if on {
                    p.selected_border
                } else {
                    p.border_strong
                },
            )),
        );
        r.on_hover_text(tip).clicked()
    };
    // Right to left, so the order here is the reverse of the order on screen.
    if out(ui, "+", "Zoom in (scroll up over the picture)", false) {
        view.zoom_by(1.25, Vec2::ZERO, tex, avail);
    }
    if out(
        ui,
        &format!("{pct:.0}%"),
        "Fit the picture to the window (or double click it)",
        fitted,
    ) {
        view.fit();
    }
    if out(ui, "-", "Zoom out (scroll down over the picture)", false) {
        view.zoom_by(0.8, Vec2::ZERO, tex, avail);
    }
}

/// Draw `tex` in the free space of `ui` with the zoom and pan of `view`,
/// handling the wheel, drags and double clicks. Returns the rectangle the
/// picture was drawn in and the rectangle of the view it is clipped to.
pub fn show(ui: &mut Ui, view: &mut View, tex: TextureId, tex_size: Vec2) -> (Rect, Rect) {
    let avail_rect = ui.available_rect_before_wrap();
    let avail = avail_rect.size();
    let resp = ui.allocate_rect(avail_rect, Sense::click_and_drag());
    if resp.double_clicked() {
        view.fit();
    }
    if resp.dragged() {
        view.pan += resp.drag_delta();
        if view.zoom.is_none() {
            // Dragging a fitted picture starts at the fitted zoom.
            view.zoom = Some(view.scale(tex_size, avail));
        }
    }
    if resp.hovered() {
        let (scroll, pinch, at) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.pointer.hover_pos(),
            )
        });
        let anchor = at.map_or(Vec2::ZERO, |a| a - avail_rect.center());
        if (pinch - 1.0).abs() > f32::EPSILON {
            view.zoom_by(pinch, anchor, tex_size, avail);
        } else if scroll != 0.0 {
            view.zoom_by((scroll * 0.004).exp(), anchor, tex_size, avail);
        }
    }
    let scale = view.scale(tex_size, avail);
    // A picture smaller than the view stays centred; a larger one may be
    // dragged until its edge reaches the middle of the view.
    let size = tex_size * scale;
    let limit = (size + avail) / 2.0 - vec2(48.0, 48.0);
    view.pan = vec2(
        view.pan.x.clamp(-limit.x.max(0.0), limit.x.max(0.0)),
        view.pan.y.clamp(-limit.y.max(0.0), limit.y.max(0.0)),
    );
    let rect = Rect::from_center_size(avail_rect.center() + view.pan, size);
    let painter = ui.painter_at(avail_rect);
    painter.image(
        tex,
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    if resp.hovered() || resp.dragged() {
        ui.output_mut(|o| {
            o.cursor_icon = if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            };
        });
    }
    (rect, avail_rect)
}

/// Counts of the displayed picture's values in 128 bins, per colour.
#[derive(Clone, Debug)]
pub struct Histogram {
    /// One row of counts per channel (one for a mono picture, three for colour).
    pub bins: Vec<[u32; BINS]>,
}

/// Bins in a [`Histogram`].
pub const BINS: usize = 128;

impl Histogram {
    /// Count the pixels of `img` as they are displayed.
    #[must_use]
    pub fn of(img: &ColorImage) -> Self {
        let mut rows = vec![[0_u32; BINS]; 3];
        let mut grey = true;
        for px in &img.pixels {
            let [r, g, b, _] = px.to_array();
            grey &= r == g && g == b;
            for (row, v) in rows.iter_mut().zip([r, g, b]) {
                row[usize::from(v) * BINS / 256] += 1;
            }
        }
        if grey {
            rows.truncate(1);
        }
        Self { bins: rows }
    }

    /// The bin below which `fraction` of the pixels lie, for channel `c`, from 0 to 1.
    #[must_use]
    pub fn percentile(&self, c: usize, fraction: f32) -> f32 {
        let row = &self.bins[c.min(self.bins.len() - 1)];
        let total: u64 = row.iter().map(|&n| u64::from(n)).sum();
        let want = (total as f32 * fraction) as u64;
        let mut seen = 0;
        for (i, &n) in row.iter().enumerate() {
            seen += u64::from(n);
            if seen >= want {
                return (i as f32 + 0.5) / BINS as f32;
            }
        }
        1.0
    }
}

/// Draw `hist` in `rect`: a filled curve per channel on a log scale.
pub fn draw_histogram(ui: &Ui, rect: Rect, hist: &Histogram) {
    let p = palette();
    let painter = ui.painter_at(rect);
    for k in 1..4 {
        let y = rect.top() + rect.height() * k as f32 / 4.0;
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, p.border),
        );
    }
    let peak = hist
        .bins
        .iter()
        .flat_map(|r| r.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);
    let scale = |n: u32| ((1.0 + n as f32).ln() / (1.0 + peak as f32).ln()).clamp(0.0, 1.0);
    let colours: [Color32; 3] = if hist.bins.len() == 1 {
        [p.accent, p.accent, p.accent]
    } else if is_night() {
        [
            Color32::from_rgb(0xFF, 0x5A, 0x4A),
            Color32::from_rgb(0xC8, 0x3C, 0x30),
            Color32::from_rgb(0x8A, 0x2A, 0x22),
        ]
    } else {
        [
            Color32::from_rgb(0xFF, 0x7A, 0x7A),
            Color32::from_rgb(0x6F, 0xD4, 0x9A),
            Color32::from_rgb(0x7A, 0xA2, 0xFF),
        ]
    };
    let step = rect.width() / BINS as f32;
    for (row, &col) in hist.bins.iter().zip(&colours) {
        let top = |i: usize| -> Pos2 {
            pos2(
                rect.left() + (i as f32 + 0.5) * step,
                rect.bottom() - scale(row[i]) * (rect.height() - 4.0),
            )
        };
        let mut mesh = Mesh::default();
        let fill = col.gamma_multiply(0.18);
        for i in 0..BINS {
            let t = top(i);
            let base = u32::try_from(mesh.vertices.len()).unwrap_or(0);
            mesh.colored_vertex(t, fill);
            mesh.colored_vertex(pos2(t.x, rect.bottom()), fill);
            if i > 0 {
                mesh.add_triangle(base - 2, base - 1, base);
                mesh.add_triangle(base - 1, base, base + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.add(Shape::line(
            (0..BINS).map(top).collect(),
            Stroke::new(1.5, col),
        ));
    }
    painter.text(
        rect.right_top() + vec2(-6.0, 4.0),
        Align2::RIGHT_TOP,
        if hist.bins.len() == 1 {
            "grey"
        } else {
            "red, green, blue"
        },
        mono(11.0),
        p.faint,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let (tex, avail) = (vec2(800.0, 600.0), vec2(400.0, 300.0));
        let mut v = View::FIT;
        let anchor = vec2(60.0, -40.0);
        // The picture point under the anchor, in preview pixels from its centre.
        let before = (anchor - v.pan) / v.scale(tex, avail);
        v.zoom_by(2.0, anchor, tex, avail);
        let after = (anchor - v.pan) / v.scale(tex, avail);
        assert!((before - after).length() < 1e-3, "{before:?} vs {after:?}");
        assert!((v.scale(tex, avail) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn centring_puts_the_pixel_in_the_middle_of_the_view() {
        let tex = vec2(4000.0, 3000.0);
        let mut v = View::FIT;
        v.centre_on(4.0, 1000.0, 500.0, tex);
        // The picture's centre is offset so that pixel (1000, 500) is at 0.
        let at = (vec2(1000.0, 500.0) - tex / 2.0) * 4.0 + v.pan;
        assert!(at.length() < 1e-3, "{at:?}");
        v.centre_on(1000.0, 0.0, 0.0, tex);
        assert_eq!(v.zoom, Some(ZOOM_RANGE.1));
    }

    #[test]
    fn the_zoom_is_clamped_and_fit_resets_it() {
        let (tex, avail) = (vec2(100.0, 100.0), vec2(100.0, 100.0));
        let mut v = View::FIT;
        for _ in 0..40 {
            v.zoom_by(2.0, Vec2::ZERO, tex, avail);
        }
        assert_eq!(v.zoom, Some(ZOOM_RANGE.1));
        for _ in 0..80 {
            v.zoom_by(0.5, Vec2::ZERO, tex, avail);
        }
        assert_eq!(v.zoom, Some(ZOOM_RANGE.0));
        v.fit();
        assert!(v.zoom.is_none() && v.pan == Vec2::ZERO);
    }

    #[test]
    fn a_small_picture_is_not_enlarged_to_fit() {
        assert!((fit_scale(vec2(100.0, 50.0), vec2(1000.0, 1000.0)) - 1.0).abs() < 1e-6);
        assert!((fit_scale(vec2(2000.0, 1000.0), vec2(1000.0, 1000.0)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_histogram_of_a_grey_picture_has_one_channel() {
        let img = ColorImage::new([4, 1], vec![Color32::from_gray(10); 4]);
        let h = Histogram::of(&img);
        assert_eq!(h.bins.len(), 1);
        assert_eq!(h.bins[0].iter().sum::<u32>(), 4);
        let colour = ColorImage::new([2, 1], vec![Color32::from_rgb(255, 0, 0); 2]);
        assert_eq!(Histogram::of(&colour).bins.len(), 3);
    }

    #[test]
    fn percentiles_follow_the_counts() {
        let mut px = vec![Color32::from_gray(0); 90];
        px.extend(vec![Color32::from_gray(255); 10]);
        let h = Histogram::of(&ColorImage::new([100, 1], px));
        assert!(h.percentile(0, 0.5) < 0.05);
        assert!(h.percentile(0, 0.99) > 0.95);
    }
}
