//! The inferred Construct tool: which datum a set of clicked things builds.
//!
//! Each click adds a pick (a second click on it removes it); [`infer`] looks at
//! what the picks are and lists the constructs they can make, the most likely
//! first. The user cycles the alternatives with Tab.

use glam::DVec3;

use crate::datum_ui::{Cls, DatumKind, Picked};

/// One possible construct: the kind and which pick fills each slot of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Cand {
    pub kind: DatumKind,
    pub order: Vec<Option<usize>>,
    /// Starting text of the value field.
    pub value: &'static str,
}

fn cand(kind: DatumKind, order: &[Option<usize>], value: &'static str) -> Cand {
    Cand { kind, order: order.to_vec(), value }
}

fn parallel(a: DVec3, b: DVec3) -> bool {
    a != DVec3::ZERO && b != DVec3::ZERO && a.cross(b).length() < 1e-4
}

/// Do two lines lie in one plane (cross or run parallel)? Unknown geometry counts as yes.
fn coplanar(a: &Picked, b: &Picked) -> bool {
    if a.n == DVec3::ZERO || b.n == DVec3::ZERO {
        return true;
    }
    let c = a.n.cross(b.n);
    if c.length() < 1e-4 {
        return true;
    }
    ((b.p - a.p).dot(c) / c.length()).abs() < 0.01
}

/// The constructs a selection can make, default first (empty: nothing fits).
pub fn infer(picks: &[Picked]) -> Vec<Cand> {
    use DatumKind::*;
    let of = |c: Cls| -> Vec<usize> { picks.iter().enumerate().filter(|(_, p)| p.cls == c).map(|(i, _)| i).collect() };
    let (pt, ln, pl, cy, ci, cu) =
        (of(Cls::Point), of(Cls::Line), of(Cls::Plane), of(Cls::Cyl), of(Cls::Circle), of(Cls::Curve));
    let s = |i: usize| Some(i);
    match (pt.len(), ln.len(), pl.len(), cy.len(), ci.len(), cu.len()) {
        (1, 0, 0, 0, 0, 0) => vec![cand(PointAt, &[s(pt[0])], "")],
        (2, 0, 0, 0, 0, 0) => vec![cand(AxisTwoPoints, &[s(pt[0]), s(pt[1])], "")],
        (3, 0, 0, 0, 0, 0) => vec![cand(PlaneThreePoints, &[s(pt[0]), s(pt[1]), s(pt[2])], "")],
        (0, 0, 1, 0, 0, 0) => vec![cand(PlaneOffset, &[s(pl[0])], "10")],
        (0, 1, 0, 0, 0, 0) => vec![cand(AxisLine, &[s(ln[0])], ""), cand(PlaneNormal, &[s(ln[0])], "0.5")],
        (0, 0, 0, 0, 0, 1) => vec![cand(PlaneNormal, &[s(cu[0])], "0.5")],
        (0, 1, 1, 0, 0, 0) => vec![
            cand(PlaneAngle, &[s(ln[0]), s(pl[0])], "0"),
            cand(PointEdgePlane, &[s(ln[0]), s(pl[0])], ""),
        ],
        (0, 0, 2, 0, 0, 0) => {
            let (a, b) = (&picks[pl[0]], &picks[pl[1]]);
            let mid = cand(PlaneMid, &[s(pl[0]), s(pl[1])], "");
            if parallel(a.n, b.n) {
                vec![mid]
            } else {
                vec![cand(AxisTwoPlanes, &[s(pl[0]), s(pl[1])], ""), mid]
            }
        }
        (0, 0, 3, 0, 0, 0) => vec![cand(PointThreePlanes, &[s(pl[0]), s(pl[1]), s(pl[2])], "")],
        (0, 2, 0, 0, 0, 0) => {
            let point = cand(PointTwoLines, &[s(ln[0]), s(ln[1])], "");
            if coplanar(&picks[ln[0]], &picks[ln[1]]) {
                if parallel(picks[ln[0]].n, picks[ln[1]].n) {
                    vec![cand(PlaneTwoLines, &[s(ln[0]), s(ln[1])], "")]
                } else {
                    vec![cand(PlaneTwoLines, &[s(ln[0]), s(ln[1])], ""), point]
                }
            } else {
                vec![point]
            }
        }
        (0, 0, 0, 1, 0, 0) => vec![cand(AxisCylinder, &[s(cy[0])], ""), cand(PlaneTangent, &[s(cy[0]), None], "0")],
        (0, 0, 0, 0, 1, 0) => vec![cand(PointCenter, &[s(ci[0])], ""), cand(AxisCylinder, &[s(ci[0])], "")],
        (0, 0, 1, 1, 0, 0) => vec![cand(PlaneTangent, &[s(cy[0]), s(pl[0])], "0")],
        (1, 0, 1, 0, 0, 0) => vec![cand(AxisNormal, &[s(pl[0]), s(pt[0])], "")],
        (1, 0, 0, 1, 0, 0) => vec![cand(AxisNormal, &[s(cy[0]), s(pt[0])], "")],
        _ => Vec::new(),
    }
}

