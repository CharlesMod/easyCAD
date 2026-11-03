# GUI needs from the engine

Requests from the desktop GUI (`crates/app`). Each entry says what the GUI does
today (adapter or limitation) and what would replace it.

## Model / features

1. ~~Expression-driven planes~~ — done by the engine: `offset: Expr` on `Split`,
   `Draft` (neutral), `ScrewBoss`, `StandoffsFromPcb`.
2. ~~Rollback marker~~ — done by the engine: `DocData.rollback`,
   `Document::set_rollback`, `FeatureReport.rolled_back`; new features insert at it.
3. ~~Several radii in one fillet~~ — done by the engine: `Fillet/Chamfer.sets`
   (`EdgeSet { edges, size, auto_reduce }`), `skipped` (+ `FeatureReport.skipped_edges`).
   `setback` is accepted with a warning (kernel TODO).
4. ~~Report edges in request terms~~ — done by the engine:
   `FeatureReport.edge_requests[outcome.request] = EdgeRequest { set, selector }`.
5. ~~Shell with no open faces~~ — done by the engine; the "New enclosure from PCB"
   wizard uses a closed shell + parting joint. (The sample still has the old
   "open bottom + floor plate" history so the E2E coordinates stay put.)
6. ~~Default fits per field~~ — done by the engine: `FeatureKind::default_fit(field)`.
7. ~~Standoffs~~ — done by the engine: `StandoffsFromPcb.{exclude, gussets,
   lid_holes}` (lid holes with `body: None` go through the closed shell above each
   standoff, before the parting joint splits off the lid).
8. ~~Face provenance for labels~~ — done by the engine: `RegenResult::face_origin(body,
   face)` → feature + `FaceRole`; `FaceOrigin::label` gives "Bosses · boss top".
9. ~~Section / slice~~ — done by the engine: `Body::section_view(origin, normal, q)`
   → kept half, cap faces, cap mesh, outline.
10. ~~Concurrency contract~~ — done; the GUI's kernel mutex is gone.
11. ~~BOM without a `Document`~~ — done by the engine: `DocData::bom(&AssemblyResult)`
    (`AssemblyResult::flat(regen)` for a plain `RegenResult`), `DocData::bom_tree`.

12a. ~~Fractional inches~~ — done by the engine. `units::normalize_input` still
    rewrites them (same value, harmless); it can shrink to the inch-mode suffix.
12b. ~~Profile edits reorder the list~~ — done by the engine:
    `Document::update_profile(label, p)` (in place); `add_profile` keeps the position.
12c. ~~O-ring gland corner radius~~ — engine: free-form (B-spline/offset) edges are
    now measured through their ends instead of reading as sharp corners. Analytic
    arcs already gave the right radius; please re-check the sample.
12d. ~~Snap fits on wizard boxes~~ — done by the engine: beams/roots are fitted to
    the real lid/floor and reported in `JointReport.adjustments`.
12e. ~~Fragile joint/shell at one corner radius~~ — done by the engine: the exact
    bounding box threw on a degenerate offset surface; `bbox` now falls back. The
    closed shell is also validated (two shells, believable void) with a fallback.

## Assemblies

17. ~~Movable in-context components~~ — done by the engine: `Component.context_frame`,
    set by `split_to_components`; the anchor adapter can be dropped.
18. ~~Mate DOF for the jog~~ — done by the engine: `MateReport.free` (world axes /
    directions), `Mate.{angle_limits, distance_limits}` + `MateReport.limited`.
19. ~~Interference volume for display~~ — done by the engine: `Interference.common`
    (`Overlap(Body)`, world frame).

20. ~~Root rollback leaks into component views~~ — done by the engine: components use
    their own `Component.rollback` (`Document::set_component_rollback`).
22. **Show loop-solved drive values.** Closed loops of mates are solved now
    (ENGINE.md, Mates): a mate whose drive the loop overrode has
    `MateReport.actual: Some(value)` (degrees / mm) and a `MateSolve.notes` line.
    The mate dialog / jog handle should show that value (and maybe offer "use as
    drive"), and jogging a loop's follower joint does nothing until its drive is
    the first one in mate order. Not wired yet.

## Sketch

12. ~~Offset constraint~~ — done by the engine: `Constraint::Offset` + `Sketch::offset_chain`,
    `Sketch::chain_from`.
13. ~~Editing helpers~~ — done by the engine (`sketch_edit`): `trim`, `split_at`,
    `fillet_corner`, `remove_entities` → `Remap`.
14. ~~Project~~ — done by the engine: `Sketch::project_edge(&EdgeInfo, samples, construction)`.
21. ~~Offset of line–arc chains is under-determined~~ — done by the engine:
    `Constraint::Offset` pins each joint with two equations (both offset carriers,
    or one carrier and the source joint's normal at tangent joints); the board
    outline offset is 0 DOF with rank = variables and no redundancy. The GUI uses
    `offset_chain` for every chain; `chain_from` follows equal end points (no
    `weld_ends`) and offsets are never construction. Original report: on the sample board DXF
    (`sample::board_dxf()`, layer BoardOutLine: 4 lines + 4 r3 corner arcs, fully
    constrained, 52 vars / rank 52), `Sketch::offset_chain(EntityId(0),
    "clearance + wall")` adds 16 variables and 16 equations but the rank grows by
    only 7: 9 DOF and `OverConstrained` (redundant). Pinning each tangent joint on
    the radial through the source joint (`PointOnLine`) still leaves 1 DOF with 9
    redundant equations. The GUI uses `offset_chain` (native `Constraint::Offset`)
    for line-only chains and circles and keeps its own offset (point-line
    distances, radial lines at tangent joints; 0 DOF) for chains with arcs.
    Also: `chain_from` follows shared points / `Coincident` only, so DXF loops
    with separate equal end points need the GUI to weld them first
    (`SketchSession::weld_ends`); and offsets inherit the source's construction
    flag, so offsetting a DXF reference makes construction geometry (the GUI
    clears it: the offset is the profile).

## Drawings

15. **View orbit.** "Drag inside the view with Shift+MMB" from the mock is not
    wired; the custom rotation fields build a `ViewOrientation::Rotation`.
16. ~~DimRef from HLR output~~ — done by the engine: `ViewEdge.reference` /
    `DimRef::from_view_edge(&edge)` (also `Placed` for assembly views).

## Coming (per the engine worker), not wired yet

- Wired since: per-vertex colours from `Body::mesh_colors` (renderer has a colour
  attribute), `MeshQuality::Preview` while dragging the joint plane, per-feature
  `elapsed_ms` in history tooltips, `Shape::section` for the joint's live section.
