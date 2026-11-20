//! Planar regions: picking sectors of a ring and cutting them, and the user's soap3 sketch.

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::sketch::Sketch;
use glam::DVec2;
use std::f64::consts::PI;

fn v(x: f64, y: f64) -> DVec2 {
    DVec2::new(x, y)
}

fn vol(doc: &mut Document, b: BodyId) -> f64 {
    doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap()
}

#[test]
fn cut_two_thirds_of_a_ring_leaves_one_third() {
    let mut doc = Document::new("Ring");
    // ring body
    let mut rs = Sketch::new("Ring", Plane::XY);
    rs.circle(v(0.0, 0.0), 9.8);
    rs.circle(v(0.0, 0.0), 11.4);
    let rs = doc.add_sketch(rs);
    let body = BodyId::of(doc.add_feature(
        "Ring",
        FeatureKind::Extrude { sketch: rs, profiles: ProfileSel::All, distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let ring_vol = PI * (11.4f64.powi(2) - 9.8f64.powi(2)) * 5.0;
    assert!((vol(&mut doc, body) - ring_vol).abs() < 1e-3);
    // cutter sketch: ring + 3 radial lines at 120 degrees
    let mut cs = Sketch::new("Cut", Plane::XY);
    cs.circle(v(0.0, 0.0), 9.8);
    cs.circle(v(0.0, 0.0), 11.4);
    for ang in [0.0f64, 120.0, 240.0] {
        let d = v(ang.to_radians().cos(), ang.to_radians().sin());
        cs.line(d * 9.0, d * 13.0);
    }
    let regions = cs.regions();
    assert_eq!(regions.len(), 4);
    let mut sectors: Vec<_> = regions.iter().filter(|r| r.holes.is_empty() && r.area < 100.0).collect();
    assert_eq!(sectors.len(), 3);
    sectors.sort_by(|a, b| a.sample.to_angle().total_cmp(&b.sample.to_angle()));
    let pick = vec![sectors[0].sample, sectors[1].sample];
    let cs = doc.add_sketch(cs);
    doc.add_feature(
        "Cut",
        FeatureKind::Extrude { sketch: cs, profiles: ProfileSel::Containing(pick), distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(body) },
    );
    let left = vol(&mut doc, body);
    assert!((left - ring_vol / 3.0).abs() < 1e-3, "{left} vs {}", ring_vol / 3.0);
    let shape = doc.regenerate().body_by_id(body).unwrap().shape().clone();
    assert_eq!(shape.count(easycad_model::occt::ShapeKind::Solid), 1);
}

#[test]
fn all_matches_the_old_fill() {
    // ring + island: All extrudes the ring only (the disc is a hole, as before)
    let mut s = Sketch::new("S", Plane::XY);
    s.circle(v(0.0, 0.0), 9.8);
    s.circle(v(0.0, 0.0), 11.4);
    let mut doc = Document::new("A");
    let sid = doc.add_sketch(s);
    let b = BodyId::of(doc.add_feature(
        "E",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::All, distance: 2.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    assert!((vol(&mut doc, b) - PI * (11.4f64.powi(2) - 9.8f64.powi(2)) * 2.0).abs() < 1e-3);
}

#[test]
fn soap3_sketch4_regions() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/soap3.ecad");
    let doc = Document::load_from_dir(&dir).unwrap();
    let sk = doc.data.sketch(SketchId(7)).expect("sketch 4").clone();
    let regions = sk.regions();
    // three sectors of the r22.8 disc (vertical line + line from the centre) and the
    // ring-sector between circle and the big arc; the stubs past the arc/circle bound nothing
    let mut a: Vec<f64> = regions.iter().map(|r| r.area).collect();
    a.sort_by(|x, y| x.total_cmp(y));
    let disc = PI * 22.8 * 22.8;
    let discs: f64 = regions.iter().filter(|r| r.area > 200.0).map(|r| r.area).sum();
    assert_eq!(regions.len(), 4, "{a:?}");
    assert!((discs - disc).abs() < 1e-3, "{a:?}");
    assert!(a[0] < 200.0 && a[0] > 150.0, "ring sector {a:?}");
    for r in &regions {
        assert!(r.contains(r.sample));
    }
    // every sample resolves back to its own region
    for (i, r) in regions.iter().enumerate() {
        assert_eq!(easycad_model::regions::region_at(&regions, r.sample), Some(i));
    }
}

#[test]
fn union_of_adjacent_sectors_has_no_internal_seam() {
    use easycad_model::regions::union_profiles;
    let mut cs = Sketch::new("Cut", Plane::XY);
    cs.circle(v(0.0, 0.0), 9.8);
    cs.circle(v(0.0, 0.0), 11.4);
    for ang in [0.0f64, 120.0, 240.0] {
        let d = v(ang.to_radians().cos(), ang.to_radians().sin());
        cs.line(d * 9.0, d * 13.0);
    }
    let regions = cs.regions();
    let sectors: Vec<_> = regions.iter().filter(|r| r.holes.is_empty() && r.area < 100.0).collect();
    let u = union_profiles(&[sectors[0], sectors[1]]).unwrap();
    assert_eq!(u.len(), 1);
    assert!(u[0].holes.is_empty());
    // two sectors = 2/3 of the ring, bounded by 2 radial lines + 2 arcs (shared radial edge gone)
    assert_eq!(u[0].outer.len(), 4, "{:?}", u[0].outer);
    let area = sectors[0].area + sectors[1].area;
    assert!((easycad_model::sketch::loop_area(&u[0].outer) - area).abs() < 1e-6);
    // all three sectors: the ring itself, outer circle + inner hole circle
    let u = union_profiles(&sectors).unwrap();
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].holes.len(), 1);
}
