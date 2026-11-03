//! "New enclosure from PCB": pick an EasyEDA STEP or DXF, set clearance, wall,
//! height, corner radius, joint and process, and get an ordinary editable history:
//!
//! 1. sketch "Enclosure profile": the board outline (construction, fixed) offset by
//!    `clearance + wall`, sharp corners rounded with `corner_r`;
//! 2. extrude `height` → body "Base"; 3. fillet the top edges (`edge_r`);
//! 4. closed shell `wall`; 5. the board as a locked reference (STEP) and standoffs
//!    from its holes (`standoff_h`, `boss_od`, `boss_hole`);
//! 6. a parting joint at `lid_split` → "Base" + "Lid".
//!
//! Everything is driven by named parameters, so the result edits like any model.

use std::path::{Path, PathBuf};

use casework_model::document::Document;
use casework_model::dxf::DxfDrawing;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::occt::{DVec3, Shape, SurfaceKind};
use casework_model::process::ProcessKind;
use casework_model::select::{EdgeSelector, Extremum, FaceSelector};
use casework_model::sketch::{Geom, Sketch};
use glam::DVec2;

use crate::dialogs::joint;
use crate::sketch_mode::SketchSession;

/// Mounting holes: plated M2–M3.5 sized holes.
const HOLE_MIN: f64 = 2.0;
const HOLE_MAX: f64 = 4.0;

#[derive(Clone, Debug)]
pub enum BoardKind {
    Step,
    Dxf {
        /// Layers holding the outline (the closed loop with the largest area).
        outline: String,
        /// Layers with mounting-hole circles.
        holes: Vec<String>,
    },
}

/// What the wizard read from the board file.
#[derive(Clone, Debug)]
pub struct Board {
    pub path: PathBuf,
    pub name: String,
    pub bytes: Vec<u8>,
    pub kind: BoardKind,
    /// Board size and centre (file coordinates, mm).
    pub size: DVec2,
    pub center: DVec2,
    /// Mounting holes (file coordinates) and their diameters.
    pub holes: Vec<(DVec2, f64)>,
    /// Outline polyline (file coordinates), for the preview.
    pub outline: Vec<DVec2>,
    /// STEP: bottom of the board and its thickness.
    pub bottom_z: f64,
    pub thickness: f64,
    /// STEP: overall bounding box height (board + parts).
    pub parts_top: f64,
}

