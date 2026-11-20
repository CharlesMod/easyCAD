//! Construction geometry: every plane/axis/point definition on a known model,
//! parametric updates, failure modes and serialization.
//!
//! Model: a box (0,0,0)..(40,30,20) from a sketch on XY, and a cylinder of radius 5
//! at (70,15) from z=0 to z=30 (a second body).

use easycad_model::datum::*;
use easycad_model::document::{DocData, Document};
use easycad_model::feature::{BoolOp, FeatureId, FeatureKind, ProfileSel, SketchId};
use easycad_model::geom::Plane;
use easycad_model::regen::{FeatureStatus, RegenResult};
use easycad_model::select::{EdgeKind, EdgeSelector, FaceSelector, Extremum};
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

const EPS: f64 = 1e-6;

fn extrude(doc: &mut Document, name: &str, s: SketchId, dist: &str) -> FeatureId {
    doc.add_feature(
        name,
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: dist.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    )
}

fn model() -> Document {
    let mut doc = Document::new("datums");
    let mut s = Sketch::new("Box", Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0));
    let s = doc.add_sketch(s);
    extrude(&mut doc, "Box", s, "20");
    let mut c = Sketch::new("Cyl", Plane::XY);
    c.circle(DVec2::new(70.0, 15.0), 5.0);
    let c = doc.add_sketch(c);
    extrude(&mut doc, "Cyl", c, "30");
    doc
}

fn top() -> GeomRef {
    GeomRef::Face(FaceSelector::Extreme { normal: DVec3::Z, which: Extremum::Max })
}
fn bottom() -> GeomRef {
    GeomRef::Face(FaceSelector::Extreme { normal: DVec3::NEG_Z, which: Extremum::Max })
}
fn right() -> GeomRef {
    GeomRef::Face(FaceSelector::Extreme { normal: DVec3::X, which: Extremum::Max })
}
fn cyl() -> GeomRef {
    GeomRef::Face(FaceSelector::Cylinders { radius: Some(5.0), axis: None })
}
fn line_edge(mid: DVec3, tan: DVec3) -> GeomRef {
    GeomRef::Edge(EdgeSelector::Hint { midpoint: mid, tangent: tan, kind: Some(EdgeKind::Line) })
}
fn x_edge() -> GeomRef {
    line_edge(DVec3::new(20.0, 0.0, 0.0), DVec3::X)
}
fn x_edge_top() -> GeomRef {
    line_edge(DVec3::new(20.0, 0.0, 20.0), DVec3::X)
}
fn z_edge() -> GeomRef {
    line_edge(DVec3::new(0.0, 0.0, 10.0), DVec3::Z)
}
fn y_edge_top() -> GeomRef {
    line_edge(DVec3::new(0.0, 15.0, 20.0), DVec3::Y)
}
fn vertex(x: f64, y: f64, z: f64) -> GeomRef {
    GeomRef::Vertex { hint: DVec3::new(x, y, z) }
}
fn circle_top() -> GeomRef {
    GeomRef::Edge(EdgeSelector::Circles { radius: 5.0, near: Some((DVec3::new(70.0, 15.0, 30.0), 1.0)) })
}

