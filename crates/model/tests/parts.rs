//! Parts library: generators (metric and imperial), hole rules applied as geometry,
//! InsertPart + AutoHole, BOM, and a local STEP library embedded into documents.

use std::f64::consts::PI;
use std::sync::Arc;

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::library::*;
use easycad_model::occt::{DVec3, Shape};
use easycad_model::regen::FeatureStatus;
use easycad_model::select::{EdgeSelector, FaceSelector};
use easycad_model::sketch::Sketch;
use glam::DVec2;

const IN: f64 = 25.4;

fn hex_area(s: f64) -> f64 {
    3f64.sqrt() / 2.0 * s * s
}

#[test]
fn metric_shcs_m3x10() {
    let p = BuiltinSource.build("iso4762", "M3x10").unwrap();
    let bb = p.shape.bbox().unwrap();
    assert!((bb.size().x - 5.5).abs() < 1e-3 && (bb.size().y - 5.5).abs() < 1e-3);
    assert!((bb.min.z + 10.0).abs() < 1e-6 && (bb.max.z - 3.0).abs() < 1e-6, "{bb:?}");
    let want = PI * 2.75f64.powi(2) * 3.0 - hex_area(2.5) * 1.8 + PI * 1.5f64.powi(2) * 10.0;
    assert!((p.shape.volume().unwrap() - want).abs() < 1e-3, "{} vs {want}", p.shape.volume().unwrap());
    assert!(p.shape.is_valid());
    assert_eq!(p.mate.thread.as_deref(), Some("M3"));
    assert_eq!(p.info.standard.as_deref(), Some("ISO 4762"));
    // hole rules via the part
    let h = p.hole(HoleChoice::Counterbore(Fit::Normal)).unwrap();
    assert_eq!((h.diameter, h.counterbore), (3.4, Some((6.5, 3.3))));
    assert_eq!(p.hole(HoleChoice::Clearance(Fit::Close)).unwrap().diameter, 3.2);
    assert_eq!(p.hole(HoleChoice::Clearance(Fit::Loose)).unwrap().diameter, 3.6);
    let tap = p.hole(HoleChoice::Tap).unwrap();
    assert_eq!((tap.diameter, tap.depth), (2.5, Some(10.0)));
}

#[test]
fn metric_nut_washer_insert() {
    let n = BuiltinSource.build("iso4032", "M3").unwrap();
    let bb = n.shape.bbox().unwrap();
    assert!((bb.size().y - 5.5).abs() < 1e-3 && (bb.size().x - 5.5 * 2.0 / 3f64.sqrt()).abs() < 1e-3);
    let want = (hex_area(5.5) - PI * 1.5f64.powi(2)) * 2.4;
    assert!((n.shape.volume().unwrap() - want).abs() < 1e-3);

    let w = BuiltinSource.build("iso7089", "M3").unwrap();
    assert!((w.shape.volume().unwrap() - PI * (3.5f64.powi(2) - 1.6f64.powi(2)) * 0.5).abs() < 1e-6);

    let i = BuiltinSource.build("insert_m", "M3").unwrap();
    let h = i.hole(HoleChoice::Insert).unwrap();
    assert_eq!((h.diameter, h.depth), (4.0, Some(6.7)));
    assert!((i.shape.bbox().unwrap().min.z + 5.7).abs() < 1e-6);
}

#[test]
fn imperial_shcs_and_nut_4_40() {
    let p = BuiltinSource.build("asme_shcs", "#4-40x1/2").unwrap();
    let bb = p.shape.bbox().unwrap();
    assert!((bb.size().x - 0.183 * IN).abs() < 1e-3);
    assert!((bb.size().z - (0.5 + 0.112) * IN).abs() < 1e-6, "{bb:?}");
    let clear = p.hole(HoleChoice::Clearance(Fit::Close)).unwrap();
    // ASME B18.2.8 close / normal / loose
    assert!((clear.diameter - 0.120 * IN).abs() < 1e-9);
    assert!((p.hole(HoleChoice::Clearance(Fit::Normal)).unwrap().diameter - 0.128 * IN).abs() < 1e-9);
    assert!((p.hole(HoleChoice::Clearance(Fit::Loose)).unwrap().diameter - 0.144 * IN).abs() < 1e-9);
    assert!((p.hole(HoleChoice::Tap).unwrap().diameter - 0.0890 * IN).abs() < 1e-9);

    let n = BuiltinSource.build("asme_hex_nut", "#4-40").unwrap();
    let bb = n.shape.bbox().unwrap();
    assert!((bb.size().y - 0.25 * IN).abs() < 1e-3 && (bb.size().z - 0.0938 * IN).abs() < 1e-6);

    let f = BuiltinSource.build("asme_flat", "1/4-20x3/4").unwrap();
    let bb = f.shape.bbox().unwrap();
    assert!((bb.max.z).abs() < 1e-6 && (bb.min.z + 0.75 * IN).abs() < 1e-6, "flat head is flush, overall length");
    let cs = f.hole(HoleChoice::Countersink(Fit::Normal)).unwrap();
    assert_eq!(cs.countersink.unwrap().1, 82.0);
}