impl Board {
    pub fn read(path: &Path) -> Result<Board, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "board".into());
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "dxf" => Board::from_dxf(path, name, bytes),
            "step" | "stp" => Board::from_step(path, name, bytes),
            _ => Err("pick a .step / .stp or .dxf board file".into()),
        }
    }

    fn from_dxf(path: &Path, name: String, bytes: Vec<u8>) -> Result<Board, String> {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let d = DxfDrawing::parse(&text).map_err(|e| format!("DXF: {e}"))?;
        // the outline: the layer whose largest closed loop is biggest
        let mut best: Option<(String, f64, Vec<DVec2>)> = None;
        for layer in d.layer_names() {
            let sk = d.to_sketch("probe", Plane::XY, &[layer]);
            for p in sk.profiles().profiles {
                let poly = polyline(&p.outer);
                let a = casework_model::sketch::loop_area(&p.outer).abs();
                if best.as_ref().is_none_or(|b| a > b.1) {
                    best = Some((layer.to_string(), a, poly));
                }
            }
        }
        let (outline_layer, _, outline) = best.ok_or("no closed board outline in the DXF")?;
        let (lo, hi) = bounds(&outline);
        let mut holes = Vec::new();
        let mut hole_layers = Vec::new();
        for layer in d.layer_names() {
            for (c, r) in d.circles(&[layer]) {
                let dia = 2.0 * r;
                let inside = c.x > lo.x && c.x < hi.x && c.y > lo.y && c.y < hi.y;
                if (HOLE_MIN..=HOLE_MAX).contains(&dia) && inside {
                    holes.push((c, dia));
                    if !hole_layers.iter().any(|l| l == layer) {
                        hole_layers.push(layer.to_string());
                    }
                }
            }
        }
        Ok(Board {
            path: path.to_path_buf(),
            name,
            bytes,
            kind: BoardKind::Dxf {
                outline: outline_layer,
                holes: hole_layers,
            },
            size: hi - lo,
            center: (lo + hi) / 2.0,
            holes,
            outline,
            bottom_z: 0.0,
            thickness: 1.6,
            parts_top: 1.6,
        })
    }

    fn from_step(path: &Path, name: String, bytes: Vec<u8>) -> Result<Board, String> {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let shape = Shape::read_step_str(&text).map_err(|e| format!("STEP: {}", e.0))?;
        let faces = shape.faces().map_err(|e| format!("STEP: {}", e.0))?;
        // the board: the largest horizontal planar face
        let top = faces
            .iter()
            .filter(|f| f.kind == SurfaceKind::Plane && f.normal.z.abs() > 0.99)
            .max_by(|a, b| a.area.total_cmp(&b.area))
            .ok_or("no flat board face in the STEP (is the board lying in XY?)")?;
        let bb = shape
            .face_shape(top.index)
            .and_then(|f| f.bbox())
            .map_err(|e| format!("STEP: {}", e.0))?;
        let all = shape.bbox().map_err(|e| format!("STEP: {}", e.0))?;
        let lo = DVec2::new(bb.min.x, bb.min.y);
        let hi = DVec2::new(bb.max.x, bb.max.y);
        // the opposite face of the board gives its thickness
        let z = top.point.z;
        let other = faces
            .iter()
            .filter(|f| {
                f.kind == SurfaceKind::Plane
                    && f.normal.z * top.normal.z < -0.99
                    && (f.area - top.area).abs() < 0.2 * top.area
            })
            .map(|f| f.point.z)
            .min_by(|a, b| (a - z).abs().total_cmp(&(b - z).abs()));
        let (bottom_z, thickness) = match other {
            Some(o) => (o.min(z), (o - z).abs()),
            None => (z - 1.6, 1.6),
        };
        let mut holes: Vec<(DVec2, f64)> = Vec::new();
        for f in &faces {
            if f.kind != SurfaceKind::Cylinder {
                continue;
            }
            let Some((o, axis)) = f.axis else { continue };
            let dia = 2.0 * f.radius;
            let c = DVec2::new(o.x, o.y);
            let inside = c.x > lo.x && c.x < hi.x && c.y > lo.y && c.y < hi.y;
            if axis.z.abs() > 0.99
                && (HOLE_MIN..=HOLE_MAX).contains(&dia)
                && inside
                && !holes.iter().any(|h| h.0.distance(c) < 0.05)
            {
                holes.push((c, dia));
            }
        }
        Ok(Board {
            path: path.to_path_buf(),
            name,
            bytes,
            kind: BoardKind::Step,
            size: hi - lo,
            center: (lo + hi) / 2.0,
            holes,
            outline: vec![lo, DVec2::new(hi.x, lo.y), hi, DVec2::new(lo.x, hi.y)],
            bottom_z,
            thickness,
            parts_top: all.max.z - bottom_z,
        })
    }

    pub fn source_label(&self) -> &'static str {
        match self.kind {
            BoardKind::Step => "STEP",
            BoardKind::Dxf { .. } => "DXF",
        }
    }
}

/// A loop as a polyline (arcs sampled every ~10°).
fn polyline(l: &casework_model::geom::Loop2) -> Vec<DVec2> {
    use casework_model::geom::Segment2;
    let mut out = Vec::new();
    for seg in l {
        match *seg {
            Segment2::Line { a, .. } => out.push(a),
            Segment2::Arc { center, start, end, ccw } => {
                let a0 = (start - center).to_angle();
                let mut a1 = (end - center).to_angle();
                if ccw && a1 <= a0 {
                    a1 += std::f64::consts::TAU;
                } else if !ccw && a1 >= a0 {
                    a1 -= std::f64::consts::TAU;
                }
                let r = (start - center).length();
                let n = (((a1 - a0).abs() / 10f64.to_radians()).ceil() as usize).max(1);
                for i in 0..n {
                    out.push(center + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / n as f64) * r);
                }
            }
            Segment2::Circle { center, radius } => {
                for i in 0..36 {
                    out.push(center + DVec2::from_angle(i as f64 * 10f64.to_radians()) * radius);
                }
            }
        }
    }
    out
}

fn bounds(p: &[DVec2]) -> (DVec2, DVec2) {
    p.iter().fold(
        (DVec2::splat(f64::MAX), DVec2::splat(f64::MIN)),
        |(lo, hi), q| (lo.min(*q), hi.max(*q)),
    )
}

