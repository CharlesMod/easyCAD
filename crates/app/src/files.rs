//! File choosers. The system dialog (rfd: the XDG portal on Linux) is tried first;
//! when there is none (no D-Bus session or portal, a remote or headless session),
//! rfd returns at once with nothing, which used to make Save / Export silently do
//! nothing. Then an in-app path prompt takes over (`PathPrompt`), prefilled with
//! the last folder used. `CASEWORK_FILE_DIALOG=builtin` forces the prompt.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Save,
    OpenFile,
    OpenFolder,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub title: String,
    pub kind: Kind,
    /// (label, extensions without dot)
    pub filters: Vec<(String, Vec<String>)>,
    /// Suggested file name (Save) or empty.
    pub file_name: String,
}

impl Request {
    pub fn save(title: &str, filter: &str, ext: &str, file_name: &str) -> Request {
        Request {
            title: title.into(),
            kind: Kind::Save,
            filters: vec![(filter.into(), vec![ext.into()])],
            file_name: file_name.into(),
        }
    }
    pub fn open(title: &str, filter: &str, exts: &[&str]) -> Request {
        Request {
            title: title.into(),
            kind: Kind::OpenFile,
            filters: vec![(filter.into(), exts.iter().map(|e| e.to_string()).collect())],
            file_name: String::new(),
        }
    }
    pub fn folder(title: &str) -> Request {
        Request {
            title: title.into(),
            kind: Kind::OpenFolder,
            filters: Vec::new(),
            file_name: String::new(),
        }
    }
}

pub enum Pick {
    Path(PathBuf),
    Cancelled,
    /// No system dialog could be shown: ask in the app instead.
    Unavailable,
}

/// Whether a system dialog can plausibly open (cheap environment check).
pub fn native_available() -> bool {
    if let Ok(v) = std::env::var("CASEWORK_FILE_DIALOG") {
        return v != "builtin";
    }
    if cfg!(target_os = "linux") {
        let bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|d| Path::new(&d).join("bus").exists());
        return bus;
    }
    true
}

/// Show the system dialog. A dialog that "returns nothing" faster than a person
/// could cancel it never opened.
pub fn native(req: &Request) -> Pick {
    if !native_available() {
        return Pick::Unavailable;
    }
    let mut d = rfd::FileDialog::new().set_title(&req.title);
    for (label, exts) in &req.filters {
        d = d.add_filter(label, exts);
    }
    if let Some(dir) = last_dir() {
        d = d.set_directory(dir);
    }
    if !req.file_name.is_empty() {
        d = d.set_file_name(&req.file_name);
    }
    let t0 = Instant::now();
    let r = match req.kind {
        Kind::Save => d.save_file(),
        Kind::OpenFile => d.pick_file(),
        Kind::OpenFolder => d.pick_folder(),
    };
    match r {
        Some(p) => {
            remember_dir(&p);
            Pick::Path(p)
        }
        None if t0.elapsed() < Duration::from_millis(250) => Pick::Unavailable,
        None => Pick::Cancelled,
    }
}

/// The folder the last chosen path was in (per user), else ~/Documents, else ~.
pub fn last_dir() -> Option<PathBuf> {
    let s = crate::settings::Settings::load();
    s.last_dir
        .filter(|d| d.is_dir())
        .or_else(|| home().map(|h| h.join("Documents")).filter(|d| d.is_dir()))
        .or_else(home)
}

