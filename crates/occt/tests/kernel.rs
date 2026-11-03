use occt::{dressup, CurveKind, DVec3, Edge, EdgeStatus, Frame, Shape, ShapeKind, Strategy, SurfaceKind};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * b.abs().max(1.0)
}

fn rect_face(w: f64, h: f64) -> Shape {
    let p = |x, y| DVec3::new(x, y, 0.0);
    let outer = [
        Edge::Line(p(0.0, 0.0), p(w, 0.0)),
        Edge::Line(p(w, 0.0), p(w, h)),
        Edge::Line(p(w, h), p(0.0, h)),
        Edge::Line(p(0.0, h), p(0.0, 0.0)),
    ];
    Shape::face(&Frame::XY, &outer, &[]).unwrap()
}

#[test]
fn box_volume_bbox_and_topology() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(10.0, 20.0, 30.0)).unwrap();
    assert!(close(b.volume().unwrap(), 6000.0, 1e-9));
    let bb = b.bbox().unwrap();
    assert!((bb.max - DVec3::new(10.0, 20.0, 30.0)).length() < 1e-6);
    let faces = b.faces().unwrap();
    assert_eq!(faces.len(), 6);
    assert!(faces.iter().all(|f| f.kind == SurfaceKind::Plane));
    let top = faces.iter().find(|f| f.normal.z > 0.99).unwrap();
    assert!(close(top.centroid.z, 30.0, 1e-9) && close(top.area, 200.0, 1e-9));
    let edges = b.edges().unwrap();
    assert_eq!(edges.len(), 12);
    assert!(edges.iter().all(|e| e.kind == CurveKind::Line && e.faces.len() == 2));
    assert_eq!(b.face_edges(top.index).unwrap().len(), 4);
    assert!(b.is_valid());
    assert_eq!(b.kind(), Some(ShapeKind::Solid));
}

#[test]
fn face_with_hole_extrude_revolve() {
    let p = |x, y| DVec3::new(x, y, 0.0);
    let outer = [
        Edge::Line(p(0.0, 0.0), p(20.0, 0.0)),
        Edge::Arc3(p(20.0, 0.0), p(30.0, 10.0), p(20.0, 20.0)),
        Edge::Line(p(20.0, 20.0), p(0.0, 20.0)),
        Edge::Line(p(0.0, 20.0), p(0.0, 0.0)),
    ];
    let hole = vec![Edge::Circle { center: p(10.0, 10.0), normal: DVec3::Z, radius: 3.0 }];
    let f = Shape::face(&Frame::XY, &outer, &[hole]).unwrap();
    let area = 400.0 + std::f64::consts::PI * 50.0 - std::f64::consts::PI * 9.0;
    assert!(close(f.area().unwrap(), area, 1e-6));
    let s = f.extrude(DVec3::new(0.0, 0.0, 5.0)).unwrap();
    assert!(close(s.volume().unwrap(), area * 5.0, 1e-6));
    let sym = f.extrude_ext(&Frame::XY, DVec3::Z, 10.0, true, 0.0).unwrap();
    let bb = sym.bbox().unwrap();
    assert!(close(bb.min.z, -5.0, 1e-6) && close(bb.max.z, 5.0, 1e-6));
    // revolve a 2x4 rectangle offset from the axis -> tube
    let r = Shape::face(
        &Frame { origin: DVec3::ZERO, x_dir: DVec3::X, normal: DVec3::NEG_Y },
        &[
            Edge::Line(DVec3::new(5.0, 0.0, 0.0), DVec3::new(7.0, 0.0, 0.0)),
            Edge::Line(DVec3::new(7.0, 0.0, 0.0), DVec3::new(7.0, 0.0, 4.0)),
            Edge::Line(DVec3::new(7.0, 0.0, 4.0), DVec3::new(5.0, 0.0, 4.0)),
            Edge::Line(DVec3::new(5.0, 0.0, 4.0), DVec3::new(5.0, 0.0, 0.0)),
        ],
        &[],
    )
    .unwrap();
    let tube = r.revolve(DVec3::ZERO, DVec3::Z, 360.0).unwrap();
    assert!(close(tube.volume().unwrap(), std::f64::consts::PI * (49.0 - 25.0) * 4.0, 1e-6));
}