#[test]
fn every_catalog_entry_builds_valid_solids() {
    let src = BuiltinSource;
    let mut n = 0;
    for info in src.parts() {
        let sizes = src.sizes(&info.id);
        assert!(!sizes.is_empty(), "{}", info.id);
        // first and last size of each table
        for size in [sizes.first().unwrap(), sizes.last().unwrap()] {
            let p = src.build(&info.id, size).unwrap_or_else(|e| panic!("{} {size}: {e}", info.id));
            assert!(p.shape.is_valid(), "{} {size}", info.id);
            assert!(p.shape.volume().unwrap() > 0.0, "{} {size}", info.id);
            n += 1;
        }
    }
    assert!(n >= 54, "{n}");
}

fn plate_doc() -> (Document, BodyId) {
    let mut doc = Document::new("Plate");
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::new(-20.0, -20.0), DVec2::new(20.0, 20.0));
    let s = doc.add_sketch(s);
    let f = doc.add_feature(
        "Plate",
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: 10.0.into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    (doc, BodyId::of(f))
}

#[test]
fn insert_part_with_auto_hole_and_bom() {
    let (mut doc, plate) = plate_doc();
    let lib = doc.library.clone();
    let insert = lib.search("heat set").into_iter().find(|p| p.id == "insert_m").unwrap();
    let r = doc.part_ref(&insert, "M3").unwrap();
    let f_ins = doc.add_feature(
        "Inserts",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![
                PlacementRef::PointOnFace { face: FaceSelector::top(), point: DVec3::new(10.0, 10.0, 7.0) },
                PlacementRef::PointOnFace { face: FaceSelector::top(), point: DVec3::new(-10.0, 10.0, 99.0) },
            ],
            flip: false,
            offset: 0.0.into(),
            auto_hole: Some(AutoHole { choice: HoleChoice::Insert, depth: None, fit: None }),
            target: Some(plate),
            threads: Default::default(),
        },
    );
    let shcs = BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
    let r = doc.part_ref(&shcs, "M3x10").unwrap();
    doc.add_feature(
        "Screw",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::Explicit { origin: DVec3::new(-10.0, -10.0, 10.0), axis: DVec3::Z }],
            flip: false,
            offset: Expr::from(0.0),
            auto_hole: Some(AutoHole { choice: HoleChoice::Counterbore(Fit::Normal), depth: None, fit: None }),
            target: Some(plate),
            threads: Default::default(),
        },
    );
    let out = doc.regenerate();
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let refs: Vec<_> = out.bodies.iter().filter(|b| b.reference).collect();
    assert_eq!(refs.len(), 3);
    for b in &refs {
        assert!(b.part.is_some());
    }
    // inserts sit flush with the top face, pointing up
    let ib = out.body_by_id(BodyId { feature: f_ins, index: 1 }).unwrap().shape().bbox().unwrap();
    assert!((ib.max.z - 10.0).abs() < 1e-6 && (ib.center().x + 10.0).abs() < 1e-6);
    // the screw head sits on the top face; the shank goes into the plate
    let sb = out.bodies.iter().find(|b| b.name.contains("M3x10")).unwrap().shape().bbox().unwrap();
    assert!((sb.max.z - 13.0).abs() < 1e-6 && (sb.min.z - 0.0).abs() < 1e-6);
    let v = out.body_by_id(plate).unwrap().shape().volume().unwrap();
    let insert_holes = 2.0 * PI * 4.0 * 6.7;
    let cbore = PI * 1.7f64.powi(2) * 10.0 + PI * (3.25f64.powi(2) - 1.7f64.powi(2)) * 3.3;
    assert!((v - (16000.0 - insert_holes - cbore)).abs() < 1e-3, "{v}");

    let bom = doc.bom();
    assert_eq!(bom.len(), 2);
    let ins = bom.iter().find(|l| l.part == "insert_m").unwrap();
    assert_eq!((ins.qty, ins.size.as_str()), (2, "M3"));
    assert_eq!(bom.iter().find(|l| l.part == "iso4762").unwrap().qty, 1);
}

