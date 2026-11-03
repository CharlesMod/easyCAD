//! Overhang analysis on known shapes.

use easycad_model::occt::Shape;
use easycad_model::overhang::{analyze, overhang_angle, OverhangClass, OverhangReport};
use easycad_model::process::ProcessProfile;
use glam::DVec3;

fn report(s: &Shape, up: DVec3) -> OverhangReport {
    let m = s.tessellate(0.05, 12.0).unwrap();
    analyze(&m, up, ProcessProfile::fdm_generic().critical_overhang_deg(), None)
}

/// The face whose outward normal is closest to `n`.
fn face_along(s: &Shape, n: DVec3) -> u32 {
    s.faces().unwrap().into_iter().max_by(|a, b| a.normal.dot(n).total_cmp(&b.normal.dot(n))).unwrap().index
}

#[test]
fn critical_angle_comes_from_the_process() {
    assert_eq!(ProcessProfile::fdm_generic().critical_overhang_deg(), 45.0);
    assert!(ProcessProfile::sla_generic().critical_overhang_deg() > 45.0);
    assert_eq!(overhang_angle(DVec3::NEG_Z, DVec3::Z), 90.0);
    assert_eq!(overhang_angle(DVec3::X, DVec3::Z), 0.0);
    assert_eq!(overhang_angle(DVec3::Z, DVec3::Z), 0.0);
}

#[test]
fn a_box_has_no_overhangs_and_its_bottom_rests_on_the_plate() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(20.0, 10.0, 5.0)).unwrap();
    let r = report(&b, DVec3::Z);
    assert_eq!(r.faces.len(), 6);
    assert!(r.faces.iter().all(|f| f.class == OverhangClass::Ok), "{:?}", r.faces);
    assert_eq!(r.support_area(), 0.0);
    assert_eq!(r.overhang_area(), 0.0);
    let bottom = r.face(face_along(&b, DVec3::NEG_Z)).unwrap();
    assert!(bottom.on_plate);
    assert!((bottom.area - 200.0).abs() < 1e-3);
    assert_eq!(r.faces.iter().filter(|f| f.on_plate).count(), 1);
    // on its side (+Y up) the -Y face is on the plate instead
    let r = report(&b, DVec3::Y);
    assert!(r.face(face_along(&b, DVec3::NEG_Y)).unwrap().on_plate);
    assert!(r.faces.iter().all(|f| f.class == OverhangClass::Ok));
    // upside down: the top is on the plate, still nothing to support
    let r = report(&b, DVec3::NEG_Z);
    assert!(r.face(face_along(&b, DVec3::Z)).unwrap().on_plate);
    assert_eq!(r.support_area(), 0.0);
}

#[test]
fn a_ceiling_above_the_plate_needs_support() {
    // a T: a 30 mm wide cap on a 10 mm post; the cap's underside hangs at z = 10
    let post = Shape::make_box(DVec3::new(10.0, 0.0, 0.0), DVec3::new(10.0, 10.0, 10.0)).unwrap();
    let cap = Shape::make_box(DVec3::new(0.0, 0.0, 10.0), DVec3::new(30.0, 10.0, 5.0)).unwrap();
    let t = post.fuse(&cap).unwrap().unify().unwrap();
    let r = report(&t, DVec3::Z);
    let support: Vec<_> = r.of_class(OverhangClass::Support).collect();
    assert!(!support.is_empty());
    for f in &support {
        assert!((f.max_angle - 90.0).abs() < 1e-3, "{f:?}");
        assert!(!f.on_plate);
    }
    // two 10×10 ledges either side of the post
    assert!((r.support_area() - 200.0).abs() < 1e-3, "{}", r.support_area());
    // the post's foot is on the plate and not an overhang
    let plate: Vec<_> = r.faces.iter().filter(|f| f.on_plate).collect();
    assert_eq!(plate.len(), 1);
    assert_eq!(plate[0].class, OverhangClass::Ok);
    assert!((plate[0].area - 100.0).abs() < 1e-3);
}

#[test]
fn a_tilted_box_flags_its_30_and_60_degree_faces() {
    // turned 30° about Y: its +X face leans out 30° past vertical (prints, flagged),
    // its bottom 60° (needs support at 45°); it stands on an edge, nothing on the plate
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(20.0, 10.0, 10.0)).unwrap().rotated(DVec3::ZERO, DVec3::Y, 30.0).unwrap();
    let r = report(&b, DVec3::Z);
    let n30 = DVec3::new(30f64.to_radians().cos(), 0.0, -30f64.to_radians().sin());
    let n60 = DVec3::new(-30f64.to_radians().sin(), 0.0, -30f64.to_radians().cos());
    let f30 = r.face(face_along(&b, n30)).unwrap();
    assert_eq!(f30.class, OverhangClass::Overhang, "{f30:?}");
    assert!((f30.max_angle - 30.0).abs() < 1e-3, "{f30:?}");
    assert!((f30.overhang_area - 100.0).abs() < 1e-3);
    let f60 = r.face(face_along(&b, n60)).unwrap();
    assert_eq!(f60.class, OverhangClass::Support, "{f60:?}");
    assert!((f60.max_angle - 60.0).abs() < 1e-3);
    assert!((f60.support_area - 200.0).abs() < 1e-3);
    assert!(r.faces.iter().all(|f| !f.on_plate));
    assert_eq!(r.of_class(OverhangClass::Ok).count(), 4);
    // with a 65° critical angle (resin) the bottom only overhangs
    let m = b.tessellate(0.05, 12.0).unwrap();
    let r = analyze(&m, DVec3::Z, 65.0, None);
    assert_eq!(r.face(face_along(&b, n60)).unwrap().class, OverhangClass::Overhang);
}

#[test]
fn a_lying_cylinder_needs_support_on_its_lower_quarter() {
    // axis along X, lying on the plate on a line: the lateral face leans out from
    // 0° (sides) to 90° (bottom); beyond 45° is the quarter facing down
    let (r0, l) = (5.0, 20.0);
    let c = Shape::cylinder(DVec3::new(0.0, 0.0, r0), DVec3::X, r0, l).unwrap();
    let rep = report(&c, DVec3::Z);
    let side = rep.faces.iter().max_by(|a, b| a.area.total_cmp(&b.area)).unwrap();
    let lateral = 2.0 * std::f64::consts::PI * r0 * l;
    assert!((side.area - lateral).abs() / lateral < 0.01, "{side:?}");
    assert_eq!(side.class, OverhangClass::Support);
    assert!(!side.on_plate, "a line contact is not a face on the plate");
    assert!(side.max_angle > 85.0, "{side:?}");
    for a in [side.support_area, side.overhang_area] {
        assert!((a / lateral - 0.25).abs() < 0.02, "{} of {lateral}", a);
    }
    // the end caps are walls
    assert_eq!(rep.of_class(OverhangClass::Ok).count(), 2);
    // standing on an end cap (+X up) it has no overhangs
    let rep = report(&c, DVec3::X);
    assert_eq!(rep.support_area() + rep.overhang_area(), 0.0);
    assert_eq!(rep.faces.iter().filter(|f| f.on_plate).count(), 1);
    // per-vertex angles for the heat map: the plate cap is negative
    let m = c.tessellate(0.05, 12.0).unwrap();
    assert_eq!(rep.vertex_angles.len(), m.positions.len());
    assert!(rep.vertex_angles.iter().any(|a| *a < 0.0));
}
