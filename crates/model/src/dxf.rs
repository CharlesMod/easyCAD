//! Minimal ASCII DXF reader: LINE, ARC, CIRCLE, LWPOLYLINE (bulges), POLYLINE/VERTEX
//! from the ENTITIES section, scaled to mm via `$INSUNITS`, grouped by layer.

use std::collections::BTreeMap;

use glam::DVec2;
use serde::{Deserialize, Serialize};

use crate::geom::{Plane, Segment2};
use crate::sketch::{reverse_segment, Constraint, Geom, PointId, Sketch};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DxfError {
    #[error("line {line}: {msg}")]
    Parse { line: usize, msg: String },
    #[error("not an ASCII DXF file (binary DXF is not supported)")]
    Binary,
}

/// A parsed drawing: segments in mm, grouped by layer name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DxfDrawing {
    /// `$INSUNITS` code (0 if absent).
    pub insunits: i32,
    /// Factor applied to file coordinates to get mm.
    pub scale_to_mm: f64,
    pub layers: BTreeMap<String, Vec<Segment2>>,
    /// Entity types that were present but ignored (e.g. SPLINE, TEXT), with counts.
    pub skipped: BTreeMap<String, usize>,
}

/// mm per drawing unit for a `$INSUNITS` code.
pub fn insunits_to_mm(code: i32) -> f64 {
    match code {
        1 => 25.4,           // inches
        2 => 304.8,          // feet
        3 => 1_609_344.0,    // miles
        5 => 10.0,           // cm
        6 => 1000.0,         // m
        7 => 1_000_000.0,    // km
        8 => 25.4e-6,        // microinches
        9 => 0.0254,         // mils
        10 => 914.4,         // yards
        13 => 1e-3,          // microns
        14 => 100.0,         // decimetres
        _ => 1.0,            // 0 unitless, 4 mm, others: assume mm
    }
}

struct Pairs<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> Pairs<'a> {
    fn next(&mut self) -> Result<Option<(i32, &'a str)>, DxfError> {
        if self.pos + 1 >= self.lines.len() {
            return Ok(None);
        }
        let code = self.lines[self.pos].trim();
        let val = self.lines[self.pos + 1].trim();
        let line = self.pos + 1;
        self.pos += 2;
        let code: i32 = code.parse().map_err(|_| DxfError::Parse { line, msg: format!("bad group code `{code}`") })?;
        Ok(Some((code, val)))
    }
    fn peek_code(&self) -> Option<i32> {
        self.lines.get(self.pos).and_then(|c| c.trim().parse().ok())
    }
}

fn num(v: &str, line: usize) -> Result<f64, DxfError> {
    v.parse().map_err(|_| DxfError::Parse { line, msg: format!("bad number `{v}`") })
}

/// One entity's raw group codes.
#[derive(Default)]
struct Raw {
    kind: String,
    groups: Vec<(i32, String)>,
}

impl Raw {
    fn f(&self, code: i32) -> Option<f64> {
        self.groups.iter().find(|(c, _)| *c == code).and_then(|(_, v)| v.parse().ok())
    }
    fn i(&self, code: i32) -> Option<i64> {
        self.groups.iter().find(|(c, _)| *c == code).and_then(|(_, v)| v.parse().ok())
    }
    fn layer(&self) -> String {
        self.groups.iter().find(|(c, _)| *c == 8).map(|(_, v)| v.clone()).unwrap_or_else(|| "0".into())
    }
}

/// Bulge segment from `a` to `b` (bulge = tan(θ/4), positive = CCW).
pub fn bulge_segment(a: DVec2, b: DVec2, bulge: f64) -> Segment2 {
    if bulge.abs() < 1e-12 || a.distance(b) < 1e-12 {
        return Segment2::Line { a, b };
    }
    let chord = b - a;
    let d = chord.length();
    let mid = (a + b) * 0.5;
    let left = chord.perp() / d; // rotate +90°
    let h = d / 2.0 * (1.0 - bulge * bulge) / (2.0 * bulge);
    let center = mid + left * h;
    Segment2::Arc { center, start: a, end: b, ccw: bulge > 0.0 }
}

