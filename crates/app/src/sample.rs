//! The "sensor-node" test enclosure from the mock, built through the model API:
//! a PCB-driven rounded box (base + lid), standoffs from an imported STEP board,
//! ribs, a USB-C cutout and the board outline as a DXF sketch.

use casework_model::document::Document;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::occt::{DVec3, Shape};
use casework_model::select::EdgeSelector;
use casework_model::sketch::{Constraint, Geom, Sketch};
use glam::DVec2;

pub const PCB_W: f64 = 86.0;
pub const PCB_H: f64 = 50.0;
pub const PCB_Z: f64 = 20.0;
pub const HOLE_INSET: f64 = 3.5;

/// Rectangle centred on the origin, sized by expressions, fully constrained.
pub fn centered_rect(
    name: &str,
    plane: Plane,
    w: &str,
    h: &str,
    guess: DVec2,
    center: DVec2,
) -> Sketch {
    let mut s = Sketch::new(name, plane);
    let l = s.rect(center - guess / 2.0, center + guess / 2.0);
    let corner = |s: &Sketch, e: usize| match s.entities[l[e].0 as usize].geom {
        Geom::Line { a, .. } => a,
        _ => unreachable!(),
    };
    let (p0, p2) = (corner(&s, 0), corner(&s, 2));
    s.constrain(Constraint::Length(l[0], w.into()));
    s.constrain(Constraint::Length(l[1], h.into()));
    let c = s.add_point(center);
    let diag = s.add_entity(Geom::Line { a: p0, b: p2 }, true);
    s.constrain(Constraint::Midpoint(c, diag));
    s.constrain(Constraint::Fix(c, center));
    s
}

/// The board as an EasyEDA-style DXF (mm): rounded outline plus 4 mounting holes.
pub fn board_dxf() -> String {
    let (w, h, r) = (PCB_W, PCB_H, 3.0);
    let b = (22.5f64).to_radians().tan();
    let verts = [
        (r, 0.0, 0.0),
        (w - r, 0.0, b),
        (w, r, 0.0),
        (w, h - r, b),
        (w - r, h, 0.0),
        (r, h, b),
        (0.0, h - r, 0.0),
        (0.0, r, b),
    ];
    let mut s = String::from("  0\nSECTION\n  2\nHEADER\n  9\n$INSUNITS\n 70\n     4\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n");
    s += "  0\nLWPOLYLINE\n  8\nBoardOutLine\n 90\n        8\n 70\n     1\n";
    for (x, y, bulge) in verts {
        s += &format!(" 10\n{x}\n 20\n{y}\n");
        if bulge != 0.0 {
            s += &format!(" 42\n{bulge}\n");
        }
    }
    for (x, y) in hole_positions_board() {
        s += &format!(
            "  0\nCIRCLE\n  8\nMulti-Layer holes\n 10\n{x}\n 20\n{y}\n 30\n0.0\n 40\n1.6\n"
        );
    }
    for (x0, y0, x1, y1) in [
        (40.0, 20.0, 52.0, 20.0),
        (52.0, 20.0, 52.0, 32.0),
        (52.0, 32.0, 40.0, 32.0),
        (40.0, 32.0, 40.0, 20.0),
    ] {
        s += &format!("  0\nLINE\n  8\nTopSilkLayer\n 10\n{x0}\n 20\n{y0}\n 11\n{x1}\n 21\n{y1}\n");
    }
    s += "  0\nLINE\n  8\nDocument\n 10\n0\n 20\n-5\n 11\n86\n 21\n-5\n";
    s += "  0\nENDSEC\n  0\nEOF\n";
    s
}

/// Mounting holes in board coordinates (origin at the board corner).
pub fn hole_positions_board() -> [(f64, f64); 4] {
    let i = HOLE_INSET;
    [
        (i, i),
        (PCB_W - i, i),
        (PCB_W - i, PCB_H - i),
        (i, PCB_H - i),
    ]
}

