//! Timing harness for the test enclosure (prints a table; run with
//! `cargo test -p casework-model --release --test timing -- --nocapture`).
//! Only sanity is asserted, never absolute times.

use std::time::Instant;

use casework_model::document::Document;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::{Half, JointKind, Rim};
use casework_model::process::FitClass;
use casework_model::sketch::Sketch;
use casework_model::params::Expr;
use glam::DVec2;

/// Parametric closed enclosure: rounded-rect shell (cavity cut), 4 bosses, a rib and
/// a tongue & groove parting joint.
pub fn enclosure_doc() -> (Document, BodyId) {
    let mut doc = Document::new("Timing");
    for (n, v) in [("w", "80"), ("d", "60"), ("h", "30"), ("wall", "2"), ("r", "6"), ("split", "20")] {
        doc.set_param(n, v).unwrap();
    }
    let mut outer = Sketch::new("Outer", Plane::XY);
    outer.rounded_rect(DVec2::ZERO, "w", "d", "r", (80.0, 60.0, 6.0));
    let outer = doc.add_sketch(outer);
    let body = BodyId::of(doc.add_feature(
        "Body",
        FeatureKind::Extrude { sketch: outer, profiles: ProfileSel::All, distance: "h".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let mut inner = Sketch::new("Cavity", Plane::XY.offset(2.0));
    inner.rounded_rect(DVec2::ZERO, "w - 2*wall", "d - 2*wall", "r - wall", (76.0, 56.0, 4.0));
    let inner = doc.add_sketch(inner);
    doc.add_feature(
        "Cavity",
        FeatureKind::Extrude { sketch: inner, profiles: ProfileSel::All, distance: "h - 2*wall".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(body) },
    );
    let pts = [(-28.0, -18.0), (28.0, -18.0), (28.0, 18.0), (-28.0, 18.0)].map(|p| DVec2::new(p.0, p.1)).to_vec();
    doc.add_feature(
        "Bosses",
        FeatureKind::ScrewBoss { body: Some(body), plane: Plane::XY.offset(2.0), points: pts, od: 7.0.into(), hole_dia: 2.5.into(), height: 12.0.into(), hole_depth: None, base_fillet: 0.8.into(), draft_deg: 0.0.into() },
    );
    let mut rib = Sketch::new("Rib", Plane::XY.offset(2.0));
    rib.line(DVec2::new(-15.0, 0.0), DVec2::new(15.0, 0.0));
    let rib = doc.add_sketch(rib);
    doc.add_feature("Rib", FeatureKind::Rib { sketch: rib, thickness: 1.5.into(), height: 8.0.into(), draft_deg: 0.0.into(), flip: false, body: Some(body), extent: Default::default() });
    doc.add_feature(
        "Joint",
        FeatureKind::PartingJoint {
            body: Some(body),
            plane: Plane::XY,
            offset: "split".into(),
            kind: JointKind::TongueGroove { width: 0.8.into(), height: 3.0.into(), clearance: 0.15.into(), extra_depth: 0.3.into(), tongue_on: Half::Lower, lead_in: 0.2.into() },
            rim: Some(Rim { width: 4.0.into(), height: 3.0.into() }),
            upper_name: "Lid".into(),
            class: FitClass::Slide,
            closing_radius: None,
        },
    );
    (doc, body)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn best<T>(n: usize, mut f: impl FnMut(usize) -> T) -> (f64, T) {
    let mut best = f64::INFINITY;
    let mut out = None;
    for i in 0..n {
        let t = Instant::now();
        let r = f(i);
        best = best.min(ms(t));
        out = Some(r);
    }
    (best, out.unwrap())
}

/// Best-of-5 timings (robust against other processes on the machine).
#[test]
fn enclosure_timings_best_of_5() {
    use casework_model::regen::MeshQuality;
    let (full, _) = best(5, |_| {
        let (mut doc, _) = enclosure_doc();
        let r = doc.regenerate().clone();
        assert!(r.errors().is_empty(), "{:?}", r.errors());
        r
    });
    let (mut doc, _) = enclosure_doc();
    doc.regenerate();
    let (edit, _) = best(5, |i| {
        doc.set_param("wall", Expr::from(2.1 + 0.1 * i as f64)).unwrap();
        doc.regenerate().recomputed
    });
    let (undo, _) = best(5, |_| {
        doc.undo();
        let r = doc.regenerate().recomputed;
        doc.redo();
        doc.regenerate();
        r
    });
    let (tail, _) = best(5, |i| {
        doc.set_param("split", Expr::from(20.1 + 0.1 * i as f64)).unwrap();
        doc.regenerate().recomputed
    });
    let bodies = doc.regenerate().bodies.clone();
    let shapes: Vec<_> = bodies.iter().map(|b| b.shape().clone()).collect();
    let mut tess = Vec::new();
    for q in [MeshQuality::Preview, MeshQuality::Display, MeshQuality::Final] {
        let (l, a) = q.deflection();
        let (t, n) = best(5, |_| shapes.iter().map(|s| s.tessellate(l, a).unwrap().indices.len()).sum::<usize>());
        tess.push((q, t, n));
    }
    println!("\n| step (best of 5) | ms |\n|---|---|");
    println!("| full regen (cold) | {full:.0} |");
    println!("| param edit `wall` → regen | {edit:.0} |");
    println!("| undo → regen | {undo:.2} |");
    println!("| edit of the last feature (`split`) → regen | {tail:.0} |");
    for (q, t, n) in tess {
        println!("| tessellate all bodies, {q:?} ({n} tris) | {t:.0} |");
    }
}

#[test]
fn enclosure_timings() {
    let (mut doc, _) = enclosure_doc();
    let t = Instant::now();
    let r = doc.regenerate().clone();
    let full = ms(t);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    for rep in &r.reports {
        println!("  feature {:?}: {:.0} ms", rep.feature, rep.elapsed_ms);
    }

    let t = Instant::now();
    for b in &r.bodies {
        b.mesh().unwrap();
    }
    let mesh_final = ms(t);

    let t = Instant::now();
    let tri: usize = r.bodies.iter().map(|b| b.mesh_quality(casework_model::regen::MeshQuality::Preview).unwrap().indices.len()).sum();
    let mesh_preview = ms(t);
    let tri_final: usize = r.bodies.iter().map(|b| b.mesh().unwrap().indices.len()).sum();

    doc.set_param("wall", "2.5").unwrap();
    let t = Instant::now();
    let r2 = doc.regenerate().clone();
    let edit = ms(t);
    assert!(r2.errors().is_empty(), "{:?}", r2.errors());

    let t = Instant::now();
    for b in &r2.bodies {
        b.mesh_quality(casework_model::regen::MeshQuality::Preview).unwrap();
    }
    let edit_preview = ms(t);

    doc.undo();
    let t = Instant::now();
    let r3 = doc.regenerate().clone();
    let undo = ms(t);
    assert_eq!(r3.recomputed, 0);
    let t = Instant::now();
    for b in &r3.bodies {
        b.mesh().unwrap();
    }
    let undo_mesh = ms(t);

    // an edit that only touches the last feature (joint height)
    doc.set_param("split", "21").unwrap();
    let t = Instant::now();
    let r4 = doc.regenerate().clone();
    let tail_edit = ms(t);
    assert!(r4.recomputed <= 1, "only the joint recomputes");

    println!("\n| step | ms |\n|---|---|");
    println!("| full regen (cold, 6 features) | {full:.0} |");
    println!("| final tessellation, all bodies ({tri_final} tris) | {mesh_final:.0} |");
    println!("| preview tessellation, all bodies ({tri} tris) | {mesh_preview:.0} |");
    println!("| param edit `wall` → regen | {edit:.0} |");
    println!("| … preview mesh after the edit | {edit_preview:.0} |");
    println!("| undo → regen | {undo:.1} |");
    println!("| … meshes after undo (cached) | {undo_mesh:.2} |");
    println!("| edit of the last feature only (`split`) | {tail_edit:.0} |");
}
