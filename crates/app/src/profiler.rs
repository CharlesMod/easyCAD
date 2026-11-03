//! The performance overlay (F12, or `EASYCAD_PROFILE=1` at start): which GPU and
//! API are in use, frame rate and frame intervals against a 144 Hz line, and where
//! each frame's time goes: the app's UI logic, egui's whole update, the 3D pass's
//! CPU encode and its GPU time (timestamp queries, when the adapter has them),
//! with 3D renders vs image-cache hits, draw calls and triangles.
//!
//! The renderer lives in egui_wgpu's callback resources, so samples go through a
//! process-wide recorder rather than the app struct.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

use egui::{pos2, vec2, Color32, Rect, RichText, Stroke, Ui};

use crate::theme;

/// Samples kept per series (a few seconds at 144 Hz).
const KEEP: usize = 600;

#[derive(Default)]
struct Series(VecDeque<(Instant, f32)>);

impl Series {
    fn push(&mut self, v: f32) {
        self.0.push_back((Instant::now(), v));
        while self.0.len() > KEEP {
            self.0.pop_front();
        }
    }
    /// Values from the last second.
    fn recent(&self) -> Vec<f32> {
        let now = Instant::now();
        self.0.iter().filter(|(t, _)| now.duration_since(*t).as_secs_f32() < 1.0).map(|(_, v)| *v).collect()
    }
}

#[derive(Default)]
struct Recorder {
    adapter: String,
    present: String,
    timestamps: bool,
    /// Gaps between frame starts, ms.
    interval: Series,
    /// `EasyCadApp::frame`, ms.
    app: Series,
    /// eframe's measure of the whole update (app + egui layout), ms.
    update: Series,
    /// 3D pass CPU encode, ms (only frames that rendered).
    encode: Series,
    /// 3D pass on the GPU, ms (timestamps).
    gpu: Series,
    /// 1 per frame that re-rendered the 3D view, 0 per cache hit.
    rendered: Series,
    draws: usize,
    tris: u64,
    last_frame: Option<Instant>,
}

static REC: Mutex<Option<Recorder>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Recorder) -> R) -> R {
    let mut g = REC.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Recorder::default))
}

/// The adapter in use (logged once, shown in the overlay).
pub fn set_adapter(info: &eframe::egui_wgpu::wgpu::AdapterInfo, timestamps: bool) {
    let s = format!("{} · {:?} · {:?} · {}", info.name, info.backend, info.device_type, info.driver_info);
    eprintln!("easycad: GPU {s}{}", if timestamps { "" } else { " (no GPU timestamps)" });
    with(|r| {
        r.adapter = s;
        r.timestamps = timestamps;
    });
}

/// The surface's present mode (vsync or not).
pub fn set_present(mode: &str) {
    with(|r| r.present = mode.into());
}

/// Start of a UI frame.
pub fn frame_start() {
    with(|r| {
        let now = Instant::now();
        if let Some(t) = r.last_frame {
            r.interval.push(now.duration_since(t).as_secs_f32() * 1000.0);
        }
        r.last_frame = Some(now);
    });
}

pub fn app_ms(ms: f32, update_ms: Option<f32>) {
    with(|r| {
        r.app.push(ms);
        if let Some(u) = update_ms {
            r.update.push(u);
        }
    });
}

/// A 3D prepare: `encode_ms` is `Some` when it rendered, `None` on a cache hit.
pub fn viewport(encode_ms: Option<f32>, draws: usize, tris: u64) {
    with(|r| match encode_ms {
        Some(ms) => {
            r.encode.push(ms);
            r.rendered.push(1.0);
            r.draws = draws;
            r.tris = tris;
        }
        None => r.rendered.push(0.0),
    });
}

pub fn gpu_ms(ms: f32) {
    with(|r| r.gpu.push(ms));
}

fn summary(v: &[f32]) -> Option<(f32, f32)> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    let avg = s.iter().sum::<f32>() / s.len() as f32;
    Some((avg, s[(s.len() * 99 / 100).min(s.len() - 1)]))
}

