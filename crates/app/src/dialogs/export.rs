//! Export dialog: format, "Export for: [process profile]", orient to plate, and
//! reference bodies for STEP. Assemblies add STEP assembly (product structure), 3MF
//! (an object per component, an item per occurrence) and STL per component, with
//! the process each component is made by. Runs off the UI thread on a copy of the
//! document.

use std::sync::mpsc::{channel, Receiver};

use casework_model::assembly::{AssemblyExportOptions, AssemblyFormat, ComponentId};
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
    /// Assembly export (`None`: one file of bodies, as for a single part).
    pub assembly: Option<AssemblyFormat>,
}

pub enum ExportEvent {
    None,
    Cancel,
    Export,
    /// Per-component process (`None` = the document's).
    SetProcess(ComponentId, Option<String>),
}

pub fn asm_label(f: AssemblyFormat) -> &'static str {
    match f {
        AssemblyFormat::Step => "STEP assembly",
        AssemblyFormat::ThreeMf => "3MF multi-object",
        AssemblyFormat::StlPerComponent => "STL per component",
    }
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
            assembly: crate::ui_assembly::is_assembly(d).then_some(AssemblyFormat::Step),
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
            let asm_doc = crate::ui_assembly::is_assembly(d);
            if asm_doc {
                ui.label(RichText::new("Assembly").color(theme::TEXT_DIM));
                ui.horizontal_wrapped(|ui| {
                    for f in [AssemblyFormat::Step, AssemblyFormat::ThreeMf, AssemblyFormat::StlPerComponent] {
                        if widgets::pill(ui, asm_label(f), self.assembly == Some(f)).clicked() {
                            self.assembly = Some(f);
                        }
                    }
                });
                ui.label(RichText::new("Or every body in one file").color(theme::TEXT_DIM));
            }
            ui.horizontal(|ui| {
                for (f, n) in [(ExportFormat::ThreeMf, "3MF"), (ExportFormat::Stl, "STL"), (ExportFormat::Step, "STEP")] {
                    if widgets::pill(ui, n, self.assembly.is_none() && self.format == f).clicked() {
                        self.format = f;
                        self.assembly = None;
                        self.suggest(d);
                    }
                }
            });
            if let Some(f) = self.assembly {
                ui.add_space(4.0);
                ui.label(widgets::muted(match f {
                    AssemblyFormat::Step => "One STEP with the product structure: components, instances, names and colours.",
                    AssemblyFormat::ThreeMf => "One 3MF: an object per component, an item per placed occurrence.",
                    AssemblyFormat::StlPerComponent => "A folder with one STL per component, each in its own frame (ready to slice).",
                }));
                ui.label(RichText::new("Made by (per component)").color(theme::TEXT_DIM));
                egui::Grid::new("export_procs").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    for c in d.components.iter().filter(|c| c.purchased.is_none() && !c.locked && !c.features.is_empty()) {
                        let used = d.all_occurrences().iter().any(|(_, o, _)| o.component == c.id && o.in_bom);
                        if !used {
                            continue;
                        }
                        ui.label(&c.name);
                        let cur = c.process.clone().unwrap_or_else(|| format!("{} (document)", d.active_profile));
                        egui::ComboBox::from_id_salt(("export_proc", c.id.0)).selected_text(cur).width(190.0).show_ui(ui, |ui| {
                            if ui.selectable_label(c.process.is_none(), format!("{} (document)", d.active_profile)).clicked() {
                                ev = ExportEvent::SetProcess(c.id, None);
                            }
                            for p in &d.processes {
                                if ui.selectable_label(c.process.as_deref() == Some(p.name.as_str()), &p.name).clicked() {
                                    ev = ExportEvent::SetProcess(c.id, Some(p.name.clone()));
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
                if f == AssemblyFormat::Step {
                    ui.checkbox(&mut self.include_reference, "Include reference components (PCB, parts)");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::primary(ui, &format!("Export {}…", asm_label(f))).clicked() {
                            ev = ExportEvent::Export;
                        }
                        if widgets::button(ui, "Cancel").clicked() {
                            ev = ExportEvent::Cancel;
                        }
                    });
                });
                return;
            }
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
        match self.assembly {
            Some(AssemblyFormat::StlPerComponent) => {
                return crate::files::Request::folder("Folder for one STL per component");
            }
            Some(AssemblyFormat::Step) => {
                return crate::files::Request::save("Export STEP assembly", "STEP", "step", &format!("{doc_name}.step"));
            }
            Some(AssemblyFormat::ThreeMf) => {
                return crate::files::Request::save("Export 3MF", "3MF", "3mf", &format!("{doc_name}.3mf"));
            }
            None => {}
        }
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
        if let Some(f) = self.assembly {
            let opts = AssemblyExportOptions { include_reference: self.include_reference, include_hidden: false };
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let mut doc = Document::new(&data.name);
                doc.data = data;
                doc.blobs = blobs;
                doc.library = library;
                let r = doc
                    .export_assembly(f, &path, &opts)
                    .map(|files| match files.as_slice() {
                        [one] => format!("Exported {}", one.display()),
                        many => format!("Exported {} files to {}", many.len(), path.display()),
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(r);
            });
            return rx;
        }
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
