//! Fillet / chamfer fuzz on realistic enclosures (fixed seeds): random edge sets ×
//! sizes through `occt::dressup::dress`, on the PCB wizard's bodies (the saved
//! `tests/data/wizard_*.ecad`, at several points of their history) and on generated
//! boxes with bosses, shells and cuts.
//!
//! Every request ends in one of: **applied** (every edge at its size), **reduced**
//! (every edge applied, some at a smaller auto-reduced size), **skipped** (some edges
//! left sharp, reported with a reason) or **bailed** (the pipeline errored, panicked
//! or returned an invalid / different-solid-count shape). A skipped edge is
//! **wrongly skipped** when it is still sharp on the pipeline's result and the plain
//! kernel call takes it there at the requested size: the ladder gave up on an edge
//! it could have done.
//!
//! `fillet_fuzz_full` (ignored, ~minutes) prints the histogram:
//! `cargo test -p easycad-model --release --test fillet_fuzz -- --ignored --nocapture`.
//! `fillet_fuzz_regressions` runs a fast deterministic subset (cases that bailed
//! or were wrongly skipped before) and asserts none of them does now.

use std::path::Path;

use easycad_model::document::Document;
use easycad_model::occt::dressup::{self, DressUpKind, DressUpOptions, EdgeStatus};
use easycad_model::occt::{Shape, ShapeKind};
use glam::DVec3;

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
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Bodies of a saved wizard document, after the first `at` features (`None`: all).
fn wizard_bodies(file: &str, at: Option<usize>) -> Vec<(String, Shape)> {
    let mut doc = Document::load_from_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(file)).unwrap();
    doc.set_rollback(at);
    let r = doc.regenerate();
    r.solids().filter(|b| !b.reference).map(|b| (format!("{file}@{at:?}:{}", b.name), b.shape().clone())).collect()
}

/// Generated enclosure-like solids: a rounded box, shelled (open top or closed),
/// with bosses on the floor, a slot through a wall and a hole pattern.
fn generated(seed: u64) -> (String, Shape) {
    let mut r = Rng(seed);
    let (w, d, h) = (r.f(40.0, 90.0), r.f(30.0, 60.0), r.f(15.0, 35.0));
    let mut s = Shape::make_box(DVec3::new(-w / 2.0, -d / 2.0, 0.0), DVec3::new(w, d, h)).unwrap();
    let vert: Vec<(u32, f64)> = s.edges().unwrap().iter().filter(|e| e.tangent.z.abs() > 0.99).map(|e| (e.index, 0.0)).collect();
    let cr = r.f(1.0, 8.0);
    if let Ok(x) = s.fillet_raw(&vert.iter().map(|v| v.0).collect::<Vec<_>>(), &vec![cr; vert.len()]) {
        s = x;
    }
    let t = r.f(1.2, 3.0);
    let top: Vec<u32> = s.faces().unwrap().iter().filter(|f| f.normal.z > 0.99 && f.point.z > h - 1e-6).map(|f| f.index).collect();
    let open = !r.next().is_multiple_of(3);
    if let Ok(x) = s.shell(if open { &top } else { &[] }, t, false) {
        s = x;
    }
    let bosses = r.below(5);
    for k in 0..bosses {
        let p = DVec3::new(r.f(-w / 2.0 + t + 4.0, w / 2.0 - t - 4.0), r.f(-d / 2.0 + t + 4.0, d / 2.0 - t - 4.0), t);
        let od = r.f(4.0, 8.0);
        let b = Shape::cylinder(p, DVec3::Z, od / 2.0, r.f(2.0, h * 0.5)).unwrap();
        if let Ok(x) = s.fuse(&b).and_then(|x| x.unify()) {
            s = x;
        }
        if k % 2 == 0 {
            let hole = Shape::cylinder(p, DVec3::Z, od / 4.0, h).unwrap();
            if let Ok(x) = s.cut(&hole).and_then(|x| x.unify()) {
                s = x;
            }
        }
    }
    if r.next().is_multiple_of(2) {
        // a slot through the +x wall
        let (sw, sh) = (r.f(5.0, d * 0.6), r.f(2.0, h * 0.4));
        let slot = Shape::make_box(DVec3::new(w / 2.0 - t - 1.0, -sw / 2.0, h * 0.3), DVec3::new(t + 2.0, sw, sh)).unwrap();
        if let Ok(x) = s.cut(&slot).and_then(|x| x.unify()) {
            s = x;
        }
    }
    if r.next().is_multiple_of(2) {
        // vent holes through the floor
        for i in 0..3 {
            let c = Shape::cylinder(DVec3::new(-10.0 + 10.0 * i as f64, -d / 4.0, -1.0), DVec3::Z, 1.5, t + 2.0).unwrap();
            if let Ok(x) = s.cut(&c).and_then(|x| x.unify()) {
                s = x;
            }
        }
    }
    (format!("generated #{seed}"), s)
}

