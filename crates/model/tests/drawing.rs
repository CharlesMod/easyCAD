//! 2D drawings: view extents per cube face, rotated/iso/quaternion views, HLR
//! hidden lines and source tagging, dimensions, dangling references, title block
//! and notes, balloons, parting line, SVG/DXF/PDF export, GD&T frames and datums,
//! section views, dimension text clearing, Helvetica text widths.

use easycad_model::document::Document;
use easycad_model::drawing::*;
use easycad_model::dxf::DxfDrawing;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::library::{BuiltinSource, PartSource};
use easycad_model::process::CubeFace;
use easycad_model::select::{EdgeKind, EdgeSelector, FaceSelector};
use easycad_model::sketch::Sketch;
use glam::{DQuat, DVec2, DVec3};

/// 40 × 30 × 20 block, Ø8 through hole at (20, 15), 2 mm chamfer on the top-front edge.
fn part() -> (Document, BodyId, FeatureId) {
    let mut doc = Document::new("Bracket");
    doc.set_param("w", "40").unwrap();
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0));
    let s = doc.add_sketch(s);
    let b = BodyId::of(doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let mut h = Sketch::new("H", Plane::XY);
    h.circle(DVec2::new(20.0, 15.0), 4.0);
    let h = doc.add_sketch(h);
    let hole = doc.add_feature(
        "Hole",
        FeatureKind::Extrude { sketch: h, profiles: ProfileSel::All, distance: 50.0.into(), symmetric: true, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(b) },
    );
    doc.add_feature(
        "Chamfer",
        FeatureKind::Chamfer {
            body: Some(b),
            edges: vec![EdgeSelector::Between(FaceSelector::top(), FaceSelector::AllWithNormal { normal: DVec3::NEG_Y })],
            distance: 2.0.into(),
            auto_reduce: false,
            sets: Vec::new(),
            skipped: Vec::new(),
        },
    );
    assert!(doc.regenerate().errors().is_empty());
    (doc, b, hole)
}

fn sheet() -> Sheet {
    Sheet::new(PaperSize::A3, PaperOrientation::Landscape)
}

