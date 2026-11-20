//! Construction geometry (datums) in the viewport: what each pick slot accepts,
//! the scene of origin / datum / sketch / body geometry, hover picking with
//! smart filtering, and the drawing of datum planes, axes and points.
//!
//! Planes are drawn through the GPU sketch path (translucent square with a border,
//! depth tested); axes (long dash-dot lines), points and labels are painted over
//! the image with the camera projection.

use std::sync::Arc;

use easycad_model::datum::{DatumGeom, GeomRef, OriginRef};
use easycad_model::feature::{BodyId, FeatureId, FeatureKind};
use easycad_model::geom::Plane;
use easycad_model::occt::{CurveKind, EdgePolyline, Mesh, SurfaceKind};
use easycad_model::select::{EdgeSelector, FaceSelector};
use egui::{pos2, Color32, Pos2, Rect, Stroke};
use glam::{DVec2, DVec3, Vec2, Vec3};

use crate::app::EasyCadApp;
use crate::theme;

// ---------------------------------------------------------------- kinds and slots

/// What a pick slot takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accept(pub u8);

impl Accept {
    pub const NONE: Accept = Accept(0);
    /// Origin / datum planes and planar faces.
    pub const PLANE: Accept = Accept(1);
    /// Origin / datum axes, straight edges, sketch lines.
    pub const LINE: Accept = Accept(2);
    /// Origin point, datum points, sketch points, body vertices.
    pub const POINT: Accept = Accept(4);
    /// Cylindrical / conical faces.
    pub const CYLF: Accept = Accept(8);
    /// Circular edges and sketch circles, sphere faces.
    pub const CIRCLE: Accept = Accept(16);
    /// Any edge or sketch curve (a path).
    pub const CURVE: Accept = Accept(32);

    pub const fn or(self, o: Accept) -> Accept {
        Accept(self.0 | o.0)
    }
    pub const fn has(self, o: Accept) -> bool {
        self.0 & o.0 != 0
    }
}

pub struct SlotDef {
    pub label: &'static str,
    pub hint: &'static str,
    pub accept: Accept,
    pub optional: bool,
}

pub struct FieldDef {
    pub label: &'static str,
    pub default: &'static str,
}

const fn slot(label: &'static str, hint: &'static str, accept: Accept) -> SlotDef {
    SlotDef { label, hint, accept, optional: false }
}
const fn opt(label: &'static str, hint: &'static str, accept: Accept) -> SlotDef {
    SlotDef { label, hint, accept, optional: true }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Plane,
    Axis,
    Point,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatumKind {
    PlaneOffset,
    PlaneAngle,
    PlaneMid,
    PlaneTwoLines,
    PlaneThreePoints,
    PlaneTangent,
    PlaneNormal,
    AxisCylinder,
    AxisLine,
    AxisTwoPoints,
    AxisTwoPlanes,
    AxisNormal,
    PointAt,
    PointCenter,
    PointEdgePlane,
    PointThreePlanes,
    PointTwoLines,
}

use DatumKind::*;

impl DatumKind {
    pub const PLANES: [DatumKind; 7] =
        [PlaneOffset, PlaneAngle, PlaneMid, PlaneTwoLines, PlaneThreePoints, PlaneTangent, PlaneNormal];
    pub const AXES: [DatumKind; 5] = [AxisCylinder, AxisLine, AxisTwoPoints, AxisTwoPlanes, AxisNormal];
    pub const POINTS: [DatumKind; 5] = [PointAt, PointCenter, PointEdgePlane, PointThreePlanes, PointTwoLines];

    pub fn group(self) -> Group {
        match self {
            PlaneOffset | PlaneAngle | PlaneMid | PlaneTwoLines | PlaneThreePoints | PlaneTangent
            | PlaneNormal => Group::Plane,
            AxisCylinder | AxisLine | AxisTwoPoints | AxisTwoPlanes | AxisNormal => Group::Axis,
            _ => Group::Point,
        }
    }

    /// Menu entry / dialog title.
    pub fn title(self) -> &'static str {
        match self {
            PlaneOffset => "Offset plane",
            PlaneAngle => "Plane at angle",
            PlaneMid => "Midplane",
            PlaneTwoLines => "Plane through two lines",
            PlaneThreePoints => "Plane through three points",
            PlaneTangent => "Tangent plane",
            PlaneNormal => "Plane along path",
            AxisCylinder => "Axis through cylinder",
            AxisLine => "Axis along edge or line",
            AxisTwoPoints => "Axis through two points",
            AxisTwoPlanes => "Axis at two planes",
            AxisNormal => "Axis normal to face at point",
            PointAt => "Point at vertex",
            PointCenter => "Point at center of circle",
            PointEdgePlane => "Point at edge and plane",
            PointThreePlanes => "Point at three planes",
            PointTwoLines => "Point at two lines",
        }
    }

    /// Default name stem of the new feature.
    pub fn stem(self) -> &'static str {
        match self.group() {
            Group::Plane => "Plane",
            Group::Axis => "Axis",
            Group::Point => "Point",
        }
    }

    pub fn slots(self) -> Vec<SlotDef> {
        const P: Accept = Accept::PLANE;
        const L: Accept = Accept::LINE;
        const PT: Accept = Accept::POINT;
        match self {
            PlaneOffset => vec![slot("Base plane", "Pick a plane or flat face", P)],
            PlaneAngle => vec![
                slot("Axis", "Pick an axis, edge or sketch line", L),
                opt("Base plane", "Pick a plane (optional)", P),
            ],
            PlaneMid => vec![slot("First plane", "Pick a plane or flat face", P), slot("Second plane", "Pick a plane or flat face", P)],
            PlaneTwoLines => vec![slot("First line", "Pick an axis or edge", L), slot("Second line", "Pick an axis or edge", L)],
            PlaneThreePoints => vec![
                slot("First point", "Pick a point or vertex", PT),
                slot("Second point", "Pick a point or vertex", PT),
                slot("Third point", "Pick a point or vertex", PT),
            ],
            PlaneTangent => vec![
                slot("Cylinder", "Pick a cylindrical face", Accept::CYLF),
                opt("Base plane", "Pick a plane (optional)", P),
            ],
            PlaneNormal => vec![slot("Path", "Pick an edge, axis or sketch line", L.or(Accept::CURVE))],
            AxisCylinder => vec![slot("Cylinder", "Pick a cylindrical face or circular edge", Accept::CYLF.or(Accept::CIRCLE))],
            AxisLine => vec![slot("Line", "Pick an edge, axis or sketch line", L)],
            AxisTwoPoints => vec![slot("First point", "Pick a point or vertex", PT), slot("Second point", "Pick a point or vertex", PT)],
            AxisTwoPlanes => vec![slot("First plane", "Pick a plane or flat face", P), slot("Second plane", "Pick a plane or flat face", P)],
            AxisNormal => vec![slot("Face", "Pick a flat face or plane", P), slot("Point", "Pick a point or vertex", PT)],
            PointAt => vec![slot("Point", "Pick a vertex or sketch point", PT)],
            PointCenter => vec![slot("Circle", "Pick a circular edge or round face", Accept::CIRCLE)],
            PointEdgePlane => vec![slot("Edge", "Pick an edge or axis", L), slot("Plane", "Pick a plane or flat face", P)],
            PointThreePlanes => vec![
                slot("First plane", "Pick a plane or flat face", P),
                slot("Second plane", "Pick a plane or flat face", P),
                slot("Third plane", "Pick a plane or flat face", P),
            ],
            PointTwoLines => vec![slot("First line", "Pick an edge or axis", L), slot("Second line", "Pick an edge or axis", L)],
        }
    }

    pub fn fields(self) -> &'static [FieldDef] {
        match self {
            PlaneOffset => &[FieldDef { label: "Distance (mm)", default: "10" }],
            PlaneAngle => &[FieldDef { label: "Angle (°)", default: "45" }],
            PlaneTangent => &[FieldDef { label: "Angle (°)", default: "0" }],
            PlaneNormal => &[FieldDef { label: "Position (0–1)", default: "0.5" }],
            _ => &[],
        }
    }
}

