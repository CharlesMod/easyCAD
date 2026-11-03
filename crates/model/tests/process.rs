//! Process profiles (fits), teardrop holes, build orientation, export overrides,
//! calibration coupon.

use std::f64::consts::PI;

use casework_model::document::{Document, ExportFormat, ExportOptions};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::occt::Shape;
use casework_model::params::Expr;
use casework_model::process::*;
use casework_model::sketch::Sketch;
use glam::{DVec2, DVec3};

const FDM: &str = "FDM 0.4 generic";
const MOLDED: &str = "Injection molded (nominal)";

#[test]
fn fit_resolution_and_expression_helpers() {
    let p = ProcessProfile::fdm_generic();
    let want = 3.0 + 2.0 * p.fits.slide + p.hole_comp.at(3.0);
    assert!((p.resolve(3.0, Fit::hole(FitClass::Slide)) - want).abs() < 1e-12);
    assert!((p.resolve(3.0, Fit::pin(FitClass::Slide)) - (3.0 - 2.0 * p.fits.slide - 2.0 * p.xy_comp)).abs() < 1e-12);
    assert!((p.resolve(0.2, Fit::gap(FitClass::Loose)) - (0.2 + p.fits.loose)).abs() < 1e-12);
    assert_eq!(p.resolve(3.0, Fit::NOMINAL), 3.0);
    assert_eq!(ProcessProfile::molded_nominal().resolve(3.0, Fit::hole(FitClass::Loose)), 3.0);

    let mut doc = Document::new("Fits");
    doc.set_param("bore", "hole(3)").unwrap();
    doc.set_param("shaft", "pin(3, press)").unwrap();
    doc.set_param("play", "gap(loose)").unwrap();
    doc.set_param("t", "tol(snug)").unwrap();
    assert_eq!(doc.data.values().unwrap()["bore"], 3.0, "nominal under the molded default");
    doc.set_active_profile(FDM).unwrap();
    let v = doc.data.values().unwrap();
    assert!((v["bore"] - want).abs() < 1e-12, "hole(3) under FDM = 3 + 2×slide + hole_comp");
    assert!((v["shaft"] - (3.0 + 0.1 - 0.1)).abs() < 1e-12);
    assert!((v["play"] - p.fits.loose).abs() < 1e-12);
    assert!((v["t"] - p.fits.snug).abs() < 1e-12);
    assert!(doc.set_active_profile("nope").is_err());

    // Expr with a fit round-trips through JSON; plain expressions stay plain strings
    let e = Expr::from("5").fit(Fit::hole(FitClass::Slide));
    let j = serde_json::to_string(&e).unwrap();
    assert!(j.contains("\"fit\""));
    assert_eq!(serde_json::from_str::<Expr>(&j).unwrap(), e);
    assert_eq!(serde_json::to_string(&Expr::from("5")).unwrap(), "\"5\"");
}

