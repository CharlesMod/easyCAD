//! Self-healing fillet / chamfer pipeline.
//!
//! Instead of a single `BRepFilletAPI_MakeFillet` call that either succeeds or
//! refuses, [`fillet`] and [`chamfer`] run a fallback ladder:
//!
//! 1. **Pre-heal** the input (ShapeFix + UnifySameDomain) and re-find the
//!    requested edges on it geometrically.
//! 2. **Expand** each edge to its tangent chain, then try everything at once.
//! 3. Otherwise apply **chain by chain** (convex before concave, larger sizes
//!    first), re-resolving remaining edges on the new shape after every step.
//! 4. A chain that still fails: **binary-search** its largest workable size
//!    (reported; applied only with `auto_reduce`), then try its edges one by one,
//!    searching a maximum for each edge that still fails.
//! 5. Every intermediate result is validated (`BRepCheck_Analyzer`, ShapeFix on
//!    failure) by the shim, so the returned shape is always valid.
//!
//! Partial success is not an error: you get the best valid shape plus a
//! [`DressUpReport`] saying what happened to every requested edge.

use crate::{DVec3, Result, Shape};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DressUpKind {
    Fillet,
    Chamfer,
}

/// Which rung of the ladder produced the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strategy {
    /// Nothing needed doing (no edges) or nothing succeeded.
    None,
    AllAtOnce,
    ByChains,
    Individual,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EdgeStatus {
    Applied,
    /// Applied at `applied` instead of the requested size (only with `auto_reduce`).
    AppliedReduced { requested: f64, applied: f64 },
    /// Not applied. `max_size` is the largest size found to work, if any — the UI can
    /// offer "Apply 1.8 / Skip edge".
    Failed { reason: String, max_size: Option<f64> },
}

