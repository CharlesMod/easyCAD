//! Randomized robustness (fixed seeds). `random_enclosures_never_crash_and_stay_valid`
//! throws wild parameters (planes outside the body, walls thinner than the joint):
//! never panic or crash, every success is a valid solid, every failure carries a
//! reason. `realistic_joint_refusal_rate` uses parameters a designer would enter and
//! prints the refusals split into infeasible requests and algorithm bail-outs.

use casework_model::document::Document;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::{refusal_kind, Half, JointKind, RefusalKind, Rim, SnapPositions};
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
                    offset: 0.0.into(),
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
                        let key: String = e.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().replace("#.##", "#").chars().take(80).collect();
                        *reasons.entry(format!("{:?}: {key}", refusal_kind(e))).or_default() += 1;
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
    let bailed: usize = reasons.iter().filter(|(k, _)| k.starts_with("Bailed")).map(|(_, n)| n).sum();
    println!("joints ok: {ok}, refused with a reason: {failed} ({} infeasible, {bailed} bailed)", failed - bailed);
    for (r, n) in &reasons {
        println!("  {n:3} × {r}");
    }
    assert!(ok > 40, "too few successful joints: {ok}");
}

/// A joint a designer would ask for on a wall of thickness `wall`.
fn realistic_joint(r: &mut Rng, wall: f64, room: f64) -> JointKind {
    match r.next() % 4 {
        0 => JointKind::TongueGroove {
            width: (wall * r.f(0.3, 0.5)).into(),
            height: r.f(1.5, 4.0).into(),
            clearance: r.f(0.05, 0.25).into(),
            extra_depth: r.f(0.1, 0.5).into(),
            tongue_on: half(r),
            lead_in: r.f(0.0, 0.3).into(),
        },
        1 => JointKind::Lip { width: (wall * r.f(0.35, 0.5)).into(), height: r.f(1.5, 4.0).into(), clearance: r.f(0.05, 0.25).into(), lip_on: half(r) },
        2 => JointKind::ORingGland {
            cord: r.pick(&["M1.0", "M1.5", "M2.0", "AS568-0xx"]).into(),
            squeeze: None,
            depth: None,
            width: None,
            gland_on: half(r),
            compression_rib: if r.next().is_multiple_of(3) { Some((r.f(0.3, 0.6).into(), r.f(0.1, 0.3).into())) } else { None },
        },
        _ => JointKind::SnapFit {
            positions: SnapPositions::Count((r.next() % 3) as u32 + 2),
            beam_length: r.f(6.0, 12.0).min(room - 2.0).max(4.0).into(),
            thickness: r.f(0.8, 1.6).into(),
            width: r.f(3.0, 8.0).into(),
            undercut: r.f(0.3, 0.8).into(),
            insertion_deg: r.f(20.0, 40.0).into(),
            retention_deg: r.f(60.0, 90.0).into(),
            material: r.pick(&["PLA", "PETG", "ABS", "PC", "PA6", "PP"]).into(),
            beam_on: half(r),
            clearance: r.f(0.1, 0.3).into(),
            margin: r.f(0.5, 2.0).into(),
        },
    }
}