fn extents(v: &ViewResult) -> DVec2 {
    v.max - v.min
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn cube_face_views_have_the_right_extents() {
    let (mut doc, _, _) = part();
    let d = doc.add_drawing("Views", sheet());
    let faces = [
        (CubeFace::NegY, 40.0, 20.0),
        (CubeFace::PosY, 40.0, 20.0),
        (CubeFace::PosX, 30.0, 20.0),
        (CubeFace::NegX, 30.0, 20.0),
        (CubeFace::PosZ, 40.0, 30.0),
        (CubeFace::NegZ, 40.0, 30.0),
    ];
    let ids: Vec<ViewId> = doc
        .edit_drawing(d, "views", |dr| faces.iter().enumerate().map(|(i, (f, ..))| dr.add_view(&format!("{f:?}"), ViewOrientation::Face(*f), DVec2::new(60.0 + 60.0 * i as f64, 150.0), 1.0)).collect())
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    for (id, (f, w, h)) in ids.iter().zip(faces) {
        let v = r.view(*id).unwrap();
        assert!(v.error.is_none() && !v.poly_fallback);
        let e = extents(v);
        assert!(close(e.x, w) && close(e.y, h), "{f:?}: {e:?}");
        // views are centred on their position
        let c = (v.min + v.max) / 2.0;
        let pos = doc.data.drawing(d).unwrap().views.iter().find(|x| x.id == *id).unwrap().position;
        assert!(c.distance(pos) < 1e-3, "{f:?} centre {c:?}");
    }
    // the front view sees the chamfer edge-on: the right view shows it as a 45° line
    let right = r.view(ids[2]).unwrap();
    assert!(right.edges.iter().any(|e| e.visible && e.points.len() == 2 && {
        let d = e.points[1] - e.points[0];
        (d.x.abs() - d.y.abs()).abs() < 1e-6 && (d.length() - 8f64.sqrt()).abs() < 1e-6
    }));
}

#[test]
fn rotated_scaled_iso_and_quaternion_views() {
    let (mut doc, _, _) = part();
    let d = doc.add_drawing("Rot", sheet());
    let q = DQuat::from_euler(glam::EulerRot::XYZ, 0.4, -0.7, 0.25);
    let (front, iso, quat) = doc
        .edit_drawing(d, "views", |dr| {
            let f = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 100.0), 2.0);
            dr.view_mut(f).unwrap().rotation_deg = 30.0;
            let i = dr.add_view("Iso", ViewOrientation::Iso(IsoCorner { x: 1, y: -1, z: 1 }), DVec2::new(250.0, 100.0), 1.0);
            let qv = dr.add_view("Q", ViewOrientation::Rotation(q), DVec2::new(330.0, 200.0), 1.5);
            (f, i, qv)
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let e = extents(r.view(front).unwrap());
    assert!(close(e.x, 2.0 * (40.0 * c + 20.0 * s)) && close(e.y, 2.0 * (40.0 * s + 20.0 * c)), "{e:?}");
    // iso / quaternion: HLR extents equal the projected box corners (convex part)
    for (id, o, scale) in [(iso, ViewOrientation::Iso(IsoCorner { x: 1, y: -1, z: 1 }), 1.0), (quat, ViewOrientation::Rotation(q), 1.5)] {
        let (_, x, up) = o.basis();
        let (mut lo, mut hi) = (DVec2::splat(1e9), DVec2::splat(-1e9));
        for i in 0..8 {
            let p = DVec3::new(if i & 1 == 0 { 0.0 } else { 40.0 }, if i & 2 == 0 { 0.0 } else { 30.0 }, if i & 4 == 0 { 0.0 } else { 20.0 });
            let l = DVec2::new(p.dot(x), p.dot(up));
            lo = lo.min(l);
            hi = hi.max(l);
        }
        let want = (hi - lo) * scale;
        let got = extents(r.view(id).unwrap());
        // the chamfer can only shave a corner off; allow its 2 mm
        assert!((got.x - want.x).abs() < 2.0 * scale * 1.5 && (got.y - want.y).abs() < 2.0 * scale * 1.5, "{o:?}: {got:?} vs {want:?}");
        assert!(got.x <= want.x + 1e-3 && got.y <= want.y + 1e-3);
    }
}

#[test]
fn hidden_lines_and_source_tagging() {
    let (mut doc, b, _) = part();
    let d = doc.add_drawing("HLR", sheet());
    let (front, top) = doc
        .edit_drawing(d, "views", |dr| {
            let f = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 150.0), 1.0);
            let t = dr.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(250.0, 150.0), 1.0);
            (f, t)
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let fv = r.view(front).unwrap();
    let hidden: Vec<&ViewEdge> = fv.edges.iter().filter(|e| !e.visible).collect();
    // the hole's two hidden sides (one silhouette, one coinciding with the seam edge)
    let at = |x: f64| hidden.iter().any(|e| e.points.iter().all(|p| (p.x - (100.0 - 20.0 + x)).abs() < 1e-3));
    assert!(at(16.0) && at(24.0), "{:?}", hidden.iter().map(|e| (e.kind, e.points[0])).collect::<Vec<_>>());
    let dr = doc.data.drawing(d).unwrap().clone();
    let count_hidden = |r: &DrawingResult, dr: &Drawing| primitives(r, dr).iter().filter(|p| matches!(p, Prim::Polyline { style: LineStyle::Hidden, .. })).count();
    assert_eq!(count_hidden(&r, &dr), 0, "hidden lines off");
    doc.edit_drawing(d, "hidden on", |dr| dr.view_mut(front).unwrap().hidden_lines = true).unwrap();
    let r = doc.drawing_result(d).unwrap();
    assert!(count_hidden(&r, doc.data.drawing(d).unwrap()) >= 2);

    // every sharp edge of the top view maps back to a model edge; the hole's
    // circle maps to the model's top circle
    let tv = r.view(top).unwrap();
    assert!(tv.edges.iter().filter(|e| e.visible && e.kind == HlrKindS::Sharp).all(|e| e.source.is_some()));
    let shape = doc.regenerate().body_by_id(b).unwrap().shape().clone();
    let infos = shape.edges().unwrap();
    let circle = infos.iter().find(|e| e.circle.is_some_and(|(c, _, r)| (r - 4.0).abs() < 1e-6 && (c.z - 20.0).abs() < 1e-6)).unwrap();
    assert!(tv.edges.iter().any(|e| e.visible && e.source == Some((b, circle.index))));
}

fn edge(b: BodyId, mid: DVec3, tan: DVec3) -> DimRef {
    DimRef::Edge { body: b, edge: EdgeSelector::Hint { midpoint: mid, tangent: tan, kind: Some(EdgeKind::Line) } }
}

