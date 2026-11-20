//! Regression: picking several regions of soap3's Sketch 4 must never hang the
//! regen thread (OCCT UnifySameDomain spun forever on the fused region solids).

use easycad_model::document::Document;
use easycad_model::feature::*;
use std::sync::mpsc;
use std::time::Duration;

/// Run `f` on a thread; panic if it takes longer than 20 s.
fn timed<T: Send + 'static>(what: String, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(v) => v,
        Err(_) => panic!("HANG (>20s): {what}"),
    }
}

fn run(fx: &'static str, sk: u32, mask: usize, op: BoolOp, symmetric: bool, target_feature: u32) -> (bool, f64) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(fx);
    let mut doc = Document::load_from_dir(&dir).unwrap();
    let sid = SketchId(sk);
    let regions = doc.data.sketch(sid).expect("sketch 4").clone().regions();
    let pick: Vec<_> = (0..regions.len()).filter(|i| mask >> i & 1 == 1).map(|i| regions[i].sample).collect();
    let target = if op == BoolOp::NewBody { None } else { Some(BodyId { feature: FeatureId(target_feature), index: 0 }) };
    let f = doc.add_feature(
        "Extrude 10",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::Containing(pick), distance: 10.0.into(), symmetric, reverse: false, draft_deg: 0.0.into(), op, target },
    );
    let r = doc.regenerate();
    let ok = !r.errors().iter().any(|(id, _)| *id == f);
    let vol = r.solids().map(|b| b.shape().volume().unwrap_or(0.0)).sum();
    (ok, vol)
}

fn sweep(fx: &'static str, sk: u32, targets: &[u32]) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(fx);
    let n = Document::load_from_dir(&dir).unwrap().data.sketch(SketchId(sk)).unwrap().clone().regions().len();
    for mask in 1..(1usize << n) {
        for op in [BoolOp::NewBody, BoolOp::Join, BoolOp::Cut] {
            for symmetric in [false, true] {
                for &target in targets {
                    let t0 = std::time::Instant::now();
                    let (ok, vol) = timed(format!("{fx} sketch {sk} mask {mask:b} {op:?} sym={symmetric} target={target}"), move || run(fx, sk, mask, op, symmetric, target));
                    eprintln!("{fx} mask {mask:04b} {op:?} sym={symmetric} tgt={target}: ok={ok} vol={vol:.1} {:?}", t0.elapsed());
                    assert!(t0.elapsed() < Duration::from_secs(10));
                }
            }
        }
    }
}

#[test]
fn soap3_sketch4_region_combinations_never_hang() {
    sweep("soap3.ecad", 7, &[2]);
}

#[test]
fn soap3_user_sketch3_region_combinations_never_hang() {
    sweep("soap3_user.ecad", 5, &[2, 4]);
}