fn plane(doc: &mut Document, def: PlaneDef) -> (FeatureId, Result<Plane, FeatureStatus>) {
    let id = doc.add_feature("P", FeatureKind::DatumPlane { def, flip: false });
    let got = match get(doc.regenerate(), id) {
        Some(DatumGeom::Plane(p)) => Ok(p),
        Some(o) => panic!("not a plane: {o:?}"),
        None => Err(status(doc.regenerate(), id)),
    };
    (id, got)
}
fn get(res: &RegenResult, id: FeatureId) -> Option<DatumGeom> {
    res.datums.iter().find(|(f, _)| *f == id).map(|(_, g)| *g)
}
fn status(res: &RegenResult, id: FeatureId) -> FeatureStatus {
    res.reports.iter().find(|r| r.feature == id).unwrap().status.clone()
}
/// Fresh copy of the model per case so features do not accumulate.
fn p(def: PlaneDef) -> Plane {
    let mut d = model();
    match plane(&mut d, def) {
        (_, Ok(p)) => p,
        (_, Err(s)) => panic!("plane failed: {s:?}"),
    }
}
fn perr(def: PlaneDef) -> String {
    let mut d = model();
    match plane(&mut d, def) {
        (_, Ok(p)) => panic!("expected failure, got {p:?}"),
        (_, Err(FeatureStatus::Error(m))) => m,
        (_, Err(s)) => panic!("expected an Error status, got {s:?}"),
    }
}
fn axis(def: AxisDef) -> (DVec3, DVec3) {
    let mut d = model();
    let id = d.add_feature("A", FeatureKind::DatumAxis { def });
    match get(d.regenerate(), id) {
        Some(DatumGeom::Axis { origin, dir }) => (origin, dir),
        o => panic!("no axis: {o:?} {:?}", status(d.regenerate(), id)),
    }
}
fn point(def: PointDef) -> DVec3 {
    let mut d = model();
    let id = d.add_feature("Pt", FeatureKind::DatumPoint { def });
    match get(d.regenerate(), id) {
        Some(DatumGeom::Point(p)) => p,
        o => panic!("no point: {o:?} {:?}", status(d.regenerate(), id)),
    }
}

fn close(a: DVec3, b: DVec3) {
    assert!((a - b).length() < 1e-5, "{a:?} != {b:?}");
}
fn parallel(a: DVec3, b: DVec3) {
    assert!(a.normalize().cross(b.normalize()).length() < 1e-6, "{a:?} not parallel to {b:?}");
}
fn on(p: &Plane, q: DVec3) {
    assert!((q - p.origin).dot(p.normal).abs() < 1e-5, "{q:?} not on {p:?}");
}
fn frame_ok(p: &Plane) {
    assert!((p.normal.length() - 1.0).abs() < EPS && (p.x_dir.length() - 1.0).abs() < EPS && p.normal.dot(p.x_dir).abs() < EPS, "{p:?}");
}

// ---------------------------------------------------------------- planes

#[test]
fn offset_plane_keeps_the_base_normal() {
    let pl = p(PlaneDef::Offset { base: top(), distance: "5".into() });
    close(pl.normal, DVec3::Z);
    assert!((pl.origin.z - 25.0).abs() < EPS);
    // negative distance and a downward base face
    let pl = p(PlaneDef::Offset { base: bottom(), distance: "5".into() });
    close(pl.normal, DVec3::NEG_Z);
    assert!((pl.origin.z + 5.0).abs() < EPS, "along the outward normal: {pl:?}");
    // origin plane and an offset of an offset
    let pl = p(PlaneDef::Offset { base: GeomRef::Origin(OriginRef::XZ), distance: "7".into() });
    close(pl.normal, DVec3::NEG_Y);
    assert!((pl.origin.y + 7.0).abs() < EPS);
    frame_ok(&pl);
}

#[test]
fn flip_reverses_the_normal_only() {
    let mut d = model();
    let id = d.add_feature("P", FeatureKind::DatumPlane { def: PlaneDef::Offset { base: top(), distance: "5".into() }, flip: true });
    let Some(DatumGeom::Plane(pl)) = get(d.regenerate(), id) else { panic!() };
    close(pl.normal, DVec3::NEG_Z);
    assert!((pl.origin.z - 25.0).abs() < EPS);
    frame_ok(&pl);
}

