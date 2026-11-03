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
- [ ] **[eng]** Regeneration: under 300 ms per parameter edit on the wizard enclosure (about 830 ms
      before the in-progress work). Undo stays instant. Test: `crates/model/tests/wizard_speed.rs`.
- [ ] **[eng]** Fillet/chamfer fuzz: random edge sets × radii on realistic enclosures; histogram
      applied / reduced / skipped / bailed; fix the "bailed" and wrongly "skipped" (vertex blends,
      sliver faces, cross-feature fillets).
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

## 5. Known gaps (fix, or consciously drop)
- [ ] Setback fillet corners: OpenCascade has no blend; implement or remove the toggle.
- [x] Drawings: GD&T frames, section views, hatching, collision-free dimension text, real PDF text widths.
      Done: feature control frames (14 symbols, ⌀, Ⓜ/Ⓛ, datums) and datum symbols; section
      views from a cutting line on a parent (hatched per part, arrows and letters); dimension
      text moves off other text and view lines; Helvetica AFM widths for PDF centring and
      table / frame cells. Not done: editing an existing annotation's GD&T fields (the tool
      settings apply to new frames), offset / aligned sections, hatching patterns per material.
- [ ] Parts library rows marked TODO (imperial inserts/standoffs, thread-forming heads, some button-head
      lengths, M2/M2.5 counterbores, inch loose-fit clearance): verify against ISO 4762/7380/10642/273 and ASME B18.3.
- [ ] Per-face STEP colours: no real fixture yet (the sample PCB has none).
- [x] Closed loops of mates are solved: the occurrences of a loop are solved together (least squares,
      drives used in mate order); redundant-but-consistent loops get a note, inconsistent ones
      `OverConstrained("the loop through … cannot close …")`. Tree mates still just propagate.
- [ ] Pattern/mirror of whole-body features (fillet, shell) returns "cannot be patterned".
- [ ] Threads are cosmetic only.

## 6. After the MVP
- [x] Google Drive sync (`crates/sync`; in the app: File ▸ Settings ▸ Google Drive, status bar, conflict notices).
- [ ] Sync follow-ups: read the synced parts library folder (`library.json`) in the Parts screen; reload
      `settings.json` when another computer changed it; offer "keep theirs / keep mine" for a conflict copy;
      a per-root on/off switch and a custom Documents folder in Settings.
- [x] Publish the Google OAuth app (in production since 2026-11-03; branding at charlesmod.github.io/easycad-site).
- [ ] GitHub Actions with a cached OpenCascade build.
- [ ] macOS notarization and Windows code signing; installers.
- [ ] Quick-start guide and shortcut reference.