#[test]
fn draft_extrude_tapers_inward() {
    let f = rect_face(20.0, 20.0);
    let s = f.extrude_ext(&Frame::XY, DVec3::Z, 10.0, false, 5.0).unwrap();
    let v = s.volume().unwrap();
    assert!(v < 4000.0 && v > 3000.0, "drafted volume {v}");
    let bb = s.bbox().unwrap();
    assert!(close(bb.size().x, 20.0, 1e-3), "base keeps its size: {:?}", bb);
    // the top face shrinks by 2*10*tan(5deg)
    let top = s.faces().unwrap().into_iter().find(|f| f.normal.z > 0.99).unwrap();
    let side = 20.0 - 2.0 * 10.0 * 5f64.to_radians().tan();
    assert!(close(top.area, side * side, 1e-3), "top {} vs {}", top.area, side * side);
}

#[test]
fn booleans_patterns_mirror_split() {
    let a = Shape::make_box(DVec3::ZERO, DVec3::splat(10.0)).unwrap();
    let c = Shape::cylinder(DVec3::new(5.0, 5.0, -1.0), DVec3::Z, 2.0, 12.0).unwrap();
    let cut = a.cut(&c).unwrap();
    assert!(close(cut.volume().unwrap(), 1000.0 - std::f64::consts::PI * 40.0, 1e-6));
    let common = a.common(&c).unwrap();
    assert!(close(common.volume().unwrap(), std::f64::consts::PI * 40.0, 1e-6));
    let fused = a.fuse(&Shape::make_box(DVec3::new(10.0, 0.0, 0.0), DVec3::splat(10.0)).unwrap()).unwrap().unify().unwrap();
    assert!(close(fused.volume().unwrap(), 2000.0, 1e-9));
    assert_eq!(fused.faces().unwrap().len(), 6, "unify merges coplanar faces");

    let pin = Shape::cylinder(DVec3::new(20.0, 0.0, 0.0), DVec3::Z, 1.0, 1.0).unwrap();
    let lin = pin.linear_pattern(DVec3::X, 5.0, 4).unwrap();
    assert_eq!(lin.solids().unwrap().len(), 4);
    let circ = pin.circular_pattern(DVec3::ZERO, DVec3::Z, 6, 360.0).unwrap();
    assert!(close(circ.volume().unwrap(), 6.0 * std::f64::consts::PI, 1e-6));
    let m = a.mirrored(DVec3::ZERO, DVec3::X).unwrap();
    let bb = m.bbox().unwrap();
    assert!(close(bb.min.x, -10.0, 1e-6) && close(m.volume().unwrap(), 1000.0, 1e-9));

    let (below, above) = a.split(DVec3::new(0.0, 0.0, 7.0), DVec3::Z).unwrap();
    assert!(close(below.unwrap().volume().unwrap(), 700.0, 1e-9));
    assert!(close(above.unwrap().volume().unwrap(), 300.0, 1e-9));
    let (below, above) = a.split(DVec3::new(0.0, 0.0, 70.0), DVec3::Z).unwrap();
    assert!(below.is_some() && above.is_none());
}

#[test]
fn shell_and_draft() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let top = b.faces().unwrap().into_iter().find(|f| f.normal.z > 0.99).unwrap().index;
    let s = b.shell(&[top], 2.0, false).unwrap();
    let inner = 36.0 * 26.0 * 18.0;
    assert!(close(s.volume().unwrap(), 24000.0 - inner, 1e-6));
    assert!(close(s.bbox().unwrap().size().x, 40.0, 1e-6));
    let out = b.shell(&[top], 2.0, true).unwrap();
    assert!(close(out.bbox().unwrap().size().x, 44.0, 1e-3));

    let sides: Vec<u32> = b.faces().unwrap().iter().filter(|f| f.normal.z.abs() < 0.1).map(|f| f.index).collect();
    let d = b.draft(&sides, DVec3::Z, 3.0, &Frame::XY).unwrap();
    assert!(d.is_valid());
    assert!((d.volume().unwrap() - 24000.0).abs() > 10.0);
}

