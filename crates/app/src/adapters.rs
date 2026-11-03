//! Adapters for engine gaps (each one is listed in docs/GUI_NEEDS.md).
//!
//! * **Old plane bindings.** Files from before plane `offset` expressions are
//!   migrated on open ([`migrate_bindings`]).
//! * **Old design-position anchors.** Files from before `Component.context_frame`
//!   carry hidden anchor occurrences; [`migrate_anchors`] turns them into frames.
//! * **Usage counts.** "Used by" in the Parameters panel scans feature, sketch and
//!   parameter JSON for identifiers.

use std::collections::{BTreeMap, BTreeSet};

use easycad_model::document::{DocData, DocError, Document};
use easycad_model::feature::{FeatureId, FeatureKind};
use easycad_model::params::Values;

const BIND: &str = "gui.bind.";

/// Files saved before plane features had an `offset` expression (GUI_NEEDS #1, done)
/// kept "Split @ lid_split" in `meta["gui.bind.<feature>"]`. Move each into the
/// feature's `offset` (plane back through the origin) and drop the key.
pub fn migrate_bindings(d: &mut DocData) {
    let binds: Vec<(String, u32, String)> = d
        .meta
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(BIND)
                .and_then(|id| id.parse().ok())
                .map(|id| (k.clone(), id, v.clone()))
        })
        .collect();
    for (key, id, expr) in binds {
        let feats = d
            .features
            .iter_mut()
            .chain(d.components.iter_mut().flat_map(|c| c.features.iter_mut()));
        for f in feats.filter(|f| f.id == FeatureId(id)) {
            let (plane, offset) = match &mut f.kind {
                FeatureKind::Split { plane, offset, .. }
                | FeatureKind::ScrewBoss { plane, offset, .. }
                | FeatureKind::StandoffsFromPcb { plane, offset, .. } => (plane, offset),
                FeatureKind::Draft { neutral, offset, .. } => (neutral, offset),
                _ => continue,
            };
            plane.origin = glam::DVec3::ZERO;
            *offset = easycad_model::params::Expr::new(expr.as_str());
        }
        d.meta.remove(&key);
    }
}

/// Set a parameter (undoable).
pub fn set_param(doc: &mut Document, name: &str, expr: &str) -> Result<(), DocError> {
    doc.set_param(name, expr)
}

/// The document as the worker should see it: rolled back to `active` features.
pub fn effective(d: &DocData, active: Option<usize>) -> DocData {
    effective_in(d, easycad_model::assembly::ComponentId::ROOT, active)
}

/// Same, with the rollback marker in component `scope`'s history.
pub fn effective_in(
    d: &DocData,
    scope: easycad_model::assembly::ComponentId,
    active: Option<usize>,
) -> DocData {
    let mut d = d.clone();
    if let (Some(n), Some(f)) = (active, crate::scope::features_mut(&mut d, scope)) {
        f.truncate(n);
    }
    d
}