// ---------------------------------------------------------------- scene

/// Geometry of a pickable / drawable datum-like thing.
#[derive(Clone, Debug)]
pub enum G {
    Plane(Plane),
    Axis(DVec3, DVec3),
    Point(DVec3),
    /// A polyline (sketch entity); `straight` = a single line segment.
    Poly { pts: Vec<DVec3>, straight: bool },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub r: GeomRef,
    pub name: String,
    pub g: G,
    /// Index into the origin rows (0..7) when this is an origin item.
    pub origin: Option<usize>,
    pub fid: Option<FeatureId>,
}

/// Origin rows in browser order.
pub const ORIGIN_ROWS: [(OriginRef, &str); 7] = [
    (OriginRef::XY, "XY plane"),
    (OriginRef::XZ, "XZ plane"),
    (OriginRef::YZ, "YZ plane"),
    (OriginRef::XAxis, "X axis"),
    (OriginRef::YAxis, "Y axis"),
    (OriginRef::ZAxis, "Z axis"),
    (OriginRef::Origin, "Origin"),
];

pub fn origin_item(i: usize) -> Item {
    let (r, name) = ORIGIN_ROWS[i];
    let g = match i {
        0 => G::Plane(Plane::XY),
        1 => G::Plane(Plane::XZ),
        2 => G::Plane(Plane::YZ),
        3 => G::Axis(DVec3::ZERO, DVec3::X),
        4 => G::Axis(DVec3::ZERO, DVec3::Y),
        5 => G::Axis(DVec3::ZERO, DVec3::Z),
        _ => G::Point(DVec3::ZERO),
    };
    Item { r: GeomRef::Origin(r), name: name.into(), g, origin: Some(i), fid: None }
}

/// Datums of the current model, visible ones only, with their names. `below` limits
/// them to features earlier in the history than that index.
pub fn datum_items(app: &EasyCadApp, below: Option<usize>) -> Vec<Item> {
    datums_from(app, below, false)
}

/// `with_preview`: read the open dialog's preview (the datum being made is in it).
fn datums_from(app: &EasyCadApp, below: Option<usize>, with_preview: bool) -> Vec<Item> {
    let out = if with_preview && app.dialog.is_some() { app.preview.as_deref().or(app.model.as_deref()) } else { app.model.as_deref() };
    let Some(m) = out else { return Vec::new() };
    let view = app.view();
    let dlg_name = app.dialog.as_ref().map(|d| d.name.clone());
    m.result
        .datums
        .iter()
        .filter(|(fid, _)| view.datum_visible(*fid))
        .filter(|(fid, _)| match (below, view.features.iter().position(|f| f.id == *fid)) {
            (Some(b), Some(i)) => i < b,
            (_, None) => with_preview,
            _ => true,
        })
        .map(|(fid, g)| Item {
            r: GeomRef::Datum(*fid),
            name: view
                .feature(*fid)
                .map(|f| f.name.clone())
                .or_else(|| dlg_name.clone())
                .unwrap_or_else(|| "Datum".into()),
            g: match g {
                DatumGeom::Plane(p) => G::Plane(*p),
                DatumGeom::Axis { origin, dir } => G::Axis(*origin, *dir),
                DatumGeom::Point(p) => G::Point(*p),
            },
            origin: None,
            fid: Some(*fid),
        })
        .collect()
}