#[test]
fn fillet_chamfer_and_tangent_chains() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let vertical: Vec<u32> = b.edges().unwrap().iter().filter(|e| e.tangent.z.abs() > 0.99).map(|e| e.index).collect();
    assert_eq!(vertical.len(), 4);
    for e in &vertical {
        assert_eq!(b.edge_convexity(*e).unwrap(), 1);
    }
    let req: Vec<(u32, f64)> = vertical.iter().map(|e| (*e, 5.0)).collect();
    let (f, rep) = dressup::fillet(&b, &req, &Default::default()).unwrap();
    assert!(rep.all_applied(), "{rep:?}");
    assert_eq!(rep.strategy, Strategy::AllAtOnce);
    let want = (1200.0 - (4.0 - std::f64::consts::PI) * 25.0) * 20.0;
    assert!(close(f.volume().unwrap(), want, 1e-6));
    // tangent chain: top edges of the filleted box form one loop
    let chains = f.tangent_chains().unwrap();
    let top_edges: Vec<u32> = f.edges().unwrap().iter().filter(|e| (e.midpoint.z - 20.0).abs() < 1e-6).map(|e| e.index).collect();
    assert_eq!(top_edges.len(), 8);
    assert!(top_edges.iter().all(|e| chains[*e as usize] == chains[top_edges[0] as usize]));
    // filleting one top edge pulls in the whole chain
    let (g, rep) = dressup::fillet(&f, &[(top_edges[0], 1.0)], &Default::default()).unwrap();
    assert!(rep.all_applied() && rep.expanded_edges == 7, "{rep:?}");
    assert!(g.is_valid());

    let (c, rep) = dressup::chamfer(&b, &req, &Default::default()).unwrap();
    assert!(rep.all_applied());
    assert!(close(c.volume().unwrap(), (1200.0 - 4.0 * 12.5) * 20.0, 1e-6));
}

/// A ledge only 2 mm tall cannot take a 3 mm fillet on its outer top edge, but the
/// main block's top edges can: the pipeline must apply what works and report the rest.
#[test]
fn fillet_fallback_partial_success() {
    let main = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 40.0, 20.0)).unwrap();
    let ledge = Shape::make_box(DVec3::new(40.0, 0.0, 0.0), DVec3::new(3.0, 40.0, 2.0)).unwrap();
    let body = main.fuse(&ledge).unwrap();
    let edges = body.edges().unwrap();
    let pick = |p: DVec3| edges.iter().min_by(|a, b| a.midpoint.distance(p).total_cmp(&b.midpoint.distance(p))).unwrap().index;
    let main_top = [pick(DVec3::new(20.0, 0.0, 20.0)), pick(DVec3::new(20.0, 40.0, 20.0)), pick(DVec3::new(0.0, 20.0, 20.0))];
    let ledge_edge = pick(DVec3::new(43.0, 20.0, 2.0));
    let mut req: Vec<(u32, f64)> = main_top.iter().map(|e| (*e, 3.0)).collect();
    req.push((ledge_edge, 3.0));

    // a plain all-at-once fillet refuses
    let ids: Vec<u32> = req.iter().map(|r| r.0).collect();
    assert!(body.fillet_raw(&ids, &[3.0; 4]).is_err());

    let (out, rep) = dressup::fillet(&body, &req, &Default::default()).unwrap();
    assert!(out.is_valid());
    assert_eq!(rep.applied_count(), 3, "{rep:#?}");
    assert!(rep.strategy >= Strategy::ByChains);
    let failed = rep.outcomes.iter().find(|o| o.edge == ledge_edge).unwrap();
    match &failed.status {
        EdgeStatus::Failed { max_size: Some(m), .. } => assert!(*m > 1.0 && *m < 2.0, "max {m}"),
        s => panic!("unexpected {s:?}"),
    }
    assert!(out.volume().unwrap() < body.volume().unwrap());
    println!("{}", rep.summary());

    // auto_reduce applies the largest workable radius
    let opts = dressup::DressUpOptions { auto_reduce: true, ..Default::default() };
    let (out2, rep2) = dressup::fillet(&body, &req, &opts).unwrap();
    assert!(out2.is_valid());
    assert_eq!(rep2.applied_count(), 4, "{rep2:#?}");
    assert!(matches!(rep2.outcomes[3].status, EdgeStatus::AppliedReduced { applied, .. } if applied < 2.0));
}

