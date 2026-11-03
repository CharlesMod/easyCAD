//! Sketch editing helpers (GUI_NEEDS #12–14): native offset, split, trim, corner
//! fillet, removal with remapping, projection of model edges.

use std::f64::consts::PI;

use easycad_model::geom::Plane;
use easycad_model::params::Values;
use easycad_model::sketch::{loop_area, Constraint, Geom, Sketch};
use glam::{DVec2, DVec3};

fn areas(s: &Sketch) -> Vec<f64> {
    let mut v: Vec<f64> = s.profiles().profiles.iter().flat_map(|p| std::iter::once(&p.outer).chain(&p.holes)).map(|l| loop_area(l).abs()).collect();
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

fn vals(pairs: &[(&str, f64)]) -> Values {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn offset_loop_follows_its_distance() {
    let mut s = Sketch::new("S", Plane::XY);
    let ids = s.rounded_rect(DVec2::ZERO, "w", "h", "r", (50.0, 30.0, 5.0));
    let mut v = vals(&[("w", 50.0), ("h", 30.0), ("r", 5.0), ("off", 2.0)]);
    assert!(s.solve(&v).unwrap().converged);
    let (chain, closed) = s.chain_from(ids[0]);
    assert!(closed && chain.len() == 8, "{chain:?}");
    let new = s.offset_chain(ids[0], "off", &v).unwrap();
    assert_eq!(new.len(), 8);
    assert!(s.constraints.iter().any(|c| matches!(c, Constraint::Offset { .. })));
    let want = |d: f64| (50.0 + 2.0 * d) * (30.0 + 2.0 * d) - (4.0 - PI) * (5.0 + d).powi(2);
    let rep = s.solve(&v).unwrap();
    assert!(rep.converged, "{rep:?}");
    let a = areas(&s);
    assert!((a[1] - want(2.0)).abs() < 1e-3, "{a:?} vs {}", want(2.0));
    // the offset follows the parameter and the source
    v.insert("off".into(), 4.0);
    v.insert("w".into(), 60.0);
    assert!(s.solve(&v).unwrap().converged);
    let a = areas(&s);
    let want2 = (60.0 + 8.0) * (30.0 + 8.0) - (4.0 - PI) * 81.0;
    assert!((a[1] - want2).abs() < 1e-3, "{a:?} vs {want2}");
    // inward
    let mut s2 = Sketch::new("S", Plane::XY);
    let ids = s2.rounded_rect(DVec2::ZERO, 50.0, 30.0, 5.0, (50.0, 30.0, 5.0));
    s2.offset_chain(ids[0], -2.0, &Values::new()).unwrap();
    assert!(s2.solve(&Values::new()).unwrap().converged);
    let a = areas(&s2);
    assert!((a[0] - (46.0 * 26.0 - (4.0 - PI) * 9.0)).abs() < 1e-3, "{a:?}");
    // an arc smaller than the inward offset is refused
    assert!(s2.offset_chain(ids[0], -6.0, &Values::new()).is_err());
}

#[test]
fn offset_open_chain_and_circle() {
    let mut s = Sketch::new("S", Plane::XY);
    let a = s.add_point(DVec2::ZERO);
    let b = s.add_point(DVec2::new(10.0, 0.0));
    let c = s.add_point(DVec2::new(10.0, 10.0));
    let l1 = s.add_line(a, b);
    let l2 = s.add_line(b, c);
    for p in [a, b, c] {
        let q = s.point(p);
        s.constrain(Constraint::Fix(p, q));
    }
    let new = s.offset_chain(l1, 1.0, &Values::new()).unwrap();
    assert!(s.solve(&Values::new()).unwrap().converged);
    // to the left of travel (a → b → c): above the first line, left of the second
    let Geom::Line { a: na, b: nb } = s.entities[new[0].0 as usize].geom else { panic!() };
    assert!((s.point(na) - DVec2::new(0.0, 1.0)).length() < 1e-6, "{:?}", s.point(na));
    assert!((s.point(nb) - DVec2::new(9.0, 1.0)).length() < 1e-6, "{:?}", s.point(nb));
    let _ = l2;
    let ci = s.circle(DVec2::new(30.0, 0.0), 4.0);
    let nc = s.offset_chain(ci, 1.5, &Values::new()).unwrap();
    assert!(s.solve(&Values::new()).unwrap().converged);
    assert!(matches!(s.entities[nc[0].0 as usize].geom, Geom::Circle { radius, .. } if (radius - 5.5).abs() < 1e-6));
}

#[test]
fn split_trim_fillet_and_remove() {
    // split a line: two collinear pieces sharing a point
    let mut s = Sketch::new("S", Plane::XY);
    let l = s.line(DVec2::ZERO, DVec2::new(10.0, 0.0));
    s.constrain(Constraint::Horizontal(l));
    let n = s.split_at(l, DVec2::new(4.0, 0.3)).unwrap();
    assert!(s.solve(&Values::new()).unwrap().converged);
    let Geom::Line { a, b } = s.entities[n.0 as usize].geom else { panic!() };
    assert!((s.point(a) - DVec2::new(4.0, 0.0)).length() < 1e-9 && (s.point(b).x - 10.0).abs() < 1e-9);
    assert_eq!(s.constraints.iter().filter(|c| matches!(c, Constraint::Horizontal(_))).count(), 2);

    // trim an X: the arm of the horizontal line right of the crossing goes
    let mut s = Sketch::new("X", Plane::XY);
    let h = s.line(DVec2::new(-10.0, 0.0), DVec2::new(10.0, 0.0));
    let v = s.line(DVec2::new(0.0, -10.0), DVec2::new(0.0, 10.0));
    let m = s.trim(h, DVec2::new(6.0, 0.0)).unwrap();
    let h2 = m.entity(h).unwrap();
    let Geom::Line { a, b } = s.entities[h2.0 as usize].geom else { panic!() };
    assert!((s.point(a).x + 10.0).abs() < 1e-9 && s.point(b).x.abs() < 1e-9, "{:?} {:?}", s.point(a), s.point(b));
    assert!(s.constraints.iter().any(|c| matches!(c, Constraint::PointOnLine(_, e) if Some(*e) == m.entity(v))));
    assert!(s.solve(&Values::new()).unwrap().converged);
    // trimming the middle of a line crossed twice keeps both ends
    let mut s = Sketch::new("H", Plane::XY);
    let h = s.line(DVec2::new(-10.0, 0.0), DVec2::new(10.0, 0.0));
    s.line(DVec2::new(-3.0, -5.0), DVec2::new(-3.0, 5.0));
    s.line(DVec2::new(3.0, -5.0), DVec2::new(3.0, 5.0));
    s.trim(h, DVec2::new(0.5, 0.0)).unwrap();
    assert_eq!(s.entities.len(), 4);
    // a circle cut by a line becomes an arc; trimming a lone line removes it
    let mut s = Sketch::new("C", Plane::XY);
    let c = s.circle(DVec2::ZERO, 5.0);
    s.line(DVec2::new(-10.0, 3.0), DVec2::new(10.0, 3.0));
    s.trim(c, DVec2::new(0.0, 5.0)).unwrap();
    let Geom::Arc { center, start, end } = s.entities[c.0 as usize].geom else { panic!("{:?}", s.entities[c.0 as usize].geom) };
    let (cc, st, en) = (s.point(center), s.point(start), s.point(end));
    assert!((st.y - 3.0).abs() < 1e-9 && (en.y - 3.0).abs() < 1e-9 && cc.length() < 1e-9);
    assert!(st.x < 0.0 && en.x > 0.0, "the arc runs CCW from the left cut, under the centre, to the right cut");
    let lone = s.line(DVec2::new(50.0, 50.0), DVec2::new(60.0, 50.0));
    let before = s.entities.len();
    s.trim(lone, DVec2::new(55.0, 50.0)).unwrap();
    assert_eq!(s.entities.len(), before - 1);

    // corner fillet on a rectangle: area loses (4 − π)/4 · r² and stays parametric
    let mut s = Sketch::new("R", Plane::XY);
    let [bottom, _, _, left] = s.rect(DVec2::ZERO, DVec2::new(40.0, 20.0));
    let Geom::Line { a: corner, .. } = s.entities[bottom.0 as usize].geom else { panic!() };
    let _ = left;
    for i in 0..s.points.len() {
        let p = easycad_model::sketch::PointId(i as u32);
        if p != corner {
            let q = s.point(p);
            s.constrain(Constraint::Fix(p, q));
        }
    }
    let mut vv = vals(&[("fr", 3.0)]);
    let arc = s.fillet_corner(corner, "fr", &vv).unwrap();
    assert!(s.solve(&vv).unwrap().converged);
    let want = |r: f64| 800.0 - (4.0 - PI) / 4.0 * r * r;
    assert!((areas(&s)[0] - want(3.0)).abs() < 1e-3, "{:?}", areas(&s));
    vv.insert("fr".into(), 6.0);
    assert!(s.solve(&vv).unwrap().converged);
    assert!((areas(&s)[0] - want(6.0)).abs() < 1e-3, "{:?}", areas(&s));
    assert!(s.fillet_corner(corner, 50.0, &vv).is_err(), "the old corner now has no lines");
    let _ = arc;

    // removal remaps ids and drops what referred to the removed entity
    let mut s = Sketch::new("R", Plane::XY);
    let [b0, r0, t0, _] = s.rect(DVec2::ZERO, DVec2::new(10.0, 5.0));
    s.constrain(Constraint::Length(t0, 10.0.into()));
    let n0 = s.constraints.len();
    let m = s.remove_entities(&[b0]);
    assert_eq!(m.entity(b0), None);
    assert_eq!(m.entity(r0), Some(easycad_model::sketch::EntityId(0)));
    assert_eq!(s.entities.len(), 3);
    assert!(s.constraints.len() < n0);
    assert!(s.constraints.iter().any(|c| matches!(c, Constraint::Length(e, _) if Some(*e) == m.entity(t0))));
    assert!(s.solve(&Values::new()).unwrap().converged);
}

#[test]
fn project_model_edges() {
    use easycad_model::occt::Shape;
    // a 20 × 10 × 5 block with a Ø4 vertical hole and a Ø3 horizontal hole
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(20.0, 10.0, 5.0)).unwrap();
    let b = b.cut(&Shape::cylinder(DVec3::new(5.0, 3.0, -1.0), DVec3::Z, 2.0, 10.0).unwrap()).unwrap();
    let b = b.cut(&Shape::cylinder(DVec3::new(-1.0, 8.0, 2.5), DVec3::X, 1.5, 30.0).unwrap()).unwrap();
    let edges = b.edges().unwrap();
    let mut s = Sketch::new("Top", Plane::XY.offset(5.0));
    let (mut lines, mut circles, mut polys) = (0, 0, 0);
    for e in &edges {
        let made = s.project_edge(e, None, true);
        match made.as_slice() {
            [] => {}
            [one] => match s.entities[one.0 as usize].geom {
                Geom::Line { .. } => lines += 1,
                Geom::Circle { radius, .. } => {
                    assert!((radius - 2.0).abs() < 1e-9);
                    circles += 1
                }
                Geom::Arc { .. } => circles += 1,
            },
            _ => polys += 1,
        }
    }
    // box outline (top + bottom project onto each other) and the vertical hole's
    // two circles exactly; the horizontal hole's circles are seen edge-on (lines)
    assert!(lines >= 8, "{lines}");
    assert_eq!(circles, 2, "vertical hole top and bottom");
    assert_eq!(polys, 0);
    assert!(s.entities.iter().all(|e| e.construction));
    assert!(s.solve(&Values::new()).unwrap().converged);
    // a tilted circle becomes a fixed polyline through the given samples
    let tilted = Shape::cylinder(DVec3::ZERO, DVec3::new(1.0, 0.0, 1.0), 2.0, 3.0).unwrap();
    let te = tilted.edges().unwrap().into_iter().find(|e| e.circle.is_some()).unwrap();
    let samples: Vec<DVec3> = (0..=16).map(|i| {
        let (c, ax, r) = te.circle.unwrap();
        let ax = ax.normalize();
        let u = ax.any_orthonormal_vector();
        let v = ax.cross(u);
        let t = i as f64 / 16.0 * std::f64::consts::TAU;
        c + (u * t.cos() + v * t.sin()) * r
    }).collect();
    let made = s.project_edge(&te, Some(&samples), false);
    assert_eq!(made.len(), 16);
}