fn circle_pts(sk: &easycad_model::sketch::Sketch, geom: &easycad_model::sketch::Geom) -> Vec<DVec2> {
    use easycad_model::sketch::Geom;
    let arc = |c: DVec2, a0: f64, sweep: f64, r: f64| -> Vec<DVec2> {
        let n = 48;
        (0..=n).map(|i| c + DVec2::from_angle(a0 + sweep * i as f64 / n as f64) * r).collect()
    };
    match *geom {
        Geom::Line { a, b } => vec![sk.point(a), sk.point(b)],
        Geom::Circle { center, radius } => arc(sk.point(center), 0.0, std::f64::consts::TAU, radius),
        Geom::Arc { center, start, end } => {
            let c = sk.point(center);
            let (s, t) = (sk.point(start), sk.point(end));
            let a0 = (s - c).to_angle();
            let mut sw = (t - c).to_angle() - a0;
            if sw <= 0.0 {
                sw += std::f64::consts::TAU;
            }
            arc(c, a0, sw, (s - c).length())
        }
    }
}

/// Lines, curves and points of visible sketches (construction ones included).
pub fn sketch_items(app: &EasyCadApp) -> Vec<Item> {
    let view = app.view();
    let mut out = Vec::new();
    for e in view.sketches.iter().filter(|e| view.sketch_visible(e.id)) {
        let sk = &e.sketch;
        for (i, en) in sk.entities.iter().enumerate() {
            let pts: Vec<DVec3> = circle_pts(sk, &en.geom).into_iter().map(|p| sk.plane.to_world(p)).collect();
            let straight = matches!(en.geom, easycad_model::sketch::Geom::Line { .. });
            out.push(Item {
                r: GeomRef::SketchLine { sketch: e.id, entity: i as u32 },
                name: format!("{} · {} {}", sk.name, if straight { "line" } else { "curve" }, i + 1),
                g: G::Poly { pts, straight },
                origin: None,
                fid: None,
            });
        }
        for (i, p) in sk.points.iter().enumerate() {
            out.push(Item {
                r: GeomRef::SketchPoint { sketch: e.id, point: i as u32 },
                name: format!("{} · point {}", sk.name, i + 1),
                g: G::Point(sk.plane.to_world(*p)),
                origin: None,
                fid: None,
            });
        }
    }
    out
}

/// What a pick shows highlighted.
#[derive(Clone, Debug)]
pub enum Hi {
    None,
    Item(G),
    Face(BodyId, u32),
}

/// What kind of geometry a pick is, for inferring the construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cls {
    Point,
    Line,
    Plane,
    /// Cylindrical / conical face.
    Cyl,
    /// Circular edge, sketch circle, sphere face.
    Circle,
    /// Any other curve.
    Curve,
}

#[derive(Clone, Debug)]
pub struct Picked {
    pub r: GeomRef,
    pub label: String,
    pub hi: Hi,
    pub cls: Cls,
    /// A point on it (plane origin, line point, axis point, the point itself).
    pub p: DVec3,
    /// Plane normal / line or axis direction (zero when unknown).
    pub n: DVec3,
}

fn item_pick(it: &Item) -> Picked {
    let (cls, p, n) = match &it.g {
        G::Plane(pl) => (Cls::Plane, pl.origin, pl.normal),
        G::Axis(o, d) => (Cls::Line, *o, d.normalize_or_zero()),
        G::Point(p) => (Cls::Point, *p, DVec3::ZERO),
        G::Poly { pts, straight } => {
            let a = pts.first().copied().unwrap_or_default();
            let b = pts.last().copied().unwrap_or_default();
            if *straight {
                (Cls::Line, a, (b - a).normalize_or_zero())
            } else if (a - b).length() < 1e-6 {
                (Cls::Circle, a, DVec3::ZERO)
            } else {
                (Cls::Curve, a, DVec3::ZERO)
            }
        }
    };
    Picked { r: it.r.clone(), label: it.name.clone(), hi: Hi::Item(it.g.clone()), cls, p, n }
}

fn edge_pick(info: &easycad_model::occt::EdgeInfo, poly: Option<Vec<DVec3>>) -> Picked {
    let (cls, p, n) = if info.kind == CurveKind::Line {
        (Cls::Line, info.start, (info.end - info.start).normalize_or_zero())
    } else if let Some((c, ax, _)) = info.circle {
        (Cls::Circle, c, ax)
    } else {
        (Cls::Curve, info.midpoint, info.tangent)
    };
    Picked {
        r: GeomRef::Edge(EdgeSelector::from_pick(info)),
        label: crate::ui_viewport::edge_label(info).0,
        hi: match poly {
            Some(pts) => Hi::Item(G::Poly { pts, straight: info.kind == CurveKind::Line }),
            None => Hi::None,
        },
        cls,
        p,
        n,
    }
}

fn face_pick(info: &easycad_model::occt::FaceInfo, body: BodyId, f: u32) -> Picked {
    let (cls, p, n) = match info.kind {
        SurfaceKind::Plane => (Cls::Plane, info.point, info.normal),
        SurfaceKind::Cylinder | SurfaceKind::Cone => {
            let (o, d) = info.axis.unwrap_or((info.centroid, DVec3::Z));
            (Cls::Cyl, o, d.normalize_or_zero())
        }
        _ => (Cls::Circle, info.centroid, DVec3::ZERO),
    };
    Picked {
        r: GeomRef::Face(FaceSelector::from_pick(info)),
        label: crate::ui_viewport::face_desc(info),
        hi: Hi::Face(body, f),
        cls,
        p,
        n,
    }
}

