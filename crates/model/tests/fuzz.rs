//! Randomized robustness: 200 enclosures with random parameters (fixed seed). Never
//! panic or crash; every success is a valid solid; every failure carries a reason.

use casework_model::document::Document;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::{Half, JointKind, Rim, SnapPositions};
use casework_model::params::Expr;
use casework_model::process::FitClass;
use casework_model::regen::FeatureStatus;
use casework_model::sketch::Sketch;
use glam::DVec2;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
    fn pick<T: Clone>(&mut self, v: &[T]) -> T {
        v[(self.next() % v.len() as u64) as usize].clone()
    }
}

fn half(r: &mut Rng) -> Half {
    if r.next().is_multiple_of(2) {
        Half::Lower
    } else {
        Half::Upper
    }
}

fn joint(r: &mut Rng) -> JointKind {
    match r.next() % 4 {
        0 => JointKind::TongueGroove {
            width: r.f(0.4, 2.5).into(),
            height: r.f(0.5, 5.0).into(),
            clearance: r.f(0.0, 0.4).into(),
            extra_depth: r.f(0.0, 1.0).into(),
            tongue_on: half(r),
            lead_in: r.f(0.0, 0.5).into(),
        },
        1 => JointKind::Lip { width: r.f(0.4, 2.5).into(), height: r.f(0.5, 5.0).into(), clearance: r.f(0.0, 0.4).into(), lip_on: half(r) },
        2 => JointKind::ORingGland {
            cord: r.pick(&["M1.0", "M1.5", "M2.0", "AS568-0xx", "AS568-1xx", "M3.0", "nope"]).into(),
            squeeze: if r.next().is_multiple_of(3) { Some(r.f(0.05, 0.5).into()) } else { None },
            depth: None,
            width: None,
            gland_on: half(r),
            compression_rib: if r.next().is_multiple_of(2) { Some((r.f(0.3, 1.0).into(), r.f(0.1, 0.4).into())) } else { None },
        },
        _ => JointKind::SnapFit {
            positions: SnapPositions::Count((r.next() % 6) as u32 + 1),
            beam_length: r.f(3.0, 14.0).into(),
            thickness: r.f(0.6, 2.0).into(),
            width: r.f(2.0, 8.0).into(),
            undercut: r.f(0.2, 1.2).into(),
            insertion_deg: r.f(15.0, 60.0).into(),
            retention_deg: r.f(45.0, 90.0).into(),
            material: r.pick(&["PLA", "PETG", "ABS", "PC", "PA6", "PP"]).into(),
            beam_on: half(r),
            clearance: r.f(0.05, 0.4).into(),
            margin: r.f(0.5, 3.0).into(),
        },
    }
}

#[test]
fn random_enclosures_never_crash_and_stay_valid() {
    let mut rng = Rng(0x5eed_cafe_f00d_1234);
    let profiles = ["FDM 0.4 generic", "SLA generic", "Injection molded (nominal)"];
    let (mut ok, mut failed) = (0, 0);
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for it in 0..200 {
        let (w, d, h) = (rng.f(40.0, 100.0), rng.f(30.0, 80.0), rng.f(15.0, 40.0));
        let wall = rng.f(1.2, 4.0);
        let r = rng.f(0.3, 8.0).min(d / 2.0 - 1.0);
        let mut doc = Document::new("Fuzz");
        doc.set_active_profile(rng.pick(&profiles)).unwrap();
        let mut outer = Sketch::new("O", Plane::XY);
        outer.rounded_rect(DVec2::ZERO, w, d, r, (w, d, r));
        let outer = doc.add_sketch(outer);
        let body = BodyId::of(doc.add_feature(
            "Body",
            FeatureKind::Extrude { sketch: outer, profiles: ProfileSel::All, distance: h.into(), symmetric: false, reverse: false, draft_deg: rng.f(0.0, 2.0).into(), op: BoolOp::NewBody, target: None },
        ));
        let mut inner = Sketch::new("I", Plane::XY.offset(wall));
        let ri = r - wall;
        if ri > 0.05 {
            inner.rounded_rect(DVec2::ZERO, w - 2.0 * wall, d - 2.0 * wall, ri, (w - 2.0 * wall, d - 2.0 * wall, ri));
        } else {
            inner.rect(DVec2::new(-w / 2.0 + wall, -d / 2.0 + wall), DVec2::new(w / 2.0 - wall, d / 2.0 - wall));
        }
        let inner = doc.add_sketch(inner);
        doc.add_feature(
            "Cavity",
            FeatureKind::Extrude { sketch: inner, profiles: ProfileSel::All, distance: (h - 2.0 * wall).into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(body) },
        );
        let n = (rng.next() % 4) as usize;
        let pts: Vec<DVec2> = (0..n).map(|_| DVec2::new(rng.f(-w / 2.0, w / 2.0), rng.f(-d / 2.0, d / 2.0))).collect();
        if !pts.is_empty() {
            doc.add_feature(
                "Bosses",
                FeatureKind::ScrewBoss {
                    body: Some(body),
                    plane: Plane::XY.offset(wall),
                    points: pts,
                    od: rng.f(4.0, 9.0).into(),
                    hole_dia: rng.f(1.5, 4.0).into(),
                    height: rng.f(3.0, h).into(),
                    hole_depth: None,
                    base_fillet: rng.f(0.0, 1.5).into(),
                    draft_deg: rng.f(0.0, 2.0).into(),
                },
            );
        }
        let rim = if rng.next().is_multiple_of(2) { Some(Rim { width: rng.f(2.0, 6.0).into(), height: rng.f(1.0, 5.0).into() }) } else { None };
        let jf = doc.add_feature(
            "Joint",
            FeatureKind::PartingJoint {
                body: Some(body),
                plane: Plane::XY,
                offset: Expr::from(rng.f(-5.0, h + 5.0)),
                kind: joint(&mut rng),
                rim,
                upper_name: "Lid".into(),
                class: rng.pick(&[FitClass::Snug, FitClass::Slide, FitClass::Loose]),
                closing_radius: None,
            },
        );
        let out = doc.regenerate().clone();
        for rep in &out.reports {
            match &rep.status {
                FeatureStatus::Error(e) => {
                    assert!(!e.trim().is_empty(), "iteration {it}: empty error");
                    if e.contains("SIGSEGV") || e.contains("OSD_") {
                        println!("iteration {it}: caught kernel signal in {:?}: {e}", rep.feature);
                    }
                    if rep.feature == jf {
                        failed += 1;
                        let key: String = e.chars().take(70).collect();
                        *reasons.entry(key).or_default() += 1;
                    }
                }
                FeatureStatus::Unresolved(_) => {}
                _ => {
                    if rep.feature == jf {
                        ok += 1;
                    }
                }
            }
        }
        for b in out.solids() {
            assert!(b.shape().is_valid(), "iteration {it}: body {} is invalid", b.name);
            assert!(b.shape().count(casework_model::occt::ShapeKind::Solid) >= 1, "iteration {it}: body {} has no solid", b.name);
        }
        if matches!(out.report(jf).map(|r| &r.status), Some(FeatureStatus::Ok | FeatureStatus::Warning(_))) {
            assert!(out.body_by_id(BodyId::of(jf)).is_some(), "iteration {it}: joint without an upper half");
        }
    }
    println!("joints ok: {ok}, refused with a reason: {failed}");
    for (r, n) in &reasons {
        println!("  {n:3} × {r}");
    }
    assert!(ok > 40, "too few successful joints: {ok}");
}