/// 20 mm cube with a Ø5 hole along X (horizontal for the default orientation) and a
/// Ø5 hole along Z (vertical).
fn holed_block() -> (Document, BodyId) {
    let mut doc = Document::new("Block");
    let mut s = Sketch::new("B", Plane::XY);
    s.rect(DVec2::new(-10.0, -10.0), DVec2::new(10.0, 10.0));
    let s = doc.add_sketch(s);
    let b = BodyId::of(doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let mut hx = Sketch::new("HX", Plane::YZ.offset(-15.0));
    hx.circle(DVec2::new(-4.0, 10.0), 2.5); // YZ plane: local x = Y, local y = Z
    let mut hz = Sketch::new("HZ", Plane::XY.offset(-5.0));
    hz.circle(DVec2::new(5.0, 5.0), 2.5);
    for (name, sk) in [("Hole X", hx), ("Hole Z", hz)] {
        let id = doc.add_sketch(sk);
        doc.add_feature(
            name,
            FeatureKind::Extrude { sketch: id, profiles: ProfileSel::All, distance: 40.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(b) },
        );
    }
    (doc, b)
}

#[test]
fn teardrop_holes() {
    let (mut doc, b) = holed_block();
    let nominal = 8000.0 - 2.0 * PI * 6.25 * 20.0;
    let v = doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap();
    assert!((v - nominal).abs() < 1e-6, "molded: no teardrop");
    assert!(doc.regenerate().process.teardropped.is_empty());

    // FDM default: flat-topped (1.0·r) teardrop on the horizontal hole only
    doc.set_active_profile(FDM).unwrap();
    let style = doc.data.profile().teardrop.style;
    let out = doc.regenerate();
    assert_eq!(out.process.teardropped.len(), 1, "{:?}", out.process);
    assert!(out.process.teardropped[0].axis.x.abs() > 0.99);
    assert!(out.process.skipped.iter().any(|(h, why)| h.axis.z.abs() > 0.99 && why.contains("from horizontal")));
    let v = out.body_by_id(b).unwrap().shape().volume().unwrap();
    let cap = style.cap_area(2.5);
    assert!((cap - (6.25 * (1.0 - PI / 4.0) - (2.5 * (2f64.sqrt() - 1.0)).powi(2))).abs() < 1e-12);
    assert!((nominal - v - cap * 20.0).abs() < 1e-4, "removed {} vs {}", nominal - v, cap * 20.0);
    assert!(out.body_by_id(b).unwrap().shape().is_valid());

    // pointed variant: r²(1 − π/4) per mm
    let mut p = doc.data.profile();
    p.name = "FDM pointed".into();
    p.teardrop.style = TeardropStyle::Pointed45;
    doc.add_profile(p);
    doc.set_active_profile("FDM pointed").unwrap();
    let v = vol(&mut doc, b);
    assert!((nominal - v - 6.25 * (1.0 - PI / 4.0) * 20.0).abs() < 1e-4);

    // turning the part so +X faces down makes the Z hole horizontal instead
    doc.set_build_orientation(None, BuildOrientation { down: CubeFace::NegX, quarter_turns: 0 });
    let out = doc.regenerate();
    assert_eq!(out.process.teardropped.len(), 1);
    assert!(out.process.teardropped[0].axis.z.abs() > 0.99);
    assert_eq!(out.recomputed, 0, "orientation only re-runs the process stage");

    // per-hole override switches it off
    let face = out.process.teardropped[0].face;
    let pre = { doc.regenerator().clear(); doc.data.clone() };
    let _ = pre;
    let finfo = {
        doc.set_active_profile(MOLDED).unwrap();
        let s = doc.regenerate().body_by_id(b).unwrap().shape().clone();
        s.faces().unwrap()[face as usize].clone()
    };
    doc.set_active_profile("FDM pointed").unwrap();
    doc.edit("No teardrop", |d| {
        d.teardrop_overrides.push(casework_model::document::TeardropOverride { body: b, face: casework_model::select::FaceSelector::from_pick(&finfo), enabled: false });
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    assert!(out.process.teardropped.is_empty(), "{:?}", out.process);
    assert!(out.process.skipped.iter().any(|(_, w)| w == "disabled by override"));
}

fn vol(doc: &mut Document, b: BodyId) -> f64 {
    doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap()
}

#[test]
fn profile_switch_invalidates_cache_and_fits_apply() {
    let mut doc = Document::new("Boss");
    let mut s = Sketch::new("B", Plane::XY);
    s.rect(DVec2::new(-10.0, -10.0), DVec2::new(10.0, 10.0));
    let s = doc.add_sketch(s);
    let b = BodyId::of(doc.add_feature(
        "Plate",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 3.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    doc.add_feature(
        "Boss",
        FeatureKind::ScrewBoss { body: Some(b), plane: Plane::XY.offset(3.0), points: vec![DVec2::ZERO], od: 7.0.into(), hole_dia: 2.5.into(), height: 10.0.into(), hole_depth: None, base_fillet: 0.0.into(), draft_deg: 0.0.into() },
    );
    let v_m = vol(&mut doc, b);
    doc.set_active_profile(FDM).unwrap();
    let r = doc.regenerate();
    assert!(r.recomputed >= 1 && r.reused == 0, "profile switch invalidates");
    let v_f = vol(&mut doc, b);
    let p = ProcessProfile::fdm_generic();
    let d = p.resolve(2.5, Fit::hole(FitClass::Snug));
    assert!(((v_m - v_f) - PI * ((d / 2.0).powi(2) - 1.5625) * 10.0).abs() < 1e-4, "boss hole is a Hole/Snug by default");
    doc.set_active_profile(MOLDED).unwrap();
    let r = doc.regenerate();
    assert_eq!(r.recomputed, 0, "switching back is served from the cache");
    assert!((vol(&mut doc, b) - v_m).abs() < 1e-9);

    // explicit Nominal override on the dimension disables the default role
    doc.set_active_profile(FDM).unwrap();
    doc.edit("nominal", |d| {
        if let FeatureKind::ScrewBoss { hole_dia, .. } = &mut d.features[1].kind {
            *hole_dia = Expr::from(2.5).fit(Fit::NOMINAL);
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    assert!((vol(&mut doc, b) - v_m).abs() < 1e-9);
}

#[test]
fn export_with_profile_override_and_orientation() {
    let (mut doc, b) = holed_block();
    let dir = tempfile::tempdir().unwrap();
    // STEP for molding from a doc whose active profile is FDM
    doc.set_active_profile(FDM).unwrap();
    let fdm_v = vol(&mut doc, b);
    let step = dir.path().join("molded.step");
    doc.export(ExportFormat::Step, &step, &ExportOptions { profile: Some(MOLDED.into()), ..Default::default() }).unwrap();
    let back = Shape::read_step(&step).unwrap();
    let nominal = 8000.0 - 2.0 * PI * 6.25 * 20.0;
    assert!((back.volume().unwrap() - nominal).abs() < 1e-4, "override export has no teardrop");
    assert!(fdm_v < nominal);
    assert_eq!(doc.data.active_profile, FDM, "export does not change the document");

    // STL for FDM, re-oriented so +Y sits on the plate
    doc.set_build_orientation(Some(b), BuildOrientation { down: CubeFace::PosY, quarter_turns: 1 });
    let stl = dir.path().join("fdm.stl");
    doc.export(ExportFormat::Stl, &stl, &ExportOptions { profile: None, orient_to_plate: true, include_reference: false }).unwrap();
    assert!(std::fs::metadata(&stl).unwrap().len() > 84);
    let step2 = dir.path().join("oriented.step");
    doc.export(ExportFormat::Step, &step2, &ExportOptions { profile: None, orient_to_plate: true, include_reference: false }).unwrap();
    let s = Shape::read_step(&step2).unwrap();
    let bb = s.bbox().unwrap();
    assert!(bb.min.z.abs() < 1e-6, "dropped onto the plate");
    // the hole that was along X is now horizontal along Y-or-X but never vertical
    let up = BuildOrientation { down: CubeFace::PosY, quarter_turns: 1 }.quat() * DVec3::NEG_Y;
    assert!((up - DVec3::Z).length() < 1e-9, "model −Y points up after orienting +Y down");
}

#[test]
fn calibration_coupon_and_from_calibration() {
    let offs = default_coupon_offsets();
    assert_eq!(offs.len(), 13);
    assert!((offs[2]).abs() < 1e-12 && (offs[12] - 0.5).abs() < 1e-12);
    let (coupon, cells) = calibration_coupon(5.0, &offs).unwrap();
    assert!(coupon.is_valid());
    assert_eq!(coupon.count(casework_model::occt::ShapeKind::Solid), 1);
    assert_eq!(cells.len(), 13);
    assert_eq!(cells[4].notches, 5);
    let holes = coupon.faces().unwrap().into_iter().filter(|f| f.kind == casework_model::occt::SurfaceKind::Cylinder && f.normal.dot(f.point - f.axis.unwrap().0) < 0.0).count();
    assert_eq!(holes, 13);

    // feed back what a "true" profile would produce and recover it
    let truth = ProcessProfile::fdm_generic();
    let hole_comp = 0.12;
    let xy = 0.04;
    let off = |c: FitClass| 4.0 * truth.allowance(c) + hole_comp + 2.0 * xy;
    let r = CalibrationResult {
        name: "My Prusa".into(),
        kind: ProcessKind::Fdm,
        nominal: 5.0,
        press_offset: off(FitClass::Press),
        snug_offset: off(FitClass::Snug),
        slide_offset: off(FitClass::Slide),
        loose_offset: off(FitClass::Loose),
        hole_measured: Some(5.0 - hole_comp),
        pin_measured: Some(5.0 + 2.0 * xy),
        first_layer_measured: Some(0.2),
    };
    let p = ProcessProfile::from_calibration(&r);
    for c in FitClass::ALL {
        assert!((p.allowance(c) - truth.allowance(c)).abs() < 1e-9, "{c:?}");
    }
    assert!((p.hole_comp.base - hole_comp).abs() < 1e-12 && (p.xy_comp - xy).abs() < 1e-12);
    assert!(p.teardrop.enabled);
    // presets round-trip through a data dir
    let dir = tempfile::tempdir().unwrap();
    p.save_preset(dir.path()).unwrap();
    assert_eq!(ProcessProfile::load_presets(dir.path()).unwrap(), vec![p]);
}
