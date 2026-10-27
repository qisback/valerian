// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The GPU tool: which adapter, a benchmark of the kernels against the CPU code,
//! and the result as a table and a chart.
//!
//! The kernels (calibration, the registration warp and the stack's per-pixel
//! combiner) are used by `valerian stack`, `register` and `calibrate` with
//! `--backend`. The desktop app's own tools run the CPU code; this panel shows
//! what the device does, how fast, and that it agrees with the CPU to the
//! tolerances the parity tests state.

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Slider, Ui, vec2};
use valerian_gpu::bench::{Config, Row, run as run_bench};
use valerian_gpu::{AdapterRequest, GpuCompute};

use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// What the user asked for in the panel this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Nothing.
    Nothing,
    /// Run the benchmark.
    Run,
}

/// Which adapter to open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Adapter {
    /// A real GPU.
    #[default]
    Hardware,
    /// The software adapter (lavapipe, WARP).
    Software,
}

impl Adapter {
    fn request(self) -> AdapterRequest {
        match self {
            Self::Hardware => AdapterRequest::Hardware,
            Self::Software => AdapterRequest::Software,
        }
    }
}

/// A finished benchmark.
#[derive(Clone, Debug)]
pub struct GpuRun {
    /// The adapter it ran on.
    pub adapter: String,
    /// One row per stage.
    pub rows: Vec<Row>,
    /// The frame size and the frames in the combine.
    pub size: (usize, usize, usize),
}

/// The settings the panel edits, and the last result.
#[derive(Clone, Debug)]
pub struct GpuTool {
    /// The adapter to open.
    pub adapter: Adapter,
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Frames in the combine.
    pub frames: usize,
    /// The last run.
    pub result: Option<GpuRun>,
}

impl Default for GpuTool {
    fn default() -> Self {
        Self {
            adapter: Adapter::Hardware,
            width: 2048,
            height: 1536,
            frames: 12,
            result: None,
        }
    }
}

impl GpuTool {
    /// Run the benchmark. Runs on the worker thread.
    ///
    /// # Errors
    /// The reason, when no adapter opens or a stage fails.
    pub fn run(
        adapter: Adapter,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Result<GpuRun, String> {
        let gpu = GpuCompute::new(adapter.request())
            .map_err(|e| format!("No {:?} adapter could be opened: {e}", adapter.request()))?;
        let cfg = Config {
            width,
            height,
            frames,
            repeats: 2,
        };
        let rows = run_bench(&gpu, &cfg, &mut |_| {})?;
        Ok(GpuRun {
            adapter: gpu.summary(),
            rows,
            size: (width, height, frames),
        })
    }

    /// The controls. `busy` while a run is going.
    pub fn controls(&mut self, ui: &mut Ui, busy: bool) -> Ask {
        let p = palette();
        let mut ask = Ask::Nothing;
        ui.label(
            RichText::new(
                "Times calibration, the registration warp and the stack combiner on the CPU \
                 and on the GPU with made-up frames, and checks that they agree. The command \
                 line uses the GPU for stacking with --backend; these tools run the CPU code.",
            )
            .small()
            .color(p.muted),
        );
        widgets::overline(ui, "Adapter");
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.adapter, Adapter::Hardware, "Hardware");
                ui.selectable_value(&mut self.adapter, Adapter::Software, "Software");
            });
            ui.label(
                RichText::new(match self.adapter {
                    Adapter::Hardware => "A real GPU, through Vulkan, Metal or DirectX.",
                    Adapter::Software => "The fallback adapter CI uses; slower than the CPU.",
                })
                .small()
                .color(p.muted),
            );
            widgets::overline(ui, "Frames");
            ui.add(Slider::new(&mut self.width, 256..=6144).text("Width (px)"));
            ui.add(Slider::new(&mut self.height, 256..=4608).text("Height (px)"));
            ui.add(Slider::new(&mut self.frames, 3..=32).text("Frames in the combine"));
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(!busy, |ui| {
            if widgets::button(
                ui,
                if busy { "Running..." } else { "Run benchmark" },
                Kind::Outline,
                0.0,
            )
            .on_hover_text("Run each stage on the CPU and on the GPU and compare")
            .clicked()
            {
                ask = Ask::Run;
            }
        });
        if let Some(r) = &self.result {
            ui.separator();
            widgets::overline(ui, "Result");
            ui.label(RichText::new(&r.adapter).font(mono(12.5)).color(p.text));
            ui.label(
                RichText::new(format!(
                    "frames of {} x {}, {} in the combine",
                    r.size.0, r.size.1, r.size.2
                ))
                .small()
                .color(p.muted),
            );
            egui::Grid::new("gpu_rows").num_columns(4).show(ui, |ui| {
                for h in ["Stage", "CPU ms", "GPU ms", "x"] {
                    ui.label(RichText::new(h).small().color(p.muted));
                }
                ui.end_row();
                for row in &r.rows {
                    ui.label(
                        RichText::new(short(&row.name))
                            .font(mono(12.0))
                            .color(p.text),
                    );
                    ui.label(RichText::new(format!("{:.0}", row.cpu * 1000.0)).font(mono(12.0)));
                    ui.label(RichText::new(format!("{:.0}", row.gpu * 1000.0)).font(mono(12.0)));
                    ui.label(RichText::new(format!("{:.1}", row.speedup())).font(mono(12.0)));
                    ui.end_row();
                }
            });
            let worst = r.rows.iter().map(|x| x.worst).fold(0.0, f64::max);
            ui.label(
                RichText::new(format!(
                    "Largest difference from the CPU: {worst:.1e} of the data range."
                ))
                .small()
                .color(p.muted),
            );
        }
        ask
    }
}

/// A stage name short enough for the panel: `combine (sigma clip)` is
/// `combine: sigma clip`, and a long one is cut.
fn short(name: &str) -> String {
    let t = name.replace(" (", ": ").replace(')', "");
    if t.chars().count() > 24 {
        let cut: String = t.chars().take(23).collect();
        format!("{cut}.")
    } else {
        t
    }
}

/// Draw the chart of a run: for each stage a bar of the CPU time and one of the
/// GPU time, on one scale.
pub fn chart(ui: &mut Ui, run: &GpuRun) {
    let p = palette();
    let size = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let slowest = run
        .rows
        .iter()
        .map(|r| r.cpu.max(r.gpu))
        .fold(1e-6, f64::max);
    let row_h = (rect.height() / run.rows.len().max(1) as f32).min(86.0);
    let label_w = 230.0_f32.min(rect.width() * 0.34);
    let bar_w = rect.width() - label_w - 140.0;
    for (i, r) in run.rows.iter().enumerate() {
        let top = rect.top() + 20.0 + i as f32 * row_h;
        painter.text(
            egui::pos2(rect.left() + 8.0, top + row_h * 0.35),
            Align2::LEFT_CENTER,
            &r.name,
            FontId::proportional(13.0),
            p.text,
        );
        for (k, (t, colour, name)) in [(r.cpu, p.muted, "CPU"), (r.gpu, p.accent, "GPU")]
            .into_iter()
            .enumerate()
        {
            let y = top + 6.0 + k as f32 * (row_h * 0.3);
            let w = (t / slowest) as f32 * bar_w;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + label_w, y),
                    vec2(w.max(2.0), row_h * 0.22),
                ),
                3.0,
                colour,
            );
            painter.text(
                egui::pos2(rect.left() + label_w + w + 6.0, y + row_h * 0.11),
                Align2::LEFT_CENTER,
                format!("{name} {:.0} ms", t * 1000.0),
                FontId::monospace(11.5),
                Color32::from_gray(180),
            );
        }
    }
}
