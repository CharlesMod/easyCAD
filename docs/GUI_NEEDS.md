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
12b. **Profile edits reorder the list.** `Document::add_profile` removes and
    re-appends, so editing a profile moved it to the bottom of the picker. The GUI
    uses `adapters::update_profile` (replace in place, own undo label). *Want:*
    `Document::update_profile(label, p)` or in-place replacement in `add_profile`.
12c. **O-ring gland corner radius.** On the sample (outline corners filleted r 5.3,
    cavity corners r 3.3) the O-ring report says "gland corner radius 0.00 mm is
    below the 5.33 mm bend guidance" and the check "min corner radius 0.00". Looks
    like the gland path's corner radius is measured as 0 on a filleted outline.
12d. **Snap fits on wizard boxes.** `wizard::build` on the sample DXF board
    (91 × 55 outline, height 30, closed shell, lid_split 20, 4 standoffs) with
    `JointKind::SnapFit` (GUI defaults, `Count(4)`) fails "no snap-fit position could
    be placed: blocked by a boss, rib, the lid/floor or another feature" ×4. Nothing
    obvious blocks z 12–20 on the walls.
12e. **Fragile joint/shell at one corner radius.** Same box from the sample STEP
    board (rectangle outline, offset 2.5, sketch corners `corner_r`), edge_r 3,
    tongue & groove: corner_r 5 → "groove depth 1.65 needs 2.05 of material but only
    0.00 is available"; corner_r 3, 5.3, 6, 8 are fine. With edge_r 1 (< wall) the
    closed shell fails as documented. Reproduce by setting `corner_r` to 5 in
    `wizard::tests::builds_a_clean_enclosure_from_dxf_and_step`.

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
18. **Mate preview.** The dialog's draft mate is pushed into `DocData.mates` of every
    model request (id `u32::MAX` while new) and the worker runs
    `assembly::solve_mates` before `regenerate_assembly`. Fine, but *want:*
    `solve_mates` to report per-mate the remaining free DOF direction (axis / plane)
    so the jog control can show it on the model.

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
