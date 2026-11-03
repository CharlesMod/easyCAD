//! Closed loops of mates are solved together, not only checked: a four-bar, a
//! consistent redundant loop and an inconsistent one. No geometry is needed: the
//! mate solver works on connector frames.

use easycad_model::assembly::*;
use easycad_model::document::Document;
use glam::DVec3;

fn rev(doc: &mut Document, name: &str, a: (OccurrenceId, f64), b: (OccurrenceId, f64), angle: f64) -> MateId {
    let c = |(o, x): (OccurrenceId, f64)| MateConnector::new(vec![o], DVec3::new(x, 0.0, 0.0), DVec3::Z);
    let id = doc.add_mate(name, MateKind::Revolute, c(a), c(b), false);
    doc.edit_mate(id, "drive", |m| m.angle = angle.into()).unwrap();
    id
}

fn part(doc: &mut Document, name: &str, grounded: bool) -> OccurrenceId {
    let c = doc.add_component(name);
    let o = doc.add_occurrence(ComponentId::ROOT, c, &format!("{name}:1"), Transform::IDENTITY).unwrap();
    doc.edit_occurrence(o, "ground", |o| o.grounded = grounded).unwrap();
    o
}

fn at(doc: &Document, o: OccurrenceId, x: f64) -> DVec3 {
    doc.data.world_transform(&[o]).unwrap().point(DVec3::new(x, 0.0, 0.0))
}

fn angle(doc: &Document, o: OccurrenceId) -> f64 {
    let x = doc.data.world_transform(&[o]).unwrap().vector(DVec3::X);
    x.y.atan2(x.x).to_degrees()
}

#[test]
fn four_bar_loop_solves_from_its_crank() {
    // ground pivots 40 apart, crank 10, coupler 40, rocker 10 (a parallelogram)
    let mut doc = Document::new("Four-bar");
    let g = part(&mut doc, "Ground", true);
    let crank = part(&mut doc, "Crank", false);
    let coupler = part(&mut doc, "Coupler", false);
    let rocker = part(&mut doc, "Rocker", false);
    let input = rev(&mut doc, "Input", (g, 0.0), (crank, 0.0), 30.0);
    let elbow = rev(&mut doc, "Elbow", (crank, 10.0), (coupler, 0.0), 0.0);
    let pivot = rev(&mut doc, "Pivot", (g, 40.0), (rocker, 0.0), 0.0);
    let close = rev(&mut doc, "Close", (coupler, 40.0), (rocker, 10.0), 0.0);
    let rep = doc.solve_mates();
    assert!(rep.ok(), "{rep:?}");
    let st = |id: MateId| rep.mates.iter().find(|m| m.mate == id).unwrap().clone();
    assert_eq!(st(close).status, MateStatus::Solved, "the closing mate is solved, not just checked");
    // the crank's drive sets the mechanism; the coupler stays level, the rocker
    // follows at 30°, and the loop closes
    assert!((angle(&doc, crank) - 30.0).abs() < 1e-6);
    assert!(angle(&doc, coupler).abs() < 1e-6, "{}", angle(&doc, coupler));
    assert!((angle(&doc, rocker) - 30.0).abs() < 1e-6, "{}", angle(&doc, rocker));
    let tip = DVec3::new(40.0 + 10.0 * 30f64.to_radians().cos(), 10.0 * 30f64.to_radians().sin(), 0.0);
    assert!((at(&doc, coupler, 40.0) - tip).length() < 1e-6 && (at(&doc, rocker, 10.0) - tip).length() < 1e-6);
    assert!(at(&doc, coupler, 0.0).z.abs() < 1e-9);
    // the drives the loop overrode are reported with their actual values
    assert!(st(input).actual.is_none());
    assert!((st(elbow).actual.unwrap() + 30.0).abs() < 1e-6, "{:?}", st(elbow));
    assert!((st(pivot).actual.unwrap() - 30.0).abs() < 1e-6);
    assert!(rep.notes.iter().any(|n| n.contains("`Elbow`") && n.contains("-30.00°")), "{:?}", rep.notes);
    // a planar four-bar of revolutes is over-constrained in space, consistently
    assert!(rep.notes.iter().any(|n| n.contains("redundant constraint")), "{:?}", rep.notes);
    // turning the crank moves the whole loop; solving again changes nothing
    doc.edit_mate(input, "turn", |m| m.angle = 60.0.into()).unwrap();
    assert!(doc.solve_mates().ok());
    assert!((angle(&doc, rocker) - 60.0).abs() < 1e-6 && angle(&doc, coupler).abs() < 1e-6, "{} {} {}", angle(&doc, crank), angle(&doc, coupler), angle(&doc, rocker));
    let again = doc.solve_mates();
    assert!(again.moved.is_empty(), "{again:?} {:?}", [crank, coupler, rocker].map(|o| doc.data.occurrence(o).unwrap().transform));
}

/// Ground – A revolute about z at the origin, A – B rigid 20 mm along A's x, and
/// B – ground revolute about an axis `off` mm from B's origin.
fn double_hinge(off: f64) -> (Document, [OccurrenceId; 2], MateId) {
    let mut doc = Document::new("Loop");
    let g = part(&mut doc, "Ground", true);
    let a = part(&mut doc, "A", false);
    let b = part(&mut doc, "B", false);
    rev(&mut doc, "Hinge", (g, 0.0), (a, 0.0), 30.0);
    doc.add_mate(
        "Weld",
        MateKind::Rigid,
        MateConnector::new(vec![a], DVec3::new(20.0, 0.0, 0.0), DVec3::Z),
        MateConnector::new(vec![b], DVec3::ZERO, DVec3::Z),
        false,
    );
    let back = rev(&mut doc, "Back", (g, 0.0), (b, -off), 0.0);
    (doc, [a, b], back)
}

#[test]
fn consistent_redundant_loop_solves_and_says_so() {
    // the second hinge is on the first one's axis: it agrees but adds nothing
    let (mut doc, [a, b], back) = double_hinge(20.0);
    let rep = doc.solve_mates();
    assert!(rep.ok(), "{rep:?}");
    let m = rep.mates.iter().find(|m| m.mate == back).unwrap();
    assert_eq!(m.status, MateStatus::Solved);
    assert!((m.actual.unwrap() - 30.0).abs() < 1e-6, "{m:?}");
    assert!(rep.notes.iter().any(|n| n.contains("redundant constraints (they agree)") && n.contains("`A:1`")), "{:?}", rep.notes);
    assert!((angle(&doc, a) - 30.0).abs() < 1e-6 && (angle(&doc, b) - 30.0).abs() < 1e-6);
    assert!(at(&doc, b, -20.0).length() < 1e-6, "B's hinge sits on the axis");
}

#[test]
fn inconsistent_loop_is_reported() {
    // the second hinge's axis is 5 mm off the first one's: the loop cannot close
    let (mut doc, [a, _], back) = double_hinge(15.0);
    let rep = doc.solve_mates();
    assert!(!rep.ok());
    let st = &rep.mates.iter().find(|m| m.mate == back).unwrap().status;
    assert!(matches!(st, MateStatus::OverConstrained(s) if s.contains("cannot close") && s.contains("off by 5.000 mm") && s.contains("`B:1`")), "{st:?}");
    // the tree placement stays: the first hinge's drive holds
    assert!((angle(&doc, a) - 30.0).abs() < 1e-6);
    assert!(rep.mates.iter().filter(|m| m.mate != back).all(|m| m.status == MateStatus::Solved), "{rep:?}");
}
