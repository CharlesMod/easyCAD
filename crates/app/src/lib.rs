//! easyCAD desktop GUI (eframe/egui + wgpu). See docs/GUI.md.

pub mod adapters;
pub mod app;
pub mod camera;
pub mod cloud;
pub mod dialogs;
pub mod e2e_state;
pub mod files;
pub mod pickstack;
pub mod profiler;
pub mod recovery;
pub mod reload;
pub mod sample;
pub mod scope;
pub mod settings;
pub mod construct;
pub mod datum_ui;
pub mod hist_sel;
pub mod drag_arrow;
pub mod plane_pick;
pub mod sketch_mode;
pub mod sketch_move;
pub mod sketch_view;
pub mod smoke;
pub mod theme;
pub mod ui_assembly;
pub mod ui_bom;
pub mod ui_drawing;
pub mod ui_gizmo;
pub mod ui_interference;
pub mod ui_mates;
pub mod ui_overhang;
pub mod ui_main;
pub mod ui_parts;
pub mod ui_process;
pub mod ui_settings;
pub mod ui_viewport;
pub mod ui_welcome;
pub mod units;
pub mod viewcube;
pub mod viewport;
pub mod widgets;
pub mod wizard;
pub mod worker;

pub use app::EasyCadApp;

/// egui's default device setup, plus GPU timestamp queries when the adapter has
/// them (the performance overlay's GPU times).
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{wgpu, WgpuSetup, WgpuSetupCreateNew};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    let base = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| {
        let mut d = base(adapter);
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            d.required_features |= wgpu::Features::TIMESTAMP_QUERY;
        }
        d
    });
    let mut cfg = eframe::egui_wgpu::WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() };
    // EASYCAD_PRESENT=immediate|mailbox|fifo, to see uncapped frame times
    let mode = std::env::var("EASYCAD_PRESENT").ok();
    cfg.surface.present_mode = match mode.as_deref() {
        Some("immediate") => wgpu::PresentMode::AutoNoVsync,
        Some("mailbox") => wgpu::PresentMode::Mailbox,
        Some("fifo") => wgpu::PresentMode::Fifo,
        _ => cfg.surface.present_mode,
    };
    crate::profiler::set_present(&format!("{:?}", cfg.surface.present_mode));
    cfg
}

/// Native options used by the binary (wgpu, no depth in egui's pass, no MSAA: the
/// viewport renders its own 4×MSAA target).
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("easyCAD")
            .with_app_id("easycad")
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon/easycad-256.png"))
                    .unwrap_or_default(),
            ))
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        depth_buffer: 0,
        multisampling: 1,
        wgpu_options: wgpu_options(),
        ..Default::default()
    }
}
