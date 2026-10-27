// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The window: a top bar, the workflow rail on the left, the viewer in the
//! middle, the step's panel on the right, and the About page.
//!
//! The layout follows Helia's, in Valerian's night-sky colours (or the red
//! night-vision palette, F2).

use std::path::PathBuf;

use eframe::egui::{
    self, Align, CentralPanel, Context, Frame, Key, Label, Layout, Margin, Modifiers, Panel,
    RichText, Stroke, TextureHandle, TextureOptions, Ui, vec2,
};

use crate::dialogs::Target;
use crate::rail;
use crate::shortcuts;
use crate::theme::{self, error_colour, mono, palette, semibold};
use crate::widgets::{self, Kind};
use crate::workspace::Workspace;

/// Window width below which the rail shows numbers only.
const NARROW: f32 = 900.0;

/// Window state.
#[derive(Default)]
pub struct App {
    /// The About page replaces the workspace.
    pub(crate) show_about: bool,
    /// The list of keyboard shortcuts is open over the window.
    pub(crate) show_help: bool,
    /// The GPU adapter, once the user asks for it.
    pub(crate) gpu_text: Option<String>,
    /// The third-party licence list is expanded.
    pub(crate) show_third_party: bool,
    /// Its text, read on first expansion.
    pub(crate) third_party_text: Option<String>,
    /// The open image and the tools.
    pub(crate) ws: Workspace,
    /// Inner size of the window on the last frame it was a normal window.
    pub(crate) window_size: Option<[f32; 2]>,
    /// Remembered window size and recent images.
    pub(crate) prefs: crate::prefs::Prefs,
    /// The logo in the top bar.
    logo: Option<TextureHandle>,
}

