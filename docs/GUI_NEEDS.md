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
8. **Face provenance for labels.** The pick stack says "Planar face · facing up";
   the mock says "Boss top", "Floor". *Want:* the creating feature (and role, e.g.
   cap/side) per face in `RegenResult`.
9. **Section / slice.** The "Section view" button and the sketch option "Slice body
   at sketch plane" are disabled. *Want:* `Shape::section(plane)` exposed for display
   (it exists for joints) and a cap-face mesh.
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

17. **Movable in-context components.** `split_to_components` makes Base / Lid with a
    `ContextRef` resolved in the frame of the component's context occurrence, which is
    its only occurrence; moving that occurrence (a mate, the gizmo) re-expresses the
    master's half in the new frame, so it stays where it was in the world and a
    hinge never opens. *Adapter:* `adapters::anchor_in_context` adds a hidden,
    grounded, out-of-BOM "design position" occurrence of each inside the master and
    sets it as `Component.context` (marked `meta["gui.anchor.<occ>"]`, hidden in the
    tree and counts); the visible occurrence then moves. *Want:* a captured context
    frame (`Component.context_frame: Option<Transform>`, set by
    `split_to_components`), or `split_to_components` doing the anchoring itself.
18. **Mate DOF for the jog.** The dialog's draft mate rides along with each model
    request and the worker runs `assembly::solve_mates` first: fine. *Want:* the
    free direction(s) of each solved mate (axis / plane in world) so the jog control
    can draw them on the model, and joint limits (min/max angle) to stop a jog.
19. **Interference volume for display.** `Interference` has the volume but not the
    common solid, so the GUI tints the whole pair red (translucent). *Want:*
    `Interference.common: Option<Shape>` (world frame) to draw the overlap itself.

## Sketch

12. **Offset constraint.** The GUI's offset makes new geometry pinned by
    point-to-line distances (straight joints) and radial lines + radius expressions
    (tangent joints), which is fully constrained but verbose. *Want:* a native
    `Offset(loop, Expr)` constraint and a loop-walking helper.
13. **Editing helpers.** Trim/split at intersections, sketch fillet, and entity
    removal with id remapping are implemented in the GUI (`sketch_mode.rs`).
    *Want:* them in `sketch` so other front ends share them.
14. **Project.** Projection of model edges into a sketch is GUI-side (mesh
    polylines → fixed construction lines). *Want:* exact projected curves.

## Drawings

15. **View orbit.** "Drag inside the view with Shift+MMB" from the mock is not
    wired; the custom rotation fields build a `ViewOrientation::Rotation`.
16. ~~DimRef from HLR output~~ — done by the engine: `ViewEdge.reference` /
    `DimRef::from_view_edge(&edge)` (also `Placed` for assembly views).

## Coming (per the engine worker), not wired yet

- Wired since: per-vertex colours from `Body::mesh_colors` (renderer has a colour
  attribute), `MeshQuality::Preview` while dragging the joint plane, per-feature
  `elapsed_ms` in history tooltips, `Shape::section` for the joint's live section.
