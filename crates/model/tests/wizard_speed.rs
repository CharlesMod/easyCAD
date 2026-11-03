//! Regeneration speed of the PCB wizard's enclosure (the GUI's `New enclosure from
//! PCB` with defaults: FDM, tongue & groove, sample sensor-node board), saved from
//! `easycad_app::wizard::build` into `tests/data/wizard_*.ecad`.
//!
//! `cargo test -p easycad-model --release --test wizard_speed -- --nocapture`
//! prints per-feature times for a cold regen and for single parameter edits.

use std::path::Path;
use std::time::Instant;

use easycad_model::document::Document;

fn load(name: &str) -> Document {
    Document::load_from_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
}

fn table(doc: &mut Document, label: &str, ms: f64) {
    let r = doc.regenerate().clone();
    println!("{label}: {ms:.0} ms (recomputed {}, reused {})", r.recomputed, r.reused);
    for rep in &r.reports {
        let name = doc.data.feature(rep.feature).map(|f| f.name.clone()).unwrap_or_default();
        if !rep.cached {
            println!("    {:<28} {:>7.1} ms", name, rep.elapsed_ms);
        }
    }
}

fn timed(doc: &mut Document) -> f64 {
    let t = Instant::now();
    let r = doc.regenerate();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
fn wizard_enclosure_regen_times() {
    for file in ["wizard_step.ecad", "wizard_dxf.ecad"] {
        println!("== {file}");
        let mut best = [f64::INFINITY; 4];
        for round in 0..3 {
            let mut doc = load(file);
            let cold = timed(&mut doc);
            if round == 0 {
                table(&mut doc, "cold regen", cold);
            }
            doc.set_param("wall", "2.2").unwrap();
            let wall = timed(&mut doc);
            if round == 0 {
                table(&mut doc, "edit `wall`", wall);
            }
            doc.set_param("lid_split", "height - 12").unwrap();
            let split = timed(&mut doc);
            if round == 0 {
                table(&mut doc, "edit `lid_split`", split);
            }
            doc.undo();
            doc.undo();
            let undo = timed(&mut doc);
            for (b, v) in best.iter_mut().zip([cold, wall, split, undo]) {
                *b = b.min(v);
            }
        }
        println!("best of 3: cold {:.0} ms · `wall` edit {:.0} ms · `lid_split` edit {:.0} ms · undo ×2 {:.2} ms", best[0], best[1], best[2], best[3]);
    }
    // the assembly version (Enclosure master → Base + Lid in context, the board)
    let mut doc = load("wizard_step_asm.ecad");
    let t = Instant::now();
    assert!(doc.regenerate_assembly().errors().is_empty());
    let cold = t.elapsed().as_secs_f64() * 1000.0;
    doc.set_param("wall", "2.2").unwrap();
    let t = Instant::now();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let edit = t.elapsed().as_secs_f64() * 1000.0;
    println!("== assembly: cold {cold:.0} ms · `wall` edit {edit:.0} ms");
    for c in &r.components {
        println!("    {:<12} recomputed {} ({:.0} ms)", c.name, c.regen.recomputed, c.regen.reports.iter().filter(|x| !x.cached).map(|x| x.elapsed_ms).sum::<f64>());
    }
}
