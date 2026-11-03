//! Adapters for engine gaps (each one is listed in docs/GUI_NEEDS.md).
//!
//! * **Plane bindings.** `Split`/`Draft`/`ScrewBoss` planes are numeric `Plane`s,
//!   but the user wants "Split @ lid_split". The GUI stores the expression in
//!   `DocData::meta["gui.bind.<feature>"]` and rewrites the plane's offset inside
//!   the same undoable edit as every parameter change.
//! * **Rollback marker.** Not part of `DocData` yet: the GUI keeps it and sends the
//!   truncated feature list to the worker.
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

/// Re-evaluate every plane binding into its feature.
pub fn apply_bindings(d: &mut DocData) {
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
    let mut d = d.clone();
    apply_bindings(&mut d);
    if let Some(n) = active {
        d.features.truncate(n);
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
