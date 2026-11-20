//! History-strip selection and deletion: click chips to select, Del / Backspace
//! deletes the selection as one undo step, asking first when other items depend on it.

use crate::app::EasyCadApp;
use easycad_model::document::DocData;
use easycad_model::feature::{FeatureId, SketchId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistItem {
    Feature(FeatureId),
    Sketch(SketchId),
}

/// A pending "delete with dependents?" question.
#[derive(Clone, Debug)]
pub struct DeleteAsk {
    pub items: Vec<HistItem>,
    pub dependents: Vec<HistItem>,
}

pub fn item_name(d: &DocData, it: HistItem) -> String {
    match it {
        HistItem::Feature(f) => d.features.iter().find(|x| x.id == f).map(|x| x.name.clone()).unwrap_or_default(),
        HistItem::Sketch(s) => d.sketch(s).map(|x| x.name.clone()).unwrap_or_default(),
    }
}

fn mentions_datum(v: &serde_json::Value, id: u32) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            m.get("Datum").and_then(|x| x.as_u64()) == Some(id as u64) || m.values().any(|x| mentions_datum(x, id))
        }
        serde_json::Value::Array(a) => a.iter().any(|x| mentions_datum(x, id)),
        _ => false,
    }
}

/// Items that refer (by sketch id, GeomRef::Datum or plane_src) to anything in `items`,
/// transitively, excluding `items` themselves.
pub fn dependents(d: &DocData, items: &[HistItem]) -> Vec<HistItem> {
    let mut all: Vec<HistItem> = items.to_vec();
    let mut out = Vec::new();
    loop {
        let mut found = Vec::new();
        for it in &all {
            match *it {
                HistItem::Sketch(s) => {
                    for f in &d.features {
                        if f.kind.sketch() == Some(s) {
                            found.push(HistItem::Feature(f.id));
                        }
                    }
                }
                HistItem::Feature(fid) => {
                    for f in d.features.iter().filter(|f| f.id != fid) {
                        if serde_json::to_value(&f.kind).map(|v| mentions_datum(&v, fid.0)).unwrap_or(false) {
                            found.push(HistItem::Feature(f.id));
                        }
                    }
                    for s in d.sketches.iter().filter(|s| s.sketch.plane_src == Some(fid)) {
                        found.push(HistItem::Sketch(s.id));
                    }
                }
            }
        }
        let new: Vec<HistItem> = found.into_iter().filter(|i| !all.contains(i)).collect();
        if new.is_empty() {
            break;
        }
        for n in new {
            if !all.contains(&n) {
                all.push(n);
                out.push(n);
            }
        }
    }
    out
}

impl EasyCadApp {
    pub fn is_hist_selected(&self, it: HistItem) -> bool {
        self.hist_sel.contains(&it)
    }

    /// Plain click replaces the selection; Ctrl / Shift toggles an item in it.
    pub fn hist_click(&mut self, it: HistItem, add: bool) {
        if add {
            if let Some(p) = self.hist_sel.iter().position(|x| *x == it) {
                self.hist_sel.remove(p);
            } else {
                self.hist_sel.push(it);
            }
        } else {
            self.hist_sel = vec![it];
        }
    }

    /// Start deleting the selection (or `items`): ask when something depends on it.
    pub fn hist_delete_request(&mut self, items: Vec<HistItem>) {
        let items: Vec<HistItem> = items
            .into_iter()
            .filter(|i| match i {
                HistItem::Feature(f) => self.view().features.iter().any(|x| x.id == *f),
                HistItem::Sketch(s) => self.view().sketch(*s).is_some(),
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let deps = dependents(self.view(), &items);
        if deps.is_empty() {
            self.hist_delete(&items);
        } else {
            self.delete_ask = Some(DeleteAsk { items, dependents: deps });
        }
    }

    pub fn hist_delete_selected(&mut self) {
        let items = self.hist_sel.clone();
        self.hist_delete_request(items);
    }

    /// Delete `items` as one undoable edit.
    pub fn hist_delete(&mut self, items: &[HistItem]) {
        if items.is_empty() {
            return;
        }
        // a dialog editing (or built on) a deleted item closes without applying
        if let Some(dlg) = &self.dialog {
            let editing_gone = dlg.editing.is_some_and(|e| items.contains(&HistItem::Feature(e)));
            let sk_gone = matches!(&dlg.body, crate::dialogs::Body::Extrude(e)
                if e.sketch.is_some_and(|s| items.contains(&HistItem::Sketch(s))));
            if editing_gone || sk_gone {
                self.close_dialog(false);
            }
        }
        let fids: Vec<FeatureId> = items.iter().filter_map(|i| if let HistItem::Feature(f) = i { Some(*f) } else { None }).collect();
        let sids: Vec<SketchId> = items.iter().filter_map(|i| if let HistItem::Sketch(s) = i { Some(*s) } else { None }).collect();
        let scope = self.scope();
        let label = if items.len() == 1 { "Delete from history" } else { "Delete from history (several)" };
        let r = crate::scope::edit(&mut self.doc, scope, label, |d| {
            let mut removed_before = 0usize;
            let r = d.rollback;
            let mut i = 0;
            d.features.retain(|f| {
                let gone = fids.contains(&f.id);
                if gone && r.is_some_and(|r| i < r) {
                    removed_before += 1;
                }
                i += 1;
                !gone
            });
            if let Some(r) = d.rollback {
                let nr = r - removed_before;
                d.rollback = if nr >= d.features.len() { None } else { Some(nr) };
            }
            d.hidden_datums.retain(|f| !fids.contains(f));
            d.sketches.retain(|s| !sids.contains(&s.id));
            // "delete only this": sketches on a deleted datum keep their stored plane
            for s in d.sketches.iter_mut() {
                if s.sketch.plane_src.is_some_and(|p| fids.contains(&p)) {
                    s.sketch.plane_src = None;
                }
            }
            Ok(())
        });
        match r {
            Ok(()) => {
                self.hist_sel.clear();
                self.selection.clear();
                self.request_model();
            }
            Err(e) => self.warn(e.to_string()),
        }
    }
}

/// The confirm popup for a delete that has dependents.
pub fn confirm_ui(app: &mut EasyCadApp, ctx: &egui::Context) {
    let Some(ask) = app.delete_ask.clone() else { return };
    let names = |v: &[HistItem], app: &EasyCadApp| v.iter().map(|i| item_name(app.view(), *i)).collect::<Vec<_>>().join(", ");
    let what = names(&ask.items, app);
    let n = ask.dependents.len();
    let mut action = None;
    egui::Window::new("Delete")
        .id(egui::Id::NULL.with("hist_delete_confirm"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("Delete {what} and {n} dependent item(s)?"));
            ui.label(egui::RichText::new(names(&ask.dependents, app)).size(11.0).color(crate::theme::TEXT_MUTED));
            ui.horizontal(|ui| {
                if ui.button("Delete all").clicked() {
                    action = Some(1);
                }
                if ui.button("Delete only this").clicked() {
                    action = Some(2);
                }
                if ui.button("Cancel").clicked() {
                    action = Some(0);
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(0);
    }
    match action {
        Some(1) => {
            let mut all = ask.items.clone();
            all.extend(ask.dependents.iter().copied());
            app.delete_ask = None;
            app.hist_delete(&all);
        }
        Some(2) => {
            app.delete_ask = None;
            app.hist_delete(&ask.items);
        }
        Some(_) => app.delete_ask = None,
        None => {}
    }
}
