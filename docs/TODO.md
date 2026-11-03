# easyCAD: path to a usable MVP

The core path is built and passes automated tests: EasyEDA board → "New enclosure from PCB"
→ standoffs and parting joint → STEP/STL/3MF and a PDF drawing. What's missing is real-world
validation: nobody has used it by hand on real hardware yet.

Legend: **[you]** needs a human with a Mac/printer, **[eng]** engine work, **[ui]** app work.

## 1. Get it somewhere safe and building everywhere
- [ ] Push to GitHub (`CharlesMod/easycad`, private) as `main`.
- [ ] Reconcile the in-flight worker changes (the speed work, interference/mate wiring) with the
      rename and commit them. The helper `tools/rename-casework-to-easycad.sh` rewrites old
      names in any older branch.
- [ ] **[you]** Build on the Mac: `scripts/build-release.sh`. Only the Linux leg has been tested.
      Expect to fix MinGW link errors for the Windows cross-build and macOS universal-binary details.
- [ ] **[you]** Run on a real GPU (Mac Metal first): rendering, 60 fps orbit, native file dialogs,
      HiDPI, trackpad navigation, shortcuts. So far only a software GPU under Xvfb has been used.

## 2. Dogfood: the real MVP bug list
- [ ] **[you]** Real EasyEDA Pro export (STEP + outline DXF) → wizard → real enclosure → STL/STEP
      → drawing PDF. Imports have only been tested on files our own tests generated.
      Log everything in `docs/DOGFOOD.md`.
- [ ] **[you]** Print the calibration coupon and load results into the FDM profile
      (`ProcessProfile::from_calibration`). The shipped FDM allowances are generic starting points.
      Check that teardrop holes and a "slide" tongue-and-groove really behave.

## 3. Speed and "refuse less than Fusion"
- [x] **[eng]** Regeneration: under 300 ms per parameter edit on the wizard enclosure (about 830 ms
      before the in-progress work). Undo stays instant. Test: `crates/model/tests/wizard_speed.rs`.
      Done, at the edge: best of 3, `wall` edit 248–302 ms (STEP) / 251–260 (DXF), `lid_split`
      167–215, undo 0.15 ms, assembly `wall` edit 289–330, on a 20-core machine at load 12–17
      from four other agents (up to ~390 ms when it was saturated). The test asserts 1.5 × 300 ms in
      release. Ruled tongue taper (the draft algorithm hung), serial booleans, no double checks;
      what is left is the joint halves' booleans, ~100 ms (ENGINE.md, Performance).
- [x] **[eng]** Fillet/chamfer fuzz: random edge sets × radii on realistic enclosures; histogram
      applied / reduced / skipped / bailed; fix the "bailed" and wrongly "skipped" (vertex blends,
      sliver faces, cross-feature fillets). Done: `tests/fillet_fuzz.rs` (full run `#[ignore]`,
      regression subset runs normally). 400 requests: bailed 11 → 0 (results that lost the solid),
      0 wrongly skipped; smooth edges and seams are `EdgeStatus::Smooth`; failed chains are
      retried first. Left: edges next to sub-0.5 mm slivers that no size takes.
- [x] **[eng]** Overhang analysis (per-face, against build orientation) and **[ui]** a heatmap toggle.
      `model::overhang` (per-triangle areas, so curved faces report a share; plate faces are ok;
      critical angle from the process kind), view toolbar *Overhang* with legend and ±X/±Y/±Z picker.

## 4. App work still open
- [x] **[ui]** Assemblies in drawings: scope picker (assembly / component / occurrence), BOM table, balloons.
      Done: "Shows" picker + Exploded per view, BOM table tool, the Balloon tool numbers
      assembly views by the flat BOM; an assembly whose root has no solids gets assembly
      views and a BOM table on its first sheet. On screen and in SVG / DXF / PDF.
- [x] **[ui]** "Output as components" on the Split and Parting-joint dialogs. A checkbox on new
      splits/joints; a plain part's root moves into a master component first. Root imports (a
      reference PCB) go into the hidden master with it: re-insert the board as its own component.
- [x] **[ui]** Click-test linked-file Update/Break, drag-to-reparent, and one alternate navigation preset.
      `tests/ui_clicks.rs`. Found: a cached link status kept the old folder for 5 s after the
      document moved, and the drag label was painted under the rows. The presets (SolidWorks,
      Onshape, Blender) already existed; the test picks SolidWorks and Onshape in Settings and drags.