pub fn remember_dir(p: &Path) {
    let dir = if p.is_dir() && p.extension().is_none() {
        Some(p.to_path_buf())
    } else {
        p.parent().map(Path::to_path_buf)
    };
    if let Some(d) = dir {
        let mut s = crate::settings::Settings::load();
        if s.last_dir.as_deref() != Some(d.as_path()) {
            s.last_dir = Some(d);
            s.save();
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Expand a leading `~` and make the path absolute against the last folder.
pub fn resolve(text: &str) -> Option<PathBuf> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let p = if let Some(rest) = t.strip_prefix("~/") {
        home()?.join(rest)
    } else if t == "~" {
        home()?
    } else {
        PathBuf::from(t)
    };
    Some(if p.is_absolute() {
        p
    } else {
        last_dir().unwrap_or_default().join(p)
    })
}

/// The in-app fallback: a path field with a check line under it.
pub struct PathPrompt {
    pub req: Request,
    pub text: String,
    pub error: Option<String>,
    pub focus: bool,
}

pub enum PromptEvent {
    None,
    Cancel,
    Chosen(PathBuf),
}

impl PathPrompt {
    pub fn new(req: Request) -> PathPrompt {
        let dir = last_dir().unwrap_or_default();
        let text = if req.file_name.is_empty() {
            format!("{}/", dir.display())
        } else {
            dir.join(&req.file_name).display().to_string()
        };
        PathPrompt {
            req,
            text,
            error: None,
            focus: true,
        }
    }

    /// Validate the typed path for this request.
    pub fn check(&self) -> Result<PathBuf, String> {
        let p = resolve(&self.text).ok_or("type a path")?;
        match self.req.kind {
            Kind::Save => {
                let parent = p.parent().ok_or("not a file path")?;
                if !parent.is_dir() {
                    return Err(format!("folder {} does not exist", parent.display()));
                }
                if p.is_dir() && p.extension().is_none_or(|e| e != "cwk") {
                    return Err("that is a folder; add a file name".into());
                }
                Ok(p)
            }
            Kind::OpenFile => {
                if !p.is_file() {
                    return Err("no such file".into());
                }
                Ok(p)
            }
            Kind::OpenFolder => {
                if !p.is_dir() {
                    return Err("no such folder".into());
                }
                Ok(p)
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> PromptEvent {
        use crate::theme;
        let mut ev = PromptEvent::None;
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::unique("path_prompt_dim"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(110));
                ui.allocate_rect(screen, egui::Sense::click());
            });
        egui::Area::new(egui::Id::unique("path_prompt"))
            .order(egui::Order::Tooltip)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(screen.center())
            .show(ctx, |ui| {
                crate::widgets::card_frame()
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        ui.label(egui::RichText::new(&self.req.title).strong().size(14.0));
                        ui.label(crate::widgets::muted(
                            "No system file dialog is available here. Type a path (~ works).",
                        ));
                        ui.add_space(6.0);
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.text)
                                .font(theme::mono(12.5))
                                .desired_width(f32::INFINITY),
                        );
                        if self.focus {
                            r.request_focus();
                            self.focus = false;
                        }
                        if r.changed() {
                            self.error = None;
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let status = self.check();
                        match (&status, &self.error) {
                            (_, Some(e)) => {
                                ui.label(egui::RichText::new(e).size(11.5).color(theme::ORANGE_SOFT));
                            }
                            (Err(e), None) => {
                                ui.label(crate::widgets::muted(e.as_str()));
                            }
                            (Ok(p), None) if self.req.kind == Kind::Save && p.exists() => {
                                ui.label(
                                    egui::RichText::new("exists: it will be replaced")
                                        .size(11.5)
                                        .color(theme::ORANGE_SOFT),
                                );
                            }
                            (Ok(_), None) => {
                                ui.label(crate::widgets::muted("ok"));
                            }
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let verb = match self.req.kind {
                                    Kind::Save => "Save",
                                    _ => "Open",
                                };
                                let ok = crate::widgets::primary(ui, &format!("  {verb}  ")).clicked();
                                if crate::widgets::button(ui, "Cancel").clicked() {
                                    ev = PromptEvent::Cancel;
                                }
                                if ok || enter {
                                    match &status {
                                        Ok(p) => {
                                            remember_dir(p);
                                            ev = PromptEvent::Chosen(p.clone());
                                        }
                                        Err(e) => {
                                            self.error = Some(e.clone());
                                            self.focus = true;
                                        }
                                    }
                                }
                            });
                        });
                    });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ev = PromptEvent::Cancel;
        }
        if !matches!(ev, PromptEvent::None) {
            self.error = None;
        }
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_expands_home_and_relative() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(resolve("~/a.pdf").unwrap(), PathBuf::from(&home).join("a.pdf"));
        }
        assert!(resolve("  ").is_none());
        assert!(resolve("/tmp/x.pdf").unwrap().is_absolute());
        assert!(resolve("x.pdf").unwrap().is_absolute() || last_dir().is_none());
    }

    #[test]
    fn prompt_checks_the_folder() {
        let mut p = PathPrompt::new(Request::save("Export", "PDF", "pdf", "a.pdf"));
        p.text = "/definitely/not/here/a.pdf".into();
        assert!(p.check().is_err());
        p.text = std::env::temp_dir().join("a.pdf").display().to_string();
        assert!(p.check().is_ok());
        let mut o = PathPrompt::new(Request::open("Import", "STEP", &["step"]));
        o.text = "/definitely/not/here.step".into();
        assert!(o.check().is_err());
    }
}
