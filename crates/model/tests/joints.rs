//! Parting-joint robustness matrix: every joint kind on rounded rects (several
//! fillet radii, plain and drafted), an L-shape, a DXF outline with concave arcs,
//! planes crossing a boss / rib / USB slot, thin walls with and without rim
//! thickening, planes near the floor/lid, O-ring and snap-fit checks.

use std::f64::consts::PI;

use casework_model::document::Document;
use casework_model::dxf::DxfDrawing;
use casework_model::export::{open_edges, weld};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::*;
use casework_model::occt::{DVec3, Edge, Frame, Shape};
use casework_model::params::Expr;
use casework_model::process::FitClass;
use casework_model::regen::{profile_face, FeatureStatus};
use casework_model::sketch::profiles_from_segments;

const H: f64 = 30.0;

#[derive(Clone, Copy, Debug)]
pub enum Outline {
    RoundRect(f64),
    LShape,
    DxfArcs,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Extras {
    pub boss: bool,
    pub rib: bool,
    pub usb: bool,
}

fn outline_face(o: Outline) -> Shape {
    let p = |x: f64, y: f64| DVec3::new(x, y, 0.0);
    match o {
        Outline::RoundRect(r) => rrect_face(0.0, 0.0, 80.0, 60.0, r),
        Outline::LShape => {
            let pts = [p(0.0, 0.0), p(80.0, 0.0), p(80.0, 30.0), p(40.0, 30.0), p(40.0, 60.0), p(0.0, 60.0)];
            let e: Vec<Edge> = (0..6).map(|i| Edge::Line(pts[i], pts[(i + 1) % 6])).collect();
            Shape::face(&Frame::XY, &e, &[]).unwrap()
        }
        Outline::DxfArcs => {
            // convex arc on the right, concave notch on the top, via the DXF path
            let b_convex = (30.0f64).to_radians().tan(); // 120° arc
            let b_notch = -(45.0f64).to_radians().tan(); // concave 180° notch (cw)
            let verts = [(0.0, 0.0, 0.0), (80.0, 0.0, b_convex), (80.0, 60.0, 0.0), (50.0, 60.0, b_notch), (30.0, 60.0, 0.0), (0.0, 60.0, 0.0)];
            let mut s = String::from("0\nSECTION\n2\nENTITIES\n0\nLWPOLYLINE\n8\nOutline\n90\n6\n70\n1\n");
            for (x, y, b) in verts {
                s += &format!("10\n{x}\n20\n{y}\n");
                if b != 0.0 {
                    s += &format!("42\n{b}\n");
                }
            }
            s += "0\nENDSEC\n0\nEOF\n";
            let d = DxfDrawing::parse(&s).unwrap();
            let pr = profiles_from_segments(&d.segments(&["Outline"]), 1e-6);
            assert_eq!(pr.profiles.len(), 1);
            profile_face(&Plane::XY, &pr.profiles[0]).unwrap()
        }
    }
}

fn rrect_face(x0: f64, y0: f64, w: f64, d: f64, r: f64) -> Shape {
    let p = |x: f64, y: f64| DVec3::new(x0 + x, y0 + y, 0.0);
    if r <= 1e-9 {
        let pts = [p(0.0, 0.0), p(w, 0.0), p(w, d), p(0.0, d)];
        let e: Vec<Edge> = (0..4).map(|i| Edge::Line(pts[i], pts[(i + 1) % 4])).collect();
        return Shape::face(&Frame::XY, &e, &[]).unwrap();
    }
    let c = |x: f64, y: f64, a: f64| p(x + r * a.to_radians().cos(), y + r * a.to_radians().sin());
    let edges = vec![
        Edge::Line(p(r, 0.0), p(w - r, 0.0)),
        Edge::Arc3(p(w - r, 0.0), c(w - r, r, -45.0), p(w, r)),
        Edge::Line(p(w, r), p(w, d - r)),
        Edge::Arc3(p(w, d - r), c(w - r, d - r, 45.0), p(w - r, d)),
        Edge::Line(p(w - r, d), p(r, d)),
        Edge::Arc3(p(r, d), c(r, d - r, 135.0), p(0.0, d - r)),
        Edge::Line(p(0.0, d - r), p(0.0, r)),
        Edge::Arc3(p(0.0, r), c(r, r, 225.0), p(r, 0.0)),
    ];
    Shape::face(&Frame::XY, &edges, &[]).unwrap()
}

/// Closed hollow enclosure: outline extruded to H (optionally drafted), hollowed by a
/// uniform inward offset of `wall` (floor and lid included), plus extras.
pub fn enclosure(o: Outline, wall: f64, draft: f64, x: Extras, split_z: f64) -> Shape {
    let outer = outline_face(o).extrude_ext(&Frame::XY, DVec3::Z, H, false, draft).unwrap();
    let cavity = match o {
        // explicit inset outline (inner corners go sharp when r < wall)
        Outline::RoundRect(r) => rrect_face(wall, wall, 80.0 - 2.0 * wall, 60.0 - 2.0 * wall, (r - wall).max(0.0))
            .translated(DVec3::Z * wall)
            .unwrap()
            .extrude_ext(&Frame { origin: DVec3::Z * wall, ..Frame::XY }, DVec3::Z, H - 2.0 * wall, false, draft)
            .unwrap(),
        _ => outer.offset_solid(-wall).unwrap(),
    };
    let mut body = outer.cut(&cavity).unwrap();
    let bb = body.bbox().unwrap();
    if x.boss {
        // a free-standing screw boss crossing the plane, near the (-x,-y) corner
        let c = DVec3::new(bb.min.x + 12.0, bb.min.y + 12.0, wall);
        let boss = Shape::cylinder(c, DVec3::Z, 3.5, H - 2.0 * wall).unwrap();
        body = body.fuse(&boss).unwrap().cut(&Shape::cylinder(c, DVec3::Z, 1.25, H).unwrap()).unwrap();
    }
    if x.rib {
        // a rib joined to the -x wall, crossing the plane
        let rib = Shape::make_box(DVec3::new(bb.min.x + 0.5, 20.0, wall), DVec3::new(18.0, 1.5, H - 2.0 * wall)).unwrap();
        body = body.fuse(&rib).unwrap();
    }
    if x.usb {
        // a 10 × 4 mm USB slot through the front (-y) wall, straddling the plane
        let slot = Shape::make_box(DVec3::new(30.0, bb.min.y - 1.0, split_z - 2.0), DVec3::new(10.0, wall + 4.0, 4.0)).unwrap();
        body = body.cut(&slot).unwrap();
    }
    body.unify().unwrap()
}

fn doc_with(body: &Shape) -> (Document, BodyId) {
    let mut doc = Document::new("J");
    let (_, f) = doc.import_step("enclosure", body.to_step_string().unwrap().into_bytes(), false);
    (doc, BodyId { feature: f, index: 0 })
}

pub fn tg(height: f64) -> JointKind {
    JointKind::TongueGroove {
        width: 0.8.into(),
        height: height.into(),
        clearance: 0.15.into(),
        extra_depth: 0.3.into(),
        tongue_on: Half::Lower,
        lead_in: 0.2.into(),
    }
}
fn lip() -> JointKind {
    JointKind::Lip { width: 1.0.into(), height: 2.0.into(), clearance: 0.15.into(), lip_on: Half::Lower }
}
fn oring(rib: bool) -> JointKind {
    JointKind::ORingGland {
        cord: "M1.5".into(),
        squeeze: None,
        depth: None,
        width: None,
        gland_on: Half::Lower,
        compression_rib: rib.then(|| (Expr::from(0.6), Expr::from(0.2))),
    }
}
pub fn snap(n: u32) -> JointKind {
    JointKind::SnapFit {
        positions: SnapPositions::Count(n),
        beam_length: 6.0.into(),
        thickness: 1.0.into(),
        width: 5.0.into(),
        undercut: 0.5.into(),
        insertion_deg: 30.0.into(),
        retention_deg: 90.0.into(),
        material: "PETG".into(),
        beam_on: Half::Lower,
        clearance: 0.2.into(),
        margin: 2.0.into(),
    }
}

pub struct Run {
    doc: Document,
    pub lower: Shape,
    pub upper: Shape,
    pub report: JointReport,
    status: FeatureStatus,
}

pub fn run(body: &Shape, z: f64, kind: JointKind, rim: Option<(f64, f64)>) -> Result<Run, String> {
    let (mut doc, b) = doc_with(body);
    let f = doc.add_feature(
        "Joint",
        FeatureKind::PartingJoint {
            body: Some(b),
            plane: Plane::XY,
            offset: z.into(),
            kind,
            rim: rim.map(|(w, h)| Rim { width: w.into(), height: h.into() }),
            upper_name: "Lid".into(),
            class: FitClass::Slide,
            closing_radius: None,
        },
    );
    let out = doc.regenerate();
    let rep = out.report(f).unwrap().clone();
    match &rep.status {
        FeatureStatus::Ok | FeatureStatus::Warning(_) => {}
        s => return Err(format!("{s:?}")),
    }
    let lower = out.body_by_id(b).unwrap().shape().clone();
    let upper = out.body_by_id(BodyId::of(f)).unwrap().shape().clone();
    let status = rep.status.clone();
    Ok(Run { doc, lower, upper, report: rep.joint.unwrap(), status })
}

/// Two valid solids, no interference, watertight; optional shift test for the
/// horizontal clearance and the vertical (snap) clearance.
fn verify(r: &Run, label: &str, horizontal: bool, vertical: Option<f64>) -> Vec<String> {
    let mut errs = Vec::new();
    for (n, s) in [("lower", &r.lower), ("upper", &r.upper)] {
        if !s.is_valid() || s.count(casework_model::occt::ShapeKind::Solid) == 0 {
            errs.push(format!("{label}: {n} invalid"));
        }
        let m = weld(&s.tessellate(0.05, 12.0).unwrap());
        let open = open_edges(&m);
        if open != 0 {
            errs.push(format!("{label}: {n} mesh has {open} open edges"));
        }
    }
    let i0 = interference(&r.lower, &r.upper);
    if i0 > 1e-3 {
        errs.push(format!("{label}: halves interfere ({i0:.4} mm³)"));
    }
    let bump = DVec3::Z * 0.005;
    if horizontal {
        let c = r.report.clearance_per_side.unwrap();
        let near = interference(&r.lower, &r.upper.translated(DVec3::X * (0.8 * c) + bump).unwrap());
        let far = interference(&r.lower, &r.upper.translated(DVec3::X * (1.6 * c) + bump).unwrap());
        if near > 1e-4 {
            errs.push(format!("{label}: contact within the clearance ({near:.5})"));
        }
        if far <= 1e-6 {
            errs.push(format!("{label}: no contact past the clearance {c:.3} — clearance larger than reported"));
        }
    }
    if let Some(c) = vertical {
        let near = interference(&r.lower, &r.upper.translated(DVec3::Z * (0.8 * c)).unwrap());
        let far = interference(&r.lower, &r.upper.translated(DVec3::Z * (1.6 * c)).unwrap());
        if near > 1e-4 || far <= 1e-6 {
            errs.push(format!("{label}: vertical snap clearance wrong (near {near:.5}, far {far:.5})"));
        }
    }
    errs
}

type Case = (String, Shape, f64, Option<(f64, f64)>);

fn matrix() -> Vec<Case> {
    let z = 20.0;
    let mut v = Vec::new();
    for (r, draft) in [(0.5, 0.0), (3.0, 0.0), (3.0, 2.0), (6.0, 0.0), (6.0, 3.0)] {
        v.push((format!("rrect r{r} draft{draft}"), enclosure(Outline::RoundRect(r), 2.0, draft, Extras::default(), z), z, Some((4.0, 3.0))));
    }
    v.push(("L-shape".into(), enclosure(Outline::LShape, 2.0, 0.0, Extras::default(), z), z, Some((4.0, 3.0))));
    v.push(("DXF arcs".into(), enclosure(Outline::DxfArcs, 2.0, 1.0, Extras::default(), z), z, Some((4.0, 3.0))));
    let all = Extras { boss: true, rib: true, usb: true };
    v.push(("boss+rib+usb".into(), enclosure(Outline::RoundRect(4.0), 2.0, 0.0, all, z), z, Some((4.0, 3.0))));
    v.push(("4mm wall, no rim".into(), enclosure(Outline::RoundRect(4.0), 4.0, 0.0, Extras::default(), z), z, None));
    v
}

fn assert_all(errs: Vec<String>) {
    assert!(errs.is_empty(), "\n{}", errs.join("\n"));
}

#[test]
fn tongue_groove_matrix() {
    let mut errs = Vec::new();
    for (label, body, z, rim) in matrix() {
        match run(&body, z, tg(3.0), rim) {
            Ok(r) => errs.extend(verify(&r, &label, true, None)),
            Err(e) => errs.push(format!("{label}: {e}")),
        }
    }
    assert_all(errs);
}

#[test]
fn lip_matrix() {
    let mut errs = Vec::new();
    for (label, body, z, rim) in matrix() {
        match run(&body, z, lip(), rim) {
            Ok(r) => errs.extend(verify(&r, &label, true, None)),
            Err(e) => errs.push(format!("{label}: {e}")),
        }
    }
    assert_all(errs);
}

#[test]
fn oring_matrix() {
    let mut errs = Vec::new();
    for (label, body, z, rim) in matrix() {
        for rib in [false, true] {
            let rim = rim.or(Some((4.0, 3.0)));
            match run(&body, z, oring(rib), rim.map(|(_, h)| (5.0, h))) {
                Ok(r) => {
                    let o = r.report.oring.clone().unwrap();
                    if (o.squeeze - 0.25).abs() > 1e-6 {
                        errs.push(format!("{label}: squeeze {}", o.squeeze));
                    }
                    if o.fill > 0.85 {
                        errs.push(format!("{label}: fill {}", o.fill));
                    }
                    errs.extend(verify(&r, &format!("{label} rib={rib}"), rib, None));
                }
                Err(e) => errs.push(format!("{label} rib={rib}: {e}")),
            }
        }
    }
    assert_all(errs);
}

#[test]
fn snap_fit_matrix() {
    let mut errs = Vec::new();
    for (label, body, z, rim) in matrix() {
        let kind = if label.starts_with("4mm") {
            match snap(4) {
                JointKind::SnapFit { positions, thickness, width, undercut, insertion_deg, retention_deg, material, beam_on, clearance, margin, .. } => {
                    JointKind::SnapFit { positions, beam_length: 5.5.into(), thickness, width, undercut, insertion_deg, retention_deg, material, beam_on, clearance, margin }
                }
                k => k,
            }
        } else {
            snap(4)
        };
        match run(&body, z, kind, rim) {
            Ok(r) => {
                let placed = r.report.snaps.iter().filter(|s| s.placed.is_some()).count();
                if placed < 2 {
                    errs.push(format!("{label}: only {placed} snaps placed: {:?}", r.report.snaps));
                }
                errs.extend(verify(&r, &label, false, Some(r.report.clearance_per_side.unwrap())));
            }
            Err(e) => errs.push(format!("{label}: {e}")),
        }
    }
    assert_all(errs);
}

#[test]
fn thin_wall_needs_rim_and_limits_are_reported() {
    let body = enclosure(Outline::RoundRect(4.0), 2.0, 0.0, Extras::default(), 20.0);
    // a 1.5 mm tongue cannot fit a 2 mm wall without the rim …
    let wide = JointKind::TongueGroove { width: 1.5.into(), height: 3.0.into(), clearance: 0.15.into(), extra_depth: 0.3.into(), tongue_on: Half::Lower, lead_in: 0.0.into() };
    let e = run(&body, 20.0, wide.clone(), None).err().unwrap();
    assert!(e.contains("rim"), "{e}");
    // … but does with it
    let r = run(&body, 20.0, wide, Some((4.0, 3.0))).unwrap();
    assert!(r.report.rim_added && (r.report.effective_wall - 4.0).abs() < 1e-9);
    assert!((r.report.wall_thickness - 2.0).abs() < 0.05, "measured wall {}", r.report.wall_thickness);
    assert_all(verify(&r, "rim", true, None));
    // the rim adds material inward only: the outer bounding box is unchanged
    let bb = r.lower.fuse(&r.upper).unwrap().bbox().unwrap();
    assert!((bb.size().x - 80.0).abs() < 1e-4 && (bb.size().y - 60.0).abs() < 1e-4);

    // planes too close to the lid / floor give a clear reason, never a broken solid
    let e = run(&body, H - 2.5, tg(3.0), None).err().unwrap();
    assert!(e.contains("floor/lid") || e.contains("does not cut"), "{e}");
    let e = run(&body, 1.0, tg(3.0), None).err().unwrap();
    assert!(e.contains("floor") || e.contains("lid") || e.contains("cavity") || e.contains("wall"), "{e}");
    let e = run(&body, H + 5.0, tg(3.0), None).err().unwrap();
    assert!(e.contains("does not cut"), "{e}");
}

#[test]
fn fdm_vs_molded_clearance() {
    let body = enclosure(Outline::RoundRect(4.0), 2.0, 0.0, Extras::default(), 20.0);
    let molded = run(&body, 20.0, tg(3.0), Some((4.0, 3.0))).unwrap();
    let (mut doc, _) = (molded.doc, ());
    doc.set_active_profile("FDM 0.4 generic").unwrap();
    let f = doc.data.features.last().unwrap().id;
    let rep = doc.regenerate().report(f).unwrap().joint.clone().unwrap();
    let p = doc.data.profile();
    let a = p.fits.slide;
    let want = 0.15 + 2.0 * a + p.hole_comp.at(0.8 + 0.3) / 2.0 + p.xy_comp;
    assert!((molded.report.clearance_per_side.unwrap() - 0.15).abs() < 1e-9);
    assert!((rep.clearance_per_side.unwrap() - want).abs() < 1e-9, "{} vs {want}", rep.clearance_per_side.unwrap());
}

#[test]
fn oring_warnings_and_suggestions() {
    // sharp outer corners: below the cord's bend guidance → warning, not failure
    let body = enclosure(Outline::RoundRect(0.5), 2.0, 0.0, Extras::default(), 20.0);
    let r = run(&body, 20.0, oring(false), Some((5.0, 3.0))).unwrap();
    assert!(matches!(r.status, FeatureStatus::Warning(ref w) if w.contains("bend")), "{:?}", r.status);
    let o = r.report.oring.unwrap();
    assert!(o.min_corner_radius < 3.0 * 1.5);
    // explicit shallow gland: squeeze out of range → warning with suggested numbers
    let k = JointKind::ORingGland { cord: "M1.5".into(), squeeze: None, depth: Some(1.4.into()), width: None, gland_on: Half::Lower, compression_rib: None };
    let body = enclosure(Outline::RoundRect(6.0), 2.0, 0.0, Extras::default(), 20.0);
    let r = run(&body, 20.0, k, Some((5.0, 3.0))).unwrap();
    let FeatureStatus::Warning(w) = &r.status else { panic!("{:?}", r.status) };
    assert!(w.contains("squeeze") && w.contains("suggest gland depth 1.12"), "{w}");
    assert!(r.report.checks.iter().any(|c| c.name == "squeeze" && !c.ok));
    let (d, wd) = gland_for(1.5, 0.25, 0.75, 0.0, 0.0);
    assert!((d - 1.125).abs() < 1e-12 && (PI * 2.25 / 4.0 / (d * wd) - 0.75).abs() < 1e-12);
}

#[test]
fn snap_strain_and_placement() {
    let m = snap_material("PLA").unwrap();
    let c = snap_calc(5.0, 1.5, 5.0, 0.8, 30.0, &m);
    assert!(!c.pass && (c.strain - 0.072).abs() < 1e-12);
    assert!((c.suggested_length - (1.5f64 * 1.5 * 0.8 / 0.02).sqrt()).abs() < 1e-12);
    assert!(snap_calc(c.suggested_length + 1e-9, 1.5, 5.0, 0.8, 30.0, &m).pass);

    // an over-strained beam is reported (with the passing length) but still built
    let body = enclosure(Outline::RoundRect(6.0), 2.0, 0.0, Extras { boss: false, rib: false, usb: false }, 20.0);
    let short = JointKind::SnapFit {
        positions: SnapPositions::Count(2),
        beam_length: 5.0.into(),
        thickness: 1.5.into(),
        width: 5.0.into(),
        undercut: 0.8.into(),
        insertion_deg: 30.0.into(),
        retention_deg: 90.0.into(),
        material: "PLA".into(),
        beam_on: Half::Lower,
        clearance: 0.2.into(),
        margin: 2.0.into(),
    };
    let r = run(&body, 20.0, short, None).unwrap();
    let sc = r.report.snap.clone().unwrap();
    assert!(!sc.pass);
    assert!(matches!(&r.status, FeatureStatus::Warning(w) if w.contains("lengthen the beam to ≥ 9.49")), "{:?}", r.status);

    // hooks requested on a corner and on a boss are moved away
    let ring = body.section(DVec3::new(0.0, 0.0, 20.0), DVec3::Z).unwrap().fill_holes().unwrap();
    let corner = DVec3::new(80.0, 60.0, 20.0);
    let frac_corner = (0..2000)
        .map(|i| i as f64 / 2000.0)
        .min_by(|a, b| {
            let pa = ring.outer_wire_point(*a).unwrap().0;
            let pb = ring.outer_wire_point(*b).unwrap().0;
            pa.distance(corner).total_cmp(&pb.distance(corner))
        })
        .unwrap();
    // a boss merged into the +y wall at x = 40
    let boss = Shape::cylinder(DVec3::new(40.0, 60.0 - 2.0 - 3.0, 2.0), DVec3::Z, 3.5, H - 4.0).unwrap();
    let body2 = body.fuse(&boss).unwrap().unify().unwrap();
    let target = DVec3::new(40.0, 60.0, 20.0);
    let frac_boss = (0..2000)
        .map(|i| i as f64 / 2000.0)
        .min_by(|a, b| {
            let pa = ring.outer_wire_point(*a).unwrap().0;
            let pb = ring.outer_wire_point(*b).unwrap().0;
            pa.distance(target).total_cmp(&pb.distance(target))
        })
        .unwrap();
    let k = JointKind::SnapFit {
        positions: SnapPositions::At(vec![frac_corner, frac_boss]),
        beam_length: 6.0.into(),
        thickness: 1.0.into(),
        width: 5.0.into(),
        undercut: 0.5.into(),
        insertion_deg: 30.0.into(),
        retention_deg: 90.0.into(),
        material: "PETG".into(),
        beam_on: Half::Lower,
        clearance: 0.2.into(),
        margin: 2.0.into(),
    };
    let r = run(&body2, 20.0, k, None).unwrap();
    assert_eq!(r.report.snaps.len(), 2);
    let env = body2.section(DVec3::new(0.0, 0.0, 20.0), DVec3::Z).unwrap();
    let env = env.fill_holes().unwrap();
    let _ = env;
    for (s, avoid, min_d) in [(&r.report.snaps[0], corner, 6.0 + 2.5), (&r.report.snaps[1], target, 3.5 + 2.5)] {
        let placed = s.placed.unwrap_or_else(|| panic!("not placed: {s:?}"));
        assert!((placed - s.requested).abs() > 1e-6, "{s:?} was not moved");
        let p = ring.outer_wire_point(placed).unwrap().0;
        assert!(p.distance(avoid) > min_d, "{s:?} at {p:?} still too close to {avoid:?}");
    }
    assert_all(verify(&r, "moved snaps", false, Some(0.2)));
}


#[test]
fn impossible_draft_is_an_error() {
    // 30 mm × tan 2° > 0.5 mm corner radius: the drafted corner would invert
    let f = outline_face(Outline::RoundRect(0.5));
    let e = f.extrude_ext(&Frame::XY, DVec3::Z, H, false, 2.0).unwrap_err();
    assert!(e.0.contains("draft angle too large"), "{e}");
}