#[test]
fn dimensions_measure_correctly_and_go_dangling() {
    let (mut doc, b, hole) = part();
    let d = doc.add_drawing("Dims", sheet());
    let circle = DimRef::Edge { body: b, edge: EdgeSelector::Circles { radius: 4.0, near: Some((DVec3::new(20.0, 15.0, 20.0), 0.1)) } };
    let ids = doc
        .edit_drawing(d, "dims", |dr| {
            let front = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(80.0, 200.0), 2.0);
            let right = dr.add_view("Right", ViewOrientation::Face(CubeFace::PosX), DVec2::new(220.0, 200.0), 2.0);
            let top = dr.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(80.0, 80.0), 2.0);
            let left = edge(b, DVec3::new(0.0, 0.0, 10.0), DVec3::Z);
            let right_e = edge(b, DVec3::new(40.0, 0.0, 9.0), DVec3::Z);
            let chamfer = edge(b, DVec3::new(40.0, 1.0, 19.0), DVec3::new(0.0, 1.0, 1.0));
            let bottom_r = edge(b, DVec3::new(40.0, 15.0, 0.0), DVec3::Y);
            vec![
                dr.annotate(AnnotationKind::Linear { view: front, a: left.clone(), b: Some(right_e), kind: LinearKind::Horizontal, offset: 10.0 }),
                dr.annotate(AnnotationKind::Linear { view: front, a: left, b: None, kind: LinearKind::Vertical, offset: -10.0 }),
                dr.annotate(AnnotationKind::Linear { view: right, a: chamfer.clone(), b: None, kind: LinearKind::Aligned, offset: 5.0 }),
                dr.annotate(AnnotationKind::Radius { view: top, circle: circle.clone(), diameter: false, leader_deg: 45.0 }),
                dr.annotate(AnnotationKind::Radius { view: top, circle: circle.clone(), diameter: true, leader_deg: 135.0 }),
                dr.annotate(AnnotationKind::Angle { view: right, a: bottom_r.clone(), b: chamfer.clone(), radius: 15.0 }),
                dr.annotate(AnnotationKind::Chamfer { view: right, edge: chamfer, offset: DVec2::new(10.0, 10.0) }),
                dr.annotate(AnnotationKind::Ordinate {
                    view: right,
                    origin: DimRef::Vertex { body: b, near: DVec3::new(40.0, 0.0, 0.0) },
                    target: DimRef::Vertex { body: b, near: DVec3::new(40.0, 30.0, 0.0) },
                    axis: SheetAxis::X,
                    offset: -8.0,
                }),
                dr.annotate(AnnotationKind::CenterMark { view: top, circle: circle.clone() }),
                dr.annotate(AnnotationKind::Radius { view: front, circle: circle.clone(), diameter: false, leader_deg: 0.0 }),
            ]
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let get = |i: usize| r.annotation(ids[i]).unwrap().clone();
    // the front-left vertical edge is 18 long: the chamfer takes 2 mm off its top
    let want = [40.0, 18.0, 8f64.sqrt(), 4.0, 8.0, 45.0, 2.0, 30.0];
    for (i, w) in want.iter().enumerate() {
        let a = get(i);
        assert_eq!(a.status, AnnotationStatus::Ok, "#{i}: {a:?}");
        assert!((a.value.unwrap() - w).abs() < 1e-6, "#{i}: {} vs {w}", a.value.unwrap());
    }
    assert_eq!(get(0).text, "40");
    assert_eq!(get(3).text, "R4");
    assert_eq!(get(4).text, "⌀8");
    assert_eq!(get(5).text, "45°");
    assert_eq!(get(6).text, "2 × 45°");
    assert_eq!(get(8).status, AnnotationStatus::Ok);
    assert!(matches!(get(9).status, AnnotationStatus::Dangling(ref w) if w.contains("face-on")), "radius in the front view is not face-on");

    // remove the hole: its dimensions go dangling but stay in the document
    doc.edit("suppress hole", |d| {
        d.feature_mut(hole).unwrap().suppressed = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    let r = doc.drawing_result(d).unwrap();
    for i in [3, 4, 8] {
        assert!(matches!(r.annotation(ids[i]).unwrap().status, AnnotationStatus::Dangling(_)), "#{i}");
    }
    assert_eq!(r.annotation(ids[0]).unwrap().status, AnnotationStatus::Ok);
    assert_eq!(doc.data.drawing(d).unwrap().annotations.len(), ids.len());
    // and come back when it returns
    doc.undo();
    let r = doc.drawing_result(d).unwrap();
    assert_eq!(r.annotation(ids[3]).unwrap().status, AnnotationStatus::Ok);
}

#[test]
fn title_block_notes_balloons_parting_line_and_exports() {
    let (mut doc, b, _) = part();
    doc.edit("meta", |d| {
        d.meta.insert("author".into(), "Ada".into());
        d.meta.insert("part_number".into(), "CW-001".into());
        Ok::<_, ()>(())
    })
    .unwrap();
    // a library screw for the BOM balloon, and a split for the parting line
    let shcs = BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
    let pr = doc.part_ref(&shcs, "M3x10").unwrap();
    let screw = doc.add_feature(
        "Screw",
        FeatureKind::InsertPart {
            part: pr,
            placements: vec![PlacementRef::Explicit { origin: DVec3::new(5.0, 5.0, 20.0), axis: DVec3::Z }],
            flip: false,
            offset: 0.0.into(),
            auto_hole: None,
            target: Some(b),
        },
    );
    let split = doc.add_feature("Split", FeatureKind::Split { body: Some(b), plane: Plane::XY.offset(12.0), new_name: "Lid".into(), offset: 0.0.into() });
    let lid = BodyId::of(split);
    let d = doc.add_drawing("Sheet 1", sheet());
    let (front, note, balloon, pl, pl_top) = doc
        .edit_drawing(d, "content", |dr| {
            let f = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 200.0), 2.0);
            dr.view_mut(f).unwrap().bodies = vec![b, lid, BodyId::of(screw)];
            let t = dr.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(250.0, 200.0), 1.0);
            let n = dr.annotate(AnnotationKind::Note { position: DVec2::new(20.0, 40.0), text: "Width {w} mm, by {meta.author}".into(), height: 3.5 });
            let bl = dr.annotate(AnnotationKind::Balloon { view: f, body: BodyId::of(screw), position: DVec2::new(60.0, 260.0) });
            let p = dr.annotate(AnnotationKind::PartingLine { view: f, feature: split });
            let pt = dr.annotate(AnnotationKind::PartingLine { view: t, feature: split });
            (f, n, bl, p, pt)
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    assert_eq!(r.annotation(note).unwrap().text, "Width 40 mm, by Ada");
    let title: std::collections::HashMap<_, _> = r.title.iter().cloned().collect();
    assert_eq!(title["Title"], "Bracket");
    assert_eq!(title["Part no."], "CW-001");
    assert_eq!(title["Scale"], "2:1");
    assert_eq!(title["Rev"], doc.data.revision.to_string());
    let bl = r.annotation(balloon).unwrap();
    assert_eq!((bl.status.clone(), bl.text.as_str()), (AnnotationStatus::Ok, "1"));
    assert_eq!(doc.bom()[0].part, "iso4762");
    // parting line: horizontal at z = 12 in the front view (centre z = 10 → +2 mm × 2)
    let p = r.annotation(pl).unwrap();
    assert_eq!(p.status, AnnotationStatus::Ok);
    let Prim::Polyline { pts, style } = &p.prims[0] else { panic!() };
    assert_eq!(*style, LineStyle::Phantom);
    // view centre: model z 0 … 23 (the M3 head tops out at 23) → 11.5
    let _ = front;
    let want_y = 200.0 + (12.0 - 11.5) * 2.0;
    assert!((pts[0].y - want_y).abs() < 1e-6 && (pts[1].y - want_y).abs() < 1e-6, "{pts:?}");
    assert!(matches!(r.annotation(pl_top).unwrap().status, AnnotationStatus::Dangling(ref w) if w.contains("edge-on")));
    // live note: a param change updates the text
    doc.set_param("w", "42").unwrap();
    assert_eq!(doc.drawing_result(d).unwrap().annotation(note).unwrap().text, "Width 42 mm, by Ada");

    // exports
    let dir = tempfile::tempdir().unwrap();
    for ext in ["svg", "dxf", "pdf"] {
        let p = dir.path().join(format!("sheet.{ext}"));
        doc.export_drawing(d, &p).unwrap();
        assert!(std::fs::metadata(&p).unwrap().len() > 500, "{ext}");
    }
    let svg = std::fs::read_to_string(dir.path().join("sheet.svg")).unwrap();
    let x = roxmltree::Document::parse(&svg).unwrap();
    assert_eq!(x.root_element().tag_name().name(), "svg");
    assert_eq!(x.root_element().attribute("width"), Some("420mm"));
    assert!(x.descendants().filter(|n| n.has_tag_name("polyline")).count() > 20);
    assert!(x.descendants().any(|n| n.has_tag_name("text") && n.text() == Some("Width 42 mm, by Ada")));
    let dxf = DxfDrawing::parse(&std::fs::read_to_string(dir.path().join("sheet.dxf")).unwrap()).unwrap();
    assert_eq!(dxf.insunits, 4);
    for l in ["VISIBLE", "DIM", "FRAME", "PARTING"] {
        assert!(dxf.layers.contains_key(l), "layer {l}: {:?}", dxf.layer_names());
    }
    assert!(dxf.layers["VISIBLE"].len() > 20);
    assert!(dxf.skipped.get("TEXT").copied().unwrap_or(0) > 5);
    let pdf = std::fs::read(dir.path().join("sheet.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    let tail = String::from_utf8_lossy(&pdf[pdf.len() - 64..]).to_string();
    assert!(tail.contains("%%EOF") && tail.contains("startxref"), "{tail}");
    let body = String::from_utf8_lossy(&pdf);
    assert!(body.contains("/Type /Page") && body.contains("/Helvetica") && body.contains("/MediaBox"));
    // the xref offset points at the xref table
    let sx: usize = tail.split("startxref").nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();
    assert!(pdf[sx..].starts_with(b"xref"));
}

fn texts(a: &AnnotationResult) -> Vec<(DVec2, String)> {
    a.prims.iter().filter_map(|p| if let Prim::Text { pos, text, .. } = p { Some((*pos, text.clone())) } else { None }).collect()
}

fn quad_of(p: &Prim) -> Option<[DVec2; 4]> {
    if let Prim::Text { pos, height, angle_deg, text, anchor } = p {
        Some(text_quad(*pos, *height, *angle_deg, text, *anchor))
    } else {
        None
    }
}

fn quads_overlap(a: &[DVec2; 4], b: &[DVec2; 4]) -> bool {
    let (alo, ahi) = (a.iter().fold(DVec2::splat(1e9), |m, p| m.min(*p)), a.iter().fold(DVec2::splat(-1e9), |m, p| m.max(*p)));
    let (blo, bhi) = (b.iter().fold(DVec2::splat(1e9), |m, p| m.min(*p)), b.iter().fold(DVec2::splat(-1e9), |m, p| m.max(*p)));
    alo.x < bhi.x && blo.x < ahi.x && alo.y < bhi.y && blo.y < ahi.y
}

#[test]
fn gdt_feature_control_frames_and_datums() {
    let (mut doc, b, _) = part();
    let d = doc.add_drawing("GD&T", sheet());
    let circle = DimRef::Edge { body: b, edge: EdgeSelector::Circles { radius: 4.0, near: Some((DVec3::new(20.0, 15.0, 20.0), 0.1)) } };
    let bottom = edge(b, DVec3::new(20.0, 0.0, 0.0), DVec3::X);
    let (datum, fcf, flat) = doc
        .edit_drawing(d, "gdt", |dr| {
            let front = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 200.0), 2.0);
            let top = dr.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(250.0, 200.0), 2.0);
            let datum = dr.annotate(AnnotationKind::Datum { view: front, target: bottom.clone(), label: "A".into(), position: DVec2::new(90.0, 165.0) });
            let fcf = dr.annotate(AnnotationKind::FeatureControl {
                view: top,
                target: Some(circle.clone()),
                position: DVec2::new(290.0, 260.0),
                symbol: GdtSymbol::Position,
                tolerance: "0.1".into(),
                diameter: true,
                modifier: Some(MaterialCondition::Maximum),
                datums: vec!["A".into(), "B".into(), "".into()],
            });
            let flat = dr.annotate(AnnotationKind::FeatureControl {
                view: front,
                target: None,
                position: DVec2::new(40.0, 120.0),
                symbol: GdtSymbol::Flatness,
                tolerance: "{w}".into(),
                diameter: false,
                modifier: None,
                datums: vec!["A".into()],
            });
            (datum, fcf, flat)
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let dt = r.annotation(datum).unwrap();
    assert_eq!((dt.status.clone(), dt.text.as_str()), (AnnotationStatus::Ok, "A"));
    // the filled triangle stands on the bottom edge (front view: z = 0 → y = 200 − 20)
    let Some(Prim::Fill { pts }) = dt.prims.iter().find(|p| matches!(p, Prim::Fill { .. })) else { panic!("{dt:?}") };
    assert!(pts.iter().filter(|p| (p.y - 180.0).abs() < 1e-6).count() == 2, "{pts:?}");
    let f = r.annotation(fcf).unwrap();
    assert_eq!(f.status, AnnotationStatus::Ok, "{:?}", f.status);
    assert_eq!(f.text, "⌖ ⌀0.1(M) | A | B");
    assert_eq!(f.value, Some(0.1));
    let t: Vec<String> = texts(f).into_iter().map(|t| t.1).collect();
    assert_eq!(t, ["⌀0.1", "M", "A", "B"], "empty datum cells are dropped");
    // the leader's arrow touches the hole's rim (top view, scale 2: r = 8)
    let c = DVec2::new(250.0, 200.0);
    let Some(Prim::Arrow { tip, .. }) = f.prims.iter().find(|p| matches!(p, Prim::Arrow { .. })) else { panic!() };
    assert!((tip.distance(c) - 8.0).abs() < 1e-6, "{tip}");
    // every text fits its cell: the frame's outline spans the cells it measured
    let Some(Prim::Polyline { pts: outline, .. }) = f.prims.iter().find(|p| matches!(p, Prim::Polyline { pts, .. } if pts.len() == 5)) else { panic!() };
    let right = outline.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    for p in &f.prims {
        if let Some(q) = quad_of(p) {
            assert!(q.iter().all(|c| c.x <= right + 0.5 && c.x >= 290.0), "{p:?} outside the frame (right {right})");
        }
    }
    // form tolerances take no datums; the tolerance substitutes params
    let fl = r.annotation(flat).unwrap();
    assert_eq!(fl.text, "⏥ 40");
    // exports: datum triangle as a filled polygon / SOLID
    let dr = doc.data.drawing(d).unwrap().clone();
    let svg = to_svg(&r, &dr);
    assert!(svg.matches("<polygon").count() >= 2);
    let dxf = DxfDrawing::parse(&to_dxf(&r, &dr)).unwrap();
    assert!(dxf.layers.contains_key("DIM"));
    assert!(to_pdf(&r, &dr).starts_with(b"%PDF-"));
}

#[test]
fn section_view_hatches_the_cut_faces() {
    let (mut doc, _, _) = part();
    let d = doc.add_drawing("Section", sheet());
    let front = doc.edit_drawing(d, "front", |dr| dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 150.0), 2.0)).unwrap();
    let r = doc.drawing_result(d).unwrap();
    let ff = r.view(front).unwrap().frame;
    // a vertical cut through the hole's axis (x = 20 → sheet x 100), placed to the right
    let sec = doc
        .edit_drawing(d, "section", |dr| dr.add_section_view(front, &ff, DVec2::new(100.0, 100.0), DVec2::new(100.0, 200.0), DVec2::new(250.0, 150.0)))
        .unwrap()
        .unwrap();
    let dr = doc.data.drawing(d).unwrap().clone();
    let sv = dr.views.iter().find(|v| v.id == sec).unwrap();
    let sc = sv.section.as_ref().unwrap();
    assert_eq!((sv.name.as_str(), sc.label.as_str()), ("Section A-A", "A"));
    assert!((sc.normal - DVec3::X).length() < 1e-9 && (sc.origin.x - 20.0).abs() < 1e-9, "{sc:?}");
    // it looks along −X like a right view: model y to the sheet's left, z up
    let (n, x, up) = sv.orientation.basis();
    assert!((n - DVec3::X).length() < 1e-9 && (x - DVec3::Y).length() < 1e-9 && (up - DVec3::Z).length() < 1e-9, "{n} {x} {up}");
    let r = doc.drawing_result(d).unwrap();
    let v = r.view(sec).unwrap();
    assert!(v.error.is_none(), "{:?}", v.error);
    let e = extents(v);
    assert!(close(e.x, 60.0) && close(e.y, 40.0), "{e}");
    // hatching fills the cut face, not the hole (8 wide at the view's centre)
    assert!(v.hatch.len() > 10, "{}", v.hatch.len());
    let c = v.frame.sheet(DVec3::new(20.0, 15.0, 10.0));
    let across = v.frame.sheet_dir(DVec3::Y).normalize();
    for h in &v.hatch {
        let mid = (h[0] + h[1]) / 2.0;
        assert!(((mid - c).dot(across)).abs() > 7.99, "hatch inside the hole: {h:?}");
        assert!(mid.x > v.min.x - 1e-6 && mid.x < v.max.x + 1e-6 && mid.y > v.min.y - 1e-6 && mid.y < v.max.y + 1e-6);
    }
    // the cutting line on the front view: chain line at x = 100 past the view, arrows pointing left
    let Some(Prim::Polyline { pts, .. }) = v.marks.iter().find(|p| matches!(p, Prim::Polyline { style: LineStyle::Center, .. })) else { panic!() };
    assert!(pts.iter().all(|p| (p.x - 100.0).abs() < 1e-6));
    let fv = r.view(front).unwrap();
    assert!(pts.iter().map(|p| p.y).fold(f64::MIN, f64::max) > fv.max.y && pts.iter().map(|p| p.y).fold(f64::MAX, f64::min) < fv.min.y);
    let arrows: Vec<DVec2> = v.marks.iter().filter_map(|p| if let Prim::Arrow { dir, .. } = p { Some(*dir) } else { None }).collect();
    assert_eq!(arrows.len(), 2);
    assert!(arrows.iter().all(|d| (*d - DVec2::NEG_X).length() < 1e-9), "{arrows:?}");
    let caps: Vec<&String> = v.marks.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text) } else { None }).collect();
    assert_eq!(caps, ["A", "A", "SECTION A-A"]);
    // exports carry the hatch on its own layer
    let dr = doc.data.drawing(d).unwrap().clone();
    let dxf = DxfDrawing::parse(&to_dxf(&r, &dr)).unwrap();
    assert!(dxf.layers.get("HATCH").is_some_and(|l| l.len() > 10), "{:?}", dxf.layer_names());
    assert!(to_svg(&r, &dr).contains("SECTION A-A"));
    // turning the parent so it no longer sees the plane edge-on is reported
    doc.edit_drawing(d, "turn", |dr| dr.view_mut(front).unwrap().orientation = ViewOrientation::Face(CubeFace::PosX)).unwrap();
    let r = doc.drawing_result(d).unwrap();
    assert!(r.view(sec).unwrap().error.as_deref().unwrap_or("").contains("edge-on"));
}