#[derive(Default, Debug)]
struct Hist {
    applied: usize,
    reduced: usize,
    skipped: usize,
    bailed: usize,
    edges_wrongly_skipped: usize,
    edges_skipped: usize,
    /// smooth edges (left alone, nothing to round; not counted as skipped)
    edges_smooth: usize,
    /// skipped edges the kernel takes alone, at full size, on the *input*: lost to
    /// the order of the ladder (not always recoverable: the edges may interact)
    edges_order: usize,
    bails: Vec<String>,
    wrong: Vec<String>,
    /// skipped edges by (auto-reduce on, a max size was found, reason)
    reasons: std::collections::BTreeMap<(bool, bool, String), usize>,
}

#[derive(Clone, Debug)]
struct Case {
    kind: DressUpKind,
    edges: Vec<u32>,
    size: f64,
    reduce: bool,
}

/// Random request on `s`: a few random edges, or every edge of a random face, or
/// the edges meeting at a random vertex (corner blends).
fn random_case(r: &mut Rng, s: &Shape) -> Case {
    let edges: Vec<_> = s.edges().unwrap().into_iter().filter(|e| e.faces.len() == 2 && e.length > 1e-3).collect();
    let pick = match r.below(3) {
        0 => {
            let n = 1 + r.below(6);
            (0..n).map(|_| edges[r.below(edges.len())].index).collect::<Vec<_>>()
        }
        1 => {
            let nf = s.faces().unwrap().len();
            s.face_edges(r.below(nf) as u32).unwrap_or_default()
        }
        _ => {
            let e = &edges[r.below(edges.len())];
            let v = if r.next().is_multiple_of(2) { e.start } else { e.end };
            edges.iter().filter(|x| x.start.distance(v) < 1e-6 || x.end.distance(v) < 1e-6).map(|x| x.index).collect()
        }
    };
    let mut pick = pick;
    pick.sort_unstable();
    pick.dedup();
    let size = [0.3, 0.5, 1.0, 1.5, 2.0, 3.0, 5.0][r.below(7)] * r.f(0.8, 1.2);
    let kind = if r.next().is_multiple_of(4) { DressUpKind::Chamfer } else { DressUpKind::Fillet };
    Case { kind, edges: pick, size, reduce: r.next().is_multiple_of(2) }
}