#[test]
fn snap_to_circular_edge() {
    let (mut doc, plate) = plate_doc();
    // a Ø3.4 through hole to drop a screw into
    let mut s = Sketch::new("H", Plane::XY);
    s.circle(DVec2::new(5.0, 5.0), 1.7);
    let s = doc.add_sketch(s);
    doc.add_feature(
        "Hole",
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: 30.0.into(),
            symmetric: true,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::Cut,
            target: Some(plate),
        },
    );
    let shcs = BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
    let r = doc.part_ref(&shcs, "M3x8").unwrap();
    let f = doc.add_feature(
        "Screw",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::CircularEdge {
                edge: EdgeSelector::Circles { radius: 1.7, near: Some((DVec3::new(5.0, 5.0, 10.0), 0.1)) },
            }],
            flip: false,
            offset: 0.0.into(),
            auto_hole: None,
            target: Some(plate),
            threads: Default::default(),
        },
    );
    let out = doc.regenerate();
    assert_eq!(out.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(f));
    let bb = out.body_by_id(BodyId::of(f)).unwrap().shape().bbox().unwrap();
    assert!((bb.center().x - 5.0).abs() < 1e-6 && (bb.max.z - 13.0).abs() < 1e-6, "{bb:?}");
    // the bottom circular edge snaps with the axis pointing out of the bottom face
    doc.edit("flip edge", |d| {
        if let FeatureKind::InsertPart { placements, .. } = &mut d.feature_mut(f).unwrap().kind {
            placements[0] = PlacementRef::CircularEdge {
                edge: EdgeSelector::Circles { radius: 1.7, near: Some((DVec3::new(5.0, 5.0, 0.0), 0.1)) },
            };
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    let bb = out.body_by_id(BodyId::of(f)).unwrap().shape().bbox().unwrap();
    assert!((bb.min.z + 3.0).abs() < 1e-6 && (bb.max.z - 8.0).abs() < 1e-6, "{bb:?}");
}

#[test]
fn local_step_library_is_embedded_in_the_document() {
    let lib_dir = tempfile::tempdir().unwrap();
    let part = Shape::cylinder(DVec3::ZERO, DVec3::Z, 4.0, 6.0).unwrap();
    part.write_step(&lib_dir.path().join("knob.step")).unwrap();
    std::fs::write(
        lib_dir.path().join("library.json"),
        r#"{"parts":[{"id":"knob-6","file":"knob.step","name":"Rotary knob 8 mm","vendor":"ACME","part_number":"KN-8-6","tags":["knob","encoder"],"category":"other"}]}"#,
    )
    .unwrap();
    let src = Arc::new(LocalStepSource::open("shelf", lib_dir.path()).unwrap());
    assert_eq!(src.search("encoder").len(), 1);
    assert!(src.search("screw").is_empty());

    let (mut doc, plate) = plate_doc();
    doc.library.add(src.clone());
    let info = doc.library.search("knob acme").into_iter().next().unwrap();
    assert_eq!(info.source, "local:shelf");
    let r = doc.part_ref(&info, "-").unwrap();
    assert!(r.blob.is_some());
    doc.add_feature(
        "Knob",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::PointOnFace { face: FaceSelector::top(), point: DVec3::new(0.0, 0.0, 10.0) }],
            flip: false,
            offset: 0.0.into(),
            auto_hole: None,
            target: Some(plate),
            threads: Default::default(),
        },
    );
    let save = tempfile::tempdir().unwrap();
    let p = save.path().join("Plate.ecad");
    doc.save_to_dir(&p).unwrap();
    drop(lib_dir); // the library folder is gone, the document still regenerates
    let mut back = Document::load_from_dir(&p).unwrap();
    let out = back.regenerate();
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let knob = out.bodies.iter().find(|b| b.reference).unwrap();
    assert!((knob.shape().volume().unwrap() - PI * 16.0 * 6.0).abs() < 1e-6);
    assert_eq!(back.bom()[0].part_number.as_deref(), Some("KN-8-6"));
}

use easycad_model::params::Expr;

fn tris(s: &Shape) -> usize {
    s.tessellate(0.05, 12.0).unwrap().indices.len()
}

