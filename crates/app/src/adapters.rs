//! Adapters for engine gaps (each one is listed in docs/GUI_NEEDS.md).
//!
//! * **Plane bindings.** `Split`/`Draft`/`ScrewBoss` planes are numeric `Plane`s,
//!   but the user wants "Split @ lid_split". The GUI stores the expression in
//!   `DocData::meta["gui.bind.<feature>"]` and rewrites the plane's offset inside
//!   the same undoable edit as every parameter change.
//! * **Rollback marker.** Not part of `DocData` yet: the GUI keeps it and sends the
//!   truncated feature list to the worker.
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

pub fn bind_key(f: FeatureId) -> String {
    format!("{BIND}{}", f.0)
}

/// Bind a feature's plane offset (along the plane normal, through the origin) to an expression.
pub fn bind_plane_offset(doc: &mut Document, f: FeatureId, expr: &str) {
    let key = bind_key(f);
    let expr = expr.to_owned();
    let _ = doc.edit("Bind plane", move |d| {
        d.meta.insert(key, expr);
        apply_bindings(d);
        Ok::<_, DocError>(())
    });
}

pub fn binding(d: &DocData, f: FeatureId) -> Option<&str> {
    d.meta.get(&bind_key(f)).map(|s| s.as_str())
}

/// Re-evaluate every plane binding into its feature (root and components, each
/// with its own params in scope).
pub fn apply_bindings(d: &mut DocData) {
    apply_bindings_root(d);
    let bound: Vec<u32> = d
        .meta
        .keys()
        .filter_map(|k| k.strip_prefix(BIND).and_then(|id| id.parse().ok()))
        .collect();
    if bound.is_empty() {
        return;
    }
    let comps: Vec<casework_model::assembly::ComponentId> = d
        .components
        .iter()
        .filter(|c| c.features.iter().any(|f| bound.contains(&f.id.0)))
        .map(|c| c.id)
        .collect();
    for c in comps {
        if let Some(mut v) = d.component_view(c) {
            apply_bindings_root(&mut v);
            if let Some(comp) = d.component_mut(c) {
                comp.features = v.features;
            }
        }
    }
}

fn apply_bindings_root(d: &mut DocData) {
    let Ok(vals) = d.values() else { return };
    let binds: Vec<(u32, String)> = d
        .meta
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(BIND)
                .and_then(|id| id.parse().ok())
                .map(|id| (id, v.clone()))
        })
        .collect();
    for (id, expr) in binds {
        let Ok(v) = casework_model::params::eval(&expr, &vals) else {
            continue;
        };
        if let Some(f) = d.feature_mut(FeatureId(id)) {
            let plane = match &mut f.kind {
                FeatureKind::Split { plane, .. }
                | FeatureKind::ScrewBoss { plane, .. }
                | FeatureKind::StandoffsFromPcb { plane, .. } => plane,
                FeatureKind::Draft { neutral, .. } => neutral,
                _ => continue,
            };
            plane.origin = plane.normal * v;
        }
    }
}

/// Set a parameter (undoable) and refresh plane bindings in the same step.
pub fn set_param(doc: &mut Document, name: &str, expr: &str) -> Result<(), DocError> {
    let (name, expr) = (name.to_owned(), expr.to_owned());
    doc.edit(&format!("Set {name}"), move |d| {
        d.params.set(&name, expr.as_str())?;
        d.values()?;
        apply_bindings(d);
        Ok::<_, DocError>(())
    })
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
    apply_bindings(&mut d);
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
        let mut s = refs_of(&f.kind);
        if let Some(b) = binding(d, f.id) {
            s.extend(refs_of(&b));
        }
        bump(s);
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
        let mut s = refs_of(&f.kind);
        if let Some(b) = binding(d, f.id) {
            s.extend(refs_of(&b));
        }
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

/// Replace a process profile in place (or append a new one) under an undo label
/// that says what changed. `Document::add_profile` moves the profile to the end
/// of the list, which reorders the profile picker on every edit.
pub fn update_profile(
    doc: &mut Document,
    label: &str,
    p: casework_model::process::ProcessProfile,
) {
    let _ = doc.edit(label, |d| {
        match d.processes.iter_mut().find(|q| q.name == p.name) {
            Some(q) => *q = p,
            None => d.processes.push(p),
        }
        Ok::<_, casework_model::document::DocError>(())
    });
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
    fn bindings_follow_parameters() {
        let mut doc = crate::sample::sensor_node();
        let split = doc
            .data
            .features
            .iter()
            .find(|f| matches!(f.kind, FeatureKind::Split { .. }))
            .unwrap()
            .id;
        let z = |d: &DocData| match &d.feature(split).unwrap().kind {
            FeatureKind::Split { plane, .. } => plane.origin.z,
            _ => unreachable!(),
        };
        assert!((z(&doc.data) - 28.0).abs() < 1e-9);
        set_param(&mut doc, "height", "40").unwrap();
        assert!((z(&doc.data) - 36.0).abs() < 1e-9);
        doc.undo();
        assert!(
            (z(&doc.data) - 28.0).abs() < 1e-9,
            "undo restores the bound plane too"
        );
        let counts = usage_counts(&doc.data);
        assert_eq!(counts["lid_split"], 1);
        assert!(counts["wall"] >= 3, "{counts:?}");
        assert!(counts["height"] >= 2, "extrude + lid_split");
    }
}