fn run_case(name: &str, s: &Shape, c: &Case, h: &mut Hist) {
    if c.edges.is_empty() {
        return;
    }
    let req: Vec<(u32, f64, bool)> = c.edges.iter().map(|e| (*e, c.size, c.reduce)).collect();
    let label = format!("{name} {:?} {:?} size {:.3} reduce {}", c.kind, c.edges, c.size, c.reduce);
    let solids = s.count(ShapeKind::Solid);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dressup::dress(c.kind, s, &req, &DressUpOptions::default())));
    let (out, rep) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            h.bailed += 1;
            h.bails.push(format!("{label}: error {}", e.0));
            return;
        }
        Err(_) => {
            h.bailed += 1;
            h.bails.push(format!("{label}: panic"));
            return;
        }
    };
    if !out.is_valid() || out.count(ShapeKind::Solid) != solids {
        h.bailed += 1;
        h.bails.push(format!("{label}: result valid {} with {} solids (input {solids}): {}", out.is_valid(), out.count(ShapeKind::Solid), rep.summary()));
        return;
    }
    h.edges_smooth += rep.outcomes.iter().filter(|o| o.status == EdgeStatus::Smooth).count();
    let failed: Vec<_> = rep.outcomes.iter().filter(|o| matches!(o.status, EdgeStatus::Failed { .. })).collect();
    if failed.is_empty() {
        if rep.outcomes.iter().any(|o| matches!(o.status, EdgeStatus::AppliedReduced { .. })) {
            h.reduced += 1;
        } else {
            h.applied += 1;
        }
        return;
    }
    h.skipped += 1;
    let infos = s.edges().unwrap();
    for o in failed {
        h.edges_skipped += 1;
        if let EdgeStatus::Failed { reason, max_size } = &o.status {
            let conv = s.edge_convexity(o.edge).unwrap_or(9);
            if std::env::var("FUZZ_V").is_ok() && conv == 0 {
                println!("  unsuitable {label}: edge {} {:?} faces {:?} {reason}", o.edge, infos[o.edge as usize].kind, infos[o.edge as usize].faces);
            }
            *h.reasons.entry((c.reduce, max_size.is_some(), format!("convexity {conv} · {reason}"))).or_default() += 1;
        }
        // still sharp on the result, and the kernel takes it there at full size
        let Some(e) = out.match_edge(infos[o.edge as usize].midpoint, infos[o.edge as usize].tangent, 1e-3).ok().flatten() else { continue };
        let more = match c.kind {
            DressUpKind::Fillet => out.fillet_raw(&[e], &[c.size]),
            DressUpKind::Chamfer => out.chamfer_raw(&[e], &[c.size]),
        };
        if more.is_ok_and(|x| x.is_valid() && x.count(ShapeKind::Solid) == solids) {
            h.edges_wrongly_skipped += 1;
            h.wrong.push(format!("{label}: edge {} {:?}", o.edge, o.status));
        }
        let alone = match c.kind {
            DressUpKind::Fillet => s.fillet_raw(&[o.edge], &[c.size]),
            DressUpKind::Chamfer => s.chamfer_raw(&[o.edge], &[c.size]),
        };
        if alone.is_ok_and(|x| x.is_valid() && x.count(ShapeKind::Solid) == solids) {
            h.edges_order += 1;
            if std::env::var("FUZZ_V").is_ok() {
                println!("  order-lost {label}: edge {}", o.edge);
            }
        }
    }
}

fn bodies() -> Vec<(String, Shape)> {
    let mut v = Vec::new();
    for file in ["wizard_step.ecad", "wizard_dxf.ecad"] {
        // after the shell, after the standoffs, and the finished halves
        for at in [Some(3), Some(5), None] {
            v.extend(wizard_bodies(file, at));
        }
    }
    for seed in 1..=6 {
        v.push(generated(seed * 7919));
    }
    v
}

fn report(h: &Hist, cases: usize) {
    println!(
        "{cases} requests: applied {} · reduced {} · skipped {} · bailed {}  ({} smooth edges left alone; {} skipped edges, {} wrongly, {} lost to order)",
        h.applied, h.reduced, h.skipped, h.bailed, h.edges_smooth, h.edges_skipped, h.edges_wrongly_skipped, h.edges_order
    );
    for ((reduce, max, why), n) in &h.reasons {
        println!("  {n:>4} skipped edges · auto-reduce {reduce} · max size found {max} · {why}");
    }
    for b in &h.bails {
        println!("  BAILED {b}");
    }
    for w in &h.wrong {
        println!("  WRONGLY SKIPPED {w}");
    }
}

