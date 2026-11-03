//! End-to-end: build a printable enclosure through the model API, export it and
//! re-import the STEP.

use std::f64::consts::PI;
use std::io::Read;

use casework_model::document::Document;
use casework_model::export;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::occt::Shape;
use casework_model::params::Expr;
use casework_model::regen::FeatureStatus;
use casework_model::select::{EdgeSelector, FaceSelector};
use casework_model::sketch::{Constraint, Geom, Sketch};
use glam::{DVec2, DVec3};

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs()
}

/// Rectangle centred on the origin, sized by expressions, fully constrained.
fn centered_rect(name: &str, plane: Plane, w: &str, h: &str, guess: DVec2) -> Sketch {
    let mut s = Sketch::new(name, plane);
    let l = s.rect(-guess / 2.0, guess / 2.0);
    let corner = |s: &Sketch, e: usize| match s.entities[l[e].0 as usize].geom {
        Geom::Line { a, .. } => a,
        _ => unreachable!(),
    };
    let (p0, p2) = (corner(&s, 0), corner(&s, 2));
    s.constrain(Constraint::Length(l[0], w.into()));
    s.constrain(Constraint::Length(l[1], h.into()));
    let c = s.add_point(DVec2::ZERO);
    let diag = s.add_entity(Geom::Line { a: p0, b: p2 }, true);
    s.constrain(Constraint::Midpoint(c, diag));
    s.constrain(Constraint::Fix(c, DVec2::ZERO));
    s
}

