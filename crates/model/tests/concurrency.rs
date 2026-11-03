//! Concurrency stress (see "Concurrency contract" in docs/ENGINE.md).
//!
//! 1. Several threads, each with its own documents, regenerate, mesh, compute
//!    hidden-line drawings, export and re-import at the same time; every result must
//!    match a serial reference.
//! 2. One set of shapes shared by every thread: meshing (which rewrites the
//!    triangulation stored inside the shared topology), STL export, poly HLR,
//!    booleans, measurement and STEP round trips all run on the same `Shape`s.
//!
//! `CW_STRESS_ROUNDS=n` multiplies the work (default 2).

mod common;

use easycad_model::document::{Document, ExportFormat, ExportOptions};
use easycad_model::drawing::{PaperOrientation, PaperSize, Sheet, ViewOrientation};
use easycad_model::occt::Shape;
use easycad_model::process::CubeFace;
use easycad_model::regen::MeshQuality;
use glam::{DVec2, DVec3};

fn rounds() -> usize {
    std::env::var("CW_STRESS_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2)
}

/// Bodies' (name, volume) and solid count of a regenerated enclosure.
fn summary(doc: &mut Document) -> (Vec<(String, f64)>, usize) {
    let out = doc.regenerate().clone();
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let vols = out.bodies.iter().map(|b| (b.name.clone(), b.shape().volume().unwrap())).collect();
    let solids = out.bodies.iter().filter(|b| !b.reference).map(|b| b.shape().solids().unwrap().len()).sum();
    (vols, solids)
}

fn wall(t: usize, r: usize) -> String {
    format!("{}", 1.8 + 0.1 * ((t + r) % 4) as f64)
}

#[test]
fn parallel_documents_regen_mesh_hlr_export() {
    let dir = tempfile::tempdir().unwrap();
    let threads = 6;
    let rounds = rounds();
    // serial reference per wall value / profile
    let mut reference = std::collections::HashMap::new();
    for k in 0..4 {
        for fdm in [false, true] {
            let (mut doc, _) = common::enclosure_doc();
            doc.set_param("wall", wall(k, 0).as_str()).unwrap();
            if fdm {
                doc.set_active_profile("FDM 0.4 generic").unwrap();
            }
            reference.insert((k, fdm), summary(&mut doc));
        }
    }
    let reference = &reference;
    std::thread::scope(|sc| {
        for t in 0..threads {
            let dir = dir.path().to_path_buf();
            sc.spawn(move || {
                for r in 0..rounds {
                    let fdm = t % 2 == 1;
                    let (mut doc, _) = common::enclosure_doc();
                    doc.set_param("wall", wall(t, r).as_str()).unwrap();
                    if fdm {
                        doc.set_active_profile("FDM 0.4 generic").unwrap();
                    }
                    let (vols, solids) = summary(&mut doc);
                    let (rvols, rsolids) = &reference[&((t + r) % 4, fdm)];
                    assert_eq!(solids, *rsolids, "thread {t}");
                    for ((n, v), (rn, rv)) in vols.iter().zip(rvols) {
                        assert_eq!(n, rn);
                        assert!((v - rv).abs() < 1e-6 * rv.max(1.0), "thread {t} {n}: {v} vs serial {rv}");
                    }
                    let out = doc.regenerate().clone();
                    for b in &out.bodies {
                        for q in [MeshQuality::Preview, MeshQuality::Display, MeshQuality::Final] {
                            assert!(!b.mesh_quality(q).unwrap().indices.is_empty());
                        }
                    }
                    let d = doc.add_drawing("S", Sheet::new(PaperSize::A3, PaperOrientation::Landscape));
                    doc.edit_drawing(d, "v", |dr| {
                        let v = dr.add_view("F", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 150.0), 1.0);
                        dr.view_mut(v).unwrap().hidden_lines = true;
                        dr.add_view("P", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(250.0, 150.0), 1.0);
                        let p = dr.add_view("Q", ViewOrientation::Face(CubeFace::PosX), DVec2::new(330.0, 80.0), 1.0);
                        dr.view_mut(p).unwrap().exact = false;
                    })
                    .unwrap();
                    let res = doc.drawing_result(d).unwrap();
                    assert!(res.views.iter().all(|v| v.error.is_none() && !v.edges.is_empty()));
                    let base = dir.join(format!("t{t}r{r}"));
                    for (fmt, ext) in [(ExportFormat::Step, "step"), (ExportFormat::Stl, "stl"), (ExportFormat::ThreeMf, "3mf")] {
                        doc.export(fmt, &base.with_extension(ext), &ExportOptions::default()).unwrap();
                    }
                    doc.export_drawing(d, &base.with_extension("svg")).unwrap();
                    doc.export_drawing(d, &base.with_extension("pdf")).unwrap();
                    let back = Shape::read_step(&base.with_extension("step")).unwrap();
                    assert_eq!(back.solids().unwrap().len(), solids, "thread {t}");
                    // re-import as an assembly and regenerate a document from it
                    let data = std::fs::read(base.with_extension("step")).unwrap();
                    let mut doc2 = Document::new("re");
                    doc2.import_step("enc", data, true);
                    assert!(doc2.regenerate().errors().is_empty());
                }
            });
        }
    });
}