impl DxfDrawing {
    pub fn parse(src: &str) -> Result<DxfDrawing, DxfError> {
        if src.starts_with("AutoCAD Binary DXF") {
            return Err(DxfError::Binary);
        }
        let mut p = Pairs { lines: src.lines().collect(), pos: 0 };
        let mut insunits = 0;
        let mut entities: Vec<Raw> = Vec::new();
        let mut section = String::new();
        let mut cur: Option<Raw> = None;
        while let Some((code, val)) = p.next()? {
            let line = p.pos - 1;
            match (code, val) {
                (0, "SECTION") => {
                    if let Some((2, name)) = p.next()? {
                        section = name.to_string();
                    }
                    continue;
                }
                (0, "ENDSEC") => {
                    if let Some(r) = cur.take() {
                        entities.push(r);
                    }
                    section.clear();
                    continue;
                }
                (0, "EOF") => break,
                _ => {}
            }
            match section.as_str() {
                "HEADER" => {
                    if code == 9 && val == "$INSUNITS" {
                        if let Some((70, v)) = p.next()? {
                            insunits = num(v, line)? as i32;
                        }
                    }
                }
                "ENTITIES" => {
                    if code == 0 {
                        if let Some(r) = cur.take() {
                            entities.push(r);
                        }
                        cur = Some(Raw { kind: val.to_string(), groups: Vec::new() });
                    } else if let Some(r) = cur.as_mut() {
                        r.groups.push((code, val.to_string()));
                    }
                }
                _ => {}
            }
        }
        let _ = p.peek_code();
        let scale = insunits_to_mm(insunits);
        let mut d = DxfDrawing { insunits, scale_to_mm: scale, ..Default::default() };
        let pt = |x: f64, y: f64| DVec2::new(x, y) * scale;
        let mut i = 0;
        while i < entities.len() {
            let e = &entities[i];
            let layer = e.layer();
            let mut segs = Vec::new();
            match e.kind.as_str() {
                "LINE" => {
                    let (a, b) = (
                        pt(e.f(10).unwrap_or(0.0), e.f(20).unwrap_or(0.0)),
                        pt(e.f(11).unwrap_or(0.0), e.f(21).unwrap_or(0.0)),
                    );
                    if a.distance(b) > 1e-9 {
                        segs.push(Segment2::Line { a, b });
                    }
                }
                "CIRCLE" => {
                    let r = e.f(40).unwrap_or(0.0) * scale;
                    if r > 0.0 {
                        segs.push(Segment2::Circle { center: pt(e.f(10).unwrap_or(0.0), e.f(20).unwrap_or(0.0)), radius: r });
                    }
                }
                "ARC" => {
                    let c = pt(e.f(10).unwrap_or(0.0), e.f(20).unwrap_or(0.0));
                    let r = e.f(40).unwrap_or(0.0) * scale;
                    let (a0, a1) = (e.f(50).unwrap_or(0.0).to_radians(), e.f(51).unwrap_or(0.0).to_radians());
                    let mut seg = Segment2::Arc {
                        center: c,
                        start: c + DVec2::from_angle(a0) * r,
                        end: c + DVec2::from_angle(a1) * r,
                        ccw: true,
                    };
                    // extrusion direction (0,0,-1) mirrors the OCS
                    if e.f(230).unwrap_or(1.0) < 0.0 {
                        seg = mirror_x(&seg);
                    }
                    if r > 0.0 {
                        segs.push(seg);
                    }
                }
                "LWPOLYLINE" => {
                    let closed = e.i(70).unwrap_or(0) & 1 == 1;
                    let mut verts: Vec<(DVec2, f64)> = Vec::new();
                    let mut x = None;
                    for (c, v) in &e.groups {
                        match c {
                            10 => x = v.parse::<f64>().ok(),
                            20 => {
                                if let (Some(xx), Ok(yy)) = (x.take(), v.parse::<f64>()) {
                                    verts.push((pt(xx, yy), 0.0));
                                }
                            }
                            42 => {
                                if let Some(last) = verts.last_mut() {
                                    last.1 = v.parse().unwrap_or(0.0);
                                }
                            }
                            _ => {}
                        }
                    }
                    segs.extend(poly_segments(&verts, closed));
                }
                "POLYLINE" => {
                    let closed = e.i(70).unwrap_or(0) & 1 == 1;
                    let mut verts = Vec::new();
                    let mut j = i + 1;
                    while j < entities.len() && entities[j].kind == "VERTEX" {
                        let v = &entities[j];
                        verts.push((pt(v.f(10).unwrap_or(0.0), v.f(20).unwrap_or(0.0)), v.f(42).unwrap_or(0.0)));
                        j += 1;
                    }
                    if j < entities.len() && entities[j].kind == "SEQEND" {
                        j += 1;
                    }
                    segs.extend(poly_segments(&verts, closed));
                    i = j;
                    d.layers.entry(layer).or_default().extend(segs);
                    continue;
                }
                other => {
                    *d.skipped.entry(other.to_string()).or_default() += 1;
                }
            }
            if !segs.is_empty() {
                d.layers.entry(layer).or_default().extend(segs);
            }
            i += 1;
        }
        Ok(d)
    }

    pub fn layer_names(&self) -> Vec<&str> {
        self.layers.keys().map(|s| s.as_str()).collect()
    }

    /// Segments of the given layers (all layers if `layers` is empty).
    pub fn segments(&self, layers: &[&str]) -> Vec<Segment2> {
        self.layers
            .iter()
            .filter(|(k, _)| layers.is_empty() || layers.contains(&k.as_str()))
            .flat_map(|(_, v)| v.iter().copied())
            .collect()
    }