#[test]
fn dimension_text_moves_off_other_text_and_geometry() {
    let (mut doc, b, _) = part();
    let d = doc.add_drawing("Clutter", sheet());
    let bottom = edge(b, DVec3::new(20.0, 0.0, 0.0), DVec3::X);
    let ids = doc
        .edit_drawing(d, "dims", |dr| {
            let front = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 150.0), 1.0);
            vec![
                dr.annotate(AnnotationKind::Linear { view: front, a: bottom.clone(), b: None, kind: LinearKind::Horizontal, offset: -10.0 }),
                // the same dimension again: its text would sit on the first one's
                dr.annotate(AnnotationKind::Linear { view: front, a: bottom.clone(), b: None, kind: LinearKind::Horizontal, offset: -10.0 }),
                // a dimension line just under the top edge: the text would cross it
                dr.annotate(AnnotationKind::Linear { view: front, a: bottom.clone(), b: None, kind: LinearKind::Horizontal, offset: 18.5 }),
            ]
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let a = |i: usize| r.annotation(ids[i]).unwrap();
    let q = |i: usize| a(i).prims.iter().find_map(quad_of).unwrap();
    assert_eq!(texts(a(0))[0].0, DVec2::new(100.0, 140.0 - 10.0 + 1.5), "a free text stays put");
    assert!(!quads_overlap(&q(0), &q(1)), "{:?} / {:?}", q(0), q(1));
    assert!(texts(a(1))[0].0.y == texts(a(0))[0].0.y, "moved along its line, not off it");
    // moved within the 40 mm line: nothing to extend
    assert!(texts(a(1))[0].0.x != texts(a(0))[0].0.x);
    assert_eq!(a(1).prims.len(), a(0).prims.len());
    // the third clears every drawn edge of the view
    let dr = doc.data.drawing(d).unwrap().clone();
    let view_lines: Vec<Vec<DVec2>> = r.views[0].edges.iter().filter(|e| e.visible).map(|e| e.points.clone()).collect();
    let q2 = q(2);
    let (lo, hi) = (q2.iter().fold(DVec2::splat(1e9), |m, p| m.min(*p)), q2.iter().fold(DVec2::splat(-1e9), |m, p| m.max(*p)));
    for l in &view_lines {
        for w in l.windows(2) {
            for k in 0..=20 {
                let p = w[0] + (w[1] - w[0]) * (k as f64 / 20.0);
                assert!(!(p.x > lo.x && p.x < hi.x && p.y > lo.y && p.y < hi.y), "text box {lo}..{hi} crosses an edge at {p}");
            }
        }
    }
    assert_eq!(a(2).text, "40");
    // the top edge spans the whole line, so the text went past its end: extended
    assert_eq!(a(2).prims.len(), a(0).prims.len() + 1, "{:?}", texts(a(2)));
    let _ = dr;
}