impl App {
    /// The app for the running window.
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut app = Self {
            // Tests and snapshots must not read or write the user's real settings file.
            prefs: if cfg!(test) {
                crate::prefs::Prefs::default()
            } else {
                crate::prefs::Prefs::load_default()
            },
            ..Self::default()
        };
        theme::set_night(&cc.egui_ctx, app.prefs.night);
        app.logo = crate::branding::bar_logo().map(|img| {
            cc.egui_ctx
                .load_texture("logo", img, TextureOptions::LINEAR)
        });
        if let Some(p) = open {
            app.ws.open(&p);
        }
        app
    }

    /// Switch the night-vision palette on or off.
    pub(crate) fn set_night(&mut self, ctx: &Context, on: bool) {
        theme::set_night(ctx, on);
        self.prefs.remember_night(on);
        self.ws.mark_dirty();
    }

    /// Show or hide the list of shortcuts, with a notice.
    fn set_help(&mut self, on: bool) {
        self.show_help = on;
        self.ws.notify(if on {
            "Showing the keyboard shortcuts: F1, ? or Esc closes them."
        } else {
            "Closed the keyboard shortcuts."
        });
    }

    /// The keys. Plain keys (letters, digits, Enter) are left to a text box that has the
    /// keyboard; function keys and chords are always ours. Every action says what it did.
    fn shortcuts(&mut self, ctx: &Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let press = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        // Help first, so ? and F1 work from anywhere.
        if press(Modifiers::COMMAND, Key::F1) {
            self.show_about = !self.show_about;
            self.show_help = false;
            self.ws.notify(if self.show_about {
                "Showing the About page: Esc closes it."
            } else {
                "Closed the About page."
            });
        }
        if press(Modifiers::NONE, Key::F1)
            || (!typing
                && (press(Modifiers::NONE, Key::Questionmark)
                    || press(Modifiers::SHIFT, Key::Slash)))
        {
            self.set_help(!self.show_help);
        }
        // Escape: the nearest thing to dismiss, else cancel the running work.
        let esc_has_work =
            self.show_help || self.show_about || self.ws.review.is_some() || self.ws.cancellable();
        if esc_has_work && press(Modifiers::NONE, Key::Escape) {
            if self.show_help {
                self.set_help(false);
            } else if self.show_about {
                self.show_about = false;
                self.ws.notify("Closed the About page.");
            } else if self.ws.cancellable() {
                self.ws.cancel();
            } else {
                self.ws.cancel_review();
            }
        }
        if self.show_help || self.show_about {
            // Only help, About and Esc work while a page covers the window.
            return;
        }
        if press(ctrl_shift, Key::O) {
            if self.ws.chooser.busy() {
                self.ws.notify("A file dialog is already open.");
            } else {
                self.ws
                    .notify("Choose a folder of frames to sort in the dialog.");
                self.ws.browse(ctx, Target::SortFolder);
            }
        } else if press(Modifiers::COMMAND, Key::O) {
            if self.ws.chooser.busy() {
                self.ws.notify("A file dialog is already open.");
            } else {
                self.ws.notify("Choose a FITS image to open in the dialog.");
                self.ws.browse(ctx, Target::Image);
            }
        }
        if press(Modifiers::COMMAND, Key::Enter) || press(Modifiers::NONE, Key::F5) {
            self.ws.run_current(ctx);
        }
        if !typing && self.ws.review.is_some() && press(Modifiers::NONE, Key::Enter) {
            self.ws.accept_review(ctx);
        }
        if press(Modifiers::ALT, Key::ArrowRight) {
            self.ws.step_by(1);
        }
        if press(Modifiers::ALT, Key::ArrowLeft) {
            self.ws.step_by(-1);
        }
        let night = press(Modifiers::NONE, Key::F2) || (!typing && press(Modifiers::NONE, Key::N));
        if night {
            let on = !theme::is_night();
            self.set_night(ctx, on);
            self.ws.notify(if on {
                "Night vision on (F2 or N turns it off)."
            } else {
                "Night vision off."
            });
        }
        if !typing {
            if press(Modifiers::NONE, Key::Plus) || press(Modifiers::NONE, Key::Equals) {
                self.ws.zoom_key(1.25);
            }
            if press(Modifiers::NONE, Key::Minus) {
                self.ws.zoom_key(0.8);
            }
            if press(Modifiers::NONE, Key::Num0) {
                self.ws.zoom_to(true);
            }
            if press(Modifiers::NONE, Key::Num1) {
                self.ws.zoom_to(false);
            }
        }
    }

    /// The "Recent" menu: the images opened lately, newest first, at the right of the
    /// top bar. Not drawn while there are none.
    fn recent_menu(&mut self, ui: &mut Ui) {
        if self.prefs.recent.is_empty() {
            return;
        }
        let mut chosen = None;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button("Recent", |ui| {
                for f in &self.prefs.recent {
                    let name = f.file_name().map_or_else(
                        || f.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    if ui
                        .button(name)
                        .on_hover_text(f.display().to_string())
                        .clicked()
                    {
                        chosen = Some(f.clone());
                        ui.close();
                    }
                }
            });
        });
        if let Some(f) = chosen {
            if f.exists() {
                self.show_about = false;
                self.ws.open(&f);
            } else {
                self.ws.status = Some((format!("{} is no longer there", f.display()), true));
                self.prefs.forget_recent(&f);
            }
        }
    }

    fn topbar(&mut self, ui: &mut Ui) {
        let p = palette();
        let width = ui.ctx().content_rect().width();
        Panel::top("topbar")
            .exact_size(64.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(16, 10))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let mut about = false;
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(40.0, 40.0), egui::Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About Valerian")
                    });
                    if let Some(logo) = &self.logo {
                        let tint = if theme::is_night() {
                            egui::Color32::from_rgb(0xFF, 0x4A, 0x3A)
                        } else {
                            egui::Color32::WHITE
                        };
                        ui.painter()
                            .image(logo.id(), rect, crate::branding::BAR_LOGO_UV, tint);
                    } else {
                        widgets::paint_moon(ui.painter(), rect.center(), 16.0, p.accent, p.panel);
                    }
                    about |= resp.on_hover_text("About Valerian").clicked();
                    let name = ui
                        .add(
                            Label::new(
                                RichText::new("Valerian").font(semibold(20.0)).color(p.text),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text("About Valerian");
                    about |= name.clicked();
                    if width >= NARROW + 100.0
                        && let Some(d) = &self.ws.doc
                    {
                        let file = d
                            .path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        ui.label(
                            RichText::new(format!(
                                "{file}  .  {} x {}  .  {} plane{}",
                                d.full.width(),
                                d.full.height(),
                                d.full.planes.len(),
                                if d.full.planes.len() == 1 { "" } else { "s" }
                            ))
                            .font(mono(13.0))
                            .color(p.muted),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::button(ui, "About", Kind::Outline, 0.0)
                            .on_hover_text("About Valerian (Ctrl+F1)")
                            .clicked()
                        {
                            about = true;
                        }
                        ui.add_enabled_ui(!self.ws.chooser.busy(), |ui| {
                            if widgets::button(ui, "Open", Kind::Outline, 0.0)
                                .on_hover_text("Open a FITS image (Ctrl+O)")
                                .clicked()
                            {
                                self.show_about = false;
                                let ctx = ui.ctx().clone();
                                self.ws.browse(&ctx, Target::Image);
                            }
                        });
                        let gpu_on = self.ws.tool == crate::workspace::Tool::Gpu;
                        if widgets::chip_tall(ui, gpu_on, "GPU")
                            .on_hover_text("Open a graphics adapter and time the kernels")
                            .clicked()
                        {
                            self.show_about = false;
                            self.ws.toggle_gpu();
                        }
                        let night = theme::is_night();
                        if widgets::chip_tall(ui, night, "Night")
                            .on_hover_text(
                                "Red-on-black night-vision palette, for use at the telescope (F2)",
                            )
                            .clicked()
                        {
                            let ctx = ui.ctx().clone();
                            self.set_night(&ctx, !night);
                        }
                        self.recent_menu(ui);
                    });
                    if about {
                        self.show_about = true;
                    }
                });
            });
    }

    /// The strip above the viewer: what is running, or what just happened.
    fn notice_strip(&mut self, ui: &mut Ui) {
        let Some(n) = self.ws.notice() else { return };
        let p = palette();
        let (mut dismiss, mut cancel) = (false, false);
        let (stroke, ink) = if n.error {
            (error_colour(), error_colour())
        } else {
            (p.border_strong, p.text)
        };
        Frame::new()
            .fill(p.raised)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    if n.running {
                        ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                    }
                    ui.add(
                        Label::new(RichText::new(&n.text).font(semibold(14.0)).color(ink)).wrap(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if n.running
                            && widgets::chip(
                                ui,
                                false,
                                if n.cancellable {
                                    "Cancel"
                                } else {
                                    "Cancelling..."
                                },
                            )
                            .on_hover_text("Stop this step; nothing from it is kept (Esc)")
                            .clicked()
                            && n.cancellable
                        {
                            cancel = true;
                        }
                        if !n.running
                            && widgets::chip(ui, false, "Dismiss")
                                .on_hover_text("Hide this message")
                                .clicked()
                        {
                            dismiss = true;
                        }
                        if !n.detail.is_empty() {
                            ui.add(
                                Label::new(
                                    RichText::new(&n.detail).font(mono(12.0)).color(p.muted),
                                )
                                .truncate(),
                            );
                        }
                    });
                });
                if let Some(f) = n.fraction {
                    ui.add(egui::ProgressBar::new(f).desired_height(6.0).fill(p.accent));
                }
            });
        ui.add_space(8.0);
        if n.running {
            // The elapsed time and the step's own lines change without input.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        if dismiss {
            self.ws.dismiss_notice();
        }
        if cancel {
            self.ws.cancel();
        }
    }

    /// The workflow rail.
    fn rail(&mut self, ui: &mut Ui, narrow: bool) {
        let p = palette();
        let mut picked = None;
        Panel::left("rail")
            .exact_size(if narrow { 64.0 } else { 236.0 })
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(8, 12))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                if !narrow {
                    ui.add_space(2.0);
                    widgets::overline(ui, "Workflow");
                    ui.add_space(6.0);
                }
                let rows = self.ws.rail_rows();
                let current = self.ws.tool.step();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        picked = rail::show(ui, &rows, current, narrow);
                    });
            });
        if let Some(step) = picked {
            self.show_about = false;
            self.ws.select(step);
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        if let Some(size) = self.window_size {
            self.prefs.remember_window(size);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Not while maximised, full screen or minimised: those sizes are not the user's choice.
        ctx.input(|i| {
            let v = i.viewport();
            let normal = v.maximized != Some(true)
                && v.fullscreen != Some(true)
                && v.minimized != Some(true);
            if normal && let Some(r) = v.inner_rect {
                self.window_size = Some([r.width(), r.height()]);
            }
        });
        if self.prefs.is_persistent()
            && let Some(d) = &self.ws.doc
        {
            self.prefs.remember_recent(&d.path);
        }
        self.shortcuts(&ctx);
        self.ws.tick();
        if let Some(dir) = self.ws.take_pending_sort() {
            self.show_about = false;
            self.ws.start_sort(&ctx, vec![dir]);
        }
        if let Some(set) = self.ws.pending_session.take() {
            self.prefs.remember_session(&set);
        }
        self.topbar(ui);
        let p = palette();
        if !self.show_about {
            let narrow = ctx.content_rect().width() < NARROW;
            self.rail(ui, narrow);
            Panel::right("inspector")
                .default_size(if narrow { 300.0 } else { 360.0 })
                .size_range(260.0..=520.0)
                .resizable(true)
                .frame(
                    Frame::new()
                        .fill(p.panel)
                        .inner_margin(Margin::same(14))
                        .stroke(Stroke::new(1.0, p.border)),
                )
                .show(ui, |ui| self.ws.tool_panel(ui));
        }
        if self.show_help {
            match shortcuts::window(&ctx) {
                shortcuts::Act::Nothing => {}
                shortcuts::Act::Close => self.set_help(false),
                shortcuts::Act::About => {
                    self.show_help = false;
                    self.show_about = true;
                }
            }
        }
        CentralPanel::default()
            .frame(Frame::new().fill(p.ground).inner_margin(Margin::same(16)))
            .show(ui, |ui| {
                if self.show_about {
                    self.about_page(ui);
                } else {
                    self.notice_strip(ui);
                    self.ws.viewer(ui, &mut self.prefs);
                }
            });
    }
}
