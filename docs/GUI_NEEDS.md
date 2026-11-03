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
5. **Shell with no open faces** returns only the inner offset surface (the body's
   bbox becomes the cavity). The sample enclosure works around it with "shell open
   bottom + floor plate". *Want:* a closed hollow solid (or an error).
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
10. **Concurrency contract.** The GUI serializes all kernel work (regen worker,
    background export) with one mutex after a SIGSEGV when several documents
    regenerated on parallel threads in tests. *Want:* a statement of what is safe
    to run concurrently, or an internal lock.
11. **BOM without a `Document`.** `Document::bom()` only reads `DocData`; the worker
    builds a throwaway `Document` to feed `drawing::compute`. *Want:* `DocData::bom()`.

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

- Colored STEP import (per-face colours): the renderer has one colour per body
  today; it needs a per-vertex colour attribute.
- Preview vs final tessellation levels (use preview while dragging dialogs).
- Pattern/mirror recipe re-application, ribs "to body".