#[test]
fn text_widths_use_helvetica_metrics() {
    // AFM: '4' and '0' are 556, 'W' 944, 'i' 222 (1/1000 em; em = 1.4 × cap height)
    assert!((text_width("40", 2.5) - 2.0 * 0.556 * 3.5).abs() < 1e-9);
    assert!((text_width("W", 1.0) - 0.944 * 1.4).abs() < 1e-9);
    assert!(text_width("WWW", 3.5) > 4.0 * text_width("iii", 3.5));
    assert!((text_width("⌀", 1.0) - 0.778 * 1.4).abs() < 1e-9);
    // the PDF centres a dimension's text with those widths
    let (mut doc, b, _) = part();
    let d = doc.add_drawing("Widths", sheet());
    let dim = doc
        .edit_drawing(d, "dim", |dr| {
            let front = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 150.0), 1.0);
            dr.annotate(AnnotationKind::Linear { view: front, a: edge(b, DVec3::new(20.0, 0.0, 0.0), DVec3::X), b: None, kind: LinearKind::Horizontal, offset: -10.0 })
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let (pos, t) = texts(r.annotation(dim).unwrap())[0].clone();
    let pdf = String::from_utf8_lossy(&to_pdf(&r, doc.data.drawing(d).unwrap())).to_string();
    let k = 72.0 / 25.4;
    let want = (pos.x - text_width(&t, 3.5) / 2.0) * k;
    let lines: Vec<&str> = pdf.lines().collect();
    let i = lines.iter().position(|l| l.contains(&format!("({t}) Tj"))).expect("text shown");
    let tm = lines[..i].iter().rev().find(|l| l.ends_with("Tm")).unwrap();
    let e: f64 = tm.split_whitespace().nth(4).unwrap().parse().unwrap();
    assert!((e - want).abs() < 0.01, "{tm} vs {want}");
}

