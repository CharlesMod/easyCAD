//! `render_sketch`: a sketch drawn flat in its own plane for Claude, on the CPU.
//! Constrained geometry in white, what can still move in blue, construction
//! dashed, dimensions in amber with their expressions, closed regions shaded and
//! numbered, a millimetre grid and the sketch axes. The text reply carries the
//! same facts as numbers (free entities with coordinates, regions, dimensions,
//! the plane mapping, mm per pixel).

use easycad_mcp::{self as proto, json_text};
use easycad_model::feature::SketchId;
use easycad_model::geom::Segment2;
use easycad_model::params::Values;
use easycad_model::sketch::{
    point_in_polygon, polygonize, segment_point, signed_area, Constraint, ConstraintStatus, EntityId, FreeGeometry, Geom,
    PointId, Sketch,
};
use glam::DVec2;
use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use crate::app::{EasyCadApp, Mode};
use crate::mcp::{draw_caption, draw_text, plane_json, r, thick_line, v3};

const BG: [u8; 3] = [22, 24, 28];
const GRID_MINOR: [u8; 3] = [32, 35, 40];
const GRID_MAJOR: [u8; 3] = [46, 50, 57];
const GRID_TEXT: [u8; 3] = [110, 116, 125];
const AXIS_X: [u8; 3] = [150, 64, 64];
const AXIS_Y: [u8; 3] = [64, 140, 76];
const REGION: [u8; 3] = [36, 60, 92];
const REGION_TEXT: [u8; 3] = [140, 180, 230];
const FIXED: [u8; 3] = [226, 229, 233];
const FREE: [u8; 3] = [80, 160, 255];
const CONSTRUCTION: [u8; 3] = [125, 128, 134];
const DIM: [u8; 3] = [236, 178, 64];
const LABEL_BG: [u8; 3] = [18, 20, 24];

/// Which sketch, and the values its dimensions use.
struct Source {
    sketch: Sketch,
    id: Option<SketchId>,
    values: Values,
    draft: bool,
}

fn source(app: &EasyCadApp, args: &Value) -> Result<Source, String> {
    let open = match &app.mode {
        Mode::Sketch(s) => Some(s),
        _ => None,
    };
    let saved = args.get("saved").and_then(Value::as_bool).unwrap_or(false);
    let list = || {
        let v: Vec<String> = app.doc.data.sketches.iter().map(|e| format!("{} '{}'", e.id.0, e.sketch.name)).collect();
        if v.is_empty() { "none".to_string() } else { v.join(", ") }
    };
    let doc_values = || app.doc.data.values().map_err(|e| format!("the parameters do not evaluate: {e}"));
    match args.get("id").and_then(Value::as_u64) {
        Some(n) => {
            let id = SketchId(n as u32);
            if let Some(s) = open.filter(|s| s.id == Some(id) && !saved) {
                return Ok(Source { sketch: s.sketch.clone(), id: Some(id), values: s.values.clone(), draft: true });
            }
            let e = app.doc.data.sketch(id).ok_or_else(|| format!("no sketch with id {n} (sketches: {})", list()))?;
            Ok(Source { sketch: e.clone(), id: Some(id), values: doc_values()?, draft: false })
        }
        None => match open {
            Some(s) => Ok(Source { sketch: s.sketch.clone(), id: s.id, values: s.values.clone(), draft: true }),
            None => Err(format!(
                "give the sketch `id` (the user has no sketch open). Sketches: {}",
                list()
            )),
        },
    }
}

/// An entity as a 2D segment (construction included).
fn segment(sk: &Sketch, g: &Geom) -> Segment2 {
    match *g {
        Geom::Line { a, b } => Segment2::Line { a: sk.point(a), b: sk.point(b) },
        Geom::Arc { center, start, end } => {
            Segment2::Arc { center: sk.point(center), start: sk.point(start), end: sk.point(end), ccw: true }
        }
        Geom::Circle { center, radius } => Segment2::Circle { center: sk.point(center), radius },
    }
}

