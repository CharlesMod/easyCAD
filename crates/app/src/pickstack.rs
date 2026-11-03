//! Pick stack: every face/edge/body under the cursor, front to back, filtered by
//! the selection filter. Tab / Alt+wheel cycles; click selects the current one.
//! Replaces Fusion's press-and-hold.

use casework_model::feature::BodyId;
use casework_model::pick::{Hit, HitKind};

/// What a selection refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PickKind {
    Body,
    Face(u32),
    Edge(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PickKey {
    pub body: BodyId,
    pub kind: PickKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub bodies: bool,
    pub faces: bool,
    pub edges: bool,
    pub sketches: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            bodies: true,
            faces: true,
            edges: true,
            sketches: false,
        }
    }
}

impl Filter {
    pub const EDGES_ONLY: Filter = Filter {
        bodies: false,
        faces: false,
        edges: true,
        sketches: false,
    };
    pub const FACES_ONLY: Filter = Filter {
        bodies: false,
        faces: true,
        edges: false,
        sketches: false,
    };
    pub const BODIES_ONLY: Filter = Filter {
        bodies: true,
        faces: false,
        edges: false,
        sketches: false,
    };
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub key: PickKey,
    /// Ray distance of the first hit.
    pub t: f32,
    pub point: glam::Vec3,
}

/// Facts about a body the filter needs (reference bodies are only pickable whole).
pub trait BodyFacts {
    fn is_reference(&self, body: BodyId) -> bool;
    fn is_visible(&self, body: BodyId) -> bool;
}

/// Turn raw front-to-back hits into the filtered candidate list (deduplicated,
/// still front to back).
pub fn candidates(hits: &[Hit], filter: Filter, facts: &dyn BodyFacts) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let push = |key: PickKey, h: &Hit, out: &mut Vec<Candidate>| {
        if !out.iter().any(|c| c.key == key) {
            out.push(Candidate {
                key,
                t: h.t,
                point: h.point,
            });
        }
    };
    for h in hits {
        if !facts.is_visible(h.body) {
            continue;
        }
        let body_key = PickKey {
            body: h.body,
            kind: PickKind::Body,
        };
        if facts.is_reference(h.body) {
            // locked reference bodies (PCBs, library parts) pick as a whole
            if filter.bodies {
                push(body_key, h, &mut out);
            }
            continue;
        }
        match h.kind {
            HitKind::Face(f) if filter.faces => push(
                PickKey {
                    body: h.body,
                    kind: PickKind::Face(f),
                },
                h,
                &mut out,
            ),
            HitKind::Edge(e) if filter.edges => push(
                PickKey {
                    body: h.body,
                    kind: PickKind::Edge(e),
                },
                h,
                &mut out,
            ),
            _ if filter.bodies && !filter.faces && !filter.edges => push(body_key, h, &mut out),
            HitKind::Face(_) if filter.bodies && !filter.faces => push(body_key, h, &mut out),
            _ => {}
        }
    }
    out
}

/// The hover stack and its current index.
#[derive(Clone, Debug, Default)]
pub struct PickStack {
    pub items: Vec<Candidate>,
    pub index: usize,
}

impl PickStack {
    /// Replace the candidates (cursor moved). Keeps the current candidate if it is
    /// still under the cursor so Tab cycling survives small mouse moves; otherwise
    /// starts at the front-most.
    pub fn update(&mut self, items: Vec<Candidate>) {
        let cur = self.current().map(|c| c.key);
        let same = items.len() == self.items.len()
            && items.iter().zip(&self.items).all(|(a, b)| a.key == b.key);
        self.items = items;
        if same {
            return;
        }
        self.index = cur
            .and_then(|k| self.items.iter().position(|c| c.key == k))
            .unwrap_or(0);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.index = 0;
    }

    pub fn current(&self) -> Option<&Candidate> {
        self.items.get(self.index)
    }

    /// Cycle forward (+1, Tab / wheel down) or backward (−1), wrapping.
    pub fn cycle(&mut self, dir: i32) {
        let n = self.items.len();
        if n == 0 {
            self.index = 0;
            return;
        }
        self.index = (self.index as i64 + dir as i64).rem_euclid(n as i64) as usize;
    }

    pub fn select_index(&mut self, i: usize) {
        if i < self.items.len() {
            self.index = i;
        }
    }

