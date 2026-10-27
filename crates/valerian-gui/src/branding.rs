// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Valerian window and taskbar icon, embedded in the executable.
//!
//! The image is not GPL: see `LOGO-LICENCE.md`.

use eframe::egui;

/// Window and taskbar icon.
const WINDOW_ICON: &[u8] = include_bytes!("../../../assets/branding/icons/valerian-256.png");

/// The window and taskbar icon, decoded to straight RGBA8. The bytes are ours
/// and fixed, so a failure here is a build error and not a runtime condition.
pub fn window_icon() -> Option<egui::IconData> {
    let (rgba, width, height) = decode(WINDOW_ICON)?;
    Some(egui::IconData {
        rgba,
        width,
        height,
    })
}

/// The 128 px icon, for the top bar.
const BAR_ICON: &[u8] = include_bytes!("../../../assets/branding/icons/valerian-128.png");

/// The logo for the top bar as a texture source.
#[must_use]
pub fn bar_logo() -> Option<egui::ColorImage> {
    let (rgba, w, h) = decode(BAR_ICON)?;
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &rgba,
    ))
}

/// The part of [`bar_logo`] that is the galaxy and not the name under it.
pub const BAR_LOGO_UV: egui::Rect = egui::Rect {
    min: egui::pos2(0.14, 0.03),
    max: egui::pos2(0.86, 0.75),
};

/// RGBA pixels and size of an embedded PNG.
fn decode(bytes: &'static [u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_window_icon_decodes() {
        let icon = super::window_icon().expect("embedded icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