#[test]
fn modeled_threads_on_parts() {
    let b = BuiltinSource;
    // a screw: the threaded shank weighs less than the plain Ø3 cylinder
    let (plain, thr) = (b.build("iso4762", "M3x10").unwrap(), b.build_threaded("iso4762", "M3x10").unwrap());
    assert!(thr.shape.is_valid() && thr.shape.solids().unwrap().len() == 1);
    let (vp, vt) = (plain.shape.volume().unwrap(), thr.shape.volume().unwrap());
    assert!(vp - vt > 8.0 && vp - vt < 18.0, "{vp} {vt}");
    assert_eq!(thr.bbox_z(), plain.bbox_z(), "same length and head");
    assert_eq!(thr.modeled_thread().map(|t| t.pitch), Some(0.5));
    let (tp, tt) = (tris(&plain.shape), tris(&thr.shape));
    eprintln!("M3x10 SHCS: {tp} triangles cosmetic, {tt} modelled");
    assert!(tt < 40_000, "{tt}");
    // a nut, an insert and a standoff: threaded bores leave more material than the
    // cosmetic Ø d bore, and stay valid
    for (id, size) in [("iso4032", "M3"), ("insert_m", "M3"), ("standoff_m_mf", "M3x10")] {
        let (p, t) = (b.build(id, size).unwrap(), b.build_threaded(id, size).unwrap());
        assert!(t.shape.is_valid(), "{id}");
        let (vp, vt) = (p.shape.volume().unwrap(), t.shape.volume().unwrap());
        eprintln!("{id} {size}: {vp:.2} -> {vt:.2} mm³, {} triangles", tris(&t.shape));
        if id != "standoff_m_mf" {
            assert!(vt > vp, "{id}: {vp} {vt}");
        }
    }
    // inch
    let t = b.build_threaded("asme_shcs", "#4-40x1/4").unwrap();
    assert!(t.shape.is_valid());
    // thread-forming screws keep their cosmetic shank
    let (p, t) = (b.build("pt_pan", "M3x8").unwrap(), b.build_threaded("pt_pan", "M3x8").unwrap());
    assert!(t.modeled_thread().is_none() && (p.shape.volume().unwrap() - t.shape.volume().unwrap()).abs() < 1e-9);
}

trait BboxZ {
    fn bbox_z(&self) -> (i64, i64);
}
impl BboxZ for PartInstance {
    fn bbox_z(&self) -> (i64, i64) {
        let bb = self.shape.bbox().unwrap();
        ((bb.min.z * 1e4).round() as i64, (bb.max.z * 1e4).round() as i64)
    }
}

#[test]
fn modeled_tapped_hole() {
    let (mut doc, plate) = plate_doc();
    let shcs = BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
    let r = doc.part_ref(&shcs, "M3x8").unwrap();
    let screw = doc.add_feature(
        "Screw",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::Explicit { origin: DVec3::new(-10.0, -10.0, 10.0), axis: DVec3::Z }],
            flip: false,
            offset: Expr::from(0.0),
            auto_hole: Some(AutoHole { choice: HoleChoice::Tap, depth: None, fit: None }),
            target: Some(plate),
            threads: Threads::Cosmetic,
        },
    );
    let out = doc.regenerate();
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let v_plain = out.body_by_id(plate).unwrap().shape().volume().unwrap();
    assert!((16000.0 - v_plain - PI * 1.25f64.powi(2) * 8.0).abs() < 1e-3, "tap drill Ø2.5 x 8");
    doc.edit("model threads", |d| {
        if let FeatureKind::InsertPart { threads, .. } = &mut d.feature_mut(screw).unwrap().kind {
            *threads = Threads::Modeled;
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    let rep = out.report(screw).unwrap();
    assert_eq!(rep.status, FeatureStatus::Ok, "{rep:?}");
    let body = out.body_by_id(plate).unwrap().shape();
    assert!(body.is_valid());
    // the thread's void (core D1 = 2.46 plus the ridge out to Ø3) over 8 mm
    let removed = 16000.0 - body.volume().unwrap();
    let t = easycad_model::occt::Thread::parse("M3").unwrap();
    let core = PI * (t.minor() / 2.0).powi(2) * 8.0;
    assert!(removed > core + 3.0 && removed < PI * 1.5f64.powi(2) * 8.0, "{removed}");
    let screw_body = out.bodies.iter().find(|b| b.name.contains("M3x8")).unwrap();
    assert!(screw_body.shape().is_valid());
    eprintln!("tapped plate: {} triangles", tris(body));

    // a part without a standard thread warns and stays cosmetic
    let magnet = BuiltinSource.parts().into_iter().find(|p| p.category == Category::Magnet).unwrap();
    let size = BuiltinSource.sizes(&magnet.id)[0].clone();
    let r = doc.part_ref(&magnet, &size).unwrap();
    let m = doc.add_feature(
        "Magnet",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::Explicit { origin: DVec3::new(10.0, 10.0, 10.0), axis: DVec3::Z }],
            flip: false,
            offset: Expr::from(0.0),
            auto_hole: None,
            target: Some(plate),
            threads: Threads::Modeled,
        },
    );
    let out = doc.regenerate();
    assert!(matches!(out.report(m).unwrap().status, FeatureStatus::Warning(ref w) if w.contains("cosmetic")), "{:?}", out.report(m));
}
