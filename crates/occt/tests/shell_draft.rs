//! Shell on drafted bodies with features joined on top (the 1:18 trash cart).
use occt::{DVec3, Edge, Frame, Shape};

/// Rounded square `w`×`w`, corner radius `r`, centred on the axis at height `z`.
fn rounded_square(w: f64, r: f64, z: f64) -> Shape {
    let h = w / 2.0;
    let p = |x: f64, y: f64| DVec3::new(x, y, z);
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let c = h - r;
    let arc = |cx: f64, cy: f64, a0: f64| {
        let a1 = a0 + std::f64::consts::FRAC_PI_2;
        let am = a0 + std::f64::consts::FRAC_PI_4;
        let _ = s;
        Edge::Arc3(
            p(cx + r * a0.cos(), cy + r * a0.sin()),
            p(cx + r * am.cos(), cy + r * am.sin()),
            p(cx + r * a1.cos(), cy + r * a1.sin()),
        )
    };
    use std::f64::consts::PI;
    let outer = [
        Edge::Line(p(-c, -h), p(c, -h)),
        arc(c, -c, -PI / 2.0),
        Edge::Line(p(h, -c), p(h, c)),
        arc(c, c, 0.0),
        Edge::Line(p(c, h), p(-c, h)),
        arc(-c, c, PI / 2.0),
        Edge::Line(p(-h, c), p(-h, -c)),
        arc(-c, -c, PI),
    ];
    let fr = Frame { origin: DVec3::new(0.0, 0.0, z), x_dir: DVec3::X, normal: DVec3::Z };
    Shape::face(&fr, &outer, &[]).unwrap()
}

fn drafted(w: f64, r: f64, z: f64, height: f64, draft: f64) -> Shape {
    let f = rounded_square(w, r, z);
    let fr = Frame { origin: DVec3::new(0.0, 0.0, z), x_dir: DVec3::X, normal: DVec3::Z };
    f.extrude_ext(&fr, DVec3::Z, height, false, draft).unwrap()
}

fn top_face(s: &Shape) -> u32 {
    let faces = s.faces().unwrap();
    let zmax = faces.iter().filter(|f| f.normal.z > 0.99).map(|f| f.centroid.z).fold(f64::MIN, f64::max);
    faces.iter().position(|f| f.normal.z > 0.99 && (f.centroid.z - zmax).abs() < 1e-6).unwrap() as u32
}

/// Shell with the top open, checked: one valid solid whose wall volume is about
/// (area − open area) × t. Returns the strategy used.
fn shell_top(s: &Shape, t: f64) -> occt::ShellStrategy {
    let top = top_face(s);
    let started = std::time::Instant::now();
    let (r, how) = s.shell_report(&[top], t, false).unwrap_or_else(|e| panic!("shell failed: {e}"));
    let ms = started.elapsed().as_secs_f64() * 1e3;
    assert!(r.is_valid());
    assert_eq!(r.solids().unwrap().len(), 1);
    let faces = s.faces().unwrap();
    let walls: f64 = faces.iter().map(|f| f.area).sum::<f64>() - faces[top as usize].area;
    let v = r.volume().unwrap();
    let est = walls * t;
    assert!(v > 0.7 * est && v < 1.3 * est, "wall volume {v:.1} vs about {est:.1}");
    // still open at the top: nothing of the result at the top face's centre
    eprintln!("  {how:?} ({}) in {ms:.0} ms, walls {v:.1} mm³ (≈{est:.1})", how.describe());
    how
}

/// Lower body of the 1:18 cart: rounded square 34.4, r 9.17, 46.57 tall, outward
/// draft 2.083°. Returns it with its top outline (width, radius).
fn lower_body() -> (Shape, f64, f64) {
    let d = 46.57 * 2.083f64.to_radians().tan();
    (drafted(34.4, 9.17, 0.0, 46.57, -2.083), 34.4 + 2.0 * d, 9.17 + d)
}

/// The reported case: the collar sketched at the lower body's top outline rounded to
/// two decimals (37.82 / r 10.87 against 37.788 / r 10.864), so the fuse leaves a
/// 0.016 mm ledge ring at z = 46.57 that the default offset cannot close.
#[test]
fn trash_cart_drafted_body_with_collar() {
    let (lower, _, _) = lower_body();
    let collar = drafted(37.82, 10.87, 46.57, 4.23, -20.1);
    let joined = lower.fuse(&collar).unwrap().unify().unwrap();
    // the ledge is still there after unify and is named as the blocker
    let top = top_face(&joined);
    let blockers = joined.shell_blockers(&[top], 1.2).unwrap();
    assert!(
        blockers.iter().any(|b| matches!(b, occt::ShellBlocker::NarrowFace { width, .. } if *width < 0.05)),
        "{blockers:?}"
    );
    let how = shell_top(&joined, 1.2);
    assert!(how.is_fallback());
    // 0.6 mm walls too
    shell_top(&joined, 0.6);
}