#[test]
fn realistic_joint_refusal_rate() {
    let mut rng = Rng(0x0dd_ba11_c0ff_ee42);
    let profiles = ["FDM 0.4 generic", "SLA generic", "Injection molded (nominal)"];
    let n_iter: usize = std::env::var("CW_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let (mut ok, mut adjusted, mut upstream_failed) = (0, 0, 0);
    let mut reasons = std::collections::BTreeMap::<(RefusalKind, String), usize>::new();
    let mut upstream_reasons = std::collections::BTreeMap::<String, usize>::new();
    let mut kinds = std::collections::BTreeMap::<RefusalKind, usize>::new();
    for it in 0..n_iter {
        let (w, d, h) = (rng.f(50.0, 140.0), rng.f(40.0, 100.0), rng.f(20.0, 50.0));
        let wall = rng.f(1.6, 3.0);
        let r = rng.f(1.0, 10.0).min(d / 2.0 - 1.0);
        let mut doc = Document::new("Realistic");
        doc.set_active_profile(rng.pick(&profiles)).unwrap();
        let mut outer = Sketch::new("O", Plane::XY);
        outer.rounded_rect(DVec2::ZERO, w, d, r, (w, d, r));
        let outer = doc.add_sketch(outer);
        let body = BodyId::of(doc.add_feature(
            "Body",
            FeatureKind::Extrude { sketch: outer, profiles: ProfileSel::All, distance: h.into(), symmetric: false, reverse: false, draft_deg: rng.f(0.0, 1.5).into(), op: BoolOp::NewBody, target: None },
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
        // corner bosses, inset from the walls (sometimes merged into them)
        let od = rng.f(5.0, 8.0);
        let n = (rng.next() % 5) as usize;
        let inset = rng.f(-1.0, 4.0);
        let (bx, by) = (w / 2.0 - wall - od / 2.0 - inset, d / 2.0 - wall - od / 2.0 - inset);
        let pts: Vec<DVec2> = [(-bx, -by), (bx, -by), (bx, by), (-bx, by)].iter().take(n).map(|p| DVec2::new(p.0, p.1)).collect();
        if !pts.is_empty() {
            doc.add_feature(
                "Bosses",
                FeatureKind::ScrewBoss {
                    body: Some(body),
                    plane: Plane::XY.offset(wall),
                    points: pts,
                    od: od.into(),
                    hole_dia: rng.f(2.0, 3.0).into(),
                    height: rng.f(4.0, h * 0.5).into(),
                    hole_depth: None,
                    base_fillet: rng.f(0.0, 1.0).into(),
                    draft_deg: rng.f(0.0, 1.0).into(),
                    offset: 0.0.into(),
                },
            );
        }
        let split = rng.f(wall + 4.0, h - wall - 4.0);
        let rim = if rng.next().is_multiple_of(2) { Some(Rim { width: (wall + rng.f(1.0, 3.0)).into(), height: rng.f(2.0, 5.0).into() }) } else { None };
        let jf = doc.add_feature(
            "Joint",
            FeatureKind::PartingJoint {
                body: Some(body),
                plane: Plane::XY,
                offset: Expr::from(split),
                kind: realistic_joint(&mut rng, wall, split.min(h - split)),
                rim,
                upper_name: "Lid".into(),
                class: rng.pick(&[FitClass::Snug, FitClass::Slide, FitClass::Loose]),
                closing_radius: None,
            },
        );
        let out = doc.regenerate().clone();
        let rep = out.report(jf).unwrap();
        let upstream: Vec<String> = out.errors().into_iter().filter(|(f, _)| *f != jf).map(|(_, e)| e).collect();
        match &rep.status {
            FeatureStatus::Error(_) if !upstream.is_empty() => {
                // not the joint's fault (e.g. a draft too large for the corner radius)
                upstream_failed += 1;
                let key: String = upstream[0].chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().chars().take(90).collect();
                *upstream_reasons.entry(key).or_default() += 1;
            }
            FeatureStatus::Error(e) => {
                let kind = refusal_kind(e);
                *kinds.entry(kind).or_default() += 1;
                // group by message shape (numbers stripped)
                let key: String = e.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>().replace("#.##", "#").chars().take(90).collect();
                *reasons.entry((kind, key)).or_default() += 1;
                if std::env::var("CW_FUZZ_VERBOSE").is_ok() {
                    println!("iteration {it}: {kind:?}: {e}");
                    for (f, err) in out.errors().iter().filter(|(f, _)| *f != jf) {
                        println!("    upstream {f:?}: {err}");
                    }
                }
            }
            FeatureStatus::Ok | FeatureStatus::Warning(_) => {
                ok += 1;
                if rep.joint.as_ref().is_some_and(|j| !j.adjustments.is_empty()) {
                    adjusted += 1;
                }
                assert!(out.body_by_id(BodyId::of(jf)).is_some(), "iteration {it}: joint without an upper half");
            }
            other => panic!("iteration {it}: unexpected joint status {other:?}"),
        }
        for b in out.solids() {
            assert!(b.shape().is_valid(), "iteration {it}: body {} is invalid", b.name);
        }
    }
    let inf = kinds.get(&RefusalKind::Infeasible).copied().unwrap_or(0);
    let bail = kinds.get(&RefusalKind::Bailed).copied().unwrap_or(0);
    println!("realistic joints: {ok} built ({adjusted} with adjustments), {inf} infeasible, {bail} bailed, {upstream_failed} not reached (an earlier feature failed) (of {n_iter})");
    for ((k, r), n) in &reasons {
        println!("  {n:3} × {k:?}: {r}");
    }
    for (r, n) in &upstream_reasons {
        println!("  {n:3} × upstream: {r}");
    }
    // measured: 195 of 200 built, 0 bailed (see docs/ENGINE.md "Robustness")
    if n_iter == 200 {
        assert!(ok >= 190, "too few joints built: {ok}");
        assert!(bail <= 2, "the algorithm bailed on {bail} buildable joints");
    }
}
