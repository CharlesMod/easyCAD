//! Export dialog: format, "Export for: [process profile]", orient to plate, and
//! reference bodies for STEP. Runs off the UI thread on a copy of the document.

use std::sync::mpsc::{channel, Receiver};

use casework_model::document::{BlobStore, DocData, Document, ExportFormat, ExportOptions};
use casework_model::library::Library;
use casework_model::process::ProcessKind;
use egui::{RichText, Ui};

use crate::theme;
use crate::widgets;

pub struct ExportDialog {
    pub format: ExportFormat,
    pub profile: String,
    pub orient: bool,
    pub include_reference: bool,
}

pub enum ExportEvent {
    None,
    Cancel,
    Export,
}

pub fn ext(f: ExportFormat) -> &'static str {
    match f {
        ExportFormat::Stl => "stl",
        ExportFormat::ThreeMf => "3mf",
        ExportFormat::Step => "step",
    }
}

impl ExportDialog {
    pub fn new(d: &DocData) -> Self {
        ExportDialog {
            format: ExportFormat::ThreeMf,
            profile: d.active_profile.clone(),
            orient: true,
            include_reference: false,
        }
    }

    /// STEP for a molder typically goes out nominal / injection molded.
    fn suggest(&mut self, d: &DocData) {
        let want = match self.format {
            ExportFormat::Step => d.processes.iter().find(|p| p.kind == ProcessKind::Molded),
            _ => d.processes.iter().find(|p| p.name == d.active_profile),
        };
        if let Some(p) = want {
            self.profile = p.name.clone();
        }
        self.orient = self.format != ExportFormat::Step;
    }

    pub fn ui(&mut self, ui: &mut Ui, d: &DocData) -> ExportEvent {
        let mut ev = ExportEvent::None;
        widgets::card_frame().inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_width(340.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("EXPORT").strong().size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(widgets::muted("Enter ✓ · Esc ✕")));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (f, n) in [(ExportFormat::ThreeMf, "3MF"), (ExportFormat::Stl, "STL"), (ExportFormat::Step, "STEP")] {
                    if widgets::pill(ui, n, self.format == f).clicked() {
                        self.format = f;
                        self.suggest(d);
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Export for").color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("export_profile").selected_text(&self.profile).width(190.0).show_ui(ui, |ui| {
                        for p in &d.processes {
                            let label = if p.name == d.active_profile { format!("{} (active)", p.name) } else { p.name.clone() };
                            if ui.selectable_label(self.profile == p.name, label).clicked() {
                                self.profile = p.name.clone();
                            }
                        }
                    });
                });
            });
            ui.label(widgets::muted("Fits and teardrops are resolved for this profile. The document is not changed."));
            ui.checkbox(&mut self.orient, "Orient each body to its build plate (plate face down, z = 0)");
            if self.format == ExportFormat::Step {
                ui.checkbox(&mut self.include_reference, "Include reference bodies (PCB, parts)");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary(ui, &format!("Export {}…", ext(self.format).to_uppercase())).clicked() {
                        ev = ExportEvent::Export;
                    }
                    if widgets::button(ui, "Cancel").clicked() {
                        ev = ExportEvent::Cancel;
                    }
                });
            });
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ev = ExportEvent::Cancel;
        }
        ev
    }

    /// The file chooser request for this export.
    pub fn request(&self, doc_name: &str) -> crate::files::Request {
        let e = ext(self.format);
        crate::files::Request::save(
            &format!("Export {}", e.to_uppercase()),
            &e.to_uppercase(),
            e,
            &format!("{doc_name}.{e}"),
        )
    }

    /// Export to `path` in the background.
    pub fn start(
        &self,
        path: std::path::PathBuf,
        data: DocData,
        blobs: BlobStore,
        library: Library,
    ) -> Receiver<Result<String, String>> {
        spawn(
            self.format,
            path,
            ExportOptions {
                profile: Some(self.profile.clone()),
                orient_to_plate: self.orient,
                include_reference: self.include_reference,
            },
            data,
            blobs,
            library,
        )
    }
}

pub fn spawn(
    format: ExportFormat,
    path: std::path::PathBuf,
    opts: ExportOptions,
    data: DocData,
    blobs: BlobStore,
    library: Library,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _kernel = crate::worker::kernel_lock();
        let mut doc = Document::new(&data.name);
        doc.data = data;
        doc.blobs = blobs;
        doc.library = library;
        let r = doc
            .export(format, &path, &opts)
            .map(|_| format!("Exported {}", path.display()))
            .map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    rx
}