fn samples(s: &Segment2) -> Vec<DVec2> {
    let n = match s {
        Segment2::Line { .. } => 1,
        _ => 96,
    };
    (0..=n).map(|k| segment_point(s, k as f64 / n as f64)).collect()
}

/// 1, 2 or 5 × 10^k, at least `v`.
fn nice(v: f64) -> f64 {
    let e = 10f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * e >= v {
            return m * e;
        }
    }
    10.0 * e
}

fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Sketch coordinates → pixels.
struct View {
    min: DVec2,
    k: f64,
    margin: f64,
    h: u32,
}

impl View {
    fn px(&self, p: DVec2) -> (f32, f32) {
        let x = (p.x - self.min.x) * self.k + self.margin;
        let y = self.h as f64 - ((p.y - self.min.y) * self.k + self.margin);
        (x as f32, y as f32)
    }
    fn mm(&self, x: f64, y: f64) -> DVec2 {
        DVec2::new((x - self.margin) / self.k + self.min.x, (self.h as f64 - y - self.margin) / self.k + self.min.y)
    }
}

fn blend(img: &mut RgbaImage, x: u32, y: u32, c: [u8; 3], a: f32) {
    let p = img.get_pixel_mut(x, y);
    for (v, c) in p.0.iter_mut().zip(c) {
        *v = (*v as f32 * (1.0 - a) + c as f32 * a) as u8;
    }
}

fn polyline(img: &mut RgbaImage, pts: &[(f32, f32)], w: f32, c: [u8; 3], dashed: bool) {
    let mut along = 0.0f32;
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        if !dashed {
            thick_line(img, a, b, w, c);
        } else {
            // 7 px on, 5 off, continuing along the polyline
            let mut t = 0.0;
            while t < len {
                let phase = (along + t) % 12.0;
                let run = if phase < 7.0 { 7.0 - phase } else { 12.0 - phase };
                let t1 = (t + run).min(len);
                if phase < 7.0 {
                    let p = |u: f32| (a.0 + (b.0 - a.0) * u / len.max(1e-6), a.1 + (b.1 - a.1) * u / len.max(1e-6));
                    thick_line(img, p(t), p(t1), w, c);
                }
                t = t1 + 1e-3;
            }
        }
        along += len;
    }
}

fn dot(img: &mut RgbaImage, p: (f32, f32), r: f32, c: [u8; 3]) {
    thick_line(img, p, p, r, c);
}