    /// Full circles on the given layers: `(center, radius)` (e.g. mounting holes).
    pub fn circles(&self, layers: &[&str]) -> Vec<(DVec2, f64)> {
        self.segments(layers)
            .into_iter()
            .filter_map(|s| match s {
                Segment2::Circle { center, radius } => Some((center, radius)),
                _ => None,
            })
            .collect()
    }

    /// Convert layers into a sketch with coincident constraints at shared endpoints.
    pub fn to_sketch(&self, name: &str, plane: Plane, layers: &[&str]) -> Sketch {
        segments_to_sketch(name, plane, &self.segments(layers), 1e-6)
    }
}

fn mirror_x(s: &Segment2) -> Segment2 {
    let m = |p: DVec2| DVec2::new(-p.x, p.y);
    match *s {
        Segment2::Arc { center, start, end, ccw } => {
            reverse_segment(&Segment2::Arc { center: m(center), start: m(start), end: m(end), ccw: !ccw })
        }
        other => other,
    }
}

fn poly_segments(verts: &[(DVec2, f64)], closed: bool) -> Vec<Segment2> {
    let n = verts.len();
    let mut out = Vec::new();
    if n < 2 {
        return out;
    }
    let last = if closed { n } else { n - 1 };
    for k in 0..last {
        let (a, bulge) = verts[k];
        let b = verts[(k + 1) % n].0;
        if a.distance(b) > 1e-9 {
            out.push(bulge_segment(a, b, bulge));
        }
    }
    out
}

/// Build a sketch from segments: shared endpoints (within `tol`) become coincident
/// constraints between separate points; arcs become CCW sketch arcs.
pub fn segments_to_sketch(name: &str, plane: Plane, segs: &[Segment2], tol: f64) -> Sketch {
    let mut sk = Sketch::new(name, plane);
    let mut ends: Vec<PointId> = Vec::new();
    for s in segs {
        match *s {
            Segment2::Line { a, b } => {
                let (pa, pb) = (sk.add_point(a), sk.add_point(b));
                sk.add_line(pa, pb);
                ends.extend([pa, pb]);
            }
            Segment2::Arc { center, start, end, ccw } => {
                let (st, en) = if ccw { (start, end) } else { (end, start) };
                let c = sk.add_point(center);
                let (ps, pe) = (sk.add_point(st), sk.add_point(en));
                sk.add_entity(Geom::Arc { center: c, start: ps, end: pe }, false);
                ends.extend([ps, pe]);
            }
            Segment2::Circle { center, radius } => {
                let c = sk.add_point(center);
                sk.add_entity(Geom::Circle { center: c, radius }, false);
            }
        }
    }
    // Union endpoints: each point is constrained to the first point of its cluster.
    let mut leader: Vec<Option<PointId>> = vec![None; ends.len()];
    for i in 0..ends.len() {
        if leader[i].is_some() {
            continue;
        }
        for j in i + 1..ends.len() {
            if leader[j].is_none() && sk.point(ends[i]).distance(sk.point(ends[j])) <= tol.max(1e-9) {
                leader[j] = Some(ends[i]);
            }
        }
    }
    for (j, l) in leader.iter().enumerate() {
        if let Some(l) = l {
            sk.constrain(Constraint::Coincident(*l, ends[j]));
        }
    }
    sk
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulge_semicircle_and_quarter() {
        let s = bulge_segment(DVec2::new(0.0, 0.0), DVec2::new(2.0, 0.0), 1.0);
        let Segment2::Arc { center, ccw, .. } = s else { panic!() };
        assert!(center.distance(DVec2::new(1.0, 0.0)) < 1e-12 && ccw);
        // quarter circle CCW from (1,0) to (0,1) around origin: bulge tan(22.5°)
        let s = bulge_segment(DVec2::new(1.0, 0.0), DVec2::new(0.0, 1.0), (22.5f64).to_radians().tan());
        let Segment2::Arc { center, .. } = s else { panic!() };
        assert!(center.length() < 1e-12, "{center:?}");
        // negative bulge → clockwise, centre on the other side
        let s = bulge_segment(DVec2::new(0.0, 1.0), DVec2::new(1.0, 0.0), -(22.5f64).to_radians().tan());
        let Segment2::Arc { center, ccw, .. } = s else { panic!() };
        assert!(center.length() < 1e-12 && !ccw);
    }

    #[test]
    fn polyline_vertex_and_layers() {
        let src = "0\nSECTION\n2\nENTITIES\n0\nPOLYLINE\n8\nEdge\n70\n1\n0\nVERTEX\n8\nEdge\n10\n0\n20\n0\n0\nVERTEX\n8\nEdge\n10\n10\n20\n0\n0\nVERTEX\n8\nEdge\n10\n10\n20\n10\n0\nSEQEND\n0\nTEXT\n8\nNotes\n0\nENDSEC\n0\nEOF\n";
        let d = DxfDrawing::parse(src).unwrap();
        assert_eq!(d.layers["Edge"].len(), 3);
        assert_eq!(d.skipped["TEXT"], 1);
        assert_eq!(d.scale_to_mm, 1.0);
    }
}