    /// The current candidate is hidden behind the first one (shown as x-ray).
    pub fn current_is_hidden(&self) -> bool {
        self.index > 0 && self.current().is_some()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use casework_model::feature::FeatureId;
    use glam::Vec3;

    struct Facts(Vec<BodyId>);
    impl BodyFacts for Facts {
        fn is_reference(&self, b: BodyId) -> bool {
            self.0.contains(&b)
        }
        fn is_visible(&self, _: BodyId) -> bool {
            true
        }
    }

    fn hit(body: BodyId, kind: HitKind, t: f32) -> Hit {
        Hit {
            body,
            kind,
            t,
            point: Vec3::new(0.0, 0.0, -t),
        }
    }

    fn scene() -> (Vec<Hit>, BodyId, BodyId) {
        let base = BodyId::of(FeatureId(1));
        let pcb = BodyId::of(FeatureId(9));
        let hits = vec![
            hit(pcb, HitKind::Face(3), 1.0),
            hit(pcb, HitKind::Face(4), 1.6),
            hit(base, HitKind::Edge(7), 5.0),
            hit(base, HitKind::Face(12), 5.0),
            hit(base, HitKind::Face(2), 9.0),
            hit(base, HitKind::Face(12), 9.0), // same face again (back of a cylinder)
        ];
        (hits, base, pcb)
    }

    #[test]
    fn filter_and_dedupe() {
        let (hits, base, pcb) = scene();
        let f = Facts(vec![pcb]);
        let all = candidates(&hits, Filter::default(), &f);
        let keys: Vec<PickKey> = all.iter().map(|c| c.key).collect();
        assert_eq!(
            keys,
            vec![
                PickKey {
                    body: pcb,
                    kind: PickKind::Body
                },
                PickKey {
                    body: base,
                    kind: PickKind::Edge(7)
                },
                PickKey {
                    body: base,
                    kind: PickKind::Face(12)
                },
                PickKey {
                    body: base,
                    kind: PickKind::Face(2)
                },
            ]
        );
        let edges = candidates(&hits, Filter::EDGES_ONLY, &f);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].key.kind, PickKind::Edge(7));
        let faces = candidates(&hits, Filter::FACES_ONLY, &f);
        assert_eq!(
            faces.iter().map(|c| c.key.kind).collect::<Vec<_>>(),
            vec![PickKind::Face(12), PickKind::Face(2)]
        );
        let bodies = candidates(&hits, Filter::BODIES_ONLY, &f);
        assert_eq!(
            bodies.iter().map(|c| c.key.body).collect::<Vec<_>>(),
            vec![pcb, base]
        );
    }

    #[test]
    fn cycling_wraps_both_ways() {
        let (hits, _, pcb) = scene();
        let mut s = PickStack::default();
        s.update(candidates(&hits, Filter::default(), &Facts(vec![pcb])));
        assert_eq!(s.index, 0);
        assert!(!s.current_is_hidden());
        s.cycle(1);
        assert_eq!(s.index, 1);
        assert!(s.current_is_hidden());
        s.cycle(1);
        s.cycle(1);
        assert_eq!(s.index, 3);
        s.cycle(1);
        assert_eq!(s.index, 0, "wraps to the front");
        s.cycle(-1);
        assert_eq!(s.index, 3, "wraps to the back");
        let mut empty = PickStack::default();
        empty.cycle(1);
        assert!(empty.current().is_none());
    }

    #[test]
    fn index_survives_small_moves() {
        let (hits, base, pcb) = scene();
        let f = Facts(vec![pcb]);
        let mut s = PickStack::default();
        s.update(candidates(&hits, Filter::default(), &f));
        s.cycle(1);
        s.cycle(1); // Face(12)
        let cur = s.current().unwrap().key;
        // same list again (cursor moved within the same faces)
        s.update(candidates(&hits, Filter::default(), &f));
        assert_eq!(s.current().unwrap().key, cur);
        // the cursor leaves the PCB: the list changes but Face(12) is still there
        let fewer: Vec<Hit> = hits.iter().filter(|h| h.body == base).cloned().collect();
        s.update(candidates(&fewer, Filter::default(), &f));
        assert_eq!(s.current().unwrap().key, cur);
        assert_eq!(s.index, 1);
        // something completely different: back to the front-most
        let other = vec![
            hit(base, HitKind::Face(40), 3.0),
            hit(base, HitKind::Face(41), 4.0),
        ];
        s.update(candidates(&other, Filter::default(), &f));
        assert_eq!(s.index, 0);
        assert_eq!(s.current().unwrap().key.kind, PickKind::Face(40));
    }
}
