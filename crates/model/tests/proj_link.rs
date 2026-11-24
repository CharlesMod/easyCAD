//! Associative projection: a sketch's projected model edges follow the model.

use easycad_model::document::{DocData, Document};
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::regen::FeatureStatus;
use easycad_model::sketch::Sketch;
use glam::DVec2;
use easycad_model::occt;

fn extrude(sketch: SketchId, dist: f64) -> FeatureKind {
    FeatureKind::Extrude {
        sketch,
        profiles: ProfileSel::All,
        distance: dist.into(),
        symmetric: false,
        reverse: false,
        draft_deg: 0.0.into(),
        op: BoolOp::NewBody,
        target: None,
    }
}

/// Sketch A (z = 0) built by `make`, extruded 10; the model edges on top (z = 10)
/// selected by `pick` are projected into sketch B on z = 10, which is extruded 5.
fn linked(make: impl FnOnce(&mut Sketch), pick: impl Fn(&occt::EdgeInfo) -> bool) -> (Document, SketchId, FeatureId, FeatureId) {
    let mut doc = Document::new("Link");
    let mut a = Sketch::new("A", Plane::XY);
    make(&mut a);
    let sa = doc.add_sketch(a);
    let fa = doc.add_feature("A", extrude(sa, 10.0));
    let out = doc.regenerate();
    let edges = out.body_by_id(BodyId::of(fa)).unwrap().shape().edges().unwrap();
    let mut b = Sketch::new("B", Plane::XY.offset(10.0));
    for e in edges.iter().filter(|e| pick(e)) {
        assert!(!b.project_edge_from(BodyId::of(fa), e, None, false).is_empty());
    }
    assert!(!b.proj_srcs.is_empty());
    let sb = doc.add_sketch(b);
    let fb = doc.add_feature("B", extrude(sb, 5.0));
    (doc, sa, fa, fb)
}

fn translate(doc: &mut Document, s: SketchId, d: DVec2) {
    doc.edit("Move", |data| {
        let sk = data.sketch_mut(s).unwrap();
        sk.points.iter_mut().for_each(|p| *p += d);
        Ok::<_, ()>(())
    })
    .unwrap();
}

/// (centre of B's extrusion in XY, its volume), asserting B regenerated cleanly.
fn b_state(doc: &mut Document, fb: FeatureId) -> (DVec2, f64) {
    let out = doc.regenerate();
    assert_eq!(out.report(fb).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(fb).unwrap().status);
    let sh = out.body_by_id(BodyId::of(fb)).unwrap().shape();
    let bb = sh.bbox().unwrap();
    let c = (bb.min + bb.max) / 2.0;
    (DVec2::new(c.x, c.y), sh.volume().unwrap())
}

fn check_follows(mut doc: Document, sa: SketchId, fb: FeatureId, start: DVec2, vol: f64, moves: [DVec2; 3]) {
    let (c0, v0) = b_state(&mut doc, fb);
    assert!((c0 - start).length() < 1e-6 && (v0 - vol).abs() < 1e-3 * vol, "{c0:?} {v0}");
    let mut at = start;
    // a small move, then one past 0.25 x the bbox diagonal, then back
    for d in moves {
        translate(&mut doc, sa, d);
        at += d;
        let (c, v) = b_state(&mut doc, fb);
        assert!((c - at).length() < 1e-6, "B follows A: {c:?} vs {at:?}");
        assert!((v - vol).abs() < 1e-3 * vol, "B's extrude is valid: {v} vs {vol}");
    }
}

#[test]
fn projected_circle_follows_a_move() {
    let (doc, sa, _, fb) = linked(
        |a| {
            a.circle(DVec2::new(20.0, 15.0), 5.0);
        },
        |e| e.circle.is_some_and(|(c, _, _)| (c.z - 10.0).abs() < 1e-6),
    );
    check_follows(doc, sa, fb, DVec2::new(20.0, 15.0), std::f64::consts::PI * 25.0 * 5.0, [DVec2::new(2.0, 1.0), DVec2::new(40.0, -30.0), DVec2::new(-42.0, 29.0)]);
}