pub fn render_sketch(app: &mut EasyCadApp, args: &Value) -> Result<Vec<Value>, String> {
    let src = source(app, args)?;
    let sk = &src.sketch;
    let max_w = args.get("max_width").and_then(Value::as_u64).unwrap_or(900).clamp(240, 1600) as u32;
    let labels = args.get("labels").and_then(Value::as_str).unwrap_or("entities");
    let report = sk.analyze(&src.values).map_err(|e| format!("the sketch does not evaluate: {e}"))?;
    let free = sk.free_geometry(&src.values).ok().flatten();
    let is_free_e = |i: usize| free.as_ref().is_some_and(|f: &FreeGeometry| f.entities[i]);
    let is_free_p = |i: usize| free.as_ref().is_some_and(|f: &FreeGeometry| f.points[i]);
    let segs: Vec<Segment2> = sk.entities.iter().map(|e| segment(sk, &e.geom)).collect();
    let pts: Vec<Vec<DVec2>> = segs.iter().map(samples).collect();

    // ---- frame: the geometry, plus the origin when it is near
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for p in pts.iter().flatten().chain(sk.points.iter()) {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    if !lo.is_finite() {
        lo = DVec2::splat(-10.0);
        hi = DVec2::splat(10.0);
    }
    let span = (hi - lo).max_element().max(1e-3);
    let d0 = DVec2::ZERO.clamp(lo, hi).distance(DVec2::ZERO);
    if d0 <= span {
        lo = lo.min(DVec2::ZERO);
        hi = hi.max(DVec2::ZERO);
    }
    let size = (hi - lo).max(DVec2::splat(span * 0.2));
    let c = (lo + hi) / 2.0;
    let (lo, hi) = (c - size / 2.0 * 1.08, c + size / 2.0 * 1.08);
    let margin = 56.0;
    let max_h = (max_w as f64 * 1.25).max(240.0);
    let k = ((max_w as f64 - 2.0 * margin) / (hi.x - lo.x)).min((max_h - 2.0 * margin) / (hi.y - lo.y));
    let w = ((hi.x - lo.x) * k + 2.0 * margin).ceil() as u32;
    let h = ((hi.y - lo.y) * k + 2.0 * margin + 30.0).ceil() as u32;
    let view = View { min: lo, k, margin, h: h - 30 };
    let mut img = RgbaImage::from_pixel(w, h, Rgba([BG[0], BG[1], BG[2], 255]));

    // ---- grid with millimetre labels, then the axes
    let step = nice(48.0 / k);
    let (a, b) = (view.mm(0.0, h as f64), view.mm(w as f64, 0.0));
    let line_v = |img: &mut RgbaImage, x: f32, c: [u8; 3]| {
        let xi = x.round() as i64;
        if xi >= 0 && xi < w as i64 {
            for y in 0..h {
                img.put_pixel(xi as u32, y, Rgba([c[0], c[1], c[2], 255]));
            }
        }
    };
    let line_h = |img: &mut RgbaImage, y: f32, c: [u8; 3]| {
        let yi = y.round() as i64;
        if yi >= 0 && yi < h as i64 {
            for x in 0..w {
                img.put_pixel(x, yi as u32, Rgba([c[0], c[1], c[2], 255]));
            }
        }
    };
    let mut gx = (a.x / step).floor() as i64;
    while gx as f64 * step <= b.x {
        let x = view.px(DVec2::new(gx as f64 * step, 0.0)).0;
        line_v(&mut img, x, if gx % 5 == 0 { GRID_MAJOR } else { GRID_MINOR });
        gx += 1;
    }
    let mut gy = (a.y / step).floor() as i64;
    while gy as f64 * step <= b.y {
        let y = view.px(DVec2::new(0.0, gy as f64 * step)).1;
        line_h(&mut img, y, if gy % 5 == 0 { GRID_MAJOR } else { GRID_MINOR });
        gy += 1;
    }
    let (ox, oy) = view.px(DVec2::ZERO);
    line_h(&mut img, oy, AXIS_X);
    line_v(&mut img, ox, AXIS_Y);

    // ---- closed regions, shaded and numbered
    let prof = sk.profiles();
    let mut region_json = Vec::new();
    let mut region_labels = Vec::new();
    for (ri, pr) in prof.profiles.iter().enumerate() {
        let outer = polygonize(&pr.outer);
        let holes: Vec<Vec<DVec2>> = pr.holes.iter().map(polygonize).collect();
        let (mut pl, mut ph) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for p in &outer {
            pl = pl.min(*p);
            ph = ph.max(*p);
        }
        let (x0, y1) = view.px(pl);
        let (x1, y0) = view.px(ph);
        let (mut sum, mut n, mut any) = (DVec2::ZERO, 0usize, None);
        for y in (y0.floor().max(0.0) as u32)..(y1.ceil().min(h as f32) as u32) {
            for x in (x0.floor().max(0.0) as u32)..(x1.ceil().min(w as f32) as u32) {
                let q = view.mm(x as f64 + 0.5, y as f64 + 0.5);
                if point_in_polygon(q, &outer) && !holes.iter().any(|hp| point_in_polygon(q, hp)) {
                    blend(&mut img, x, y, REGION, 0.75);
                    sum += DVec2::new(x as f64, y as f64);
                    n += 1;
                    any.get_or_insert((x, y));
                }
            }
        }
        let area = signed_area(&outer).abs() - holes.iter().map(|h| signed_area(h).abs()).sum::<f64>();
        region_json.push(json!({
            "index": ri, "area_mm2": r(area), "holes": holes.len(),
            "bbox": [[r(pl.x), r(pl.y)], [r(ph.x), r(ph.y)]],
        }));
        if n > 0 {
            let c = sum / n as f64;
            let q = view.mm(c.x, c.y);
            let at = if point_in_polygon(q, &outer) && !holes.iter().any(|hp| point_in_polygon(q, hp)) {
                (c.x as i32, c.y as i32)
            } else {
                let (x, y) = any.unwrap();
                (x as i32, y as i32)
            };
            region_labels.push((at, format!("R{ri}")));
        }
    }

    // ---- entities: construction first, then constrained, then free on top
    let mut order: Vec<usize> = (0..sk.entities.len()).collect();
    order.sort_by_key(|&i| (!sk.entities[i].construction, is_free_e(i)));
    for &i in &order {
        let e = &sk.entities[i];
        let px: Vec<(f32, f32)> = pts[i].iter().map(|p| view.px(*p)).collect();
        let (col, wd, dash) = if e.construction {
            (CONSTRUCTION, 0.8, true)
        } else if is_free_e(i) {
            (FREE, 1.6, false)
        } else {
            (FIXED, 1.4, false)
        };
        polyline(&mut img, &px, wd, col, dash);
    }
    // points the entities use
    let mut used = vec![false; sk.points.len()];
    for e in &sk.entities {
        match e.geom {
            Geom::Line { a, b } => {
                used[a.0 as usize] = true;
                used[b.0 as usize] = true;
            }
            Geom::Arc { center, start, end } => {
                for p in [center, start, end] {
                    used[p.0 as usize] = true;
                }
            }
            Geom::Circle { center, .. } => used[center.0 as usize] = true,
        }
    }
    for (i, p) in sk.points.iter().enumerate() {
        let q = view.px(*p);
        if is_free_p(i) {
            dot(&mut img, q, 3.2, FREE);
        } else if used[i] || labels == "all" {
            dot(&mut img, q, 2.2, FIXED);
        }
    }

    // ---- dimensions
    let ent = |e: &EntityId| sk.entities.get(e.0 as usize).map(|x| x.geom);
    let pt = |p: &PointId| sk.points.get(p.0 as usize).copied().unwrap_or_default();
    let mut dims_json = Vec::new();
    let mut dim_labels: Vec<((f32, f32), String)> = Vec::new();
    for (ci, c) in sk.constraints.iter().enumerate() {
        let (kind, expr, on, a, b): (&str, _, Vec<String>, DVec2, DVec2) = match c {
            Constraint::Distance(p, q, e) => ("distance", e, vec![format!("#p{}", p.0), format!("#p{}", q.0)], pt(p), pt(q)),
            Constraint::DistanceX(p, q, e) => {
                ("distance_x", e, vec![format!("#p{}", p.0), format!("#p{}", q.0)], pt(p), DVec2::new(pt(q).x, pt(p).y))
            }
            Constraint::DistanceY(p, q, e) => {
                ("distance_y", e, vec![format!("#p{}", p.0), format!("#p{}", q.0)], pt(p), DVec2::new(pt(p).x, pt(q).y))
            }
            Constraint::Length(l, e) => match ent(l) {
                Some(Geom::Line { a, b }) => ("length", e, vec![format!("#e{}", l.0)], pt(&a), pt(&b)),
                _ => continue,
            },
            Constraint::PointLineDistance(p, l, e) => match ent(l) {
                Some(Geom::Line { a, b }) => {
                    let (la, lb) = (pt(&a), pt(&b));
                    let d = (lb - la).normalize_or_zero();
                    let foot = la + d * (pt(p) - la).dot(d);
                    ("point_line_distance", e, vec![format!("#p{}", p.0), format!("#e{}", l.0)], pt(p), foot)
                }
                _ => continue,
            },
            Constraint::Radius(x, e) | Constraint::Diameter(x, e) => {
                let (cen, rad) = match ent(x) {
                    Some(Geom::Circle { center, radius }) => (pt(&center), radius),
                    Some(Geom::Arc { center, start, .. }) => (pt(&center), pt(&start).distance(pt(&center))),
                    _ => continue,
                };
                let k = if matches!(c, Constraint::Radius(..)) { "radius" } else { "diameter" };
                (k, e, vec![format!("#e{}", x.0)], cen, cen + DVec2::new(1.0, 1.0).normalize() * rad)
            }
            Constraint::Angle(l1, l2, e) => match (ent(l1), ent(l2)) {
                (Some(Geom::Line { a, b }), Some(Geom::Line { a: c2, b: d2 })) => {
                    let m1 = (pt(&a) + pt(&b)) / 2.0;
                    let m2 = (pt(&c2) + pt(&d2)) / 2.0;
                    ("angle", e, vec![format!("#e{}", l1.0), format!("#e{}", l2.0)], m1, m2)
                }
                _ => continue,
            },
            Constraint::Offset { distance, source, target } => {
                let mid = |id: &EntityId| pts.get(id.0 as usize).map(|v| v[v.len() / 2]).unwrap_or_default();
                let (Some(s0), Some(t0)) = (source.first(), target.first()) else { continue };
                ("offset", distance, vec![format!("#e{}", s0.0), format!("#e{}", t0.0)], mid(s0), mid(t0))
            }
            _ => continue,
        };
        let value = expr.eval(&src.values).ok();
        let plain = expr.0.trim().parse::<f64>().is_ok();
        let txt = match value {
            Some(v) if plain => num(v),
            Some(v) => format!("{}={}", expr.0.trim(), num(v)),
            None => format!("{}=?", expr.0.trim()),
        };
        let txt = match kind {
            "radius" => format!("R{txt}"),
            "diameter" => format!("D{txt}"),
            "angle" => format!("{txt}°"),
            _ => txt,
        };
        let (pa, pb) = (view.px(a), view.px(b));
        if kind != "angle" {
            polyline(&mut img, &[pa, pb], 0.5, DIM, kind == "offset");
        }
        dim_labels.push((((pa.0 + pb.0) / 2.0, (pa.1 + pb.1) / 2.0), txt));
        let mut o = json!({"constraint": ci, "kind": kind, "expr": expr.0, "on": on});
        if let Some(v) = value {
            o["value"] = json!(r(v));
        }
        dims_json.push(o);
    }

    // ---- labels last, over everything
    for ((x, y), t) in region_labels {
        draw_text(&mut img, x - 8, y - 8, 2, &t, REGION_TEXT, None);
    }
    let many = sk.entities.len() > 80;
    if labels != "none" && !many {
        for (i, e) in sk.entities.iter().enumerate() {
            let v = &pts[i];
            let (m, t) = match e.geom {
                Geom::Line { .. } => {
                    let (a, b) = (v[0], v[1]);
                    ((a + b) / 2.0, (b - a).normalize_or_zero())
                }
                _ => {
                    let m = v[v.len() / 8];
                    (m, (v[v.len() / 8 + 1] - m).normalize_or_zero())
                }
            };
            let (x, y) = view.px(m);
            let n = DVec2::new(-t.y, t.x); // left normal in sketch space; pixels flip y
            let (dx, dy) = (n.x as f32 * 14.0, -n.y as f32 * 14.0);
            let col = if e.construction { CONSTRUCTION } else if is_free_e(i) { FREE } else { [170, 174, 180] };
            draw_text(&mut img, (x + dx - 8.0) as i32, (y + dy - 6.0) as i32, 1, &format!("E{i}"), col, Some(LABEL_BG));
        }
    }
    if labels == "all" {
        for (i, p) in sk.points.iter().enumerate() {
            let (x, y) = view.px(*p);
            let col = if is_free_p(i) { FREE } else { GRID_TEXT };
            draw_text(&mut img, x as i32 + 4, y as i32 + 3, 1, &format!("P{i}"), col, None);
        }
    }
    if labels != "none" {
        for ((x, y), t) in &dim_labels {
            let tw = t.chars().count() as f32 * 12.0;
            draw_text(&mut img, (x - tw / 2.0) as i32, (y - 10.0) as i32, 2, t, DIM, Some(LABEL_BG));
        }
    }
    // grid scale along the bottom, caption on top
    draw_text(&mut img, 8, h as i32 - 22, 1, &format!("GRID {} MM", num(step)), GRID_TEXT, Some(BG));
    let status = match report.status {
        ConstraintStatus::FullyConstrained => "FULLY CONSTRAINED".to_string(),
        ConstraintStatus::UnderConstrained => format!("DOF {}", report.dof),
        ConstraintStatus::OverConstrained => "OVER-CONSTRAINED".to_string(),
        ConstraintStatus::Conflicting => "NOT SOLVED".to_string(),
    };
    let caption = format!("{}{} - {}", if src.draft { "DRAFT " } else { "" }, sk.name, status);
    draw_caption(&mut img, &caption.chars().take(((w - 30) / 12) as usize).collect::<String>());

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;

    // ---- the same facts as numbers
    let p2 = |p: DVec2| json!([r(p.x), r(p.y)]);
    let mut free_list = Vec::new();
    for (i, e) in sk.entities.iter().enumerate() {
        if !is_free_e(i) || e.construction {
            continue;
        }
        let mut o = json!({"entity": format!("#e{i}")});
        match e.geom {
            Geom::Line { a, b } => {
                o["kind"] = json!("line");
                o["a"] = p2(pt(&a));
                o["b"] = p2(pt(&b));
                o["moves"] = json!([a, b].iter().filter(|p| is_free_p(p.0 as usize)).map(|p| format!("#p{}", p.0)).collect::<Vec<_>>());
            }
            Geom::Arc { center, start, end } => {
                o["kind"] = json!("arc");
                o["center"] = p2(pt(&center));
                o["radius"] = json!(r(pt(&start).distance(pt(&center))));
                o["moves"] = json!([center, start, end].iter().filter(|p| is_free_p(p.0 as usize)).map(|p| format!("#p{}", p.0)).collect::<Vec<_>>());
            }
            Geom::Circle { center, radius } => {
                o["kind"] = json!("circle");
                o["center"] = p2(pt(&center));
                o["radius"] = json!(r(radius));
            }
        }
        free_list.push(o);
    }
    let free_construction = sk.entities.iter().enumerate().filter(|(i, e)| e.construction && is_free_e(*i)).count();
    let plane = &sk.plane;
    let mut out = json!({
        "sketch": sk.name,
        "id": src.id.map(|i| i.0),
        "source": if src.draft { "the user's open sketch, as drawn now (not finished, so not in the document yet)" } else { "the document" },
        "plane": plane_json(plane),
        "plane_mapping": {"origin": v3(plane.origin), "sketch_x": v3(plane.x_dir), "sketch_y": v3(plane.y_dir()), "normal": v3(plane.normal)},
        "solve": {"status": report.status, "dof": report.dof, "converged": report.converged, "max_residual": report.max_residual},
        "counts": {"points": sk.points.len(), "entities": sk.entities.len(), "constraints": sk.constraints.len()},
        "regions": region_json,
        "open_chains": prof.open.len(),
        "dimensions": dims_json,
        "image": {
            "width": w, "height": h, "mm_per_px": r(1.0 / k), "grid_mm": num(step),
            "legend": "white = fully constrained, blue = can still move, grey dashed = construction, amber = dimensions (expr=value), shaded R0.. = closed regions (what an extrude can use), red line = sketch x axis, green = sketch y axis; labels E3 = entity #e3, P3 = point #p3; sketch +y is up",
        },
    });
    match &free {
        Some(_) if report.dof > 0 => {
            out["free"] = json!(free_list.iter().take(40).collect::<Vec<_>>());
            if free_list.len() > 40 {
                out["free_more"] = json!(free_list.len() - 40);
            }
            if free_construction > 0 {
                out["free_construction"] = json!(free_construction);
            }
        }
        Some(_) => {}
        None => out["free"] = json!("too large to analyse; the DOF count is exact"),
    }
    if many && labels != "none" {
        out["note"] = json!("more than 80 entities: entity labels left out");
    }
    Ok(vec![proto::image(&png), json_text(&out)])
}