/// Plain-words name of a reference (for edited datums, whose picks have no highlight).
pub fn ref_label(d: &easycad_model::document::DocData, r: &GeomRef) -> String {
    match r {
        GeomRef::Origin(o) => ORIGIN_ROWS.iter().find(|(x, _)| x == o).map(|(_, n)| n.to_string()).unwrap_or_default(),
        GeomRef::Datum(f) => d.feature(*f).map(|f| f.name.clone()).unwrap_or_else(|| "Datum".into()),
        GeomRef::Face(_) => "Face".into(),
        GeomRef::Edge(_) => "Edge".into(),
        GeomRef::Vertex { hint } => format!("Vertex ({:.1}, {:.1}, {:.1})", hint.x, hint.y, hint.z),
        GeomRef::SketchLine { sketch, entity } => {
            format!("{} · line {}", d.sketch(*sketch).map(|s| s.name.as_str()).unwrap_or("Sketch"), entity + 1)
        }
        GeomRef::SketchPoint { sketch, point } => {
            format!("{} · point {}", d.sketch(*sketch).map(|s| s.name.as_str()).unwrap_or("Sketch"), point + 1)
        }
    }
}

// ---------------------------------------------------------------- sizes and caches

/// Half-size of the datum squares and axis half-length base.
pub fn half(app: &EasyCadApp) -> f64 {
    crate::plane_pick::half_size(app)
}

fn square_mesh(pl: &Plane, h: f64) -> Mesh {
    let c = [DVec2::new(-h, -h), DVec2::new(h, -h), DVec2::new(h, h), DVec2::new(-h, h)];
    let pts: Vec<[f32; 3]> = c.iter().map(|p| pl.to_world(*p).as_vec3().to_array()).collect();
    let n = pl.normal.as_vec3().to_array();
    let mut ring = pts.clone();
    ring.push(pts[0]);
    Mesh {
        positions: pts,
        normals: vec![n; 4],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        tri_face: vec![0, 0],
        edges: vec![EdgePolyline { edge: 0, points: ring }],
    }
}

type SquareKey = [u64; 10];
thread_local! {
    static SQUARES: std::cell::RefCell<Vec<(SquareKey, Arc<Mesh>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn square(pl: &Plane, h: f64) -> Arc<Mesh> {
    let key: SquareKey = [
        pl.origin.x.to_bits(),
        pl.origin.y.to_bits(),
        pl.origin.z.to_bits(),
        pl.x_dir.x.to_bits(),
        pl.x_dir.y.to_bits(),
        pl.x_dir.z.to_bits(),
        pl.normal.x.to_bits(),
        pl.normal.y.to_bits(),
        pl.normal.z.to_bits(),
        h.to_bits(),
    ];
    SQUARES.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((_, m)) = c.iter().find(|(k, _)| *k == key) {
            return m.clone();
        }
        if c.len() > 96 {
            c.clear();
        }
        let m = Arc::new(square_mesh(pl, h));
        c.push((key, m.clone()));
        m
    })
}

// ---------------------------------------------------------------- what is shown

/// The accept of the open datum dialog's active slot (nothing when none is open).
pub fn active_accept(app: &EasyCadApp) -> Accept {
    match &app.dialog {
        Some(d) => match &d.body {
            crate::dialogs::Body::Datum(dd) => dd.active_accept(),
            _ => Accept::NONE,
        },
        None => Accept::NONE,
    }
}

/// Index limit for datum references: only features before the one being made/edited.
pub fn history_limit(app: &EasyCadApp) -> Option<usize> {
    let d = app.dialog.as_ref()?;
    d.edit_index(app.view()).or_else(|| app.rollback())
}

/// Items drawn now: visible origin rows, visible datums, and the origin rows the
/// active slot accepts.
pub fn shown_items(app: &EasyCadApp) -> Vec<Item> {
    let acc = active_accept(app);
    let mut out = Vec::new();
    for i in 0..7 {
        let want = match i {
            0..=2 => acc.has(Accept::PLANE),
            3..=5 => acc.has(Accept::LINE),
            _ => acc.has(Accept::POINT),
        };
        if app.origin_vis[i] || want {
            out.push(origin_item(i));
        }
    }
    out.extend(datums_from(app, None, true));
    out
}

fn same(a: &GeomRef, b: &GeomRef) -> bool {
    a == b
}

/// Squares for the GPU path: (mesh, hovered/picked).
pub fn draw_list(app: &EasyCadApp) -> Vec<(Arc<Mesh>, bool)> {
    let h = half(app);
    let hover = app.datum_hover.as_ref().map(|p| p.r.clone());
    let picked = picked_refs(app);
    let pp_hover = match app.plane_pick.as_ref().and_then(|p| p.hover.as_ref()) {
        Some(crate::plane_pick::Target::Datum(f)) => Some(GeomRef::Datum(*f)),
        Some(crate::plane_pick::Target::Origin(i)) => Some(origin_item(*i).r),
        _ => None,
    };
    shown_items(app)
        .into_iter()
        .filter_map(|it| match &it.g {
            G::Plane(p) => {
                let is_new = it.fid.is_some_and(|f| app.view().feature(f).is_none());
                let hot = is_new
                    || hover.as_ref().is_some_and(|r| same(r, &it.r))
                    || pp_hover.as_ref().is_some_and(|r| same(r, &it.r))
                    || picked.iter().any(|r| same(r, &it.r));
                Some((square(p, h), hot))
            }
            _ => None,
        })
        .collect()
}

/// References held by the open datum dialog's slots.
pub fn picked_refs(app: &EasyCadApp) -> Vec<GeomRef> {
    match app.dialog.as_ref().map(|d| &d.body) {
        Some(crate::dialogs::Body::Datum(dd)) => dd.held().into_iter().map(|(r, _)| r.clone()).collect(),
        _ => Vec::new(),
    }
}

/// Faces to highlight: the hovered candidate and the picked slots.
pub fn face_highlights(app: &EasyCadApp) -> Vec<(BodyId, u32, bool)> {
    let mut v = Vec::new();
    if let Some(Picked { hi: Hi::Face(b, f), .. }) = &app.datum_hover {
        v.push((*b, *f, true));
    }
    if let Some(crate::dialogs::Body::Datum(dd)) = app.dialog.as_ref().map(|d| &d.body) {
        for (_, hi) in dd.held() {
            if let Hi::Face(b, f) = hi {
                v.push((*b, *f, false));
            }
        }
    }
    v
}

