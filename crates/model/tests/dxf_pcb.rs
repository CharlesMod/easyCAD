//! DXF import (inch drawing, rounded-rect outline + 4 mounting holes) and
//! standoffs generated from a PCB given as DXF or as STEP.

use std::f64::consts::PI;

use casework_model::document::Document;
use casework_model::dxf::DxfDrawing;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::occt::{DVec3, Shape};
use casework_model::regen::{profile_face, FeatureStatus};
use casework_model::sketch::{loop_area, ConstraintStatus, Sketch};
use glam::DVec2;

/// 3.0" × 2.0" board, 0.25" corner radius (LWPOLYLINE with bulges), four Ø0.125"
/// holes on their own layer, a silkscreen line, `$INSUNITS = 1` (inches).
fn board_dxf() -> String {
    let b = (22.5f64).to_radians().tan();
    let verts = [
        (0.25, 0.0, 0.0),
        (2.75, 0.0, b),
        (3.0, 0.25, 0.0),
        (3.0, 1.75, b),
        (2.75, 2.0, 0.0),
        (0.25, 2.0, b),
        (0.0, 1.75, 0.0),
        (0.0, 0.25, b),
    ];
    let mut s = String::from("  0\nSECTION\n  2\nHEADER\n  9\n$ACADVER\n  1\nAC1015\n  9\n$INSUNITS\n 70\n     1\n  0\nENDSEC\n");
    s += "  0\nSECTION\n  2\nENTITIES\n";
    s += "  0\nLWPOLYLINE\n  5\n2F\n  8\nBoardOutLine\n 90\n        8\n 70\n     1\n";
    for (x, y, bulge) in verts {
        s += &format!(" 10\n{x}\n 20\n{y}\n");
        if bulge != 0.0 {
            s += &format!(" 42\n{bulge}\n");
        }
    }
    for (x, y) in [(0.3, 0.3), (2.7, 0.3), (2.7, 1.7), (0.3, 1.7)] {
        s += &format!("  0\nCIRCLE\n  8\nHoles\n 10\n{x}\n 20\n{y}\n 30\n0.0\n 40\n0.0625\n");
    }
    s += "  0\nLINE\n  8\nSilk\n 10\n1.0\n 20\n1.0\n 11\n2.0\n 21\n1.0\n";
    s += "  0\nARC\n  8\nSilk\n 10\n1.5\n 20\n1.5\n 40\n0.1\n 50\n0.0\n 51\n90.0\n";
    s += "  0\nENDSEC\n  0\nEOF\n";
    s
}

const IN: f64 = 25.4;

fn board_area_mm2() -> f64 {
    let (w, h, r) = (3.0 * IN, 2.0 * IN, 0.25 * IN);
    w * h - (4.0 - PI) * r * r
}

#[test]
fn dxf_inches_layers_and_profiles() {
    let d = DxfDrawing::parse(&board_dxf()).unwrap();
    assert_eq!(d.insunits, 1);
    assert_eq!(d.scale_to_mm, 25.4);
    assert_eq!(d.layer_names(), vec!["BoardOutLine", "Holes", "Silk"]);
    assert_eq!(d.layers["BoardOutLine"].len(), 8);
    let holes = d.circles(&["Holes"]);
    assert_eq!(holes.len(), 4);
    for (c, r) in &holes {
        assert!((r * 2.0 - 0.125 * IN).abs() < 1e-9);
        assert!(c.x > 7.0 && c.y > 7.0);
    }
    assert!(holes.iter().any(|(c, _)| c.distance(DVec2::new(2.7 * IN, 1.7 * IN)) < 1e-9));

    // Outline layer alone: one rounded-rect profile, exact area.
    let sk = d.to_sketch("Board", Plane::XY, &["BoardOutLine"]);
    assert_eq!(sk.entities.len(), 8);
    assert_eq!(sk.constraints.len(), 8, "one coincident per shared corner");
    let rep = sk.analyze(&Default::default()).unwrap();
    assert!(rep.converged);
    assert_ne!(rep.status, ConstraintStatus::Conflicting);
    let pr = sk.profiles();
    assert_eq!(pr.profiles.len(), 1);
    assert!((loop_area(&pr.profiles[0].outer) - board_area_mm2()).abs() < 0.05);

    // Outline + holes: one profile with 4 holes; extrude to a 1.6 mm board.
    let sk = d.to_sketch("Board", Plane::XY, &["BoardOutLine", "Holes"]);
    let pr = sk.profiles();
    assert_eq!(pr.profiles.len(), 1);
    assert_eq!(pr.profiles[0].holes.len(), 4);
    let board = profile_face(&sk.plane, &pr.profiles[0]).unwrap().extrude(DVec3::new(0.0, 0.0, 1.6)).unwrap();
    let hole_a = PI * (0.0625 * IN).powi(2);
    let want = (board_area_mm2() - 4.0 * hole_a) * 1.6;
    assert!((board.volume().unwrap() - want).abs() / want < 1e-6, "{} vs {want}", board.volume().unwrap());
    let bb = board.bbox().unwrap();
    assert!((bb.size().x - 76.2).abs() < 1e-6 && (bb.size().y - 50.8).abs() < 1e-6);

    // The silkscreen layer holds an open chain (line) and an open arc.
    let silk = d.to_sketch("Silk", Plane::XY, &["Silk"]).profiles();
    assert!(silk.profiles.is_empty());
    assert_eq!(silk.open.len(), 2);
}

