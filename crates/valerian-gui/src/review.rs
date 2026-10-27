// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The review step of dropping a folder: what was found, sorted into groups, for the
//! user to correct before the Calibrate, Register and Stack boxes are filled.
//!
//! The groups, the exclusions and the explanations come from `sorter.rs`; this only
//! draws them and reports Accept or Cancel.

use std::collections::BTreeMap;

use eframe::egui::{self, Frame, Layout, Margin, RichText, ScrollArea, Ui};

use crate::sorter::{Kind, Review};
use crate::theme::{error_colour, mono, palette, semibold};
use crate::widgets::{self, Kind as Look};

/// What the user decided on the review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Nothing yet.
    Nothing,
    /// Fill the folder boxes from the groups.
    Accept,
    /// Throw the result away.
    Cancel,
}

/// Rows of a group's list shown without scrolling.
const LIST_ROWS: f32 = 9.0;
/// Height of one row of the list.
const ROW: f32 = 26.0;

/// Draw the review.
pub fn show(ui: &mut Ui, r: &mut Review) -> Act {
    let p = palette();
    let mut act = Act::Nothing;
    let lights = r.count(Kind::Light).1;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Sort the frames")
                        .font(semibold(20.0))
                        .color(p.text),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} files read from {}",
                        r.frames.len(),
                        r.root.display()
                    ))
                    .font(mono(12.0))
                    .color(p.muted),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                let counts: Vec<String> = Kind::ALL
                    .iter()
                    .filter(|k| r.count(**k).0 > 0)
                    .map(|k| {
                        let (all, used) = r.count(*k);
                        format!("{} {used}/{all}", k.label())
                    })
                    .collect();
                ui.label(
                    RichText::new(counts.join("   "))
                        .font(semibold(14.0))
                        .color(p.text),
                );
            });
            let warns = r.warnings();
            for w in &warns {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0);
                    let blocking = w.starts_with("No frames are set as lights");
                    ui.label(
                        RichText::new(format!("- {w}"))
                            .font(mono(12.0))
                            .color(if blocking { error_colour() } else { p.muted }),
                    );
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.add_enabled_ui(lights > 0, |ui| {
                    if widgets::button(ui, "Accept sorting", Look::Primary, 0.0)
                        .on_hover_text(
                            "Fill the Calibrate, Register and Stack folders from these groups (Enter)",
                        )
                        .clicked()
                    {
                        act = Act::Accept;
                    }
                });
                if widgets::button(ui, "Cancel sorting", Look::Outline, 0.0)
                    .on_hover_text("Throw this result away; nothing is changed (Esc)")
                    .clicked()
                {
                    act = Act::Cancel;
                }
            });
            ui.add_space(4.0);
            for kind in Kind::ALL {
                group(ui, r, kind);
            }
        });
    act
}

/// "Ha, 300 s x 25" for the distinct setups of the frames at `idx`.
fn setups(r: &Review, idx: &[usize]) -> String {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for &i in idx {
        *by.entry(r.frames[i].setup()).or_default() += 1;
    }
    if by.len() > 3 {
        return format!("{} setups", by.len());
    }
    by.into_iter()
        .map(|(k, n)| {
            if k.is_empty() {
                format!("{n} frames")
            } else {
                format!("{k} x {n}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn group(ui: &mut Ui, r: &mut Review, kind: Kind) {
    let p = palette();
    let idx: Vec<usize> = r
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == kind)
        .map(|(i, _)| i)
        .collect();
    if idx.is_empty() {
        if kind != Kind::Other {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("{}: none", kind.label()))
                        .font(semibold(14.0))
                        .color(p.faint),
                );
            });
        }
        return;
    }
    let used = idx.iter().filter(|&&i| r.frames[i].include).count();
    let title = format!(
        "{}: {used} of {} used   {}",
        kind.label(),
        idx.len(),
        setups(r, &idx)
    );
    let mut moves: Vec<(usize, Kind)> = Vec::new();
    let mut bulk: Option<Kind> = None;
    let mut all: Option<bool> = None;
    Frame::new()
        .fill(p.panel)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new(title).font(semibold(14.0)).color(p.text))
                .id_salt(("group", kind.label()))
                .default_open(idx.len() <= 10 || kind == Kind::Other)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if widgets::chip(ui, false, "Use all")
                            .on_hover_text("Include every frame of this group")
                            .clicked()
                        {
                            all = Some(true);
                        }
                        if widgets::chip(ui, false, "Leave all out")
                            .on_hover_text("Exclude every frame of this group")
                            .clicked()
                        {
                            all = Some(false);
                        }
                        ui.label(RichText::new("Move all to").small().color(p.muted));
                        for k in Kind::CHOICES {
                            if k != kind
                                && widgets::chip(ui, false, k.label())
                                    .on_hover_text(format!(
                                        "Assign every {} frame to {}",
                                        kind.label(),
                                        k.label()
                                    ))
                                    .clicked()
                            {
                                bulk = Some(k);
                            }
                        }
                    });
                    let rows = idx.len();
                    ScrollArea::vertical()
                        .id_salt(("rows", kind.label()))
                        .max_height(ROW * LIST_ROWS)
                        .auto_shrink([false, true])
                        .show_rows(ui, ROW, rows, |ui, range| {
                            for &i in &idx[range] {
                                row(ui, r, i, &mut moves);
                            }
                        });
                });
        });
    if let Some(on) = all {
        r.include_all(kind, on);
    }
    if let Some(to) = bulk {
        r.assign_all(kind, to);
    }
    for (i, to) in moves {
        r.assign(i, to);
    }
}

fn row(ui: &mut Ui, r: &mut Review, i: usize, moves: &mut Vec<(usize, Kind)>) {
    let p = palette();
    let f = &mut r.frames[i];
    ui.horizontal(|ui| {
        ui.set_height(ROW - 4.0);
        let name = f.name();
        ui.checkbox(&mut f.include, RichText::new(name).font(mono(12.0)));
        ui.label(RichText::new(f.setup()).font(mono(11.5)).color(p.muted));
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            let mut to = f.kind;
            egui::ComboBox::from_id_salt(("kind", i))
                .width(96.0)
                .selected_text(f.kind.label())
                .show_ui(ui, |ui| {
                    for k in Kind::CHOICES {
                        ui.selectable_value(&mut to, k, k.label());
                    }
                    ui.selectable_value(&mut to, Kind::Other, "Leave unsorted");
                });
            if to != f.kind {
                moves.push((i, to));
            }
            let why = if f.note.is_empty() {
                f.source.label().to_owned()
            } else {
                f.note.clone()
            };
            ui.add(egui::Label::new(RichText::new(why).font(mono(11.0)).color(p.faint)).truncate());
        });
    });
}