// ---------------------------------------------------------------- projection helpers

fn scr(app: &EasyCadApp, rect: Rect, w: DVec3) -> Option<Pos2> {
    let size = Vec2::new(rect.width(), rect.height());
    app.camera.project(w.as_vec3(), size).map(|s| pos2(rect.left() + s.x, rect.top() + s.y))
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    if l2 < 1e-6 {
        return p.distance(a);
    }
    let t = (((p - a).x * ab.x + (p - a).y * ab.y) / l2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Axis endpoints for drawing and picking.
fn axis_ends(app: &EasyCadApp, o: DVec3, d: DVec3) -> (DVec3, DVec3) {
    let l = half(app) * 1.4;
    let d = d.normalize_or_zero();
    (o - d * l, o + d * l)
}

/// Distance in px from a screen point to a world polyline.
fn poly_dist(app: &EasyCadApp, rect: Rect, p: Pos2, pts: &[DVec3]) -> Option<f32> {
    let s: Vec<Option<Pos2>> = pts.iter().map(|w| scr(app, rect, *w)).collect();
    let mut best: Option<f32> = None;
    for w in s.windows(2) {
        if let (Some(a), Some(b)) = (w[0], w[1]) {
            let d = seg_dist(p, a, b);
            best = Some(best.map_or(d, |x| x.min(d)));
        }
    }
    best
}

fn ray_plane(o: Vec3, d: Vec3, pl: &Plane, h: f64) -> Option<f32> {
    let (n, p0) = (pl.normal.as_vec3(), pl.origin.as_vec3());
    let den = n.dot(d);
    if den.abs() < 1e-6 {
        return None;
    }
    let t = n.dot(p0 - o) / den;
    if t <= 0.0 {
        return None;
    }
    let q = o + d * t - p0;
    let (x, y) = (q.dot(pl.x_dir.as_vec3()), q.dot(pl.y_dir().as_vec3()));
    (x.abs() <= h as f32 && y.abs() <= h as f32).then_some(t)
}

// ---------------------------------------------------------------- picking

#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct Score(u8, f32);

fn face_ok(info: &easycad_model::occt::FaceInfo, acc: Accept) -> bool {
    match info.kind {
        SurfaceKind::Plane => acc.has(Accept::PLANE),
        SurfaceKind::Cylinder | SurfaceKind::Cone => acc.has(Accept::CYLF) || (acc.has(Accept::CIRCLE) && info.kind == SurfaceKind::Cylinder),
        SurfaceKind::Sphere => acc.has(Accept::CIRCLE),
        _ => false,
    }
}

fn edge_ok(info: &easycad_model::occt::EdgeInfo, acc: Accept) -> bool {
    acc.has(Accept::CURVE)
        || (info.kind == CurveKind::Line && acc.has(Accept::LINE))
        || (info.circle.is_some() && acc.has(Accept::CIRCLE))
}

/// The best thing under the pointer for a slot that takes `acc`.
pub fn pick(app: &EasyCadApp, pos: Pos2, rect: Rect, acc: Accept) -> Option<Picked> {
    if acc == Accept::NONE {
        return None;
    }
    let size = Vec2::new(rect.width(), rect.height());
    let local = pos - rect.min;
    let (o, d) = app.camera.ray(Vec2::new(local.x, local.y), size);
    let h = half(app);
    let mut best: Option<(Score, Picked)> = None;
    let offer = |s: Score, p: Picked, best: &mut Option<(Score, Picked)>| {
        if best.as_ref().is_none_or(|b| s < b.0) {
            *best = Some((s, p));
        }
    };

    // origin, datum and sketch items
    // origin rows the slot takes are pickable even when not drawn; datums only
    // from earlier in the history than the one being made
    let mut items: Vec<Item> = (0..7).map(origin_item).collect();
    items.extend(datum_items(app, history_limit(app)));
    items.extend(sketch_items(app));
    for it in &items {
        match &it.g {
            G::Plane(pl) if acc.has(Accept::PLANE) => {
                if let Some(t) = ray_plane(o, d, pl, h) {
                    offer(Score(2, t), item_pick(it), &mut best);
                }
            }
            G::Axis(ao, ad) if acc.has(Accept::LINE) => {
                let (a, b) = axis_ends(app, *ao, *ad);
                if let (Some(sa), Some(sb)) = (scr(app, rect, a), scr(app, rect, b)) {
                    let dist = seg_dist(pos, sa, sb);
                    if dist <= 6.0 {
                        offer(Score(1, dist), item_pick(it), &mut best);
                    }
                }
            }
            G::Point(p) if acc.has(Accept::POINT) => {
                if let Some(s) = scr(app, rect, *p) {
                    let dist = s.distance(pos);
                    if dist <= 10.0 {
                        offer(Score(0, dist), item_pick(it), &mut best);
                    }
                }
            }
            G::Poly { pts, straight }
                if (acc.has(Accept::LINE) && *straight) || acc.has(Accept::CURVE) || (acc.has(Accept::CIRCLE) && !*straight) =>
            {
                if let Some(dist) = poly_dist(app, rect, pos, pts) {
                    if dist <= 6.0 {
                        offer(Score(1, dist), item_pick(it), &mut best);
                    }
                }
            }
            _ => {}
        }
    }

    // body vertices, edges and faces
    if let Some(m) = app.model.as_deref() {
        if acc.has(Accept::POINT) {
            let mut near: Vec<(f32, DVec3)> = Vec::new();
            for b in m.bodies.iter().filter(|b| app.body_visible(b.body.id)) {
                for e in &b.edges {
                    for v in [e.start, e.end] {
                        if let Some(s) = scr(app, rect, v) {
                            let dist = s.distance(pos);
                            if dist <= 9.0 {
                                near.push((dist, v));
                            }
                        }
                    }
                }
            }
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((dist, v)) = near.into_iter().find(|(_, v)| crate::ui_viewport::point_visible(app, m, v.as_vec3())) {
                offer(
                    Score(0, dist + 0.5),
                    Picked {
                        r: GeomRef::Vertex { hint: v },
                        label: format!("Vertex ({:.1}, {:.1}, {:.1})", v.x, v.y, v.z),
                        hi: Hi::Item(G::Point(v)),
                        cls: Cls::Point,
                        p: v,
                        n: DVec3::ZERO,
                    },
                    &mut best,
                );
            }
        }
        let tol = if app.camera.ortho {
            easycad_model::pick::EdgeTolerance::ortho(app.camera.world_per_point(size.y) * 5.0)
        } else {
            easycad_model::pick::EdgeTolerance::perspective(5.0, app.camera.fov_y, size.y)
        };
        let mut first_t: Option<f32> = None;
        let mut edge: Option<(f32, Picked)> = None;
        let mut face: Option<(f32, Picked)> = None;
        for hit in m.pick_all(o, d, tol, &|_| false) {
            if !app.body_visible(hit.body) || m.ctx_body(hit.body).is_some() {
                continue;
            }
            let Some(mb) = m.meshed(hit.body) else { continue };
            first_t.get_or_insert(hit.t);
            match hit.kind {
                easycad_model::pick::HitKind::Edge(e) if edge.is_none() => {
                    if let Some(info) = mb.edges.get(e as usize).filter(|i| edge_ok(i, acc)) {
                        let poly: Vec<DVec3> = mb
                            .mesh
                            .iter()
                            .flat_map(|m| m.edges.iter())
                            .filter(|pl| pl.edge == e)
                            .flat_map(|pl| pl.points.iter().map(|p| Vec3::from_array(*p).as_dvec3()))
                            .collect();
                        edge = Some((
                            hit.t,
                            edge_pick(info, Some(poly)),
                        ));
                    }
                }
                easycad_model::pick::HitKind::Face(f) if face.is_none() => {
                    if let Some(info) = mb.faces.get(f as usize).filter(|i| face_ok(i, acc)) {
                        face = Some((
                            hit.t,
                            face_pick(info, hit.body, f),
                        ));
                    }
                }
                _ => {}
            }
        }
        // an edge or face only counts when nothing unrelated is in front of it
        let slack = first_t.map(|t| t + (t * 0.01).max(0.05));
        if let Some((t, p)) = edge {
            if slack.is_none_or(|s| t <= s) {
                offer(Score(1, 4.0), p, &mut best);
            }
        }
        if let Some((t, p)) = face {
            if slack.is_none_or(|s| t <= s) {
                offer(Score(2, t), p, &mut best);
            }
        }
    }
    best.map(|b| b.1)
}

/// Turn an existing viewport selection key into a pick for `acc` (used to prefill).
pub fn from_key(m: &crate::worker::Output, key: &crate::pickstack::PickKey, acc: Accept) -> Option<Picked> {
    use crate::pickstack::PickKind;
    let mb = m.meshed(key.body)?;
    match key.kind {
        PickKind::Face(f) => {
            let info = mb.faces.get(f as usize).filter(|i| face_ok(i, acc))?;
            Some(face_pick(info, key.body, f))
        }
        PickKind::Edge(e) => {
            let info = mb.edges.get(e as usize).filter(|i| edge_ok(i, acc))?;
            Some(edge_pick(info, None))
        }
        PickKind::Body => None,
    }
}

// ---------------------------------------------------------------- viewport input

/// Hover and click while a datum dialog's slot waits for a pick. Returns true when
/// the datum picker owns the pointer (the normal selection stays out of the way).
pub fn interact(app: &mut EasyCadApp, resp: &egui::Response, rect: Rect) -> bool {
    let acc = active_accept(app);
    if acc == Accept::NONE {
        app.datum_hover = None;
        return false;
    }
    let hover = resp.hover_pos().and_then(|p| pick(app, p, rect, acc));
    app.datum_hover = hover.clone();
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let (Some(p), Some(crate::dialogs::Body::Datum(dd))) = (hover, app.dialog.as_mut().map(|d| &mut d.body)) {
            if dd.auto.is_some() {
                dd.toggle(p, resp.interact_pointer_pos());
            } else {
                dd.fill(p);
            }
            app.request_preview();
        }
    }
    true
}

// ---------------------------------------------------------------- painting

pub const AXIS_COLOR: Color32 = Color32::from_rgb(230, 200, 90);
pub const DATUM_COLOR: Color32 = Color32::from_rgb(150, 215, 190);
const HOT: Color32 = Color32::from_rgb(240, 135, 60);

fn axis_color(it: &Item) -> Color32 {
    match it.origin {
        Some(3) => Color32::from_rgb(235, 90, 90),
        Some(4) => Color32::from_rgb(110, 205, 110),
        Some(5) => Color32::from_rgb(90, 140, 240),
        _ => AXIS_COLOR,
    }
}

/// Dash-dot along a screen polyline.
fn dash_dot(p: &egui::Painter, pts: &[Pos2], col: Color32, width: f32) {
    const PATTERN: [(f32, bool); 4] = [(12.0, true), (4.0, false), (2.5, true), (4.0, false)];
    let (mut k, mut left) = (0usize, PATTERN[0].0);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        if len < 0.01 {
            continue;
        }
        let dir = (b - a) / len;
        let mut at = 0.0;
        while at < len {
            let step = left.min(len - at);
            if PATTERN[k].1 {
                p.line_segment([a + dir * at, a + dir * (at + step)], Stroke::new(width, col));
            }
            at += step;
            left -= step;
            if left <= 1e-3 {
                k = (k + 1) % PATTERN.len();
                left = PATTERN[k].0;
            }
        }
    }
}

