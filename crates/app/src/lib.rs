//! Casework desktop GUI (eframe/egui + wgpu). See docs/GUI.md.

pub mod adapters;
pub mod app;
pub mod camera;
pub mod dialogs;
pub mod pickstack;
pub mod sample;
pub mod settings;
pub mod sketch_mode;
pub mod smoke;
pub mod theme;
pub mod ui_drawing;
pub mod ui_main;
pub mod ui_parts;
pub mod ui_process;
pub mod ui_viewport;
pub mod units;
pub mod viewcube;
pub mod viewport;
pub mod widgets;
pub mod worker;

pub use app::CaseworkApp;

/// Native options used by the binary (wgpu, no depth in egui's pass, no MSAA: the
/// viewport renders its own 4×MSAA target).
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Casework")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        depth_buffer: 0,
        multisampling: 1,
        ..Default::default()
    }
}