fn plate(doc: &mut Document) -> BodyId {
    let mut s = Sketch::new("Plate", Plane::XY);
    s.rect(DVec2::new(-50.0, -40.0), DVec2::new(50.0, 40.0));
    let s = doc.add_sketch(s);
    BodyId::of(doc.add_feature(
        "Plate",
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: 4.0.into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    ))
}

const BOSS_V: f64 = 4.0 * PI * (9.0 - 1.5625) * 5.0; // 4 × (Ø6 − Ø2.5) × 5

#[test]
fn standoffs_from_dxf() {
    let mut doc = Document::new("Standoffs");
    let plate = plate(&mut doc);
    let imp = doc.import_dxf("board.dxf", board_dxf().into_bytes());
    let f = doc.add_feature(
        "Standoffs",
        FeatureKind::StandoffsFromPcb {
            body: Some(plate),
            source: PcbSource::Dxf {
                import: imp,
                layers: vec!["Holes".into()],
                min_dia: 2.5,
                max_dia: 4.0,
                offset: DVec2::new(-1.5 * IN, -IN),
            },
            plane: Plane::XY.offset(4.0),
            od: 6.0.into(),
            hole_dia: 2.5.into(),
            height: 5.0.into(),
            base_fillet: 0.0.into(),
            draft_deg: 0.0.into(),
            offset: 0.0.into(),
            exclude: Vec::new(),
            gussets: None,
            lid_holes: None,
        },
    );
    let out = doc.regenerate();
    assert_eq!(out.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(f));
    let s = out.body_by_id(plate).unwrap().shape();
    assert!((s.volume().unwrap() - (100.0 * 80.0 * 4.0 + BOSS_V)).abs() < 1e-3);
    // bosses are centred on the (shifted) board holes
    let holes: Vec<DVec3> = s.faces().unwrap().iter().filter(|f| (f.radius - 1.25).abs() < 1e-6).map(|f| f.axis.unwrap().0).collect();
    assert_eq!(holes.len(), 4);
    assert!(holes.iter().any(|p| (p.x - 1.2 * IN).abs() < 1e-6 && (p.y - 0.7 * IN).abs() < 1e-6), "{holes:?}");
}

#[test]
fn standoffs_from_step_pcb() {
    // A fake EasyEDA board: 76.2 × 50.8 × 1.6 with four Ø3.175 holes, floating at z = 9.
    let mut board = Shape::make_box(DVec3::new(-1.5 * IN, -IN, 9.0), DVec3::new(3.0 * IN, 2.0 * IN, 1.6)).unwrap();
    for (x, y) in [(0.3, 0.3), (2.7, 0.3), (2.7, 1.7), (0.3, 1.7)] {
        let c = DVec3::new((x - 1.5) * IN, (y - 1.0) * IN, 8.0);
        board = board.cut(&Shape::cylinder(c, DVec3::Z, 0.0625 * IN, 5.0).unwrap()).unwrap();
    }
    // plus a component sitting on it (a connector) that must not confuse hole finding
    let part = Shape::make_box(DVec3::new(-10.0, -5.0, 10.6), DVec3::new(20.0, 10.0, 6.0)).unwrap();
    let pcb = Shape::compound(&[&board, &part]).unwrap();
    let step = pcb.to_step_string().unwrap();

    let mut doc = Document::new("PCB");
    let plate = plate(&mut doc);
    let (imp, _) = doc.import_step("pcb", step.into_bytes(), true);
    let f = doc.add_feature(
        "Standoffs",
        FeatureKind::StandoffsFromPcb {
            body: Some(plate),
            source: PcbSource::Step { import: imp, min_dia: 2.5, max_dia: 4.0 },
            plane: Plane::XY.offset(4.0),
            od: 6.0.into(),
            hole_dia: 2.5.into(),
            height: 5.0.into(),
            base_fillet: 0.5.into(),
            draft_deg: 1.0.into(),
            offset: 0.0.into(),
            exclude: Vec::new(),
            gussets: None,
            lid_holes: None,
        },
    );
    let out = doc.regenerate();
    assert!(matches!(out.report(f).unwrap().status, FeatureStatus::Ok), "{:?}", out.report(f));
    let pcb_body = out.bodies.iter().find(|b| b.reference).unwrap();
    assert_eq!(pcb_body.name, "pcb");
    let s = out.body_by_id(plate).unwrap().shape();
    let added = s.volume().unwrap() - 100.0 * 80.0 * 4.0;
    // drafted (1°) bosses are a little smaller than straight ones; fillets add a bit
    assert!(added > 0.9 * BOSS_V && added < BOSS_V, "{added} vs {BOSS_V}");
    assert!((s.bbox().unwrap().max.z - 9.0).abs() < 1e-6, "standoffs reach the board");
}
