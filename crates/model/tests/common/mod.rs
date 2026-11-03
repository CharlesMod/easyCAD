//! Shared test fixtures.

use casework_model::document::Document;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::{Half, JointKind, Rim};
use casework_model::process::FitClass;
use casework_model::sketch::Sketch;
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