- [x] **[ui]** Wire the interference overlap solid and mate free-motion display (GUI_NEEDS #18/#19).
      Free motion: arrows / rings for the mate being edited or the Mates row clicked. The overlap
      solid (wired in fef16bd) is verified by `ui_clicks::mate_free_motion_and_interference_overlap`.
- [x] **[ui]** History strip hint when the active root has no features of its own (`ui_main::history_hint`;
      also an empty part and an empty active component).
- [x] **[ui+eng]** Native sketch offset for chains that contain arcs (GUI_NEEDS #21): each offset
      joint gets two equations (tangent joints pinned on the source joint's normal); 0 DOF, no
      redundancy on the board outline, S-curves, sharp corners, circles. The GUI's own offset is gone.
- [x] **[ui]** "Modeled threads" toggle when placing a part (`FeatureKind::InsertPart.threads`): a checkbox
      in the Parts placement banner (`ui_clicks::place_a_part_with_modeled_threads`).

## 5. Known gaps (fix, or consciously drop)
- [x] Setback fillet corners: OpenCascade has no blend; implement or remove the toggle.
      Removed (the `setback` field, its warning and the dialog's disabled toggle; old files load).
      Why: OCCT 7.8's `BRepFilletAPI_MakeFillet` / `ChFi3d` has no setback option (corners
      are a rolling ball for three edges, a `GeomPlate` filling for more), so it would mean
      trimming every fillet back at the vertex and fitting our own G1 N-sided patch, then
      sewing it in: a second blend engine, fragile on mixed convex/concave corners. For the
      FDM/SLA enclosures this targets, the rolling-ball corner prints the same at these radii.
- [x] Drawings: GD&T frames, section views, hatching, collision-free dimension text, real PDF text widths.
      Done: feature control frames (14 symbols, ⌀, Ⓜ/Ⓛ, datums) and datum symbols; section
      views from a cutting line on a parent (hatched per part, arrows and letters); dimension
      text moves off other text and view lines; Helvetica AFM widths for PDF centring and
      table / frame cells. Not done: editing an existing annotation's GD&T fields (the tool
      settings apply to new frames), offset / aligned sections, hatching patterns per material.
- [x] Parts library rows marked TODO: verified against ISO 4762/7380-1/10642/273, ASME B18.3/B18.22.1/B18.2.8
      and vendor tables (ruthex inserts, EJOT PT); #2-56 rows and the B18.2.8 inch close/normal/loose classes added.
      Still marked: shortest ISO 7380-1 lengths, #6-32/#10-32 inch inserts, M2/M2.5 counterbore depths (no source).
- [x] Per-face STEP colours: read, shown and re-exported; fixture `crates/model/tests/data/face_colors.step`
      (OCCT XCAF; the test also reads a plain-`STYLED_ITEM` variant). Assembly imports keep part colours only.
- [x] Closed loops of mates are solved: the occurrences of a loop are solved together (least squares,
      drives used in mate order); redundant-but-consistent loops get a note, inconsistent ones
      `OverConstrained("the loop through … cannot close …")`. Tree mates still just propagate.
- [x] Pattern/mirror of whole-body features: fillets, chamfers and drafts follow the copies (selectors moved
      onto each instance); whole-body selectors, shell, split and joints get a precise "why not" error.
- [x] Threads are cosmetic only: `InsertPart.threads = Modeled` gives ISO 68-1 / UN helical threads on screws,
      nuts, inserts, standoffs and tapped auto-holes (cosmetic stays the default; ~1 s and ~16 k triangles per M3×10).
      Not done: PT-screw thread form, thread runout. The Parts placement banner has a "Modeled threads" checkbox.

## 6. After the MVP
- [x] Google Drive sync (`crates/sync`; in the app: File ▸ Settings ▸ Google Drive, status bar, conflict notices).
- [ ] Sync follow-ups: read the synced parts library folder (`library.json`) in the Parts screen; reload
      `settings.json` when another computer changed it; offer "keep theirs / keep mine" for a conflict copy;
      a per-root on/off switch and a custom Documents folder in Settings.
- [x] Publish the Google OAuth app (in production since 2026-11-03; branding at charlesmod.github.io/easycad-site).
- [ ] GitHub Actions with a cached OpenCascade build.
- [ ] macOS notarization and Windows code signing; installers.
- [ ] Quick-start guide and shortcut reference.