fn screen_poly(app: &EasyCadApp, rect: Rect, a: DVec3, b: DVec3) -> Vec<Pos2> {
    // sample so a segment crossing the camera plane still draws its visible part
    let n = 32;
    (0..=n)
        .filter_map(|i| scr(app, rect, a.lerp(b, i as f64 / n as f64)))
        .collect()
}

fn paint_g(app: &EasyCadApp, p: &egui::Painter, rect: Rect, g: &G, col: Color32, width: f32) {
    match g {
        G::Axis(o, d) => {
            let (a, b) = axis_ends(app, *o, *d);
            dash_dot(p, &screen_poly(app, rect, a, b), col, width);
        }
        G::Point(w) => {
            if let Some(s) = scr(app, rect, *w) {
                p.circle_filled(s, 3.5 + width - 1.5, col);
                p.circle_stroke(s, 5.5 + width - 1.5, Stroke::new(1.0, Color32::from_black_alpha(160)));
            }
        }
        G::Poly { pts, .. } => {
            let s: Vec<Pos2> = pts.iter().filter_map(|w| scr(app, rect, *w)).collect();
            p.add(egui::Shape::line(s, Stroke::new(width + 1.0, col)));
        }
        G::Plane(pl) => {
            let h = half(app);
            let c = [DVec2::new(-h, -h), DVec2::new(h, -h), DVec2::new(h, h), DVec2::new(-h, h)];
            let s: Vec<Pos2> = c.iter().filter_map(|q| scr(app, rect, pl.to_world(*q))).collect();
            if s.len() == 4 {
                p.add(egui::Shape::convex_polygon(s, col.gamma_multiply(0.25), Stroke::new(width, col)));
            }
        }
    }
}