/// A fake PCB as STEP text: board with 4 Ø3.2 holes at z = 20, a USB-C
/// receptacle at the front edge and a chip.
pub fn pcb_step() -> String {
    let (w, h) = (PCB_W, PCB_H);
    // rounded outline + holes straight from the DXF, so STEP and DXF agree
    let d = casework_model::dxf::DxfDrawing::parse(&board_dxf()).unwrap();
    let mut sk = d.to_sketch(
        "board",
        Plane::XY.offset(PCB_Z),
        &["BoardOutLine", "Multi-Layer holes"],
    );
    for p in &mut sk.points {
        *p -= DVec2::new(w / 2.0, h / 2.0);
    }
    let pr = sk.profiles().profiles.remove(0);
    let board = casework_model::regen::profile_face(&sk.plane, &pr)
        .unwrap()
        .extrude(DVec3::new(0.0, 0.0, 1.6))
        .unwrap();
    let usb = Shape::make_box(
        DVec3::new(-4.5, -h / 2.0 - 0.2, PCB_Z + 1.6),
        DVec3::new(9.0, 7.5, 3.2),
    )
    .unwrap();
    let chip = Shape::make_box(
        DVec3::new(-6.0, -2.0, PCB_Z + 1.6),
        DVec3::new(12.0, 12.0, 1.2),
    )
    .unwrap();
    Shape::compound(&[&board, &usb, &chip])
        .unwrap()
        .to_step_string()
        .unwrap()
}

fn set_comment(doc: &mut Document, name: &str, comment: &str) {
    let _ = doc.edit("Comment", |d| {
        if let Some(p) = d.params.list.iter_mut().find(|p| p.name == name) {
            p.comment = comment.into();
        }
        Ok::<_, ()>(())
    });
}

