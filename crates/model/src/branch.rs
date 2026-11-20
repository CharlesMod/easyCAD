//! History branches and the history diff.
//!
//! A branch is a saved alternative of the whole document's history. The live
//! history is always `DocData` itself, named `branch`; every other branch sits in
//! `branches` as a full snapshot (its own `branches` emptied). Starting a branch
//! stores the current state as a branch and continues on a new one cut back to the
//! split point, so nothing is lost; switching swaps the live data with a stored
//! one. Every action is one undo step (it goes through `Document::edit`).
//!
//! `history_diff` compares two document states by feature and sketch id, which is
//! what the history panel draws for an undo/redo preview and for a branch tab.

use serde::{Deserialize, Serialize};

use crate::document::{DocData, Document};
use crate::feature::{FeatureId, SketchId};

/// Added to `next_id` of a branch cut from another, so ids the two sides allocate
/// afterwards do not collide and a diff by id stays meaningful.
const ID_GAP: u32 = 1000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub id: u32,
    pub name: String,
    /// The branch this was started from (by name), if any.
    #[serde(default)]
    pub parent: Option<String>,
    /// How many features it shared with its parent when it was started.
    #[serde(default)]
    pub split: usize,
    /// The whole document state of this branch (its own `branches` is empty).
    pub data: DocData,
}

pub(crate) fn main_name() -> String {
    "main".into()
}
pub(crate) fn is_main(s: &String) -> bool {
    s == "main"
}
pub(crate) fn is_zero(n: &u32) -> bool {
    *n == 0
}
pub(crate) fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

impl DocData {
    /// Where the head is, as a count of live features (0..=n).
    pub fn head_pos(&self) -> usize {
        self.rollback.unwrap_or(self.features.len()).min(self.features.len())
    }

    fn branch_names(&self) -> Vec<&str> {
        std::iter::once(self.branch.as_str()).chain(self.branches.iter().map(|b| b.name.as_str())).collect()
    }

    fn next_branch_id(&self) -> u32 {
        self.branches.iter().map(|b| b.id).chain([self.branch_id]).max().unwrap_or(0) + 1
    }

    /// Start a branch at `at` features ("Try N"): the current state is stored as a
    /// branch, the live history becomes a new one cut back to `at` features.
    /// Returns the new branch's name.
    pub fn branch_here(&mut self, at: usize) -> Result<String, String> {
        let at = at.min(self.features.len());
        let mut n = 1;
        while self.branch_names().contains(&format!("Try {n}").as_str()) {
            n += 1;
        }
        let name = format!("Try {n}");
        let id = self.next_branch_id();
        let mut kept = std::mem::take(&mut self.branches);
        let stored = Branch {
            id: self.branch_id,
            name: self.branch.clone(),
            parent: self.branch_parent.clone(),
            split: self.branch_split,
            data: self.clone(),
        };
        kept.push(stored);
        self.branches = kept;
        self.features.truncate(at);
        self.rollback = None;
        self.next_id += ID_GAP;
        self.branch_parent = Some(self.branch.clone());
        self.branch_split = at;
        self.branch = name.clone();
        self.branch_id = id;
        Ok(name)
    }

    /// Swap the live history with the stored branch `name`.
    pub fn switch_branch(&mut self, name: &str) -> Result<(), String> {
        if name == self.branch {
            return Ok(());
        }
        let idx = self.branches.iter().position(|b| b.name == name).ok_or_else(|| format!("no branch named {name}"))?;
        let mut branches = std::mem::take(&mut self.branches);
        let cur = Branch {
            id: self.branch_id,
            name: self.branch.clone(),
            parent: self.branch_parent.clone(),
            split: self.branch_split,
            data: self.clone(),
        };
        let target = std::mem::replace(&mut branches[idx], cur);
        let mut nd = target.data;
        nd.doc_id = self.doc_id.clone();
        nd.revision = self.revision;
        nd.modified_by_device = self.modified_by_device.clone();
        nd.name = self.name.clone();
        nd.branches = branches;
        nd.branch = target.name;
        nd.branch_id = target.id;
        nd.branch_parent = target.parent;
        nd.branch_split = target.split;
        *self = nd;
        Ok(())
    }

    /// Switch to the parent branch and roll the head to the split point.
    pub fn back_to_split(&mut self) -> Result<(), String> {
        let parent = self.branch_parent.clone().ok_or("this branch has no parent")?;
        let split = self.branch_split;
        self.switch_branch(&parent)?;
        let n = self.features.len();
        self.rollback = if split < n { Some(split) } else { None };
        Ok(())
    }