#[test]
fn enclosure_end_to_end() {
    let mut doc = Document::new("Enclosure");
    for (n, v) in [("w", "80"), ("d", "60"), ("h", "30"), ("wall", "2"), ("r", "6"), ("split_z", "25"), ("boss_od", "7")] {
        doc.set_param(n, v).unwrap();
    }

    // Sketch + extrude 30
    let sk = centered_rect("Outline", Plane::XY, "w", "d", DVec2::new(50.0, 40.0));
    let mut probe = sk.clone();
    let rep = probe.solve(&doc.data.params.evaluate().unwrap()).unwrap();
    assert!(rep.converged && rep.dof == 0, "{rep:?}");
    let sk = doc.add_sketch(sk);
    let body_f = doc.add_feature(
        "Body",
        FeatureKind::Extrude {
            sketch: sk,
            profiles: ProfileSel::All,
            distance: "h".into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let base_id = BodyId::of(body_f);
    let v = doc.regenerate().body_by_id(base_id).unwrap().shape().volume().unwrap();
    assert!(close(v, 80.0 * 60.0 * 30.0, 1e-9));

    // Fillet the vertical edges, r = 6
    doc.add_feature(
        "Corners",
        FeatureKind::Fillet { body: None, edges: vec![EdgeSelector::Parallel { dir: DVec3::Z }], radius: "r".into(), auto_reduce: false },
    );
    let a_out = 80.0 * 60.0 - (4.0 - PI) * 36.0;
    let v = doc.regenerate().body_by_id(base_id).unwrap().shape().volume().unwrap();
    assert!(close(v, a_out * 30.0, 1e-6), "{v}");

    // Shell 2 mm, top face removed
    doc.add_feature(
        "Shell",
        FeatureKind::Shell { body: None, open_faces: vec![FaceSelector::top()], thickness: "wall".into(), outward: false },
    );
    let a_in = 76.0 * 56.0 - (4.0 - PI) * 16.0;
    let shell_v = a_out * 30.0 - a_in * 28.0;
    let v = doc.regenerate().body_by_id(base_id).unwrap().shape().volume().unwrap();
    assert!(close(v, shell_v, 1e-4), "{v} vs {shell_v}");

    // Split into base + lid at z = 25
    let split = doc.add_feature(
        "Split",
        FeatureKind::Split { body: Some(base_id), plane: Plane::XY.offset(25.0), new_name: "Lid".into() },
    );
    let lid_id = BodyId::of(split);
    {
        let out = doc.regenerate();
        assert!(out.errors().is_empty(), "{:?}", out.errors());
        let base = out.body_by_id(base_id).unwrap().shape();
        let lid = out.body_by_id(lid_id).unwrap().shape();
        assert!(close(base.volume().unwrap(), a_out * 25.0 - a_in * 23.0, 1e-4));
        assert!(close(lid.volume().unwrap(), (a_out - a_in) * 5.0, 1e-4));
        assert!(close(lid.bbox().unwrap().min.z, 25.0, 1e-6) && close(base.bbox().unwrap().max.z, 25.0, 1e-6));
    }

    // 4 screw bosses on the floor (z = 2)
    let pts: Vec<DVec2> = [(-30.0, -20.0), (30.0, -20.0), (30.0, 20.0), (-30.0, 20.0)].iter().map(|p| DVec2::new(p.0, p.1)).collect();
    let bosses = doc.add_feature(
        "Bosses",
        FeatureKind::ScrewBoss {
            body: Some(base_id),
            plane: Plane::XY.offset(2.0),
            points: pts.clone(),
            od: "boss_od".into(),
            hole_dia: 2.5.into(),
            height: 20.0.into(),
            hole_depth: None,
            base_fillet: 1.0.into(),
            draft_deg: 0.0.into(),
        },
    );
    let before = a_out * 25.0 - a_in * 23.0;
    let boss_v = 4.0 * PI * (3.5f64.powi(2) - 1.25f64.powi(2)) * 20.0;
    {
        let out = doc.regenerate();
        assert_eq!(out.report(bosses).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(bosses));
        let v = out.body_by_id(base_id).unwrap().shape().volume().unwrap() - before;
        // fillets add a little material on top of the plain bosses
        assert!(v > boss_v && v < boss_v + 40.0, "boss volume {v} vs {boss_v}");
    }

    // A rib at y = 10, mirrored to y = -10
    let mut rib_sk = Sketch::new("Rib", Plane::XY.offset(2.0));
    rib_sk.line(DVec2::new(-20.0, 10.0), DVec2::new(20.0, 10.0));
    let rib_sk = doc.add_sketch(rib_sk);
    let rib = doc.add_feature(
        "Rib",
        FeatureKind::Rib { sketch: rib_sk, thickness: 1.5.into(), height: 8.0.into(), draft_deg: 0.0.into(), flip: false, body: Some(base_id) },
    );
    doc.add_feature("Mirror rib", FeatureKind::Mirror { source: PatternSource::Features(vec![rib]), plane: Plane::XZ });

    // Vent slots through the floor, linear pattern ×3
    let mut slot = Sketch::new("Vent", Plane::XY);
    slot.rect(DVec2::new(-10.0, -6.0), DVec2::new(10.0, -4.0));
    let slot = doc.add_sketch(slot);
    let vent = doc.add_feature(
        "Vent",
        FeatureKind::Extrude {
            sketch: slot,
            profiles: ProfileSel::All,
            distance: 5.0.into(),
            symmetric: true,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::Cut,
            target: Some(base_id),
        },
    );
    doc.add_feature(
        "Vents",
        FeatureKind::Pattern {
            source: PatternSource::Features(vec![vent]),
            kind: PatternKind::Linear { dir: DVec3::Y, spacing: 5.0.into(), count: Expr::from("3") },
        },
    );

    let (base, lid) = {
        let out = doc.regenerate();
        assert!(out.errors().is_empty(), "{:?}", out.errors());
        let base = out.body_by_id(base_id).unwrap().clone();
        let lid = out.body_by_id(lid_id).unwrap().clone();
        (base, lid)
    };
    let bv = base.shape().volume().unwrap();
    let with_bosses = bv - before + 2.0 * 480.0 - 0.0;
    let expected = before + boss_v + 2.0 * 480.0 - 3.0 * 80.0;
    assert!((bv - expected).abs() < 40.0, "base {bv} vs {expected} ({with_bosses})");
    assert!(base.shape().is_valid() && lid.shape().is_valid());
    assert_eq!(base.shape().count(casework_model::occt::ShapeKind::Solid), 1);
    let bb = base.shape().bbox().unwrap();
    assert!(close(bb.size().x, 80.0, 1e-4) && close(bb.size().y, 60.0, 1e-4) && close(bb.size().z, 25.0, 1e-4));

    // display mesh with face ids
    let m = base.mesh().unwrap();
    assert!(m.indices.len() > 100 && m.indices.len() == m.tri_face.len());

    // Exports
    let dir = tempfile::tempdir().unwrap();
    let bodies = [&base, &lid];
    let step = dir.path().join("enclosure.step");
    export::write_step(&bodies, &step).unwrap();
    let stl = dir.path().join("enclosure.stl");
    export::write_stl(&bodies, &stl).unwrap();
    let tmf = dir.path().join("enclosure.3mf");
    export::write_3mf(&bodies, &tmf).unwrap();

    // STEP re-import: same solids, same volumes
    let back = Shape::read_step(&step).unwrap();
    let solids = back.solids().unwrap();
    assert_eq!(solids.len(), 2);
    let mut vols: Vec<f64> = solids.iter().map(|s| s.volume().unwrap()).collect();
    vols.sort_by(f64::total_cmp);
    let mut want = [bv, lid.shape().volume().unwrap()];
    want.sort_by(f64::total_cmp);
    assert!(close(vols[0], want[0], 1e-6) && close(vols[1], want[1], 1e-6), "{vols:?} vs {want:?}");

    // STL: size matches the triangle count, every triangle has a normal
    let data = std::fs::read(&stl).unwrap();
    let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
    assert!(n > 500);
    assert_eq!(data.len(), 84 + 50 * n);

    // 3MF: valid zip with the 3 required parts and a manifold mesh per object
    let mut z = zip::ZipArchive::new(std::fs::File::open(&tmf).unwrap()).unwrap();
    for name in ["[Content_Types].xml", "_rels/.rels", "3D/3dmodel.model"] {
        assert!(z.by_name(name).is_ok(), "missing {name}");
    }
    let mut model = String::new();
    z.by_name("3D/3dmodel.model").unwrap().read_to_string(&mut model).unwrap();
    assert!(model.contains("unit=\"millimeter\""));
    assert_eq!(model.matches("<object ").count(), 2);
    assert_eq!(model.matches("<triangle ").count(), n);
    for b in bodies {
        let w = export::weld(&b.shape().tessellate(export::PRINT_LIN_DEFL, export::PRINT_ANG_DEFL_DEG).unwrap());
        assert_eq!(export::open_edges(&w), 0, "{} mesh is not watertight", b.name);
    }

    // The document round-trips through its folder format and regenerates identically.
    let cwk = dir.path().join("Enclosure.cwk");
    doc.save_to_dir(&cwk).unwrap();
    let mut doc2 = Document::load_from_dir(&cwk).unwrap();
    assert_eq!(doc2.data, doc.data);
    let v2 = doc2.regenerate().body_by_id(base_id).unwrap().shape().volume().unwrap();
    assert!(close(v2, bv, 1e-9));
}
