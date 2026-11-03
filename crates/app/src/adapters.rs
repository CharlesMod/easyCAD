//! Adapters for engine gaps (each one is listed in docs/GUI_NEEDS.md).
//!
//! * **Old plane bindings.** Files from before plane `offset` expressions are
//!   migrated on open ([`migrate_bindings`]).
//! * **Rollback inside a component.** `DocData.rollback` is the root's; while a
//!   component is active the GUI keeps its marker and truncates its features.
//! * **Movable in-context components.** A component made by `split_to_components`
//!   is built in the frame of its context occurrence, so moving its only occurrence
//!   re-expresses the master's half and it stays put. Each gets a hidden "design
//!   position" anchor occurrence inside the master as its `context`
//!   ([`anchor_in_context`]); its visible occurrence then moves (hinged lids).
//! * **Usage counts.** "Used by" in the Parameters panel scans feature, sketch and
//!   parameter JSON for identifiers.

use std::collections::{BTreeMap, BTreeSet};

use casework_model::document::{DocData, DocError, Document};
use casework_model::feature::{FeatureId, FeatureKind};
use casework_model::params::Values;

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
            *offset = casework_model::params::Expr::new(expr.as_str());
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
    effective_in(d, casework_model::assembly::ComponentId::ROOT, active)
}

/// Same, with the rollback marker in component `scope`'s history.
pub fn effective_in(
    d: &DocData,
    scope: casework_model::assembly::ComponentId,
    active: Option<usize>,
) -> DocData {
    let mut d = d.clone();
    // the root's saved marker: truncate here and clear it, so component views
    // (which copy the root's fields) are not cut at the root's index
    if let Some(r) = d.rollback.take() {
        d.features.truncate(r);
    }
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

/// Is this occurrence a design-position anchor (hidden from the tree and counts)?
pub fn is_anchor(d: &DocData, o: casework_model::assembly::OccurrenceId) -> bool {
    d.meta.contains_key(&format!("{ANCHOR}{}", o.0))
}

/// Give in-context components a fixed frame: an anchor occurrence of each inside the
/// master component (`master` = the master's occurrence path), placed where the
/// master is, grounded and out of the BOM, set as the component's `context`. One
/// undo step. See GUI_NEEDS ("movable in-context components").
pub fn anchor_in_context(
    doc: &mut Document,
    master: &[casework_model::assembly::OccurrenceId],
    comps: &[casework_model::assembly::ComponentId],
) -> Result<(), DocError> {
    use casework_model::assembly::{Occurrence, OccurrenceId, Transform};
    let master = master.to_vec();
    doc.edit("Anchor in-context components", |d| {
        let mc = d.component_at(&master).map_err(DocError::Other)?;
        for c in comps {
            let name = d.component_name(*c);
            let id = OccurrenceId(d.next_id);
            d.next_id += 1;
            let o = Occurrence {
                id,
                component: *c,
                name: format!("{name} (design position)"),
                transform: Transform::IDENTITY,
                visible: true,
                grounded: true,
                explode: glam::DVec3::ZERO,
                in_bom: false,
            };
            d.component_mut(mc)
                .ok_or_else(|| DocError::Other("master component vanished".into()))?
                .children
                .push(o);
            d.meta.insert(format!("{ANCHOR}{}", id.0), "1".into());
            let mut p = master.clone();
            p.push(id);
            if let Some(comp) = d.component_mut(*c) {
                comp.context = Some(p);
            }
        }
        Ok(())
    })
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
                *plane = casework_model::geom::Plane::XY.offset(28.0);
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
}