/// Two block components (one placed twice), a section through the assembly and a
/// BOM table whose columns fit their text.
#[test]
fn assembly_section_and_bom_table_fit() {
    use easycad_model::assembly::{ComponentId, Scope, Transform};
    let mut doc = Document::new("Stack");
    let mk = |doc: &mut Document, name: &str, w: f64, h: f64| {
        let c = doc.add_component(name);
        let mut s = Sketch::new(name, Plane::XY);
        s.rect(DVec2::ZERO, DVec2::new(w, 20.0));
        let sk = doc.add_sketch_in(c, s).unwrap();
        doc.add_feature_in(c, name, FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: h.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None }).unwrap();
        c
    };
    let base = mk(&mut doc, "Base plate with a long descriptive name", 60.0, 5.0);
    let block = mk(&mut doc, "Block", 20.0, 10.0);
    let b0 = doc.add_occurrence(ComponentId::ROOT, base, "Base:1", Transform::IDENTITY).unwrap();
    doc.add_occurrence(ComponentId::ROOT, block, "Block:1", Transform::translation(DVec3::new(5.0, 0.0, 5.0))).unwrap();
    doc.add_occurrence(ComponentId::ROOT, block, "Block:2", Transform::translation(DVec3::new(35.0, 0.0, 5.0))).unwrap();
    let d = doc.add_drawing("Asm", sheet());
    let (front, table, bal) = doc
        .edit_drawing(d, "views", |dr| {
            let f = dr.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(100.0, 200.0), 1.0);
            dr.view_mut(f).unwrap().scope = Scope::Assembly;
            let t = dr.annotate(AnnotationKind::BomTable { position: DVec2::new(30.0, 90.0), indented: false });
            let bl = dr.annotate(AnnotationKind::ItemBalloon { view: f, path: vec![b0], body: None, position: DVec2::new(60.0, 240.0) });
            (f, t, bl)
        })
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let ff = r.view(front).unwrap().frame;
    // a horizontal cut through both layers (z = 7.5 → sheet y 200), looking down at it
    let sec = doc
        .edit_drawing(d, "section", |dr| dr.add_section_view(front, &ff, DVec2::new(50.0, 200.0), DVec2::new(150.0, 200.0), DVec2::new(100.0, 270.0)))
        .unwrap()
        .unwrap();
    let r = doc.drawing_result(d).unwrap();
    let v = r.view(sec).unwrap();
    assert!(v.error.is_none(), "{:?}", v.error);
    // looking down at the two blocks' cut faces; the base plate lies wholly behind the plane (kept, not cut)
    let (n, ..) = doc.data.drawing(d).unwrap().views.iter().find(|x| x.id == sec).unwrap().orientation.basis();
    assert!((n - DVec3::Z).length() < 1e-9, "{n}");
    let dirs: std::collections::BTreeSet<i64> = v.hatch.iter().map(|h| { let d = h[1] - h[0]; (d.y.atan2(d.x).to_degrees().rem_euclid(180.0)).round() as i64 }).collect();
    assert_eq!(dirs.into_iter().collect::<Vec<_>>(), [45, 135], "parts hatched in alternating directions");
    assert_eq!(r.annotation(bal).unwrap().text, "1");
    let t = r.annotation(table).unwrap();
    assert_eq!(t.status, AnnotationStatus::Ok, "{:?}", t.status);
    let mut cols: Vec<f64> = t.prims.iter().filter_map(|p| if let Prim::Polyline { pts, .. } = p { (pts.len() == 2 && pts[0].x == pts[1].x).then_some(pts[0].x) } else { None }).collect();
    let Some(Prim::Polyline { pts: outline, .. }) = t.prims.first() else { panic!() };
    cols.push(outline.iter().map(|p| p.x).fold(f64::MIN, f64::max));
    cols.sort_by(f64::total_cmp);
    for p in &t.prims {
        if let Prim::Text { pos, height, text, .. } = p {
            let right = cols.iter().copied().find(|x| *x > pos.x + 1e-9).unwrap();
            assert!(pos.x + text_width(text, *height) < right, "`{text}` overflows its column ({} > {right})", pos.x + text_width(text, *height));
        }
    }
    assert!(texts(t).iter().any(|(_, s)| s == "Base plate with a long descriptive name"));
}