    /// Keep the live branch and delete every other.
    pub fn keep_branch(&mut self) {
        self.branches.clear();
        self.branch_parent = None;
        self.branch_split = 0;
    }

    /// Delete branch `name`. Deleting the live one moves to its parent (or the
    /// first other branch) first. Refuses to delete the only branch.
    pub fn discard_branch(&mut self, name: &str) -> Result<(), String> {
        if self.branches.is_empty() {
            return Err("this is the only branch".into());
        }
        if name == self.branch {
            let to = self
                .branch_parent
                .clone()
                .filter(|p| self.branches.iter().any(|b| &b.name == p))
                .unwrap_or_else(|| self.branches[0].name.clone());
            self.switch_branch(&to)?;
        }
        let before = self.branches.len();
        self.branches.retain(|b| b.name != name);
        if self.branches.len() == before {
            return Err(format!("no branch named {name}"));
        }
        Ok(())
    }
}

/// Where a state's head sits: before everything, or after a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadAt {
    Start,
    After(FeatureId),
}

/// What differs between the live history (`cur`) and another state (`other`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistDiff {
    /// In `cur`, gone in `other`.
    pub removed_features: Vec<FeatureId>,
    /// In `other` (in its order), not in `cur`.
    pub added_features: Vec<FeatureId>,
    /// In both, with different content.
    pub changed_features: Vec<FeatureId>,
    pub removed_sketches: Vec<SketchId>,
    pub added_sketches: Vec<SketchId>,
    pub changed_sketches: Vec<SketchId>,
    /// Set when `other`'s head lands somewhere other than `cur`'s.
    pub head: Option<HeadAt>,
    pub params_changed: bool,
}

impl HistDiff {
    pub fn is_empty(&self) -> bool {
        *self == HistDiff::default()
    }
}

fn head_at(d: &DocData) -> HeadAt {
    match d.head_pos() {
        0 => HeadAt::Start,
        k => HeadAt::After(d.features[k - 1].id),
    }
}

/// Compare the live history against `other` (an undo/redo target or a branch).
pub fn history_diff(cur: &DocData, other: &DocData) -> HistDiff {
    let mut out = HistDiff::default();
    for f in &cur.features {
        match other.features.iter().find(|g| g.id == f.id) {
            None => out.removed_features.push(f.id),
            Some(g) if g != f => out.changed_features.push(f.id),
            Some(_) => {}
        }
    }
    for g in &other.features {
        if !cur.features.iter().any(|f| f.id == g.id) {
            out.added_features.push(g.id);
        }
    }
    for s in &cur.sketches {
        match other.sketches.iter().find(|t| t.id == s.id) {
            None => out.removed_sketches.push(s.id),
            Some(t) if t.sketch != s.sketch => out.changed_sketches.push(s.id),
            Some(_) => {}
        }
    }
    for t in &other.sketches {
        if !cur.sketches.iter().any(|s| s.id == t.id) {
            out.added_sketches.push(t.id);
        }
    }
    if head_at(cur) != head_at(other) {
        out.head = Some(head_at(other));
    }
    out.params_changed = cur.params != other.params;
    out
}