impl EdgeStatus {
    pub fn is_applied(&self) -> bool {
        !matches!(self, EdgeStatus::Failed { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EdgeOutcome {
    /// Index into the request slice.
    pub request: usize,
    /// Edge index on the *input* shape.
    pub edge: u32,
    /// Tangent-chain group number (shared by edges filleted together).
    pub chain: usize,
    pub status: EdgeStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DressUpReport {
    pub kind: DressUpKind,
    pub strategy: Strategy,
    /// The input was healed / unified before dressing.
    pub healed: bool,
    /// One outcome per requested edge.
    pub outcomes: Vec<EdgeOutcome>,
    /// Number of extra edges pulled in by tangent-chain expansion.
    pub expanded_edges: usize,
}

impl DressUpReport {
    pub fn applied_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.status.is_applied()).count()
    }
    pub fn all_applied(&self) -> bool {
        self.outcomes.iter().all(|o| o.status == EdgeStatus::Applied)
    }
    /// e.g. "3 of 4 edges done · edge 7 maxes out at 1.80 mm".
    pub fn summary(&self) -> String {
        let mut s = format!("{} of {} edges done", self.applied_count(), self.outcomes.len());
        for o in &self.outcomes {
            match &o.status {
                EdgeStatus::Applied => {}
                EdgeStatus::AppliedReduced { applied, .. } => {
                    s += &format!(" · edge {} reduced to {:.2} mm", o.edge, applied)
                }
                EdgeStatus::Failed { max_size: Some(m), .. } => {
                    s += &format!(" · edge {} maxes out at {:.2} mm", o.edge, m)
                }
                EdgeStatus::Failed { reason, .. } => s += &format!(" · edge {} failed: {}", o.edge, reason),
            }
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DressUpOptions {
    /// Apply the largest workable size when the requested one fails.
    pub auto_reduce: bool,
    pub pre_heal: bool,
    pub expand_tangent_chains: bool,
    /// Binary-search iterations for the maximum size.
    pub search_steps: u32,
    /// Smallest size considered "workable" during search (mm).
    pub min_size: f64,
}

impl Default for DressUpOptions {
    fn default() -> Self {
        DressUpOptions { auto_reduce: false, pre_heal: true, expand_tangent_chains: true, search_steps: 7, min_size: 0.05 }
    }
}

/// Robust fillet: `edges` are `(edge index on shape, radius)`.
pub fn fillet(shape: &Shape, edges: &[(u32, f64)], opts: &DressUpOptions) -> Result<(Shape, DressUpReport)> {
    let req: Vec<(u32, f64, bool)> = edges.iter().map(|&(e, s)| (e, s, opts.auto_reduce)).collect();
    run(DressUpKind::Fillet, shape, &req, opts)
}

/// Fillet or chamfer with a per-edge `auto_reduce`: `edges` are `(edge index,
/// size, auto_reduce)` (`opts.auto_reduce` is ignored). A tangent chain is reduced
/// as a whole only when every requested edge in it allows it.
pub fn dress(kind: DressUpKind, shape: &Shape, edges: &[(u32, f64, bool)], opts: &DressUpOptions) -> Result<(Shape, DressUpReport)> {
    run(kind, shape, edges, opts)
}

/// Robust chamfer: `edges` are `(edge index on shape, distance)`.
pub fn chamfer(shape: &Shape, edges: &[(u32, f64)], opts: &DressUpOptions) -> Result<(Shape, DressUpReport)> {
    let req: Vec<(u32, f64, bool)> = edges.iter().map(|&(e, s)| (e, s, opts.auto_reduce)).collect();
    run(DressUpKind::Chamfer, shape, &req, opts)
}

/// Geometric identity of an edge that survives re-indexing.
#[derive(Clone, Copy, Debug)]
struct EdgeKey {
    point: DVec3,
    tangent: DVec3,
}

#[derive(Clone, Debug)]
struct Item {
    key: EdgeKey,
    size: f64,
    /// Request index this edge stands for (`None` = chain expansion).
    request: Option<usize>,
    /// Apply the largest workable size when the requested one fails.
    reduce: bool,
}

const MATCH_TOL: f64 = 1e-3;

fn apply(kind: DressUpKind, s: &Shape, edges: &[u32], sizes: &[f64]) -> Result<Shape> {
    let r = match kind {
        DressUpKind::Fillet => s.fillet_raw(edges, sizes)?,
        DressUpKind::Chamfer => s.chamfer_raw(edges, sizes)?,
    };
    if !r.is_valid() {
        return Err(crate::Error("result is not a valid solid".into()));
    }
    Ok(r)
}

/// Resolve items on `s`; dedupes edges, keeping the first size.
fn resolve(s: &Shape, items: &[&Item]) -> Result<(Vec<u32>, Vec<f64>, Vec<bool>)> {
    let mut idx = Vec::new();
    let mut sizes = Vec::new();
    let mut found = Vec::new();
    for it in items {
        match s.match_edge(it.key.point, it.key.tangent, MATCH_TOL)? {
            Some(e) => {
                found.push(true);
                if !idx.contains(&e) {
                    idx.push(e);
                    sizes.push(it.size);
                }
            }
            None => found.push(false),
        }
    }
    Ok((idx, sizes, found))
}

/// Largest scale factor in (0, 1) for which `items` succeed, with its shape.
fn search_max(
    kind: DressUpKind,
    s: &Shape,
    items: &[&Item],
    opts: &DressUpOptions,
) -> Result<Option<(f64, Shape)>> {
    let (idx, sizes, _) = resolve(s, items)?;
    if idx.is_empty() {
        return Ok(None);
    }
    let max_size = sizes.iter().cloned().fold(0.0, f64::max);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut best = None;
    for _ in 0..opts.search_steps {
        let mid = (lo + hi) / 2.0;
        if mid * max_size < opts.min_size {
            lo = mid;
            continue;
        }
        let scaled: Vec<f64> = sizes.iter().map(|r| r * mid).collect();
        match apply(kind, s, &idx, &scaled) {
            Ok(r) => {
                lo = mid;
                best = Some((mid, r));
            }
            Err(_) => hi = mid,
        }
    }
    Ok(best)
}

fn run(kind: DressUpKind, shape: &Shape, edges: &[(u32, f64, bool)], opts: &DressUpOptions) -> Result<(Shape, DressUpReport)> {
    let mut report = DressUpReport { kind, strategy: Strategy::None, healed: false, outcomes: Vec::new(), expanded_edges: 0 };
    if edges.is_empty() {
        return Ok((shape.clone(), report));
    }
    let infos = shape.edges()?;
    let mut statuses: Vec<Option<EdgeStatus>> = vec![None; edges.len()];
    let mut items: Vec<Item> = Vec::new();
    for (k, &(e, size, reduce)) in edges.iter().enumerate() {
        match infos.get(e as usize) {
            Some(info) if size > 0.0 => {
                items.push(Item { key: EdgeKey { point: info.midpoint, tangent: info.tangent }, size, request: Some(k), reduce })
            }
            Some(_) => statuses[k] = Some(EdgeStatus::Failed { reason: "size must be positive".into(), max_size: None }),
            None => statuses[k] = Some(EdgeStatus::Failed { reason: "edge index out of range".into(), max_size: None }),
        }
    }

    // 1. Pre-heal.
    let mut cur = shape.clone();
    if opts.pre_heal {
        if let Ok(h) = shape.heal() {
            if h.is_valid() && h.count(crate::ShapeKind::Solid) == shape.count(crate::ShapeKind::Solid) {
                let all: Vec<&Item> = items.iter().collect();
                let (_, _, found) = resolve(&h, &all)?;
                if found.iter().all(|f| *f) {
                    cur = h;
                    report.healed = true;
                }
            }
        }
    }

    // 2. Tangent-chain grouping (+ expansion).
    let chains = cur.tangent_chains()?;
    let cur_infos = cur.edges()?;
    let mut groups: Vec<(u32, Vec<Item>)> = Vec::new(); // (chain id, items)
    let mut item_chain: Vec<usize> = Vec::new();
    for it in &items {
        let e = cur.match_edge(it.key.point, it.key.tangent, MATCH_TOL)?;
        let chain = e.map(|e| chains[e as usize]).unwrap_or(u32::MAX - item_chain.len() as u32);
        let g = match groups.iter().position(|(c, _)| *c == chain) {
            Some(g) => g,
            None => {
                groups.push((chain, Vec::new()));
                groups.len() - 1
            }
        };
        groups[g].1.push(it.clone());
        item_chain.push(g);
    }
    if opts.expand_tangent_chains {
        for (chain, group) in groups.iter_mut() {
            let (size, reduce) = (group[0].size, group[0].reduce);
            let present: Vec<u32> =
                group.iter().filter_map(|it| cur.match_edge(it.key.point, it.key.tangent, MATCH_TOL).ok().flatten()).collect();
            for info in &cur_infos {
                if chains[info.index as usize] == *chain && !present.contains(&info.index) && info.faces.len() == 2 {
                    group.push(Item { key: EdgeKey { point: info.midpoint, tangent: info.tangent }, size, request: None, reduce });
                    report.expanded_edges += 1;
                }
            }
        }
    }

    let set_status = |statuses: &mut Vec<Option<EdgeStatus>>, it: &Item, st: EdgeStatus| {
        if let Some(k) = it.request {
            if statuses[k].is_none() {
                statuses[k] = Some(st);
            }
        }
    };

    // 3. Everything at once.
    let all: Vec<&Item> = groups.iter().flat_map(|(_, g)| g.iter()).collect();
    let (idx, sizes, found) = resolve(&cur, &all)?;
    let mut done = false;
    if found.iter().all(|f| *f) && !idx.is_empty() {
        if let Ok(r) = apply(kind, &cur, &idx, &sizes) {
            cur = r;
            report.strategy = Strategy::AllAtOnce;
            for it in &all {
                set_status(&mut statuses, it, EdgeStatus::Applied);
            }
            done = true;
        }
    }

    if !done {
        report.strategy = Strategy::ByChains;
        // Order: convex chains first, then larger sizes first.
        let mut order: Vec<(i32, f64, usize)> = groups
            .iter()
            .enumerate()
            .map(|(g, (_, items))| {
                let conv = cur
                    .match_edge(items[0].key.point, items[0].key.tangent, MATCH_TOL)
                    .ok()
                    .flatten()
                    .and_then(|e| cur.edge_convexity(e).ok())
                    .unwrap_or(0);
                (conv, items.iter().map(|i| i.size).fold(0.0, f64::max), g)
            })
            .collect();
        order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));

        for &(_, _, g) in &order {
            let group: Vec<&Item> = groups[g].1.iter().collect();
            let (idx, sizes, found) = resolve(&cur, &group)?;
            for (it, f) in group.iter().zip(&found) {
                if !f {
                    set_status(
                        &mut statuses,
                        it,
                        EdgeStatus::Failed { reason: "edge consumed by an earlier step".into(), max_size: None },
                    );
                }
            }
            if idx.is_empty() {
                continue;
            }
            match apply(kind, &cur, &idx, &sizes) {
                Ok(r) => {
                    cur = r;
                    for it in &group {
                        set_status(&mut statuses, it, EdgeStatus::Applied);
                    }
                    continue;
                }
                Err(_) if group.len() == 1 => {}
                Err(_) => {
                    // 4a. Whole-chain max (reported, or applied with auto_reduce).
                    if group.iter().all(|it| it.reduce) {
                        if let Some((f, r)) = search_max(kind, &cur, &group, opts)? {
                            cur = r;
                            for it in &group {
                                set_status(
                                    &mut statuses,
                                    it,
                                    EdgeStatus::AppliedReduced { requested: it.size, applied: it.size * f },
                                );
                            }
                            report.strategy = report.strategy.max(Strategy::Individual);
                            continue;
                        }
                    }
                }
            }
            // 4b. Edge by edge.
            report.strategy = report.strategy.max(Strategy::Individual);
            for it in &group {
                let (idx, sizes, found) = resolve(&cur, &[it])?;
                if !found[0] {
                    set_status(
                        &mut statuses,
                        it,
                        EdgeStatus::Failed { reason: "edge consumed by an earlier step".into(), max_size: None },
                    );
                    continue;
                }
                let err = match apply(kind, &cur, &idx, &sizes) {
                    Ok(r) => {
                        cur = r;
                        set_status(&mut statuses, it, EdgeStatus::Applied);
                        continue;
                    }
                    Err(e) => e.0,
                };
                match search_max(kind, &cur, &[it], opts)? {
                    Some((f, r)) if it.reduce => {
                        cur = r;
                        set_status(&mut statuses, it, EdgeStatus::AppliedReduced { requested: it.size, applied: it.size * f });
                    }
                    Some((f, _)) => {
                        set_status(&mut statuses, it, EdgeStatus::Failed { reason: err, max_size: Some(it.size * f) })
                    }
                    None => set_status(&mut statuses, it, EdgeStatus::Failed { reason: err, max_size: None }),
                }
            }
        }
        if statuses.iter().all(|s| !matches!(s, Some(st) if st.is_applied())) {
            report.strategy = Strategy::None;
        }
    }

    // 5. Final validation (each step is validated; this is a belt-and-braces check).
    if !cur.is_valid() {
        cur = shape.clone();
        for s in statuses.iter_mut() {
            *s = Some(EdgeStatus::Failed { reason: "result failed validation".into(), max_size: None });
        }
        report.strategy = Strategy::None;
    }

    for (k, &(e, _, _)) in edges.iter().enumerate() {
        let chain = items.iter().position(|it| it.request == Some(k)).map(|i| item_chain[i]).unwrap_or(usize::MAX);
        report.outcomes.push(EdgeOutcome {
            request: k,
            edge: e,
            chain,
            status: statuses[k]
                .clone()
                .unwrap_or(EdgeStatus::Failed { reason: "not processed".into(), max_size: None }),
        });
    }
    Ok((cur, report))
}
