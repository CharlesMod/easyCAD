//! `easycad --smoke <dir>`: exercise file I/O without dialogs, through the same
//! functions the dialogs call (save/open, import STEP/DXF, export STEP/STL/3MF,
//! drawing PDF/DXF/SVG). Returns a log, or the first failure.

use std::path::Path;
use std::time::Duration;

use easycad_model::document::{ExportFormat, ExportOptions};

use crate::app::{EasyCadApp, Mode};
use crate::worker::Worker;

fn check_file(p: &Path, min: u64, log: &mut Vec<String>) -> Result<(), String> {
    let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let n = bytes.len() as u64;
    if n < min {
        return Err(format!("{} is only {n} bytes", p.display()));
    }
    let magic: &[u8] = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "pdf" => b"%PDF",
        "step" => b"ISO-10303-21",
        "3mf" => b"PK",
        "svg" => b"<?xml",
        _ => b"",
    };
    if !bytes.starts_with(magic) {
        return Err(format!(
            "{} does not look like a .{} file",
            p.display(),
            p.extension().and_then(|e| e.to_str()).unwrap_or("?")
        ));
    }
    log.push(format!("ok  {} ({n} bytes)", p.display()));
    Ok(())
}

fn settle(app: &mut EasyCadApp, what: &str) -> Result<(), String> {
    if !app.settle(Duration::from_secs(600)) {
        return Err(format!("{what}: regeneration timed out"));
    }
    Ok(())
}