#[test]
fn angle_plane_about_an_axis() {
    let x = GeomRef::Origin(OriginRef::XAxis);
    // default base: the plane through the axis and world Z; 0 deg is XZ
    let pl = p(PlaneDef::Angle { axis: x.clone(), base: None, angle: "0".into() });
    parallel(pl.normal, DVec3::Y);
    on(&pl, DVec3::new(5.0, 0.0, 9.0));
    let pl = p(PlaneDef::Angle { axis: x.clone(), base: None, angle: "90".into() });
    parallel(pl.normal, DVec3::Z);
    let pl = p(PlaneDef::Angle { axis: x.clone(), base: None, angle: "45".into() });
    let s = std::f64::consts::FRAC_1_SQRT_2;
    parallel(pl.normal, DVec3::new(0.0, s, s));
    assert!(pl.normal.dot(DVec3::X).abs() < EPS);
    on(&pl, DVec3::new(9.0, 0.0, 0.0));
    // explicit base: XY, angle measured from it, right-hand about +X
    let pl = p(PlaneDef::Angle { axis: x.clone(), base: Some(GeomRef::Origin(OriginRef::XY)), angle: "0".into() });
    close(pl.normal, DVec3::Z);
    let pl = p(PlaneDef::Angle { axis: x.clone(), base: Some(GeomRef::Origin(OriginRef::XY)), angle: "90".into() });
    close(pl.normal, DVec3::NEG_Y);
    // through a box edge: the plane contains it
    let pl = p(PlaneDef::Angle { axis: x_edge_top(), base: Some(top()), angle: "30".into() });
    on(&pl, DVec3::new(0.0, 0.0, 20.0));
    on(&pl, DVec3::new(40.0, 0.0, 20.0));
    assert!((pl.normal.dot(DVec3::Z) - 30f64.to_radians().cos()).abs() < 1e-6);
    // base perpendicular to the axis is undefined
    let m = perr(PlaneDef::Angle { axis: x, base: Some(GeomRef::Origin(OriginRef::YZ)), angle: "10".into() });
    assert!(m.contains("perpendicular"), "{m}");
}

#[test]
fn midplane_of_parallel_and_non_parallel_planes() {
    let pl = p(PlaneDef::Midplane { a: top(), b: bottom() });
    parallel(pl.normal, DVec3::Z);
    assert!((pl.origin.z - 10.0).abs() < EPS);
    // inclined: +X face (x = 40) and the top (z = 20) -> through their edge, normal X - Z
    let pl = p(PlaneDef::Midplane { a: right(), b: top() });
    close(pl.normal, DVec3::new(1.0, 0.0, -1.0).normalize());
    on(&pl, DVec3::new(40.0, 7.0, 20.0));
    on(&pl, DVec3::new(0.0, 3.0, -20.0 + 20.0 + 20.0 - 40.0)); // x - z = 20
    frame_ok(&pl);
    let m = perr(PlaneDef::Midplane { a: top(), b: top() });
    assert!(m.contains("coincide"), "{m}");
    let m = perr(PlaneDef::Midplane { a: top(), b: GeomRef::Origin(OriginRef::XAxis) });
    assert!(m.contains("plane is needed"), "{m}");
}

#[test]
fn plane_through_two_lines() {
    // parallel lines: the front face plane (y = 0)
    let pl = p(PlaneDef::TwoLines { a: x_edge(), b: x_edge_top() });
    parallel(pl.normal, DVec3::Y);
    on(&pl, DVec3::new(3.0, 0.0, 11.0));
    // intersecting lines
    let pl = p(PlaneDef::TwoLines { a: x_edge(), b: z_edge() });
    parallel(pl.normal, DVec3::Y);
    // skew lines are refused
    let m = perr(PlaneDef::TwoLines { a: x_edge(), b: y_edge_top() });
    assert!(m.contains("coplanar"), "{m}");
    // the same line twice
    let m = perr(PlaneDef::TwoLines { a: x_edge(), b: x_edge() });
    assert!(m.contains("same line"), "{m}");
    // an origin axis and a sketch-less edge
    let pl = p(PlaneDef::TwoLines { a: GeomRef::Origin(OriginRef::XAxis), b: GeomRef::Origin(OriginRef::ZAxis) });
    parallel(pl.normal, DVec3::Y);
}