/// What to tell the user when the picks make nothing.
pub fn nothing_hint(n: usize) -> &'static str {
    match n {
        0 => "Click points, edges, faces or planes. Enter finishes, Esc clears.",
        _ => "These don't make a construct: click one again to remove it.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum_ui::Hi;
    use easycad_model::datum::{GeomRef, OriginRef};

    fn pk(cls: Cls, p: DVec3, n: DVec3) -> Picked {
        Picked { r: GeomRef::Origin(OriginRef::Origin), label: String::new(), hi: Hi::None, cls, p, n }
    }

    #[test]
    fn counts_pick_the_type() {
        let pt = pk(Cls::Point, DVec3::ZERO, DVec3::ZERO);
        let pl = pk(Cls::Plane, DVec3::ZERO, DVec3::Z);
        let ln = pk(Cls::Line, DVec3::ZERO, DVec3::X);
        assert_eq!(infer(&[pt.clone()])[0].kind, DatumKind::PointAt);
        assert_eq!(infer(&[pt.clone(), pt.clone()])[0].kind, DatumKind::AxisTwoPoints);
        assert_eq!(infer(&[pt.clone(), pt.clone(), pt.clone()])[0].kind, DatumKind::PlaneThreePoints);
        assert_eq!(infer(&[pl.clone()])[0].kind, DatumKind::PlaneOffset);
        let lp = infer(&[ln.clone(), pl.clone()]);
        assert_eq!(lp[0].kind, DatumKind::PlaneAngle);
        assert_eq!(lp[1].kind, DatumKind::PointEdgePlane);
        assert!(infer(&[pt.clone(), ln.clone()]).is_empty());
    }

    #[test]
    fn two_planes_parallel_or_not() {
        let a = pk(Cls::Plane, DVec3::ZERO, DVec3::Z);
        let b = pk(Cls::Plane, DVec3::new(0.0, 0.0, 5.0), DVec3::Z);
        let c = pk(Cls::Plane, DVec3::ZERO, DVec3::X);
        assert_eq!(infer(&[a.clone(), b]).iter().map(|c| c.kind).collect::<Vec<_>>(), vec![DatumKind::PlaneMid]);
        assert_eq!(
            infer(&[a, c]).iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![DatumKind::AxisTwoPlanes, DatumKind::PlaneMid]
        );
    }

    #[test]
    fn skew_lines_only_make_a_point() {
        let a = pk(Cls::Line, DVec3::ZERO, DVec3::X);
        let b = pk(Cls::Line, DVec3::new(0.0, 0.0, 3.0), DVec3::Y);
        assert_eq!(infer(&[a.clone(), b]).iter().map(|c| c.kind).collect::<Vec<_>>(), vec![DatumKind::PointTwoLines]);
        let c = pk(Cls::Line, DVec3::ZERO, DVec3::Y);
        assert_eq!(infer(&[a, c])[0].kind, DatumKind::PlaneTwoLines);
    }
}