pub fn run(dir: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut log = Vec::new();
    let mut app =
        EasyCadApp::with_parts(crate::sample::sensor_node(), Worker::spawn(|| {}), false);
    settle(&mut app, "sample")?;
    let m = app.model.clone().ok_or("no model")?;
    if !m.result.errors().is_empty() {
        return Err(format!("sample has errors: {:?}", m.result.errors()));
    }
    let bodies = m.bodies.len();
    log.push(format!(
        "ok  sample: {bodies} bodies, {} triangles",
        m.triangles()
    ));

    // save → reopen
    let saved = app.save_to(dir.join("sensor-node.ecad"))?;
    let before = app.doc.data.clone();
    app.open_path(&saved)?;
    settle(&mut app, "reopen")?;
    if app.doc.data != before {
        return Err("reopened document differs from the saved one".into());
    }
    if app.model.as_ref().map(|m| m.bodies.len()) != Some(bodies) {
        return Err("reopened document regenerates differently".into());
    }
    log.push(format!("ok  save + reopen {}", saved.display()));

    // import STEP (reference body) and DXF (sketch mode → finish)
    let step = dir.join("board.step");
    std::fs::write(&step, crate::sample::pcb_step()).map_err(|e| e.to_string())?;
    app.import_path(&step)?;
    settle(&mut app, "STEP import")?;
    let refs = app
        .model
        .as_ref()
        .map(|m| m.bodies.iter().filter(|b| b.body.reference).count())
        .unwrap_or(0);
    if refs < 2 {
        return Err(format!(
            "STEP import: expected 2 reference bodies, found {refs}"
        ));
    }
    log.push("ok  import STEP as a locked reference body".into());
    let dxf = dir.join("board.dxf");
    std::fs::write(&dxf, crate::sample::board_dxf()).map_err(|e| e.to_string())?;
    let sketches = app.doc.data.sketches.len();
    app.import_path(&dxf)?;
    if !matches!(app.mode, Mode::Sketch(_)) {
        return Err("DXF import did not open sketch mode".into());
    }
    app.finish_sketch();
    if app.doc.data.sketches.len() != sketches + 1 {
        return Err("DXF import did not add a sketch".into());
    }
    settle(&mut app, "DXF import")?;
    log.push("ok  import DXF into a sketch".into());

    // exports, as the Export dialog runs them (background thread, profile override)
    for (fmt, ext, profile, orient) in [
        (
            ExportFormat::Step,
            "step",
            "Injection molded (nominal)",
            false,
        ),
        (ExportFormat::Stl, "stl", "FDM 0.4 generic", true),
        (ExportFormat::ThreeMf, "3mf", "FDM 0.4 generic", true),
    ] {
        let p = dir.join(format!("sensor-node.{ext}"));
        let data = app.doc.data.clone();
        let rx = crate::dialogs::export::spawn(
            fmt,
            p.clone(),
            ExportOptions {
                profile: Some(profile.into()),
                orient_to_plate: orient,
                include_reference: false,
            },
            data,
            app.doc.blobs.clone(),
            app.doc.library.clone(),
        );
        rx.recv_timeout(Duration::from_secs(600))
            .map_err(|e| format!("{ext}: {e}"))??;
        check_file(&p, 1000, &mut log)?;
    }

    // drawing sheet: created and computed like the Drawing screen, then exported
    app.mode = Mode::Drawing(Box::default());
    crate::ui_drawing::sync(&mut app);
    settle(&mut app, "drawing")?;
    let (r, d) = match &app.mode {
        Mode::Drawing(dr) => (
            dr.result.clone().ok_or("drawing not computed")?,
            dr.draft.clone().ok_or("no drawing")?,
        ),
        _ => return Err("left drawing mode".into()),
    };
    if r.views.iter().any(|v| v.edges.is_empty()) {
        return Err("a drawing view has no edges".into());
    }
    for ext in ["pdf", "dxf", "svg"] {
        let p = dir.join(format!("sheet.{ext}"));
        std::fs::write(&p, crate::ui_drawing::export_bytes(&r, &d, ext))
            .map_err(|e| e.to_string())?;
        check_file(&p, 500, &mut log)?;
    }
    // the drawing travels with the document
    crate::ui_drawing::commit_now(&mut app).map_err(|e| e.to_string())?;
    let p = app.save_to(dir.join("with-drawing.ecad"))?;
    app.open_path(&p)?;
    if app.doc.data.drawings.is_empty() {
        return Err("drawing lost on save/reopen".into());
    }
    log.push("ok  drawing saved and reopened".into());

    // assembly: the wizard's Base + Lid + board, saved, reopened, exported three ways
    let step = dir.join("board.step");
    std::fs::write(&step, crate::sample::pcb_step()).map_err(|e| e.to_string())?;
    let board = crate::wizard::Board::read(&step)?;
    let doc = crate::wizard::build(&board, &crate::wizard::Settings::new("FDM 0.4 generic"))?;
    app.load(doc, None);
    settle(&mut app, "assembly")?;
    let p = app.save_to(dir.join("assembly.ecad"))?;
    app.open_path(&p)?;
    settle(&mut app, "assembly reopened")?;
    let placed = app.model.as_ref().map(|m| m.ctx.len()).unwrap_or(0);
    if placed < 3 {
        return Err(format!("assembly reopened with {placed} placed bodies"));
    }
    log.push(format!("ok  assembly saved and reopened ({placed} placed bodies)"));
    for f in [
        easycad_model::assembly::AssemblyFormat::Step,
        easycad_model::assembly::AssemblyFormat::ThreeMf,
        easycad_model::assembly::AssemblyFormat::StlPerComponent,
    ] {
        let mut ex = crate::dialogs::export::ExportDialog::new(&app.doc.data);
        ex.assembly = Some(f);
        ex.include_reference = true;
        let out = match f {
            easycad_model::assembly::AssemblyFormat::Step => dir.join("assembly.step"),
            easycad_model::assembly::AssemblyFormat::ThreeMf => dir.join("assembly.3mf"),
            easycad_model::assembly::AssemblyFormat::StlPerComponent => dir.join("stl"),
        };
        let rx = ex.start(out.clone(), app.doc.data.clone(), app.doc.blobs.clone(), app.doc.library.clone());
        let msg = rx
            .recv_timeout(Duration::from_secs(600))
            .map_err(|e| format!("{}: {e}", crate::dialogs::export::asm_label(f)))??;
        if out.is_dir() {
            for name in ["Base.stl", "Lid.stl"] {
                check_file(&out.join(name), 1000, &mut log)?;
            }
        } else {
            check_file(&out, 1000, &mut log)?;
        }
        log.push(format!("ok  {} ({msg})", crate::dialogs::export::asm_label(f)));
    }
    Ok(log)
}
