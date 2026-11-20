//! Picking regions of a sketch in the 3D view while an Extrude/Revolve dialog is open:
//! hovering tints the region under the pointer, a click toggles it in or out of the
//! dialog's selection, and the selected regions are filled.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use easycad_model::feature::SketchId;
use easycad_model::regions::{region_at, selected_regions, selection_of, Region};
use easycad_model::sketch::Sketch;
use egui::{Color32, Mesh, Painter, Pos2, Rect, Response};
use glam::{DVec2, DVec3, Vec2};

use crate::app::EasyCadApp;
use crate::theme;

pub struct RegionGeom {
    pub region: Region,
    pub pts: Vec<DVec2>,
    pub tris: Vec<[usize; 3]>,
}

type Cache = HashMap<(String, SketchId), (Sketch, Arc<Vec<RegionGeom>>)>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
}

thread_local! {
    static ONE: RefCell<Option<(Sketch, Arc<Vec<RegionGeom>>)>> = const { RefCell::new(None) };
}

/// Regions of a sketch with their triangles (one-slot cache, for the sketch editor).
pub fn geoms_of(sk: &Sketch) -> Arc<Vec<RegionGeom>> {
    ONE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((old, g)) = c.as_ref() {
            if old == sk {
                return g.clone();
            }
        }
        let g = build(sk);
        *c = Some((sk.clone(), g.clone()));
        g
    })
}

fn build(sk: &Sketch) -> Arc<Vec<RegionGeom>> {
    Arc::new(
        sk.regions()
            .into_iter()
            .map(|region| {
                let (pts, tris) = crate::sketch_view::triangulate_region(&region);
                RegionGeom { region, pts, tris }
            })
            .collect(),
    )
}

fn geoms(doc_id: &str, id: SketchId, sk: &Sketch) -> Arc<Vec<RegionGeom>> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let k = (doc_id.to_string(), id);
        if let Some((old, g)) = c.get(&k) {
            if old == sk {
                return g.clone();
            }
        }
        let g = build(sk);
        c.insert(k, (sk.clone(), g.clone()));
        g
    })
}

/// The regions of the sketch the open dialog extrudes/revolves, if it is one.
fn target(app: &EasyCadApp) -> Option<(SketchId, Sketch, Arc<Vec<RegionGeom>>)> {
    let (id, _) = app.dialog.as_ref()?.region_target()?;
    let sk = app.view().sketch(id)?.clone();
    let g = geoms(&app.doc.data.doc_id, id, &sk);
    Some((id, sk, g))
}

/// Sketch-plane point under a viewport-local position.
fn unproject(app: &EasyCadApp, sk: &Sketch, l: Vec2, size: Vec2) -> Option<DVec2> {
    let (o, d) = app.camera.ray(l, size);
    let (o, d) = (o.as_dvec3(), d.as_dvec3());
    let pl = &sk.plane;
    let den = pl.normal.dot(d);
    if den.abs() < 1e-9 {
        return None;
    }
    let t = pl.normal.dot(pl.origin - o) / den;
    if t < 0.0 {
        return None;
    }
    Some(pl.to_local(o + d * t))
}

fn hover_region(app: &EasyCadApp, ui_pos: Option<Pos2>, rect: Rect) -> Option<(usize, Arc<Vec<RegionGeom>>, SketchId, Sketch)> {
    let (id, sk, g) = target(app)?;
    let p = ui_pos?;
    let l = p - rect.min;
    let q = unproject(app, &sk, Vec2::new(l.x, l.y), Vec2::new(rect.width(), rect.height()))?;
    let regs: Vec<Region> = g.iter().map(|r| r.region.clone()).collect();
    let i = region_at(&regs, q)?;
    Some((i, g, id, sk))
}

/// Toggle region `i` in the open dialog's selection (starting from what it picks now).
pub fn toggle(app: &mut EasyCadApp, sk: &Sketch, regions: &[Region], i: usize) {
    let Some(d) = app.dialog.as_mut() else { return };
    let Some((_, sel)) = d.region_target() else { return };
    let mut cur = selected_regions(sel, sk, regions);
    match cur.iter().position(|&k| k == i) {
        Some(p) => {
            cur.remove(p);
        }
        None => cur.push(i),
    }
    d.set_region_sel(selection_of(regions, &cur));
}

/// Handle hover/click. Returns true when the click was taken (so it is not also a body pick).
pub fn interact(app: &mut EasyCadApp, resp: &Response, rect: Rect) -> bool {
    if app.dialog.as_ref().and_then(|d| d.region_target()).is_none() {
        return false;
    }
    if !resp.clicked_by(egui::PointerButton::Primary) {
        return false;
    }
    let pos = resp.interact_pointer_pos();
    let Some((i, g, _, sk)) = hover_region(app, pos, rect) else { return false };
    let regs: Vec<Region> = g.iter().map(|r| r.region.clone()).collect();
    toggle(app, &sk, &regs, i);
    app.request_preview();
    true
}

/// Fill the selected regions and tint the hovered one.
pub fn paint(app: &EasyCadApp, painter: &Painter, rect: Rect, hover: Option<Pos2>) {
    let Some((_, sk, g)) = target(app) else { return };
    let Some((_, sel)) = app.dialog.as_ref().and_then(|d| d.region_target()) else { return };
    let regs: Vec<Region> = g.iter().map(|r| r.region.clone()).collect();
    let picked = selected_regions(sel, &sk, &regs);
    let hov = hover_region(app, hover, rect).map(|h| h.0);
    let size = Vec2::new(rect.width(), rect.height());
    let a = theme::ACCENT;
    let sel_col = Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 110);
    let hov_col = Color32::from_rgba_unmultiplied(255, 255, 255, 46);
    for (i, rg) in g.iter().enumerate() {
        let is_sel = picked.contains(&i);
        let is_hov = hov == Some(i);
        if !is_sel && !is_hov {
            continue;
        }
        let w = |p: DVec2| sk.plane.to_world(p).as_vec3();
        let scr: Vec<Option<Pos2>> = rg.pts.iter().map(|p| app.camera.project(w(*p), size).map(|v| rect.min + egui::vec2(v.x, v.y))).collect();
        for (col, on) in [(sel_col, is_sel), (hov_col, is_hov)] {
            if !on {
                continue;
            }
            let mut m = Mesh::default();
            for p in &scr {
                m.colored_vertex(p.unwrap_or(rect.min), col);
            }
            for t in &rg.tris {
                if t.iter().all(|&k| scr[k].is_some()) {
                    m.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
                }
            }
            painter.add(egui::Shape::mesh(m));
        }
    }
    let _ = DVec3::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycad_model::geom::Plane;

    #[test]
    fn triangles_cover_exactly_the_region_with_holes() {
        let mut s = Sketch::new("r", Plane::XY);
        s.circle(DVec2::ZERO, 9.8);
        s.circle(DVec2::ZERO, 11.4);
        s.rect(DVec2::new(20.0, 0.0), DVec2::new(30.0, 10.0));
        s.rect(DVec2::new(23.0, 3.0), DVec2::new(26.0, 6.0));
        for g in geoms_of(&s).iter() {
            let a: f64 = g.tris.iter().map(|t| ((g.pts[t[1]] - g.pts[t[0]]).perp_dot(g.pts[t[2]] - g.pts[t[0]]) / 2.0).abs()).sum();
            // polygonal approximation of the arcs: within 1%
            assert!((a - g.region.area).abs() / g.region.area < 0.01, "{a} vs {}", g.region.area);
        }
    }
}