#[test]
fn projected_rectangle_follows_a_move() {
    let (doc, sa, _, fb) = linked(
        |a| {
            a.rect(DVec2::new(5.0, 5.0), DVec2::new(65.0, 45.0));
        },
        |e| e.start.z > 9.9 && e.end.z > 9.9,
    );
    // 60 x 40 x 10: 0.25 x the diagonal is 18.2; equal edges sit 40 / 60 apart, so a
    // move of 21 is past the hint but still nearer its own edge than the twin
    check_follows(doc, sa, fb, DVec2::new(35.0, 25.0), 60.0 * 40.0 * 5.0, [DVec2::new(2.0, 1.0), DVec2::new(13.0, 12.0), DVec2::new(-15.0, -13.0)]);
}

#[test]
fn lost_source_keeps_geometry_and_warns() {
    let (mut doc, _, fa, fb) = linked(
        |a| {
            a.circle(DVec2::new(20.0, 15.0), 5.0);
        },
        |e| e.circle.is_some_and(|(c, _, _)| (c.z - 10.0).abs() < 1e-6),
    );
    let (c0, v0) = b_state(&mut doc, fb);
    doc.edit("Suppress A", |d| {
        d.feature_mut(fa).unwrap().suppressed = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    let FeatureStatus::Warning(w) = &out.report(fb).unwrap().status else { panic!("{:?}", out.report(fb).unwrap().status) };
    assert!(w.contains("projected"), "{w}");
    // the last good geometry still builds B's extrude
    let sh = out.body_by_id(BodyId::of(fb)).unwrap().shape();
    let bb = sh.bbox().unwrap();
    let c = (bb.min + bb.max) / 2.0;
    assert!((DVec2::new(c.x, c.y) - c0).length() < 1e-6 && (sh.volume().unwrap() - v0).abs() < 1e-6);
}

#[test]
fn source_round_trips_and_old_docs_load() {
    let (doc, ..) = linked(
        |a| {
            a.circle(DVec2::new(20.0, 15.0), 5.0);
        },
        |e| e.circle.is_some_and(|(c, _, _)| (c.z - 10.0).abs() < 1e-6),
    );
    let json = doc.data.to_json().unwrap();
    assert!(json.contains("proj_srcs"));
    let back = DocData::from_json(&json).unwrap();
    assert_eq!(back.sketches[1].sketch.proj_srcs, doc.data.sketches[1].sketch.proj_srcs);
    assert_eq!(back.sketches[1].sketch, doc.data.sketches[1].sketch);

    // an old file has no such field: it loads, and the projection stays frozen
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    for s in v["sketches"].as_array_mut().unwrap() {
        s["sketch"].as_object_mut().unwrap().remove("proj_srcs");
    }
    let old = DocData::from_json(&v.to_string()).unwrap();
    assert!(old.sketches.iter().all(|s| s.sketch.proj_srcs.is_empty()));
}

#[test]
fn relinked_geometry_is_written_back_to_the_stored_sketch() {
    let (mut doc, sa, _, fb) = linked(
        |a| {
            a.circle(DVec2::new(20.0, 15.0), 5.0);
        },
        |e| e.circle.is_some_and(|(c, _, _)| (c.z - 10.0).abs() < 1e-6),
    );
    let sb = doc.data.sketches[1].id;
    // an old frozen entity must not be touched by the write-back
    translate(&mut doc, sa, DVec2::new(40.0, -30.0));
    let res = doc.regenerate().clone();
    assert_eq!(res.report(fb).unwrap().status, FeatureStatus::Ok);
    assert!(doc.data.apply_relinked(&res));
    let b = doc.data.sketch(sb).unwrap().clone();
    let c = b.points[0];
    assert!((c - DVec2::new(60.0, -15.0)).length() < 1e-6, "{c:?}");
    assert!(b.constraints.iter().any(|k| matches!(k, easycad_model::sketch::Constraint::Fix(_, q) if (*q - DVec2::new(60.0, -15.0)).length() < 1e-6)));
    // stored, editor and regen agree: nothing left to write back, regen is unchanged
    let again = doc.regenerate().clone();
    assert!(!doc.data.apply_relinked(&again));
    assert_eq!(b_state(&mut doc, fb).0, DVec2::new(60.0, -15.0));
}