#[test]
fn plane_through_three_points() {
    let pl = p(PlaneDef::ThreePoints { a: vertex(0.0, 0.0, 20.0), b: vertex(40.0, 0.0, 20.0), c: vertex(0.0, 30.0, 20.0) });
    close(pl.normal, DVec3::Z);
    assert!((pl.origin.z - 20.0).abs() < EPS);
    let m = perr(PlaneDef::ThreePoints { a: vertex(0.0, 0.0, 0.0), b: vertex(40.0, 0.0, 0.0), c: GeomRef::Origin(OriginRef::Origin) });
    assert!(m.contains("line"), "{m}");
    let m = perr(PlaneDef::ThreePoints { a: vertex(0.0, 0.0, 0.0), b: vertex(40.0, 0.0, 0.0), c: GeomRef::Origin(OriginRef::XY) });
    assert!(m.contains("point is needed"), "{m}");
}

#[test]
fn tangent_plane_to_a_cylinder() {
    // axis Z: default zero direction is world X
    let pl = p(PlaneDef::Tangent { face: cyl(), base: None, angle: "0".into() });
    close(pl.normal, DVec3::X);
    assert!((pl.origin.x - 75.0).abs() < EPS && (pl.origin.y - 15.0).abs() < 1e-5);
    let pl = p(PlaneDef::Tangent { face: cyl(), base: None, angle: "90".into() });
    close(pl.normal, DVec3::Y);
    assert!((pl.origin.y - 20.0).abs() < 1e-5);
    // from a base plane: XZ (normal -Y)
    let pl = p(PlaneDef::Tangent { face: cyl(), base: Some(GeomRef::Origin(OriginRef::XZ)), angle: "0".into() });
    close(pl.normal, DVec3::NEG_Y);
    assert!((pl.origin.y - 10.0).abs() < 1e-5);
    // at 45 degrees the plane touches the cylinder
    let pl = p(PlaneDef::Tangent { face: cyl(), base: None, angle: "45".into() });
    let d = (DVec3::new(70.0, 15.0, 15.0) - pl.origin).dot(pl.normal).abs();
    assert!((d - 5.0).abs() < 1e-5, "distance from the axis is the radius: {d}");
    // a flat face is not a cylinder
    let m = perr(PlaneDef::Tangent { face: top(), base: None, angle: "0".into() });
    assert!(m.contains("cylind"), "{m}");
}

#[test]
fn plane_normal_to_a_path() {
    let pl = p(PlaneDef::NormalAt { path: x_edge(), t: "0.5".into() });
    parallel(pl.normal, DVec3::X);
    assert!((pl.origin.x - 20.0).abs() < 1e-5);
    let pl = p(PlaneDef::NormalAt { path: x_edge(), t: "0.25".into() });
    assert!((pl.origin.x - 10.0).abs() < 1e-5 || (pl.origin.x - 30.0).abs() < 1e-5, "{pl:?}");
    // a circle: the point is on it, the normal is its tangent
    let c = DVec3::new(70.0, 15.0, 30.0);
    for t in ["0", "0.25", "0.5", "0.9"] {
        let pl = p(PlaneDef::NormalAt { path: circle_top(), t: t.into() });
        assert!(((pl.origin - c).length() - 5.0).abs() < 1e-5, "{t}: {pl:?}");
        assert!(pl.normal.z.abs() < EPS && pl.normal.dot(pl.origin - c).abs() < 1e-5, "{t}: {pl:?}");
    }
    // quarter turns are 90 degrees apart
    let a = p(PlaneDef::NormalAt { path: circle_top(), t: "0".into() }).origin - c;
    let b = p(PlaneDef::NormalAt { path: circle_top(), t: "0.25".into() }).origin - c;
    assert!(a.dot(b).abs() < 1e-5);
    // an axis: t is mm along it
    let pl = p(PlaneDef::NormalAt { path: GeomRef::Origin(OriginRef::ZAxis), t: "12".into() });
    close(pl.origin, DVec3::new(0.0, 0.0, 12.0));
    close(pl.normal, DVec3::Z);
    let m = perr(PlaneDef::NormalAt { path: x_edge(), t: "1.5".into() });
    assert!(m.contains("between 0 and 1"), "{m}");
    let m = perr(PlaneDef::NormalAt { path: top(), t: "0.5".into() });
    assert!(m.contains("path"), "{m}");
}

