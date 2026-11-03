//! Assembly BOM window: indented (per parent) or flat (rolled up), made vs
//! purchased, CSV export.

use easycad_model::assembly::{AsmBomRow, AssemblyBom, BomKind};
use egui::{vec2, Margin, RichText, Ui};

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;

#[derive(Default)]
pub struct BomUi {
    pub flat: bool,
    /// (model id, BOM) computed for the shown assembly.
    pub cache: Option<(u64, AssemblyBom)>,
}

fn kind_text(k: &BomKind) -> (String, String) {
    match k {
        BomKind::Made { process } => ("made".into(), process.clone()),
        BomKind::Purchased { source, part, size, vendor, part_number } => {
            let mut s = part.clone();
            if !size.is_empty() {
                s += &format!(" {size}");
            }
            if let Some(v) = vendor {
                s += &format!(" · {v}");
            }
            if let Some(p) = part_number {
                s += &format!(" · {p}");
            }
            (format!("purchased ({source})"), s)
        }
        BomKind::Assembly => ("assembly".into(), String::new()),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// CSV text of a BOM (indented: level column and per-parent quantity).
pub fn csv(rows: &[AsmBomRow], flat: bool) -> String {
    let mut out = String::from(if flat {
        "item,name,qty,kind,process / part\n"
    } else {
        "level,name,qty per parent,total,kind,process / part\n"
    });
    for (i, r) in rows.iter().enumerate() {
        let (k, d) = kind_text(&r.kind);
        if flat {
            out += &format!("{},{},{},{},{}\n", i + 1, csv_field(&r.name), r.total, csv_field(&k), csv_field(&d));
        } else {
            out += &format!("{},{},{},{},{},{}\n", r.level, csv_field(&r.name), r.qty, r.total, csv_field(&k), csv_field(&d));
        }
    }
    out
}

impl EasyCadApp {
    pub fn open_bom(&mut self) {
        if self.model.as_ref().and_then(|m| m.asm.as_ref()).is_none() {
            self.warn("The assembly BOM needs components; the parts list of a single part is on the drawing");
            return;
        }
        self.asm.bom = Some(BomUi::default());
    }

    fn bom_now(&mut self) -> Option<AssemblyBom> {
        let m = self.model.clone()?;
        let a = m.asm.as_ref()?;
        let st = self.asm.bom.as_mut()?;
        if st.cache.as_ref().is_none_or(|c| c.0 != m.id) {
            st.cache = Some((m.id, easycad_model::assembly::bom(&m.data, &a.result)));
        }
        st.cache.as_ref().map(|c| c.1.clone())
    }
}

pub fn window(app: &mut EasyCadApp, ui: &mut Ui) {
    if app.asm.bom.is_none() {
        return;
    }
    let Some(bom) = app.bom_now() else { return };
    let mut open = true;
    let mut flat = app.asm.bom.as_ref().map(|b| b.flat).unwrap_or(false);
    let mut export = false;
    egui::Window::new("Bill of materials")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .default_pos(app.viewport_rect.center() - vec2(280.0, 200.0))
        .default_width(560.0)
        .frame(widgets::card_frame().inner_margin(Margin::same(14)))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                if widgets::pill(ui, "Indented", !flat).on_hover_text("The tree, quantity per parent").clicked() {
                    flat = false;
                }
                if widgets::pill(ui, "Flat", flat).on_hover_text("One line per part, quantities rolled up").clicked() {
                    flat = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, "Export CSV…").clicked() {
                        export = true;
                    }
                });
            });
            ui.add_space(6.0);
            let rows = if flat { &bom.flat } else { &bom.indented };
            let made = rows.iter().filter(|r| matches!(r.kind, BomKind::Made { .. })).count();
            let bought = rows.iter().filter(|r| matches!(r.kind, BomKind::Purchased { .. })).count();
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                egui::Grid::new("bom_grid").num_columns(5).striped(true).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
                    for h in [if flat { "#" } else { "" }, "Name", "Qty", "Made / bought", "Process / part"] {
                        ui.label(RichText::new(h).size(11.0).color(theme::TEXT_LABEL));
                    }
                    ui.end_row();
                    for (i, r) in rows.iter().enumerate() {
                        let (k, d) = kind_text(&r.kind);
                        ui.label(RichText::new(if flat { format!("{}", i + 1) } else { String::new() }).font(theme::mono(11.0)).color(theme::TEXT_MUTED));
                        let indent = if flat { String::new() } else { "    ".repeat(r.level) };
                        ui.label(RichText::new(format!("{indent}{}", r.name)).size(12.5));
                        let q = if flat { format!("{}", r.total) } else if r.total != r.qty { format!("{} (×{} in all)", r.qty, r.total) } else { format!("{}", r.qty) };
                        ui.label(RichText::new(q).font(theme::mono(12.0)));
                        let col = match r.kind {
                            BomKind::Made { .. } => theme::ACCENT_LINK,
                            BomKind::Purchased { .. } => theme::GREEN,
                            BomKind::Assembly => theme::TEXT_DIM,
                        };
                        ui.label(RichText::new(k).size(12.0).color(col));
                        ui.label(RichText::new(d).size(12.0).color(theme::TEXT_DIM));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(widgets::muted(format!("{made} made · {bought} purchased line{} · design masters (hidden, out of the BOM) are left out", if bought == 1 { "" } else { "s" })));
        });
    if let Some(b) = app.asm.bom.as_mut() {
        b.flat = flat;
    }
    if !open {
        app.asm.bom = None;
    }
    if export {
        let rows = if flat { bom.flat.clone() } else { bom.indented.clone() };
        let name = format!("{}-bom.csv", app.doc.data.name);
        let req = crate::files::Request::save("Export BOM", "CSV", "csv", &name);
        app.ask_path(req, move |app, p| match std::fs::write(&p, csv(&rows, flat)) {
            Ok(()) => app.toast(format!("BOM written to {}", p.display())),
            Err(e) => app.warn(format!("BOM export failed: {e}")),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_and_levels() {
        let rows = vec![
            AsmBomRow { level: 0, component: None, name: "Base".into(), qty: 1, total: 1, kind: BomKind::Made { process: "FDM, 0.4".into() } },
            AsmBomRow { level: 1, component: None, name: "Screw \"M3\"".into(), qty: 4, total: 8, kind: BomKind::Purchased { source: "lib".into(), part: "ISO 7380".into(), size: "M3x8".into(), vendor: None, part_number: None } },
        ];
        let c = csv(&rows, false);
        assert!(c.contains("0,Base,1,1,made,\"FDM, 0.4\""), "{c}");
        assert!(c.contains("1,\"Screw \"\"M3\"\"\",4,8,purchased (lib),ISO 7380 M3x8"), "{c}");
        assert!(csv(&rows, true).starts_with("item,"));
    }
}