#[test]
#[ignore]
fn fillet_fuzz_full() {
    let n: usize = std::env::var("FUZZ_N").ok().and_then(|x| x.parse().ok()).unwrap_or(400);
    let bodies = bodies();
    let mut r = Rng(0x5eed_f111e7);
    let mut h = Hist::default();
    for k in 0..n {
        let bi = r.below(bodies.len());
        let c = random_case(&mut r, &bodies[bi].1);
        if std::env::var("FUZZ_V").is_ok() {
            println!("#{k} {} {c:?}", bodies[bi].0);
        }
        run_case(&bodies[bi].0, &bodies[bi].1, &c, &mut h);
    }
    report(&h, n);
}

fn body_named(name: &str) -> Shape {
    if let Some(seed) = name.strip_prefix("generated #") {
        return generated(seed.parse().unwrap()).1;
    }
    let (file, rest) = name.split_once('@').unwrap();
    let (at, body) = rest.split_once(':').unwrap();
    let at = at.strip_prefix("Some(").map(|x| x.trim_end_matches(')').parse().unwrap());
    wizard_bodies(file, at).into_iter().find(|(n, _)| n.ends_with(&format!(":{body}"))).unwrap().1
}

/// Fast, deterministic: requests that bailed (a chamfer wider than its hole came
/// back as a valid *shell*), hit seams / smooth edges, or lost chains to the order
/// of the ladder before, plus the first 20 requests of the full fuzz.
#[test]
fn fillet_fuzz_regressions() {
    use DressUpKind::{Chamfer, Fillet};
    // (body, kind, edges, size, auto-reduce, edges that must be applied now)
    type Reg<'a> = (&'a str, DressUpKind, &'a [u32], f64, bool, &'a [u32]);
    let cases: &[Reg] = &[
        // bailed: the result had no solid left
        ("generated #7919", Fillet, &[33, 43, 58, 59], 1.768, true, &[58, 59]),
        ("wizard_dxf.ecad@None:Base", Chamfer, &[177], 1.662, true, &[177]),
        ("wizard_dxf.ecad@Some(5):Base", Fillet, &[104, 112, 113, 114], 1.677, false, &[104]),
        // a cylinder seam in the set ("no suitable edges")
        ("generated #31676", Fillet, &[12, 28, 29], 4.039, true, &[12, 29]),
        // chains lost to the order of the ladder
        ("generated #39595", Fillet, &[50, 51, 56, 57, 58, 59, 60], 3.033, false, &[56, 57]),
        ("generated #15838", Fillet, &[28, 37, 40, 41], 0.570, false, &[37, 40]),
    ];
    let mut h = Hist::default();
    for &(name, kind, edges, size, reduce, must) in cases {
        let s = body_named(name);
        let c = Case { kind, edges: edges.to_vec(), size, reduce };
        run_case(name, &s, &c, &mut h);
        let req: Vec<(u32, f64, bool)> = edges.iter().map(|e| (*e, size, reduce)).collect();
        let (out, rep) = dressup::dress(kind, &s, &req, &DressUpOptions::default()).unwrap();
        assert_eq!(out.count(ShapeKind::Solid), s.count(ShapeKind::Solid), "{name}: {}", rep.summary());
        for e in must {
            assert!(rep.outcomes.iter().any(|o| o.edge == *e && o.status.is_applied()), "{name}: edge {e}: {}", rep.summary());
        }
        println!("{name}: {}", rep.summary());
    }
    report(&h, cases.len());
    assert_eq!(h.bailed, 0);
    assert_eq!(h.edges_wrongly_skipped, 0);
    assert!(h.edges_smooth >= 1, "the seam is reported as smooth");

    // the first requests of the full fuzz
    let bodies = bodies();
    let mut r = Rng(0x5eed_f111e7);
    let mut h = Hist::default();
    for _ in 0..20 {
        let bi = r.below(bodies.len());
        let c = random_case(&mut r, &bodies[bi].1);
        run_case(&bodies[bi].0, &bodies[bi].1, &c, &mut h);
    }
    report(&h, 20);
    assert_eq!(h.bailed, 0);
    assert_eq!(h.edges_wrongly_skipped, 0);
}