#[test]
fn plane_normal_to_a_sketch_line_and_on_sketch_points() {
    let mut d = model();
    let mut s = Sketch::new("S", Plane::XZ);
    let e = s.line(DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0));
    s.circle(DVec2::new(3.0, 4.0), 2.0);
    let sid = d.add_sketch(s);
    let (_, got) = plane(&mut d, PlaneDef::NormalAt { path: GeomRef::SketchLine { sketch: sid, entity: e.0 }, t: "0.5".into() });
    let pl = got.unwrap();
    close(pl.origin, DVec3::new(5.0, 0.0, 0.0));
    parallel(pl.normal, DVec3::X);
    // sketch points are placed through the sketch plane (XZ: local y is world z)
    let pt = d.add_feature("Pt", FeatureKind::DatumPoint { def: PointDef::At { point: GeomRef::SketchPoint { sketch: sid, point: 2 } } });
    let Some(DatumGeom::Point(p3)) = get(d.regenerate(), pt) else { panic!("{:?}", status(d.regenerate(), pt)) };
    close(p3, DVec3::new(3.0, 0.0, 4.0));
    // the circle's centre and axis
    let ax = d.add_feature("Ax", FeatureKind::DatumAxis { def: AxisDef::Cylinder { face: GeomRef::SketchLine { sketch: sid, entity: 1 } } });
    let Some(DatumGeom::Axis { origin, dir }) = get(d.regenerate(), ax) else { panic!() };
    close(origin, DVec3::new(3.0, 0.0, 4.0));
    parallel(dir, DVec3::Y);
}

// ---------------------------------------------------------------- axes

#[test]
fn axes() {
    let (o, d) = axis(AxisDef::Cylinder { face: cyl() });
    parallel(d, DVec3::Z);
    assert!((o.x - 70.0).abs() < 1e-5 && (o.y - 15.0).abs() < 1e-5);
    let (o, d) = axis(AxisDef::Cylinder { face: circle_top() });
    parallel(d, DVec3::Z);
    close(o, DVec3::new(70.0, 15.0, 30.0));
    let (o, d) = axis(AxisDef::Line { line: x_edge() });
    parallel(d, DVec3::X);
    assert!(o.y.abs() < 1e-5 && o.z.abs() < 1e-5);
    let (o, d) = axis(AxisDef::TwoPoints { a: vertex(0.0, 0.0, 0.0), b: vertex(40.0, 30.0, 20.0) });
    close(o, DVec3::ZERO);
    close(d, DVec3::new(40.0, 30.0, 20.0).normalize());
    let (o, d) = axis(AxisDef::TwoPlanes { a: top(), b: right() });
    parallel(d, DVec3::Y);
    assert!((o.x - 40.0).abs() < 1e-5 && (o.z - 20.0).abs() < 1e-5);
    // normal to a flat face through a point, to a cylinder, to the origin plane
    let (o, d) = axis(AxisDef::NormalAt { face: top(), point: vertex(40.0, 30.0, 20.0) });
    close(o, DVec3::new(40.0, 30.0, 20.0));
    close(d, DVec3::Z);
    // the cylinder's only vertices are on its seam, at x = 75
    let (o, d) = axis(AxisDef::NormalAt { face: cyl(), point: vertex(75.0, 15.0, 30.0) });
    close(o, DVec3::new(75.0, 15.0, 30.0));
    close(d, DVec3::X);
    let (o, d) = axis(AxisDef::NormalAt { face: GeomRef::Origin(OriginRef::XY), point: vertex(40.0, 30.0, 20.0) });
    close(o, DVec3::new(40.0, 30.0, 0.0));
    close(d, DVec3::Z);
}

#[test]
fn axis_errors() {
    for def in [
        AxisDef::TwoPlanes { a: top(), b: bottom() },
        AxisDef::TwoPoints { a: vertex(0.0, 0.0, 0.0), b: GeomRef::Origin(OriginRef::Origin) },
        AxisDef::Line { line: top() },
        AxisDef::Cylinder { face: top() },
    ] {
        let mut d = model();
        let id = d.add_feature("A", FeatureKind::DatumAxis { def });
        assert!(matches!(status(d.regenerate(), id), FeatureStatus::Error(_)));
        assert!(get(d.regenerate(), id).is_none());
    }
}