/// Build the sensor-node enclosure. Returns the document with a clean undo stack.
pub fn sensor_node() -> Document {
    let mut doc = Document::new("sensor-node");
    for (n, v, c) in [
        ("pcb_w", "86", "from DXF"),
        ("pcb_h", "50", "from DXF"),
        ("clearance", "0.3", "PCB to wall"),
        ("wall", "2", ""),
        ("height", "32", ""),
        ("lid_split", "height - 4", ""),
        ("boss_od", "7", ""),
        ("boss_hole", "2.5", "M3 self-tap"),
        ("draft", "1 deg", "molding"),
    ] {
        doc.set_param(n, v).expect("param");
        if !c.is_empty() {
            set_comment(&mut doc, n, c);
        }
    }

    // Board outline sketch from the DXF (reference geometry for the profile)
    let dxf = board_dxf();
    let imp = doc.import_dxf("sensor-node_outline.dxf", dxf.clone().into_bytes());
    let drawing = casework_model::dxf::DxfDrawing::parse(&dxf).expect("dxf");
    let mut board = drawing.to_sketch(
        "Board outline",
        Plane::XY,
        &["BoardOutLine", "Multi-Layer holes"],
    );
    // centre the board on the origin
    for p in &mut board.points {
        *p -= DVec2::new(PCB_W / 2.0, PCB_H / 2.0);
    }
    for e in &mut board.entities {
        e.construction = true;
    }
    doc.add_sketch(board);
    let _ = imp;

    // Enclosure profile: PCB + clearance + wall on every side
    let profile = centered_rect(
        "Enclosure profile",
        Plane::XY,
        "pcb_w + 2*(clearance + wall)",
        "pcb_h + 2*(clearance + wall)",
        DVec2::new(90.0, 54.0),
        DVec2::ZERO,
    );
    let sk = doc.add_sketch(profile);
    let body = doc.add_feature(
        "Extrude",
        FeatureKind::Extrude {
            sketch: sk,
            profiles: ProfileSel::All,
            distance: "height".into(),
            symmetric: false,
            reverse: false,
            draft_deg: "0".into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let base = BodyId::of(body);
    doc.edit("Rename body", |d| {
        if let Some(f) = d.feature_mut(body) {
            f.name = "Base".into();
        }
        Ok::<_, ()>(())
    })
    .ok();
    doc.add_feature(
        "Fillet",
        FeatureKind::Fillet {
            body: None,
            edges: vec![EdgeSelector::Parallel { dir: DVec3::Z }],
            radius: "clearance + wall + 3".into(),
            auto_reduce: false,
        },
    );
    doc.add_feature(
        "Shell",
        FeatureKind::Shell {
            body: None,
            open_faces: vec![casework_model::select::FaceSelector::bottom()],
            thickness: "wall".into(),
            outward: false,
        },
    );
    // close the bottom again with a floor plate (a closed shell is not supported well)
    let mut floor = Sketch::new("Floor", Plane::XY);
    floor.rounded_rect(
        DVec2::ZERO,
        "pcb_w + 2*(clearance + wall)",
        "pcb_h + 2*(clearance + wall)",
        "clearance + wall + 3",
        (90.6, 54.6, 5.3),
    );
    let floor = doc.add_sketch(floor);
    doc.add_feature(
        "Floor",
        FeatureKind::Extrude {
            sketch: floor,
            profiles: ProfileSel::All,
            distance: "wall".into(),
            symmetric: false,
            reverse: false,
            draft_deg: "0".into(),
            op: BoolOp::Join,
            target: Some(base),
        },
    );

    // USB-C cutout through the front wall
    let front_y = -(PCB_H / 2.0 + 0.3 + 2.0);
    let plane = Plane::new(DVec3::new(0.0, front_y, 0.0), DVec3::X, DVec3::NEG_Y);
    let usb = centered_rect(
        "USB-C cutout",
        plane,
        "9",
        "3.5",
        DVec2::new(9.0, 3.5),
        DVec2::new(0.0, PCB_Z + 1.6 + 1.6),
    );
    let usb = doc.add_sketch(usb);
    doc.add_feature(
        "USB-C cutout",
        FeatureKind::Extrude {
            sketch: usb,
            profiles: ProfileSel::All,
            distance: "3*wall".into(),
            symmetric: true,
            reverse: false,
            draft_deg: "0".into(),
            op: BoolOp::Cut,
            target: Some(base),
        },
    );

    let split = doc.add_feature(
        "Split",
        FeatureKind::Split {
            body: Some(base),
            plane: Plane::XY.offset(28.0),
            new_name: "Lid".into(),
        },
    );
    crate::adapters::bind_plane_offset(&mut doc, split, "lid_split");

    let (_, pcb_f) = doc.import_step("sensor-node", pcb_step().into_bytes(), true);
    let _ = pcb_f;
    let pcb_imp = doc
        .data
        .imports
        .iter()
        .rev()
        .find(|i| i.import.name == "sensor-node")
        .map(|i| i.id)
        .unwrap();
    doc.add_feature(
        "Standoffs",
        FeatureKind::StandoffsFromPcb {
            body: Some(base),
            source: PcbSource::Step {
                import: pcb_imp,
                min_dia: 2.5,
                max_dia: 4.0,
            },
            plane: Plane::XY.offset(2.0),
            od: "boss_od".into(),
            hole_dia: "boss_hole".into(),
            height: "18".into(),
            base_fillet: "0.8".into(),
            draft_deg: "draft".into(),
        },
    );

    let mut rib = Sketch::new("Rib", Plane::XY.offset(2.0));
    rib.line(DVec2::new(-25.0, 12.0), DVec2::new(25.0, 12.0));
    let rib = doc.add_sketch(rib);
    let rib_f = doc.add_feature(
        "Rib",
        FeatureKind::Rib {
            sketch: rib,
            thickness: "1.5".into(),
            height: "8".into(),
            draft_deg: "0".into(),
            flip: false,
            body: Some(base),
            extent: Default::default(),
        },
    );
    doc.add_feature(
        "Mirror rib",
        FeatureKind::Mirror {
            source: PatternSource::Features(vec![rib_f]),
            plane: Plane::XZ,
        },
    );
    for (k, v) in [
        ("title", "sensor-node enclosure"),
        ("revision", "A"),
        ("material", "PC/ABS, black"),
        ("author", "HW"),
        ("part_number", "SN-ENC-001"),
    ] {
        doc.data.meta.insert(k.into(), v.into());
    }
    doc
}
