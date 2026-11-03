# GUI needs from the engine

Requests from the desktop GUI (`crates/app`). Each entry says what the GUI does
today (adapter or limitation) and what would replace it.

## Model / features

1. **Expression-driven planes.** `Split.plane`, `Draft.neutral`, `ScrewBoss.plane`
   and `StandoffsFromPcb.plane` are numeric `Plane`s, but users write
   "Split @ lid_split". *Adapter:* `crates/app/src/adapters.rs` stores the
   expression in `DocData::meta["gui.bind.<feature id>"]` and rewrites the plane's
   offset inside every parameter edit (same undo step) and before each regen.
   *Want:* an `offset: Expr` on these features (like `PartingJoint.offset`).
2. **Rollback marker in the document.** The history strip's "roll back to here" is
   GUI state; the worker gets a truncated feature list. *Want:* `DocData.rollback:
   Option<usize>` (saved, synced, undoable), so a reopened file keeps it.
3. **Several radii in one fillet.** The dialog has edge sets with their own radius
   expression; each set becomes its own `Fillet` feature (committed in one undo
   step). *Want:* `Fillet { sets: Vec<(Vec<EdgeSelector>, Expr, auto_reduce)> }`,
   a persisted "skipped edges" list (shown in the report), and setback corners
   (the "Setback" toggle is disabled).
4. **Report edges in request terms.** `DressUpReport` outcomes name edges on the
   feature's *input* shape, so the GUI asks the worker for the bodies after each
   prefix and matches midpoints to map outcomes back to picks. *Want:* the request
   index of the selector that produced each outcome edge.
5. ~~Shell with no open faces~~ — done by the engine; the "New enclosure from PCB"
   wizard uses a closed shell + parting joint. (The sample still has the old
   "open bottom + floor plate" history so the E2E coordinates stay put.)
6. **Default fits per field.** The GUI mirrors `regen.rs` (boss/standoff hole =
   Hole/Snug) to show the chip. *Want:* `FeatureKind::default_fit(field) -> Option<Fit>`.
7. **Standoffs.** *Want:* a per-hole exclusion list on `StandoffsFromPcb` (the GUI
   falls back to an unlinked `ScrewBoss` at the ticked holes), gusset ribs to the
   nearest wall, and matching counterbored holes in the lid (both checkboxes are
   disabled).
8. **Face provenance for labels.** The pick stack says "Planar face · facing up";
   the mock says "Boss top", "Floor". *Want:* the creating feature (and role, e.g.
   cap/side) per face in `RegenResult`.
9. **Section / slice.** The "Section view" button and the sketch option "Slice body
   at sketch plane" are disabled. *Want:* `Shape::section(plane)` exposed for display
   (it exists for joints) and a cap-face mesh.
10. ~~Concurrency contract~~ — done; the GUI's kernel mutex is gone.
11. **BOM without a `Document`.** `Document::bom()` only reads `DocData`; the worker
    builds a throwaway `Document` to feed `drawing::compute`. *Want:* `DocData::bom()`.

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
16. **DimRef from HLR output.** The GUI maps `ViewEdge.source` (body, edge index)
    to an `EdgeInfo` and builds a hint selector. *Want:* `DimRef::from_view_edge`.

## Coming (per the engine worker), not wired yet

- Wired since: per-vertex colours from `Body::mesh_colors` (renderer has a colour
  attribute), `MeshQuality::Preview` while dragging the joint plane, per-feature
  `elapsed_ms` in history tooltips, `Shape::section` for the joint's live section.