// ---------------------------------------------------------------- points

#[test]
fn points() {
    close(point(PointDef::At { point: vertex(40.0, 30.0, 20.0) }), DVec3::new(40.0, 30.0, 20.0));
    close(point(PointDef::At { point: vertex(40.2, 29.9, 20.1) }), DVec3::new(40.0, 30.0, 20.0));
    close(point(PointDef::Center { circle: circle_top() }), DVec3::new(70.0, 15.0, 30.0));
    let c = point(PointDef::Center { circle: cyl() });
    assert!((c.x - 70.0).abs() < 1e-5 && (c.y - 15.0).abs() < 1e-5);
    close(point(PointDef::EdgePlane { edge: z_edge(), plane: top() }), DVec3::new(0.0, 0.0, 20.0));
    close(point(PointDef::ThreePlanes { a: top(), b: right(), c: GeomRef::Origin(OriginRef::XZ) }), DVec3::new(40.0, 0.0, 20.0));
    close(point(PointDef::TwoLines { a: x_edge(), b: z_edge() }), DVec3::ZERO);
    // skew lines: the midpoint of the closest approach
    close(point(PointDef::TwoLines { a: x_edge(), b: y_edge_top() }), DVec3::new(0.0, 0.0, 10.0));
}

#[test]
fn point_errors() {
    for def in [
        PointDef::At { point: vertex(500.0, 500.0, 500.0) },
        PointDef::EdgePlane { edge: x_edge(), plane: top() }, // parallel
        PointDef::ThreePlanes { a: top(), b: bottom(), c: right() },
        PointDef::TwoLines { a: x_edge(), b: x_edge_top() },
        PointDef::Center { circle: x_edge() },
    ] {
        let mut d = model();
        let id = d.add_feature("Pt", FeatureKind::DatumPoint { def });
        assert!(matches!(status(d.regenerate(), id), FeatureStatus::Error(_)), "{:?}", status(d.regenerate(), id));
    }
}

// ------------------------------------------------- parametric behaviour

#[test]
fn a_parameter_moves_the_plane_the_sketch_and_the_extrude() {
    let mut doc = model();
    doc.set_param("lift", "5").unwrap();
    let (pid, got) = plane(&mut doc, PlaneDef::Offset { base: top(), distance: "lift".into() });
    assert!((got.unwrap().origin.z - 25.0).abs() < EPS);
    // a sketch on the datum plane (stored plane deliberately stale)
    let mut s = Sketch::new("On plane", Plane::XY);
    s.rect(DVec2::new(5.0, 5.0), DVec2::new(15.0, 15.0));
    s.plane_src = Some(pid);
    let sid = doc.add_sketch(s);
    let eid = extrude(&mut doc, "Tab", sid, "3");
    let res = doc.regenerate().clone();
    assert!(matches!(status(&res, eid), FeatureStatus::Ok), "{:?}", status(&res, eid));
    let tab = res.bodies.iter().find(|b| b.id.feature == eid).unwrap();
    let bb = tab.shape().bbox().unwrap();
    assert!((bb.min.z - 25.0).abs() < 1e-4 && (bb.max.z - 28.0).abs() < 1e-4, "{bb:?}");
    // the cached plane follows after a regen
    assert!(doc.data.refresh_sketch_planes(&res));
    assert!((doc.data.sketch(sid).unwrap().plane.origin.z - 25.0).abs() < EPS);
    assert!(!doc.data.refresh_sketch_planes(&res), "second refresh changes nothing");

    // change the parameter: plane, sketch and extrude all follow
    doc.set_param("lift", "12").unwrap();
    let res = doc.regenerate().clone();
    let Some(DatumGeom::Plane(pl)) = get(&res, pid) else { panic!() };
    assert!((pl.origin.z - 32.0).abs() < EPS);
    let tab = res.bodies.iter().find(|b| b.id.feature == eid).unwrap();
    let bb = tab.shape().bbox().unwrap();
    assert!((bb.min.z - 32.0).abs() < 1e-4 && (bb.max.z - 35.0).abs() < 1e-4, "{bb:?}");
    assert!(doc.data.refresh_sketch_planes(&res));
    assert!((doc.data.sketch(sid).unwrap().plane.origin.z - 32.0).abs() < EPS);
}