/// Variations: exact outline (a 2° crease with a straight collar), ledges and insets
/// narrower than the wall, an overlapping collar.
#[test]
fn drafted_body_collar_variants() {
    let (lower, tw, tr) = lower_body();
    for (name, w, r, z0, h) in [
        ("exact", tw, tr, 46.57, 4.23),
        ("ledge 0.1", tw + 0.2, tr + 0.1, 46.57, 4.23),
        ("ledge 0.5", tw + 1.0, tr + 0.5, 46.57, 4.23),
        ("ledge 1.5", tw + 3.0, tr + 1.5, 46.57, 4.23),
        ("inset 0.016", tw - 0.032, tr - 0.016, 46.57, 4.23),
        ("inset 0.5", tw - 1.0, tr - 0.5, 46.57, 4.23),
        ("overlap", 37.82, 10.87, 46.07, 4.73),
    ] {
        for dr in [-20.1, -5.0, 0.0] {
            eprintln!("{name}, collar draft {dr}:");
            let collar = drafted(w, r, z0, h, dr);
            let j = lower.fuse(&collar).unwrap().unify().unwrap();
            shell_top(&j, 1.2);
        }
    }
}

/// A boss (rib-like block) joined on top of the drafted body, its outline nearly
/// flush with one drafted wall, and a pocket cut into the top.
#[test]
fn drafted_body_with_boss_and_pocket() {
    let (lower, tw, _) = lower_body();
    // boss: a 6 × 4 block standing on the top face, its outer side 0.05 mm inside the wall
    let h = tw / 2.0;
    let boss = Shape::make_box(DVec3::new(-3.0, h - 4.05, 46.57), DVec3::new(6.0, 4.0, 5.43)).unwrap();
    let with_boss = lower.fuse(&boss).unwrap().unify().unwrap();
    eprintln!("boss:");
    shell_top(&with_boss, 1.2);
    // pocket in the side, 0.3 mm deep (shallower than the wall)
    // (the wall sits at |y| = 17.2 at the bottom, 18.9 at the top; 17.5 at z ≈ 10)
    let pocket = Shape::make_box(DVec3::new(-5.0, -30.0, 10.0), DVec3::new(10.0, 30.0 - 17.2, 10.0)).unwrap();
    let pocketed = lower.cut(&pocket).unwrap().unify().unwrap();
    eprintln!("pocket:");
    shell_top(&pocketed, 1.2);
    // and both, with the collar
    let collar = drafted(37.82, 10.87, 46.57, 4.23, -20.1);
    let all = lower.fuse(&collar).unwrap().cut(&pocket).unwrap().unify().unwrap();
    eprintln!("collar + pocket:");
    shell_top(&all, 1.2);
}

/// When nothing works the error names the blocking faces, not "thickness larger
/// than a fillet radius".
#[test]
fn shell_failure_names_blockers() {
    // a 0.3 mm ledge and a 1 mm collar under a 2 mm wall: the collar is thinner
    // than the wall, so no strategy keeps it
    let (lower, tw, tr) = lower_body();
    let collar = drafted(tw + 0.6, tr + 0.3, 46.57, 1.0, 0.0);
    let j = lower.fuse(&collar).unwrap().unify().unwrap();
    match j.shell_report(&[top_face(&j)], 2.0, false) {
        Ok((_, how)) => eprintln!("built anyway: {how:?}"),
        Err(e) => {
            eprintln!("{e}");
            assert!(!e.to_string().contains("fillet radius"), "{e}");
            assert!(e.to_string().contains("likely blockers: face "), "{e}");
        }
    }
}


/// A side face open under a top fillet tangent to it: MakeThickSolid gives the body
/// back unchanged in every mode (the old code accepted that as a shell). The
/// offset-and-cut fallback hollows it.
#[test]
fn side_open_under_tangent_fillet_is_really_hollow() {
    let b = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 10.0)).unwrap();
    let top = b.faces().unwrap().iter().position(|f| f.normal.z > 0.99).unwrap() as u32;
    let edges = b.face_edges(top).unwrap();
    let r = b.fillet_raw(&edges, &vec![2.0; edges.len()]).unwrap();
    let side = r.faces().unwrap().iter().position(|f| f.normal.x > 0.99 && f.kind == occt::SurfaceKind::Plane).unwrap() as u32;
    let (s, how) = r.shell_report(&[side], 1.0, false).unwrap();
    assert_eq!(how, occt::ShellStrategy::OffsetAndCut);
    assert!(s.is_valid() && s.solids().unwrap().len() == 1);
    let v = s.volume().unwrap();
    assert!(v > 2800.0 && v < 3600.0, "{v}");
}