/// Shapes shared between threads: the triangulation cache inside the topology is
/// rewritten by every mesh call while other threads read it.
#[test]
fn shared_shapes_across_threads() {
    let dir = tempfile::tempdir().unwrap();
    let (mut doc, _) = common::enclosure_doc();
    let bodies = doc.regenerate().bodies.clone();
    let shapes: Vec<Shape> = bodies.iter().map(|b| b.shape().clone()).collect();
    let vols: Vec<f64> = shapes.iter().map(|s| s.volume().unwrap()).collect();
    let mids: Vec<f64> = shapes.iter().map(|s| s.bbox().unwrap().center().z).collect();
    let mids = &mids;
    let rounds = rounds() * 3;
    let shapes = &shapes;
    let vols = &vols;
    std::thread::scope(|sc| {
        for t in 0..8usize {
            let dir = dir.path().to_path_buf();
            sc.spawn(move || {
                for r in 0..rounds {
                    let s = &shapes[(t + r) % shapes.len()];
                    let v = vols[(t + r) % shapes.len()];
                    match (t + r) % 8 {
                        0 | 1 => {
                            let (l, a) = [MeshQuality::Preview, MeshQuality::Display, MeshQuality::Final][r % 3].deflection();
                            assert!(!s.tessellate(l, a).unwrap().indices.is_empty());
                        }
                        2 => s.write_stl(&dir.join(format!("{t}_{r}.stl")), 0.05, false).unwrap(),
                        3 => {
                            let (h, _) = s.hlr(DVec3::new(0.3, -1.0, 0.4), DVec3::X, r % 2 == 0, 0.05).unwrap();
                            assert!(!h.is_empty());
                        }
                        4 => {
                            let c = Shape::cylinder(DVec3::new(0.0, 0.0, -5.0), DVec3::Z, 3.0, 100.0).unwrap();
                            let cut = s.cut(&c).unwrap();
                            assert!(cut.volume().unwrap() < v);
                        }
                        5 => {
                            let back = Shape::read_step_str(&s.to_step_string().unwrap()).unwrap();
                            assert!((back.volume().unwrap() - v).abs() < 1e-3 * v);
                        }
                        6 => {
                            assert!(!s.faces().unwrap().is_empty() && !s.edges().unwrap().is_empty());
                            assert!(s.is_valid());
                            s.bbox().unwrap();
                        }
                        _ => {
                            let sec = s.section(DVec3::new(0.0, 0.0, mids[(t + r) % shapes.len()]), DVec3::Z).unwrap();
                            assert!(sec.area().unwrap() > 0.0);
                        }
                    }
                }
            });
        }
    });
}