/// Axes, points, names and highlights painted over the 3D image.
pub fn paint(app: &EasyCadApp, p: &egui::Painter, rect: Rect) {
    let hover = app.datum_hover.as_ref().map(|h| h.r.clone());
    let picked = picked_refs(app);
    let h = half(app);
    for it in shown_items(app) {
        let hot = hover.as_ref().is_some_and(|r| same(r, &it.r)) || picked.iter().any(|r| same(r, &it.r));
        match &it.g {
            G::Plane(pl) => {
                let w = pl.to_world(DVec2::splat(h * 0.82));
                if let Some(s) = scr(app, rect, w) {
                    p.text(
                        s,
                        egui::Align2::CENTER_CENTER,
                        &it.name,
                        theme::prop(12.0),
                        if hot { Color32::WHITE } else { DATUM_COLOR },
                    );
                }
            }
            G::Axis(o, d) => {
                paint_g(app, p, rect, &it.g, if hot { HOT } else { axis_color(&it) }, if hot { 2.5 } else { 1.5 });
                let (_, b) = axis_ends(app, *o, *d);
                if let Some(s) = scr(app, rect, b) {
                    p.text(s + egui::vec2(6.0, -6.0), egui::Align2::LEFT_BOTTOM, &it.name, theme::prop(11.0), axis_color(&it));
                }
            }
            G::Point(w) => {
                paint_g(app, p, rect, &it.g, if hot { HOT } else { AXIS_COLOR }, 1.5);
                if let Some(s) = scr(app, rect, *w) {
                    p.text(s + egui::vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, &it.name, theme::prop(11.0), AXIS_COLOR);
                }
            }
            G::Poly { .. } => {}
        }
    }
    // hover candidate and picked slots (edges, vertices, sketch lines)
    if let Some(Picked { hi: Hi::Item(g), .. }) = &app.datum_hover {
        if !matches!(g, G::Plane(_)) {
            paint_g(app, p, rect, g, HOT, 3.0);
        }
    }
    if let Some(crate::dialogs::Body::Datum(dd)) = app.dialog.as_ref().map(|d| &d.body) {
        for (_, hi) in dd.held() {
            if let Hi::Item(g) = hi {
                if matches!(g, G::Poly { .. } | G::Point(_)) {
                    paint_g(app, p, rect, g, theme::ACCENT, 3.0);
                }
            }
        }
    }
}

/// The feature kinds that make datums.
pub fn is_datum(k: &FeatureKind) -> bool {
    matches!(k, FeatureKind::DatumPlane { .. } | FeatureKind::DatumAxis { .. } | FeatureKind::DatumPoint { .. })
}

/// The sketch whose plane this datum plane supplies (for the `plane_src` link).
pub fn plane_of(app: &EasyCadApp, fid: FeatureId) -> Option<Plane> {
    let m = app.model.as_deref()?;
    m.result.datums.iter().find(|(f, _)| *f == fid).and_then(|(_, g)| match g {
        DatumGeom::Plane(p) => Some(*p),
        _ => None,
    })
}

// ---------------------------------------------------------------- browser

fn eye_cell(ui: &mut egui::Ui, p: &egui::Painter, r: Rect, id: u64, vis: bool, name: &str) -> bool {
    use crate::widgets::{paint_icon, Icon};
    let eye = Rect::from_center_size(pos2(r.right() - 26.0, r.center().y), egui::vec2(28.0, 28.0));
    let er = ui.interact(eye, ui.scope_id().with(("datum_eye", id)), egui::Sense::click());
    if er.hovered() {
        p.rect_filled(eye, 6.0, theme::HOVER);
    }
    paint_icon(
        p,
        eye.shrink(6.0),
        if vis { Icon::Eye } else { Icon::EyeOff },
        if er.hovered() { theme::TEXT_STRONG } else { theme::TEXT_DIM },
    );
    er.on_hover_text(if vis { format!("Hide {name}") } else { format!("Show {name}") }).clicked()
}