/// The wizard's settings (expressions, so "wall + 1" works).
#[derive(Clone, Debug)]
pub struct Settings {
    pub name: String,
    pub clearance: String,
    pub wall: String,
    pub height: String,
    pub corner_r: String,
    pub standoff_h: String,
    pub joint: joint::Kind,
    /// Process profile name (one of the document's built-in profiles).
    pub profile: String,
}

impl Settings {
    pub fn new(profile: &str) -> Settings {
        Settings {
            name: "enclosure".into(),
            clearance: "0.5".into(),
            wall: "2".into(),
            height: "30".into(),
            corner_r: "6".into(),
            standoff_h: "5".into(),
            joint: joint::Kind::Tongue,
            profile: profile.into(),
        }
    }
}

fn param(doc: &mut Document, name: &str, expr: &str, comment: &str) -> Result<(), String> {
    doc.set_param(name, expr)
        .map_err(|e| format!("{name} = {expr}: {e}"))?;
    if !comment.is_empty() {
        let _ = doc.edit("Comment", |d| {
            if let Some(p) = d.params.list.iter_mut().find(|p| p.name == name) {
                p.comment = comment.into();
            }
            Ok::<_, ()>(())
        });
    }
    Ok(())
}

/// Build the document. Pure (no GUI), so it is tested directly.
pub fn build(board: &Board, s: &Settings) -> Result<Document, String> {
    let mut doc = Document::new(if s.name.trim().is_empty() { "enclosure" } else { s.name.trim() });
    if doc.data.profile_named(&s.profile).is_some() {
        doc.set_active_profile(&s.profile).map_err(|e| e.to_string())?;
    }
    let molded = doc.data.profile().kind == ProcessKind::Molded;
    let src = board.source_label();
    let step = matches!(board.kind, BoardKind::Step);
    if step {
        param(&mut doc, "pcb_w", &fmt(board.size.x), &format!("from {src}"))?;
        param(&mut doc, "pcb_h", &fmt(board.size.y), &format!("from {src}"))?;
    }
    param(&mut doc, "clearance", &s.clearance, "PCB to wall")?;
    param(&mut doc, "wall", &s.wall, "")?;
    param(&mut doc, "height", &s.height, "outside, floor to top")?;
    param(&mut doc, "corner_r", &s.corner_r, "outline corners")?;
    param(&mut doc, "edge_r", "wall + 1", "top edge round")?;
    param(&mut doc, "standoff_h", &s.standoff_h, "board above the floor")?;
    param(&mut doc, "boss_od", "7", "")?;
    param(&mut doc, "boss_hole", "2.5", "M3 self-tap")?;
    param(&mut doc, "lid_split", "height - 10", "parting plane")?;
    if molded {
        param(&mut doc, "draft", "1 deg", "molding")?;
    }
    let vals = crate::adapters::values(&doc.data);
    let ev = |e: &str| crate::widgets::eval(e, &vals);
    let wall = ev("wall")?;
    let height = ev("height")?;
    let standoff = ev("standoff_h")?;
    let board_z = wall + standoff;
    if height < 2.0 * wall + standoff + board.parts_top + 1.0 {
        return Err(format!(
            "height {height:.1} leaves no room: floor + standoffs + board{} need {:.1}",
            if step { " and parts" } else { "" },
            2.0 * wall + standoff + board.parts_top + 1.0
        ));
    }

    // 1. profile: the board outline offset by clearance + wall
    let profile = enclosure_profile(&doc, board)?;
    let sk = doc.add_sketch(profile);
    // 2. extrude
    let ext = doc.add_feature(
        "Base",
        FeatureKind::Extrude {
            sketch: sk,
            profiles: ProfileSel::All,
            distance: "height".into(),
            symmetric: false,
            reverse: false,
            draft_deg: if molded { "draft".into() } else { "0".into() },
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let base = BodyId::of(ext);
    // 3. round the top edges
    doc.add_feature(
        "Top edges",
        FeatureKind::Fillet {
            body: Some(base),
            edges: vec![EdgeSelector::OfFace(FaceSelector::Extreme {
                normal: DVec3::Z,
                which: Extremum::Max,
            })],
            radius: "edge_r".into(),
            auto_reduce: true,
        },
    );
    // 4. closed shell
    doc.add_feature(
        "Shell",
        FeatureKind::Shell {
            body: Some(base),
            open_faces: Vec::new(),
            thickness: "wall".into(),
            outward: false,
        },
    );
    // 5. the board and its standoffs
    let source = match &board.kind {
        BoardKind::Step => {
            let (imp, f) = doc.import_step(&board.name, board.bytes.clone(), true);
            // centre the board over the origin, resting on the standoffs
            let t = DVec3::new(-board.center.x, -board.center.y, board_z - board.bottom_z);
            let _ = doc.edit("Place board", |d| {
                if let Some(FeatureKind::ImportStep { translate, .. }) =
                    d.feature_mut(f).map(|f| &mut f.kind)
                {
                    *translate = t;
                }
                Ok::<_, ()>(())
            });
            PcbSource::Step {
                import: imp,
                min_dia: HOLE_MIN,
                max_dia: HOLE_MAX,
            }
        }
        BoardKind::Dxf { holes, .. } => {
            let file = board
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.dxf", board.name));
            let imp = doc.import_dxf(&file, board.bytes.clone());
            PcbSource::Dxf {
                import: imp,
                layers: holes.clone(),
                min_dia: HOLE_MIN,
                max_dia: HOLE_MAX,
                offset: -board.center,
            }
        }
    };
    if !board.holes.is_empty() {
        let so = doc.add_feature(
            "Standoffs",
            FeatureKind::StandoffsFromPcb {
                body: Some(base),
                source,
                plane: Plane::XY.offset(wall),
                od: "boss_od".into(),
                hole_dia: "boss_hole".into(),
                height: "standoff_h".into(),
                base_fillet: "0.5".into(),
                draft_deg: if molded { "draft".into() } else { "0".into() },
            },
        );
        // standoffs start on the floor, whatever the wall thickness
        crate::adapters::bind_plane_offset(&mut doc, so, "wall");
    }
    // 6. parting joint → Base + Lid
    let mut j = joint::JointD::new(&doc.data, None);
    j.kind = s.joint;
    j.body = Some(base);
    j.z = crate::dialogs::Field::new("lid_split");
    j.upper_name = "Lid".into();
    doc.add_feature(
        &format!("Joint · {}", s.joint.label()),
        j.feature_kind(),
    );
    for (k, v) in [("title", format!("{} enclosure", doc.data.name)), ("revision", "A".into())] {
        doc.data.meta.insert(k.into(), v);
    }
    Ok(doc)
}

/// The board outline (construction, fixed) and its offset by `clearance + wall`
/// with the offset's sharp corners rounded by `corner_r`.
fn enclosure_profile(doc: &Document, board: &Board) -> Result<Sketch, String> {
    use casework_model::sketch::Constraint;
    let mut sess = SketchSession::new_on(Plane::XY, &doc.data);
    sess.sketch.name = "Enclosure profile".into();
    let start = match &board.kind {
        BoardKind::Step => {
            // a rectangle sized by pcb_w × pcb_h, centred on the origin
            let r = crate::sample::centered_rect(
                "Enclosure profile",
                Plane::XY,
                "pcb_w",
                "pcb_h",
                board.size,
                DVec2::ZERO,
            );
            sess.sketch = r;
            sess.entity_layer.resize(sess.sketch.entities.len(), None);
            sess.resolve();
            sess.sketch
                .entities
                .iter()
                .position(|e| !e.construction)
                .map(|i| casework_model::sketch::EntityId(i as u32))
                .ok_or("empty board outline")?
        }
        BoardKind::Dxf { outline, .. } => {
            let text = String::from_utf8_lossy(&board.bytes).into_owned();
            let d = DxfDrawing::parse(&text).map_err(|e| format!("DXF: {e}"))?;
            let mut sk = d.to_sketch("Enclosure profile", Plane::XY, &[outline.as_str()]);
            for p in &mut sk.points {
                *p -= board.center;
            }
            sess.sketch = sk;
            sess.entity_layer.resize(sess.sketch.entities.len(), None);
            for i in 0..sess.sketch.points.len() {
                let p = sess.sketch.points[i];
                sess.sketch
                    .constrain(Constraint::Fix(casework_model::sketch::PointId(i as u32), p));
            }
            sess.resolve();
            // the outer loop runs through the left-most end point
            let s = &sess.sketch;
            let ends = |g: &Geom| match *g {
                Geom::Line { a, b } => vec![a, b],
                Geom::Arc { start, end, .. } => vec![start, end],
                Geom::Circle { .. } => Vec::new(),
            };
            let (i, _) = s
                .entities
                .iter()
                .enumerate()
                .flat_map(|(i, e)| ends(&e.geom).into_iter().map(move |p| (i, p)))
                .min_by(|a, b| s.point(a.1).x.total_cmp(&s.point(b.1).x))
                .ok_or("board outline has no lines or arcs")?;
            casework_model::sketch::EntityId(i as u32)
        }
    };
    let n_before = sess.sketch.entities.len();
    let source_points = sess.sketch.points.len();
    sess.offset_loop(start, "clearance + wall")?;
    // round the offset's sharp corners (round board corners stay concentric)
    let corners: Vec<casework_model::sketch::PointId> = {
        let s = &sess.sketch;
        let mut count = std::collections::HashMap::new();
        for e in &s.entities[n_before..] {
            if let Geom::Line { a, b } = e.geom {
                *count.entry(a).or_insert(0) += 1;
                *count.entry(b).or_insert(0) += 1;
            }
        }
        let mut c: Vec<_> = count
            .into_iter()
            .filter(|(p, n)| *n == 2 && (p.0 as usize) >= source_points)
            .map(|(p, _)| p)
            .collect();
        c.sort_by_key(|p| p.0);
        c
    };
    for p in corners {
        sess.fillet_corner(p, "corner_r")?;
    }
    for e in &mut sess.sketch.entities[..n_before] {
        e.construction = true;
    }
    sess.resolve();
    if let Some(e) = &sess.error {
        return Err(format!("enclosure profile: {e}"));
    }
    if sess.sketch.profiles().profiles.len() != 1 {
        return Err("the offset outline is not one closed profile".into());
    }
    Ok(sess.sketch)
}

fn fmt(v: f64) -> String {
    crate::dialogs::fmt((v * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, data: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn reads_the_sample_dxf_and_step() {
        let dir = std::env::temp_dir().join(format!("cw-wizard-{}", std::process::id()));
        let d = Board::read(&write(&dir, "board.dxf", &crate::sample::board_dxf())).unwrap();
        assert!((d.size - DVec2::new(86.0, 50.0)).length() < 1e-6, "{:?}", d.size);
        assert_eq!(d.holes.len(), 4);
        assert!(matches!(&d.kind, BoardKind::Dxf { outline, .. } if outline == "BoardOutLine"));
        let s = Board::read(&write(&dir, "board.step", &crate::sample::pcb_step())).unwrap();
        assert!((s.size - DVec2::new(86.0, 50.0)).length() < 0.05, "{:?}", s.size);
        assert_eq!(s.holes.len(), 4, "{:?}", s.holes);
        assert!((s.thickness - 1.6).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_a_clean_enclosure_from_dxf_and_step() {
        let dir = std::env::temp_dir().join(format!("cw-wizard-b-{}", std::process::id()));
        for (file, data) in [
            ("board.dxf", crate::sample::board_dxf()),
            ("board.step", crate::sample::pcb_step()),
        ] {
            let board = Board::read(&write(&dir, file, &data)).unwrap();
            // snap fits: see GUI_NEEDS 12d (no position placed on these boxes)
            for k in [joint::Kind::Tongue, joint::Kind::Lip, joint::Kind::ORing] {
                let s = Settings { joint: k, ..Settings::new("FDM 0.4 generic") };
                let mut doc = build(&board, &s).unwrap_or_else(|e| panic!("{file}: {e}"));
                let r = doc.regenerate().clone();
                assert!(r.errors().is_empty(), "{file} {k:?}: {:?}", r.errors());
            }
            let s = Settings::new("Injection molded (nominal)");
            let mut doc = build(&board, &s).unwrap_or_else(|e| panic!("{file}: {e}"));
            let names: Vec<_> = doc.data.features.iter().map(|f| f.kind.label()).collect();
            let r = doc.regenerate().clone();
            let statuses: Vec<_> = doc
                .data
                .features
                .iter()
                .map(|f| (f.name.clone(), r.report(f.id).map(|r| format!("{:?}", r.status))))
                .collect();
            assert!(r.errors().is_empty(), "{file}: {:?}\n{statuses:?}", r.errors());
            let solids: Vec<_> = r.solids().map(|b| b.name.clone()).collect();
            assert!(
                solids.contains(&"Base".to_string()) && solids.contains(&"Lid".to_string()),
                "{file}: {solids:?} {names:?}"
            );
            // editable: a wider clearance regenerates
            doc.set_param("clearance", "1").unwrap();
            let r = doc.regenerate().clone();
            assert!(r.errors().is_empty(), "{file} after edit: {:?}", r.errors());
            // too low is refused up front
            let mut low = Settings::new("FDM 0.4 generic");
            low.height = "8".into();
            assert!(build(&board, &low).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