/// A dress-up that eats through the body used to come back as a valid *shell* (no
/// solid left, reported as applied; see `fillet_fuzz_regressions` for the real
/// cases); a smooth edge (a fillet's own boundary) or a seam in
/// the set used to send the whole request down the slow ladder.
#[test]
fn dressup_refuses_lost_solids_and_skips_smooth_edges() {
    let plate = Shape::make_box(DVec3::ZERO, DVec3::new(20.0, 20.0, 4.0)).unwrap();
    let hole = Shape::cylinder(DVec3::new(10.0, 10.0, -1.0), DVec3::Z, 1.25, 6.0).unwrap();
    let b = plate.cut(&hole).unwrap();
    let rim = b.edges().unwrap().into_iter().find(|e| e.circle.is_some() && (e.midpoint.z - 4.0).abs() < 1e-6).unwrap().index;
    if let Ok(r) = b.chamfer_raw(&[rim], &[2.0]) {
        assert_eq!(r.count(ShapeKind::Solid), 1, "a raw chamfer may fail, never lose the solid");
    }
    for size in [1.0, 2.0, 3.0, 5.0] {
        let opts = dressup::DressUpOptions { auto_reduce: true, ..Default::default() };
        let (out, rep) = dressup::chamfer(&b, &[(rim, size)], &opts).unwrap();
        assert_eq!(out.count(ShapeKind::Solid), 1, "{size}: {}", rep.summary());
        assert!(out.is_valid());
    }

    // round the vertical edges, then ask for a fillet boundary (smooth), the hole's
    // seam and a top edge: the top edge goes through in one call, the others need nothing
    let vertical: Vec<u32> = b.edges().unwrap().iter().filter(|e| e.tangent.z.abs() > 0.99 && e.circle.is_none() && e.faces.len() == 2 && e.faces[0] != e.faces[1]).map(|e| e.index).collect();
    let r = b.fillet_raw(&vertical[..4.min(vertical.len())], &[3.0; 4][..4.min(vertical.len())]).unwrap();
    let edges = r.edges().unwrap();
    let smooth = edges.iter().find(|e| e.faces.len() == 2 && e.faces[0] != e.faces[1] && r.edge_convexity(e.index).unwrap() == 0).unwrap().index;
    let seam = edges.iter().find(|e| e.faces.len() == 1).map(|e| e.index);
    let top = edges.iter().find(|e| e.circle.is_none() && (e.midpoint.z - 4.0).abs() < 1e-6 && r.edge_convexity(e.index).unwrap() == 1).unwrap().index;
    let mut req = vec![(smooth, 1.0), (top, 1.0)];
    req.extend(seam.map(|s| (s, 1.0)));
    let (out, rep) = dressup::fillet(&r, &req, &Default::default()).unwrap();
    assert_eq!(rep.strategy, Strategy::AllAtOnce, "{}", rep.summary());
    assert_eq!(rep.outcomes[0].status, EdgeStatus::Smooth);
    assert_eq!(rep.outcomes[1].status, EdgeStatus::Applied);
    if seam.is_some() {
        assert_eq!(rep.outcomes[2].status, EdgeStatus::Smooth);
    }
    assert!(rep.all_applied() && rep.applied_count() == 1, "{}", rep.summary());
    assert!(out.volume().unwrap() < r.volume().unwrap());
    // nothing but smooth edges: nothing applied (regen turns that into an error)
    let (_, rep) = dressup::fillet(&r, &[(smooth, 1.0)], &Default::default()).unwrap();
    assert_eq!(rep.applied_count(), 0);
}

#[test]
fn errors_are_results_not_aborts() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::splat(10.0)).unwrap();
    assert!(Shape::make_box(DVec3::ZERO, DVec3::new(0.0, 1.0, 1.0)).is_err());
    assert!(b.fillet_raw(&[0], &[50.0]).is_err());
    assert!(b.fillet_raw(&[999], &[1.0]).is_err());
    assert!(b.shell(&[], 0.0, false).is_err());
    assert!(Shape::read_step_str("not a step file").is_err());
    let open = [Edge::Line(DVec3::ZERO, DVec3::X), Edge::Line(DVec3::X, DVec3::Y)];
    assert!(Shape::face(&Frame::XY, &open, &[]).is_err());
}