/// The Origin folder's rows: XY XZ YZ planes, X Y Z axes, the origin point; eye toggles.
pub fn origin_rows(app: &mut EasyCadApp, ui: &mut egui::Ui, p: &egui::Painter) {
    for (i, (_, name)) in ORIGIN_ROWS.iter().enumerate() {
        let (r, _) = crate::ui_main::row(ui, 40.0, 26.0, false);
        let vis = app.origin_vis[i];
        p.text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            *name,
            theme::prop(13.0),
            if vis { theme::TEXT } else { theme::TEXT.gamma_multiply(0.5) },
        );
        if eye_cell(ui, p, r, i as u64, vis, name) {
            app.origin_vis[i] = !vis;
        }
    }
}

/// Datum features of the view in history order: (id, name, group, visible).
pub fn construction_list(app: &EasyCadApp) -> Vec<(FeatureId, String, Group, bool)> {
    let v = app.view();
    v.features
        .iter()
        .filter_map(|f| {
            let g = match &f.kind {
                FeatureKind::DatumPlane { .. } => Group::Plane,
                FeatureKind::DatumAxis { .. } => Group::Axis,
                FeatureKind::DatumPoint { .. } => Group::Point,
                _ => return None,
            };
            Some((f.id, f.name.clone(), g, v.datum_visible(f.id)))
        })
        .collect()
}

/// Features and sketches that refer to a datum (it cannot be deleted while any do).
pub fn users_of(d: &easycad_model::document::DocData, fid: FeatureId) -> Vec<String> {
    fn mentions(v: &serde_json::Value, id: u32) -> bool {
        match v {
            serde_json::Value::Object(m) => {
                m.get("Datum").and_then(|x| x.as_u64()) == Some(id as u64)
                    || m.values().any(|x| mentions(x, id))
            }
            serde_json::Value::Array(a) => a.iter().any(|x| mentions(x, id)),
            _ => false,
        }
    }
    let mut out: Vec<String> = d
        .features
        .iter()
        .filter(|f| f.id != fid)
        .filter(|f| serde_json::to_value(&f.kind).map(|v| mentions(&v, fid.0)).unwrap_or(false))
        .map(|f| f.name.clone())
        .collect();
    out.extend(d.sketches.iter().filter(|s| s.sketch.plane_src == Some(fid)).map(|s| s.sketch.name.clone()));
    out
}

pub fn set_visible(app: &mut EasyCadApp, fid: FeatureId, v: bool) {
    let scope = app.scope();
    let _ = crate::scope::edit(&mut app.doc, scope, if v { "Show construction" } else { "Hide construction" }, |d| {
        d.set_datum_visible(fid, v);
        Ok(())
    });
}

pub fn delete(app: &mut EasyCadApp, fid: FeatureId) {
    let users = users_of(app.view(), fid);
    if !users.is_empty() {
        app.toast(format!("Used by {}: delete or re-point those first", users.join(", ")));
        return;
    }
    let scope = app.scope();
    let r = crate::scope::edit(&mut app.doc, scope, "Delete construction", |d| {
        if let Some(i) = d.features.iter().position(|f| f.id == fid) {
            d.features.remove(i);
            if let Some(r) = d.rollback {
                if i < r {
                    d.rollback = Some(r - 1);
                }
            }
        }
        d.hidden_datums.retain(|f| *f != fid);
        Ok(())
    });
    match r {
        Ok(()) => app.request_model(),
        Err(e) => app.warn(e.to_string()),
    }
}

/// The Construction folder of the browser: one row per datum with eye, Edit, Hide/Show, Delete.
pub fn construction_rows(app: &mut EasyCadApp, ui: &mut egui::Ui, p: &egui::Painter) {
    let list = construction_list(app);
    if list.is_empty() {
        return;
    }
    let (r, _) = crate::ui_main::row(ui, 26.0, 28.0, false);
    p.text(r.left_center(), egui::Align2::LEFT_CENTER, "Construction", theme::prop(13.0), theme::TEXT_DIM);
    let (mut edit, mut toggle, mut del) = (None, None, None);
    for (fid, name, g, vis) in list {
        let (r, resp) = crate::ui_main::row(ui, 40.0, 28.0, false);
        let icon = match g {
            Group::Plane => crate::widgets::Icon::Plane,
            Group::Axis => crate::widgets::Icon::Axis,
            Group::Point => crate::widgets::Icon::Point,
        };
        crate::widgets::paint_icon(
            p,
            Rect::from_center_size(r.left_center() + egui::vec2(6.0, 0.0), egui::vec2(14.0, 14.0)),
            icon,
            AXIS_COLOR.gamma_multiply(if vis { 1.0 } else { 0.5 }),
        );
        p.text(
            r.left_center() + egui::vec2(20.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &name,
            theme::prop(13.0),
            if vis { theme::TEXT } else { theme::TEXT.gamma_multiply(0.5) },
        );
        if eye_cell(ui, p, r, 1000 + fid.0 as u64, vis, &name) {
            toggle = Some((fid, !vis));
        }
        let resp = resp.on_hover_text("Double-click to edit");
        if resp.double_clicked() && toggle.is_none() {
            edit = Some(fid);
        }
        resp.context_menu(|ui| {
            if ui.button("Edit").clicked() {
                edit = Some(fid);
                ui.close();
            }
            if ui.button(if vis { "Hide" } else { "Show" }).clicked() {
                toggle = Some((fid, !vis));
                ui.close();
            }
            if ui.button("Delete").clicked() {
                del = Some(fid);
                ui.close();
            }
        });
    }
    if let Some((f, v)) = toggle {
        set_visible(app, f, v);
    }
    if let Some(f) = edit {
        app.edit_feature(f);
    }
    if let Some(f) = del {
        app.hist_delete_request(vec![crate::hist_sel::HistItem::Feature(f)]);
    }
}