impl Document {
    /// Start a branch at `at` features (see `DocData::branch_here`). One undo step.
    pub fn branch_here(&mut self, at: usize) -> Result<String, String> {
        let at = at.min(self.data.features.len());
        let label = format!("Branch here ({at})");
        self.edit(&label, |d| d.branch_here(at))
    }
    pub fn switch_branch(&mut self, name: &str) -> Result<(), String> {
        if name == self.data.branch {
            return Ok(());
        }
        self.edit(&format!("Switch to branch {name}"), |d| d.switch_branch(name))
    }
    pub fn back_to_split(&mut self) -> Result<(), String> {
        self.edit("Back to split", |d| d.back_to_split())
    }
    pub fn keep_branch(&mut self) -> Result<(), String> {
        if self.data.branches.is_empty() {
            return Err("no other branches".into());
        }
        self.edit("Keep this branch", |d| {
            d.keep_branch();
            Ok(())
        })
    }
    pub fn discard_branch(&mut self, name: &str) -> Result<(), String> {
        self.edit(&format!("Discard branch {name}"), |d| d.discard_branch(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::FeatureKind;

    fn doc3() -> Document {
        let mut d = Document::new("t");
        for n in ["A", "B", "C"] {
            d.add_feature(n, blk());
        }
        d
    }
    fn blk() -> FeatureKind {
        FeatureKind::ImportStep { import: crate::feature::ImportId(1), reference: false, translate: glam::DVec3::ZERO }
    }
    fn names(d: &DocData) -> Vec<&str> {
        d.features.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn diff_features_and_head() {
        let mut d = doc3();
        let before = d.data.clone();
        let id = d.add_feature("D", blk());
        let df = history_diff(&d.data, &before);
        assert_eq!(df.removed_features, vec![id]);
        assert!(df.added_features.is_empty());
        let df = history_diff(&before, &d.data);
        assert_eq!(df.added_features, vec![id]);
        d.set_rollback(Some(2));
        let df = history_diff(&before, &d.data);
        assert_eq!(df.head, Some(HeadAt::After(d.data.features[1].id)));
        d.data.features[0].name = "A2".into();
        let df = history_diff(&before, &d.data);
        assert_eq!(df.changed_features, vec![d.data.features[0].id]);
        assert!(history_diff(&d.data, &d.data).is_empty());
    }

    #[test]
    fn undo_peek_matches_diff() {
        let mut d = doc3();
        let id = d.add_feature("D", blk());
        let df = history_diff(&d.data, d.peek_undo().unwrap());
        assert_eq!(df.removed_features, vec![id]);
        d.undo();
        let df = history_diff(&d.data, d.peek_redo().unwrap());
        assert_eq!(df.added_features, vec![id]);
    }

    #[test]
    fn branch_create_switch_back() {
        let mut d = doc3();
        let name = d.branch_here(2).unwrap();
        assert_eq!(name, "Try 1");
        assert_eq!(names(&d.data), ["A", "B"]);
        assert_eq!(d.data.branches.len(), 1);
        assert_eq!(d.data.branch_parent.as_deref(), Some("main"));
        d.add_feature("X", blk());
        d.switch_branch("main").unwrap();
        assert_eq!(names(&d.data), ["A", "B", "C"]);
        assert_eq!(d.data.branch, "main");
        d.switch_branch("Try 1").unwrap();
        assert_eq!(names(&d.data), ["A", "B", "X"]);
        // back to the split: main, head after B
        d.back_to_split().unwrap();
        assert_eq!(d.data.branch, "main");
        assert_eq!(d.data.rollback, Some(2));
        // second branch gets the next name
        assert_eq!(d.branch_here(1).unwrap(), "Try 2");
        // undo restores the state before the branching
        d.undo();
        assert_eq!(d.data.branches.len(), 1);
        assert_eq!(d.data.branch, "main");
        // ids stay apart between the sides
        d.switch_branch("Try 1").unwrap();
        let x = d.data.features[2].id;
        d.switch_branch("main").unwrap();
        let c = d.add_feature("Y", blk());
        assert_ne!(x, c);
    }

    #[test]
    fn keep_and_discard() {
        let mut d = doc3();
        d.branch_here(1).unwrap();
        d.branch_here(1).unwrap();
        assert_eq!(d.data.branches.len(), 2);
        d.discard_branch("main").unwrap();
        assert_eq!(d.data.branches.len(), 1);
        // discarding the live branch moves to another
        let live = d.data.branch.clone();
        d.discard_branch(&live).unwrap();
        assert_ne!(d.data.branch, live);
        assert!(d.data.branches.is_empty());
        assert!(d.discard_branch("x").is_err());
        d.branch_here(0).unwrap();
        d.keep_branch().unwrap();
        assert!(d.data.branches.is_empty());
        assert!(d.keep_branch().is_err());
    }

    #[test]
    fn serde_default_and_round_trip() {
        let d = doc3();
        let json = d.data.to_json().unwrap();
        assert!(!json.contains("branches"), "an unbranched file is unchanged");
        let back = DocData::from_json(&json).unwrap();
        assert!(back.branches.is_empty());
        assert_eq!(back.branch, "main");
        let mut d = d;
        d.branch_here(2).unwrap();
        d.add_feature("X", blk());
        let back = DocData::from_json(&d.data.to_json().unwrap()).unwrap();
        assert_eq!(back, d.data);
        assert_eq!(back.branches[0].data.features.len(), 3);
    }
}
