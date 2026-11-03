//! Per-face STEP colours (AP214 `STYLED_ITEM` / `COLOUR_RGB` on faces), from a
//! committed fixture: `data/face_colors.step`, written once by OCCT XCAF with
//! `cargo test --release -p easycad-model --test step_colors -- --ignored`.
//!
//! The fixture has two parts. "Housing" is a 40 × 30 × 10 box, grey, with a red top
//! and a blue +X side. "Knob" is a Ø12 × 8 cylinder at (60, 15), green, with a
//! yellow top.

use easycad_model::document::Document;
use easycad_model::feature::BodyId;
use easycad_model::occt::{Shape, StepPart};
use easycad_model::regen::{MeshQuality, DEFAULT_COLOR};
use glam::DVec3;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/face_colors.step");
const GREY: [f32; 3] = [0.6, 0.6, 0.6];
const GREEN: [f32; 3] = [0.1, 0.6, 0.2];
const RED: [f32; 3] = [0.8, 0.1, 0.1];
const BLUE: [f32; 3] = [0.1, 0.2, 0.8];
const YELLOW: [f32; 3] = [0.9, 0.8, 0.1];

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02)
}

fn count(cs: &[[f32; 3]], c: [f32; 3]) -> usize {
    cs.iter().filter(|x| near(**x, c)).count()
}

/// Regenerates the fixture (run with `--ignored`; commit the result).
#[test]
#[ignore]
fn write_face_color_fixture() {
    let body = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 10.0)).unwrap();
    let knob = Shape::cylinder(DVec3::new(60.0, 15.0, 0.0), DVec3::Z, 6.0, 8.0).unwrap();
    let c = Shape::compound(&[&body, &knob]).unwrap();
    let faces = c.faces().unwrap();
    let fc: Vec<Option<[f32; 3]>> = faces
        .iter()
        .map(|f| {
            let n = f.normal;
            if f.centroid.x < 45.0 && n.z > 0.99 {
                Some(RED)
            } else if f.centroid.x < 45.0 && n.x > 0.99 {
                Some(BLUE)
            } else if f.centroid.x > 45.0 && n.z > 0.99 {
                Some(YELLOW)
            } else {
                None
            }
        })
        .collect();
    let parts = [StepPart { name: "Housing".into(), color: Some(GREY) }, StepPart { name: "Knob".into(), color: Some(GREEN) }];
    let step = c.to_step_string_styled(&parts, &fc).unwrap();
    std::fs::write(FIXTURE, step).unwrap();
}

#[test]
fn fixture_is_a_real_face_styled_ap214_file() {
    let txt = std::fs::read_to_string(FIXTURE).unwrap();
    assert!(txt.contains("AUTOMOTIVE_DESIGN"), "AP214");
    // part colours + three face colours, each a styled item with an RGB colour
    assert!(txt.matches("STYLED_ITEM(").count() >= 5, "{}", txt.matches("STYLED_ITEM(").count());
    assert!(txt.contains("COLOUR_RGB("));
    let styled = Shape::read_step_styled(&txt).unwrap();
    let faces = styled.shape.faces().unwrap();
    assert_eq!(faces.len(), 9);
    assert_eq!(styled.face_colors.len(), 9);
    for (f, c) in faces.iter().zip(&styled.face_colors) {
        let want = if f.centroid.x < 45.0 && f.normal.z > 0.99 {
            Some(RED)
        } else if f.centroid.x < 45.0 && f.normal.x > 0.99 {
            Some(BLUE)
        } else if f.centroid.x > 45.0 && f.normal.z > 0.99 {
            Some(YELLOW)
        } else {
            None
        };
        assert_eq!(c.is_some(), want.is_some(), "face at {:?}", f.centroid);
        if let (Some(c), Some(w)) = (c, want) {
            assert!(near(*c, w), "{c:?} vs {w:?}");
        }
    }
    let colors: Vec<_> = styled.parts.iter().map(|p| p.color.unwrap()).collect();
    assert!(near(colors[0], GREY) && near(colors[1], GREEN), "{colors:?}");

    // OCCT writes face colours as OVER_RIDING_STYLED_ITEMs; other CAD systems
    // write plain STYLED_ITEMs on the faces. Both read the same.
    let plain: String = txt
        .lines()
        .map(|l| match l.split_once("OVER_RIDING_STYLED_ITEM('overriding color',") {
            Some((head, rest)) => format!("{head}STYLED_ITEM('color',{})", &rest[..rest.rfind(',').unwrap()]) + ");",
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!plain.contains("OVER_RIDING") && plain.matches("STYLED_ITEM(").count() == 5);
    let again = Shape::read_step_styled(&plain).unwrap();
    assert_eq!(again.face_colors, styled.face_colors);
}

#[test]
fn imported_face_colors_reach_the_mesh() {
    let data = std::fs::read(FIXTURE).unwrap();
    // as one reference body
    let mut doc = Document::new("Colours");
    let (_, f) = doc.import_step("face_colors", data.clone(), true);
    let out = doc.regenerate().clone();
    let b = out.body_by_id(BodyId::of(f)).unwrap();
    let fc = b.face_colors().unwrap();
    assert_eq!(fc.len(), 9);
    assert_eq!((count(&fc, RED), count(&fc, BLUE), count(&fc, YELLOW)), (1, 1, 1));
    assert_eq!((count(&fc, GREY), count(&fc, GREEN)), (4, 2), "unstyled faces keep their part's colour");
    assert!(!fc.contains(&DEFAULT_COLOR));
    // every triangle of the housing top is red, of the knob top yellow
    let m = b.mesh_quality(MeshQuality::Display).unwrap();
    let vc = b.mesh_colors(MeshQuality::Display).unwrap();
    assert_eq!(vc.len(), m.positions.len());
    let (mut red, mut yellow) = (0, 0);
    for t in &m.indices {
        let ps: Vec<DVec3> = t.iter().map(|v| DVec3::from(m.positions[*v as usize].map(f64::from))).collect();
        let c = (ps[0] + ps[1] + ps[2]) / 3.0;
        let flat_at = |z: f64| ps.iter().all(|p| (p.z - z).abs() < 1e-4);
        if c.x < 40.0 && flat_at(10.0) {
            assert!(t.iter().all(|v| near(vc[*v as usize], RED)));
            red += 1;
        }
        if c.x > 50.0 && flat_at(8.0) {
            assert!(t.iter().all(|v| near(vc[*v as usize], YELLOW)));
            yellow += 1;
        }
    }
    assert!(red >= 2 && yellow >= 2, "{red} {yellow}");

    // as solids: one body per part, each with its own face colours
    let mut doc2 = Document::new("Solids");
    doc2.import_step("face_colors", data, false);
    let out2 = doc2.regenerate().clone();
    let housing = out2.body("Housing").unwrap().face_colors().unwrap();
    assert_eq!((housing.len(), count(&housing, RED), count(&housing, BLUE), count(&housing, GREY)), (6, 1, 1, 4));
    let knob = out2.body("Knob").unwrap().face_colors().unwrap();
    assert_eq!((knob.len(), count(&knob, YELLOW), count(&knob, GREEN)), (3, 1, 2));

    // STEP export writes the face styles back
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.step");
    let refs: Vec<&easycad_model::regen::Body> = out.bodies.iter().collect();
    easycad_model::export::write_step(&refs, &p).unwrap();
    let back = Shape::read_step_styled(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let got: Vec<[f32; 3]> = back.face_colors.iter().flatten().copied().collect();
    assert_eq!((got.len(), count(&got, RED), count(&got, BLUE), count(&got, YELLOW)), (3, 1, 1, 1));
}