#[test]
fn tessellate_and_io_roundtrip() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::splat(10.0)).unwrap();
    let c = Shape::cylinder(DVec3::new(5.0, 5.0, -1.0), DVec3::Z, 2.0, 12.0).unwrap();
    let s = b.cut(&c).unwrap();
    let m = s.tessellate(0.05, 10.0).unwrap();
    assert!(m.indices.len() > 20);
    assert_eq!(m.indices.len(), m.tri_face.len());
    assert_eq!(m.positions.len(), m.normals.len());
    let nf = s.faces().unwrap().len() as u32;
    assert!(m.tri_face.iter().all(|f| *f < nf));
    assert_eq!(m.edges.len(), s.edges().unwrap().len());

    let dir = std::env::temp_dir().join(format!("occt-io-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let step = dir.join("t.step");
    s.write_step(&step).unwrap();
    let back = Shape::read_step(&step).unwrap();
    assert!(close(back.volume().unwrap(), s.volume().unwrap(), 1e-6));
    let txt = s.to_step_string().unwrap();
    assert!(txt.starts_with("ISO-10303-21"));
    assert!(close(Shape::read_step_str(&txt).unwrap().volume().unwrap(), s.volume().unwrap(), 1e-6));
    let stl = dir.join("t.stl");
    s.write_stl(&stl, 0.05, false).unwrap();
    assert!(std::fs::metadata(&stl).unwrap().len() > 84);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn planar_regions_section_offset() {
    // hollow 40×30×20 box, 2 mm walls
    let outer = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let hollow = outer.cut(&outer.offset_solid(-2.0).unwrap()).unwrap();
    assert!((hollow.volume().unwrap() - (24000.0 - 36.0 * 26.0 * 16.0)).abs() < 1e-3);
    let ring = hollow.section(DVec3::new(0.0, 0.0, 10.0), DVec3::Z).unwrap();
    let ring_a = 1200.0 - 36.0 * 26.0;
    assert!((ring.area().unwrap() - ring_a).abs() < 1e-6, "{}", ring.area().unwrap());
    // shrinking by 0.5 on every boundary leaves a 1 mm centred band
    let band = ring.offset_region(-0.5).unwrap();
    let want = 39.0 * 29.0 - (37.0 * 27.0 - (4.0 - std::f64::consts::PI) * 0.25); // grown hole corners are rounded
    assert!((band.area().unwrap() - want).abs() < 1e-3, "{} vs {want}", band.area().unwrap());
    // growing rounds the outer corners and the hole corners stay sharp... area grows
    assert!(ring.offset_region(0.5).unwrap().area().unwrap() > ring_a);
    // shrinking past half the wall makes it vanish (error, not panic)
    assert!(ring.offset_region(-1.2).is_err());
    let filled = ring.fill_holes().unwrap();
    assert!((filled.area().unwrap() - 1200.0).abs() < 1e-6);
    assert!(ring.region_contains(DVec3::new(1.0, 15.0, 10.0)).unwrap());
    assert!(!ring.region_contains(DVec3::new(20.0, 15.0, 10.0)).unwrap());
    assert!((ring.outer_wire_length().unwrap() - 140.0).abs() < 1e-6);
    let (p, t) = ring.outer_wire_point(0.0).unwrap();
    assert!((p.z - 10.0).abs() < 1e-9 && (t.length() - 1.0).abs() < 1e-9);
}

#[test]
fn hlr_front_view_of_a_drilled_box() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let s = b.cut(&Shape::cylinder(DVec3::new(20.0, 15.0, -1.0), DVec3::Z, 4.0, 22.0).unwrap()).unwrap();
    for exact in [true, false] {
        // front view: viewer at -Y, x = +X, y = dir × x = +Z
        let (edges, fb) = s.hlr(DVec3::NEG_Y, DVec3::X, exact, 0.01).unwrap();
        assert!(!fb);
        let vis: Vec<[f64; 2]> = edges.iter().filter(|e| e.visible).flat_map(|e| e.points.clone()).collect();
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in &vis {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        assert!((lo[0] - 0.0).abs() < 1e-3 && (hi[0] - 40.0).abs() < 1e-3, "x {lo:?} {hi:?}");
        assert!((lo[1] - 0.0).abs() < 1e-3 && (hi[1] - 20.0).abs() < 1e-3, "y {lo:?} {hi:?}");
        // the hole is hidden in the front view: two hidden vertical lines at x = 16 and 24
        let hid: Vec<_> = edges.iter().filter(|e| !e.visible).collect();
        assert!(hid.iter().any(|e| e.points.iter().all(|p| (p[0] - 16.0).abs() < 1e-3)), "exact={exact}");
        assert!(hid.iter().any(|e| e.points.iter().all(|p| (p[0] - 24.0).abs() < 1e-3)));
    }
}

#[test]
fn xcaf_names_and_colors_round_trip() {
    let board = Shape::make_box(DVec3::ZERO, DVec3::new(50.0, 30.0, 1.6)).unwrap();
    let chip = Shape::make_box(DVec3::new(10.0, 10.0, 1.6), DVec3::new(8.0, 8.0, 1.2)).unwrap();
    let c = Shape::compound(&[&board, &chip]).unwrap();
    let parts = vec![
        occt::StepPart { name: "PCB".into(), color: Some([0.0, 0.5, 0.0]) },
        occt::StepPart { name: "U1".into(), color: Some([0.1, 0.1, 0.1]) },
    ];
    let step = c.to_step_string_named(&parts).unwrap();
    let (back, got) = Shape::read_step_xcaf(&step).unwrap();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(back.count(ShapeKind::Solid), 2);
    let pcb = got.iter().position(|p| p.name == "PCB").expect("names kept");
    let col = got[pcb].color.unwrap();
    assert!(col[1] > 0.4 && col[0] < 0.05 && col[2] < 0.05, "{col:?}");
    assert!(got.iter().any(|p| p.name == "U1"));
    let owners = back.face_owners().unwrap();
    assert_eq!(owners.len(), 12);
    assert_eq!(owners.iter().filter(|o| **o == 0).count(), 6);
    // plain STEP without colours still loads through the XCAF path
    let (_, plain) = Shape::read_step_xcaf(&board.to_step_string().unwrap()).unwrap();
    assert_eq!(plain.len(), 1);
}

#[test]
fn shell_without_open_faces_is_closed_hollow_solid() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let s = b.shell(&[], 2.0, false).unwrap();
    assert!(s.is_valid());
    assert_eq!(s.count(ShapeKind::Solid), 1);
    assert_eq!(s.count(ShapeKind::Shell), 2, "outer skin + internal void");
    assert!(close(s.volume().unwrap(), 24000.0 - 36.0 * 26.0 * 16.0, 1e-6), "{}", s.volume().unwrap());
    assert!(close(s.bbox().unwrap().size().x, 40.0, 1e-6));
    // outward: the original becomes the void
    let o = b.shell(&[], 2.0, true).unwrap();
    assert_eq!(o.count(ShapeKind::Shell), 2);
    assert!(close(o.bbox().unwrap().size().x, 44.0, 1e-3));
    // a rounded body (convex radius 6 > wall 2)
    let r = b.fillet_raw(&(0..12).collect::<Vec<_>>(), &[6.0; 12]).unwrap();
    let rs = r.shell(&[], 2.0, false).unwrap();
    assert!(rs.is_valid() && rs.volume().unwrap() < r.volume().unwrap() * 0.5);
    // walls thicker than the body: a clear error, never just the inner skin
    let e = b.shell(&[], 12.0, false).unwrap_err();
    assert!(e.0.contains("no open faces"), "{e}");
}

/// Meshing must not write into the (shared) input topology: a boolean on the same
/// shape gives the same result before and after meshing.
#[test]
fn meshing_leaves_input_untouched() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 20.0)).unwrap();
    let c = Shape::cylinder(DVec3::new(20.0, 15.0, -1.0), DVec3::Z, 5.0, 30.0).unwrap();
    let body = b.cut(&c).unwrap();
    let before = body.fuse(&b).unwrap().volume().unwrap();
    let fine = body.tessellate(0.01, 5.0).unwrap().indices.len();
    let coarse = body.tessellate(0.5, 30.0).unwrap().indices.len();
    assert!(coarse < fine, "each call meshes at its own density");
    let after = body.fuse(&b).unwrap().volume().unwrap();
    assert_eq!(before, after);
}