fn idents(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::String(s) => {
            let mut cur = String::new();
            for ch in s.chars().chain(std::iter::once(' ')) {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    cur.push(ch);
                } else {
                    if cur
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    {
                        out.insert(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| idents(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| idents(x, out)),
        _ => {}
    }
}

/// Identifiers referenced by any serializable value (expressions inside it).
pub fn refs_of<T: serde::Serialize>(t: &T) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Ok(v) = serde_json::to_value(t) {
        idents(&v, &mut out);
    }
    out
}

/// How many features, sketches, other parameters and plane bindings use each parameter.
pub fn usage_counts(d: &DocData) -> BTreeMap<String, usize> {
    let names: BTreeSet<String> = d.params.list.iter().map(|p| p.name.clone()).collect();
    let mut counts: BTreeMap<String, usize> = names.iter().map(|n| (n.clone(), 0)).collect();
    let mut bump = |set: BTreeSet<String>| {
        for n in set {
            if let Some(c) = counts.get_mut(&n) {
                *c += 1;
            }
        }
    };
    for f in &d.features {
        bump(refs_of(&f.kind));
    }
    for s in &d.sketches {
        bump(refs_of(&s.sketch.constraints));
    }
    for p in &d.params.list {
        let mut s = refs_of(&p.expr);
        s.remove(&p.name);
        bump(s);
    }
    counts
}

/// Which features use a parameter (for tooltips).
pub fn users_of(d: &DocData, name: &str) -> Vec<String> {
    let mut v = Vec::new();
    for f in &d.features {
        let s = refs_of(&f.kind);
        if s.contains(name) {
            v.push(f.name.clone());
        }
    }
    for s in &d.sketches {
        if refs_of(&s.sketch.constraints).contains(name) {
            v.push(format!("sketch {}", s.sketch.name));
        }
    }
    for p in &d.params.list {
        if p.name != name && refs_of(&p.expr).contains(name) {
            v.push(format!("param {}", p.name));
        }
    }
    v
}

/// Values for expression fields (parameters under the active profile).
pub fn values(d: &DocData) -> Values {
    d.values().unwrap_or_default()
}


const ANCHOR: &str = "gui.anchor.";

/// Files saved while split components needed hidden "design position" anchor
/// occurrences (GUI_NEEDS #17, done by the engine): each anchor's world placement
/// becomes its component's `context_frame` and the anchor goes.
pub fn migrate_anchors(d: &mut DocData) {
    let ids: Vec<(String, u32)> = d
        .meta
        .keys()
        .filter_map(|k| k.strip_prefix(ANCHOR).and_then(|v| v.parse().ok()).map(|id| (k.clone(), id)))
        .collect();
    if ids.is_empty() {
        return;
    }
    let all = d.all_occurrences();
    for (key, id) in ids {
        let id = easycad_model::assembly::OccurrenceId(id);
        if let Some((_, o, world)) = all.iter().find(|(_, o, _)| o.id == id) {
            if let Some(c) = d.component_mut(o.component) {
                if c.context_frame.is_none() {
                    c.context_frame = Some(*world);
                }
                c.context = None;
            }
        }
        d.occurrences.retain(|o| o.id != id);
        for c in &mut d.components {
            c.children.retain(|o| o.id != id);
        }
        d.meta.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_bindings_migrate_to_offsets() {
        let mut doc = crate::sample::sensor_node();
        let split = doc
            .data
            .features
            .iter()
            .find(|f| matches!(f.kind, FeatureKind::Split { .. }))
            .unwrap()
            .id;
        // an old file: numeric plane at 28 plus the binding in meta
        if let Some(f) = doc.data.feature_mut(split) {
            if let FeatureKind::Split { plane, offset, .. } = &mut f.kind {
                *plane = easycad_model::geom::Plane::XY.offset(28.0);
                *offset = "0".into();
            }
        }
        doc.data.meta.insert(format!("{BIND}{}", split.0), "lid_split".into());
        migrate_bindings(&mut doc.data);
        match &doc.data.feature(split).unwrap().kind {
            FeatureKind::Split { plane, offset, .. } => {
                assert!(plane.origin.length() < 1e-12);
                assert_eq!(offset.0, "lid_split");
            }
            _ => unreachable!(),
        }
        assert!(doc.data.meta.keys().all(|k| !k.starts_with(BIND)));
        let counts = usage_counts(&doc.data);
        assert_eq!(counts["lid_split"], 1);
        assert!(counts["wall"] >= 3, "{counts:?}");
        assert!(counts["height"] >= 2, "extrude + lid_split");
    }

    #[test]
    fn old_anchors_become_context_frames() {
        use easycad_model::assembly::{ComponentId, Transform};
        let mut doc = Document::new("asm");
        let master = doc.add_component("Enclosure");
        let lid = doc.add_component("Lid");
        let t = Transform::translation(glam::DVec3::new(0.0, 0.0, 5.0));
        let m_occ = doc.add_occurrence(ComponentId::ROOT, master, "Enclosure:1", t).unwrap();
        let anchor = doc.add_occurrence(master, lid, "Lid (design position)", Transform::IDENTITY).unwrap();
        doc.add_occurrence(ComponentId::ROOT, lid, "Lid:1", t).unwrap();
        let _ = doc.edit_component(lid, "ctx", |c| c.context = Some(vec![m_occ, anchor]));
        doc.data.meta.insert(format!("{ANCHOR}{}", anchor.0), "1".into());
        migrate_anchors(&mut doc.data);
        let c = doc.data.component(lid).unwrap();
        assert_eq!(c.context, None);
        assert!((c.context_frame.unwrap().translation.z - 5.0).abs() < 1e-12);
        assert!(doc.data.component(master).unwrap().children.is_empty());
        assert!(doc.data.meta.keys().all(|k| !k.starts_with(ANCHOR)));
    }
}