#[test]
fn a_plane_follows_the_body_it_was_built_on() {
    // the datum is on the top face; making the box taller moves the plane
    let mut doc = model();
    doc.set_param("h", "20").unwrap();
    let mut doc2 = Document::new("d");
    doc2.set_param("h", "20").unwrap();
    let mut s = Sketch::new("Box", Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0));
    let s = doc2.add_sketch(s);
    extrude(&mut doc2, "Box", s, "h");
    let (pid, got) = plane(&mut doc2, PlaneDef::Offset { base: top(), distance: "0".into() });
    assert!((got.unwrap().origin.z - 20.0).abs() < EPS);
    doc2.set_param("h", "33").unwrap();
    let Some(DatumGeom::Plane(pl)) = get(doc2.regenerate(), pid) else { panic!() };
    assert!((pl.origin.z - 33.0).abs() < EPS);
}

#[test]
fn datums_chain_and_unavailable_ones_report() {
    let mut doc = model();
    let (a, _) = plane(&mut doc, PlaneDef::Offset { base: top(), distance: "4".into() });
    let (b, got) = plane(&mut doc, PlaneDef::Offset { base: GeomRef::Datum(a), distance: "6".into() });
    assert!((got.unwrap().origin.z - 30.0).abs() < EPS);
    // an axis from two datum planes, a point on it
    let ax = doc.add_feature("Ax", FeatureKind::DatumAxis { def: AxisDef::TwoPlanes { a: GeomRef::Datum(b), b: right() } });
    let pt = doc.add_feature("Pt", FeatureKind::DatumPoint { def: PointDef::EdgePlane { edge: GeomRef::Datum(ax), plane: GeomRef::Datum(a) } });
    let res = doc.regenerate().clone();
    assert!(get(&res, ax).is_some());
    assert!(get(&res, pt).is_none(), "an axis parallel to the plane does not meet it");
    // suppress `a`: everything built on it fails with a plain message
    doc.edit("suppress", |d| {
        d.feature_mut(a).unwrap().suppressed = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    let res = doc.regenerate().clone();
    assert!(get(&res, a).is_none() && get(&res, b).is_none());
    let FeatureStatus::Error(m) = status(&res, b) else { panic!("{:?}", status(&res, b)) };
    assert!(m.contains("not available"), "{m}");
    // datums come in history order
    doc.edit("unsuppress", |d| {
        d.feature_mut(a).unwrap().suppressed = false;
        Ok::<_, ()>(())
    })
    .unwrap();
    let ids: Vec<_> = doc.regenerate().datums.iter().map(|(f, _)| *f).collect();
    assert_eq!(ids, vec![a, b, ax]);
}

#[test]
fn an_unmatched_face_reference_is_unresolved_not_wrong() {
    let mut doc = model();
    let far = GeomRef::Face(FaceSelector::Hint { centroid: DVec3::new(400.0, 400.0, 400.0), normal: DVec3::X, kind: None });
    let id = doc.add_feature("P", FeatureKind::DatumPlane { def: PlaneDef::Offset { base: far, distance: "1".into() }, flip: false });
    let res = doc.regenerate().clone();
    assert!(matches!(status(&res, id), FeatureStatus::Unresolved(_)), "{:?}", status(&res, id));
    assert!(get(&res, id).is_none());
}

#[test]
fn a_sketch_on_a_later_or_rolled_back_datum_falls_back_with_a_warning() {
    let mut doc = model();
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::new(1.0, 1.0), DVec2::new(5.0, 5.0));
    let sid = doc.add_sketch(s);
    // the extrude comes BEFORE the datum it follows
    let eid = extrude(&mut doc, "Early", sid, "2");
    let (pid, _) = plane(&mut doc, PlaneDef::Offset { base: top(), distance: "9".into() });
    doc.edit("follow", |d| {
        d.sketch_mut(sid).unwrap().plane_src = Some(pid);
        Ok::<_, ()>(())
    })
    .unwrap();
    let res = doc.regenerate().clone();
    let FeatureStatus::Warning(m) = status(&res, eid) else { panic!("{:?}", status(&res, eid)) };
    assert!(m.contains("construction plane"), "{m}");
    let body = res.bodies.iter().find(|b| b.id.feature == eid).unwrap();
    assert!((body.shape().bbox().unwrap().min.z).abs() < 1e-4, "built on the stored XY plane");
    assert!(!doc.data.clone().refresh_sketch_planes(&res) || doc.data.sketch(sid).unwrap().plane == Plane::XY);
}

// ---------------------------------------------------------------- misc

#[test]
fn visibility() {
    let mut d = DocData::new("v", "dev");
    let id = FeatureId(7);
    assert!(d.datum_visible(id) && !d.origin_visible);
    d.set_datum_visible(id, false);
    assert!(!d.datum_visible(id));
    d.set_datum_visible(id, false);
    assert_eq!(d.hidden_datums.len(), 1);
    let back = DocData::from_json(&d.to_json().unwrap()).unwrap();
    assert!(!back.datum_visible(id));
    d.set_datum_visible(id, true);
    assert!(d.datum_visible(id));
    assert!(!d.to_json().unwrap().contains("hidden_datums"), "default is not written");
}

#[test]
fn datum_features_round_trip_and_old_files_load() {
    let mut doc = model();
    let (pid, _) = plane(&mut doc, PlaneDef::Angle { axis: x_edge(), base: Some(top()), angle: "30".into() });
    doc.add_feature("Ax", FeatureKind::DatumAxis { def: AxisDef::NormalAt { face: cyl(), point: vertex(70.0, 15.0, 30.0) } });
    doc.add_feature("Pt", FeatureKind::DatumPoint { def: PointDef::ThreePlanes { a: top(), b: right(), c: GeomRef::Datum(pid) } });
    doc.data.set_datum_visible(pid, false);
    doc.data.origin_visible = true;
    let json = doc.data.to_json().unwrap();
    let back = DocData::from_json(&json).unwrap();
    assert_eq!(back, doc.data);
    assert_eq!(back.to_json().unwrap(), json);
    // a file saved before datums existed: no new fields anywhere
    let plain = model();
    let old = plain.data.to_json().unwrap();
    assert!(!old.contains("plane_src") && !old.contains("hidden_datums") && !old.contains("origin_visible"));
    let mut doc3 = Document::new("old");
    doc3.data = DocData::from_json(&old).unwrap();
    assert!(doc3.regenerate().datums.is_empty());
    assert_eq!(doc3.data.sketches[0].sketch.plane_src, None);
    // labels
    assert_eq!(FeatureKind::DatumPoint { def: PointDef::At { point: vertex(0.0, 0.0, 0.0) } }.label(), "Construction point");
}

#[test]
fn datums_make_no_bodies_and_use_no_sketch() {
    let mut doc = model();
    let n = doc.regenerate().bodies.len();
    let k = FeatureKind::DatumPlane { def: PlaneDef::Offset { base: top(), distance: "1".into() }, flip: false };
    assert!(k.sketch().is_none() && k.is_datum());
    doc.add_feature("P", k);
    assert_eq!(doc.regenerate().bodies.len(), n);
}

#[test]
fn stable_frames() {
    // world X projected, or Y when the normal is near X
    close(stable_x(DVec3::Z), DVec3::X);
    close(stable_x(DVec3::NEG_Y), DVec3::X);
    close(stable_x(DVec3::X), DVec3::Y);
    let s = DVec3::new(1.0, 1.0, 0.0).normalize();
    close(stable_x(s), DVec3::new(1.0, -1.0, 0.0).normalize()); // |n.x| = 0.707 < 0.9: X projected
    let _ = DVec3::ZERO;
}