/// The overlay, top right inside `area`.
pub fn ui(ctx: &egui::Context, area: Rect) {
    let present = with(|r| r.present.clone());
    let (adapter, ts, intervals, rows, renders, hits, draws, tris) = with(|r| {
        let row = |name: &'static str, s: &Series| (name, summary(&s.recent()));
        let rendered = r.rendered.recent();
        let renders = rendered.iter().filter(|v| **v > 0.5).count();
        (
            r.adapter.clone(),
            r.timestamps,
            r.interval.0.iter().rev().take(240).map(|(_, v)| *v).collect::<Vec<f32>>(),
            [
                row("UI logic", &r.app),
                row("whole update", &r.update),
                row("3D encode (CPU)", &r.encode),
                row("3D pass (GPU)", &r.gpu),
            ],
            renders,
            rendered.len() - renders,
            r.draws,
            r.tris,
        )
    });
    let fps = with(|r| r.interval.recent().len());
    // top left, under the selection filter (the view cube is top right)
    let pos = pos2(area.left() + 12.0, area.top() + 48.0);
    egui::Area::new(egui::Id::unique("perf_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .interactable(false)
        .show(ctx, |ui| {
            crate::widgets::card_frame().inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(300.0);
                let small = |t: String| RichText::new(t).size(11.0).font(theme::mono(11.0));
                ui.label(small(if adapter.is_empty() { "GPU: not reported".into() } else { adapter.clone() }));
                if adapter.contains("llvmpipe") || adapter.contains("Cpu") {
                    ui.label(RichText::new("software rendering: not your GPU").size(11.0).color(theme::ORANGE_SOFT));
                }
                ui.label(small(format!("present: {present} (EASYCAD_PRESENT=immediate uncaps)")));
                let worst = intervals.iter().copied().fold(0.0f32, f32::max);
                ui.label(
                    RichText::new(format!("{fps} frames in the last second · worst gap {worst:.1} ms"))
                        .size(12.0)
                        .strong(),
                );
                graph(ui, &intervals);
                for (name, s) in rows {
                    let text = match s {
                        Some((avg, p99)) => format!("{name:<16} {avg:6.2} ms  p99 {p99:6.2}"),
                        None if name.contains("GPU") && !ts => format!("{name:<16} (no timestamps)"),
                        None => format!("{name:<16}      –"),
                    };
                    ui.label(small(text));
                }
                ui.label(small(format!("3D renders {renders} · cache hits {hits} (last s)")));
                ui.label(small(format!("last render: {draws} draws, {} k tris", tris / 1000)));
                ui.label(RichText::new("F12 hides · frames are drawn only when something changes").size(10.0).color(theme::TEXT_MUTED));
            });
        });
}

/// Frame intervals, newest on the right, with 144 Hz and 60 Hz lines.
fn graph(ui: &mut Ui, v: &[f32]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, theme::hex(0x15191e));
    let top = 34.0f32; // ms at the top edge
    let y = |ms: f32| rect.bottom() - (ms.min(top) / top) * rect.height();
    for (ms, c) in [(1000.0 / 144.0, theme::hex(0x4caf72)), (1000.0 / 60.0, theme::ORANGE)] {
        p.hline(rect.x_range(), y(ms), Stroke::new(1.0, c.gamma_multiply(0.6)));
    }
    let n = 240.0;
    let w = rect.width() / n;
    for (i, ms) in v.iter().enumerate() {
        let x = rect.right() - (i as f32 + 1.0) * w;
        let c = if *ms <= 1000.0 / 140.0 {
            theme::hex(0x4caf72)
        } else if *ms <= 1000.0 / 60.0 {
            theme::ACCENT
        } else {
            theme::ORANGE
        };
        p.rect_filled(Rect::from_min_max(pos2(x, y(*ms)), pos2(x + w.max(1.0), rect.bottom())), 0.0, c);
    }
    p.text(rect.left_top() + vec2(4.0, 2.0), egui::Align2::LEFT_TOP, "frame gaps (ms) · 144 Hz / 60 Hz", theme::mono(9.0), Color32::from_gray(140));
}
