//! easyCAD desktop GUI (eframe/egui + wgpu). See docs/GUI.md.

pub mod adapters;
pub mod app;
pub mod camera;
pub mod cloud;
pub mod dialogs;
pub mod e2e_state;
pub mod files;
pub mod pickstack;
pub mod recovery;
pub mod sample;
pub mod scope;
pub mod settings;
pub mod sketch_mode;
pub mod smoke;
pub mod theme;
pub mod ui_assembly;
pub mod ui_bom;
pub mod ui_drawing;
pub mod ui_gizmo;
pub mod ui_interference;
pub mod ui_mates;
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
        ..Default::default()
    }
}