/// After the kernel installed its signal handlers, a fault outside any kernel call
/// still reaches Rust's handler (stack overflow report + abort) instead of OCCT's
/// "*** Abort ***" + exit(1). Runs itself in a child process.
#[test]
fn faults_outside_kernel_calls_reach_rusts_handler() {
    if std::env::var("CW_CHILD_OVERFLOW").is_ok() {
        let _ = Shape::make_box(DVec3::ZERO, DVec3::ONE).unwrap(); // installs the handlers
        #[allow(unconditional_recursion)]
        fn deep(n: u64) -> u64 {
            let buf = [n; 64];
            std::hint::black_box(&buf);
            deep(n + 1) + buf[3]
        }
        std::thread::Builder::new().stack_size(256 * 1024).spawn(|| deep(0)).unwrap().join().ok();
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args(["faults_outside_kernel_calls_reach_rusts_handler", "--exact", "--nocapture", "--test-threads=1"])
        .env("CW_CHILD_OVERFLOW", "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("overflowed its stack"), "stderr: {err}");
    assert!(!err.contains("*** Abort ***"), "stderr: {err}");
}

/// XCAF product structure: a part instanced twice under a sub-assembly, the
/// sub-assembly placed in a root; names, colours and placements round-trip.
#[test]
fn step_assembly_tree_round_trip() {
    use occt::{StepLink, StepNode, StepTree};
    let screw = Shape::cylinder(DVec3::ZERO, DVec3::Z, 1.5, 10.0).unwrap();
    let plate = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 4.0)).unwrap();
    let t = |x: f64, y: f64, z: f64| [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z];
    let rz90 = |x: f64| [0.0, -1.0, 0.0, x, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let tree = StepTree {
        nodes: vec![
            StepNode { name: "Rack".into(), children: vec![StepLink { child: 1, name: "Bracket:1".into(), matrix: t(0.0, 0.0, 0.0) }, StepLink { child: 1, name: "Bracket:2".into(), matrix: rz90(100.0) }], ..Default::default() },
            StepNode {
                name: "Bracket".into(),
                children: vec![
                    StepLink { child: 2, name: "Plate".into(), matrix: t(0.0, 0.0, 0.0) },
                    StepLink { child: 3, name: "Screw:1".into(), matrix: t(5.0, 5.0, 4.0) },
                    StepLink { child: 3, name: "Screw:2".into(), matrix: t(35.0, 5.0, 4.0) },
                ],
                ..Default::default()
            },
            StepNode { name: "Plate".into(), color: Some([0.2, 0.4, 0.8]), shape: Some(plate), children: vec![] },
            StepNode { name: "SHCS M3".into(), color: Some([0.1, 0.1, 0.1]), shape: Some(screw), children: vec![] },
        ],
        roots: vec![0],
    };
    let step = tree.to_step_string().unwrap();
    let back = StepTree::read(&step).unwrap();
    assert_eq!(back.roots.len(), 1);
    let root = &back.nodes[back.roots[0]];
    assert_eq!(root.name, "Rack");
    assert_eq!(root.children.len(), 2);
    let names: Vec<&str> = root.children.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Bracket:1", "Bracket:2"]);
    assert_eq!(root.children[0].child, root.children[1].child, "one Bracket product, instanced twice");
    let m = root.children[1].matrix;
    for (a, b) in m.iter().zip(rz90(100.0)) {
        assert!((a - b).abs() < 1e-9, "{m:?}");
    }
    let br = &back.nodes[root.children[0].child];
    assert_eq!(br.name, "Bracket");
    assert_eq!(br.children.len(), 3);
    let screw_nodes: Vec<usize> = br.children.iter().filter(|l| l.name.starts_with("Screw")).map(|l| l.child).collect();
    assert_eq!(screw_nodes.len(), 2);
    assert_eq!(screw_nodes[0], screw_nodes[1]);
    let sn = &back.nodes[screw_nodes[0]];
    assert_eq!(sn.name, "SHCS M3");
    assert!(sn.color.is_some_and(|c| (c[0] - 0.1).abs() < 0.01));
    assert!(close(sn.shape.as_ref().unwrap().volume().unwrap(), std::f64::consts::PI * 1.5 * 1.5 * 10.0, 1e-6));
    // the plain reader sees 2 plates + 4 screws, placed
    let flat = Shape::read_step_str(&step).unwrap();
    assert_eq!(flat.solids().unwrap().len(), 6);
}
