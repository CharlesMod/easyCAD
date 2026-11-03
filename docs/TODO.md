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
- [ ] **[eng]** Overhang analysis (per-face, against build orientation) and **[ui]** a heatmap toggle.

## 4. App work still open
- [ ] **[ui]** Assemblies in drawings: scope picker (assembly / component / occurrence), BOM table, balloons.
- [ ] **[ui]** "Output as components" on the Split and Parting-joint dialogs.
- [ ] **[ui]** Click-test linked-file Update/Break, drag-to-reparent, and one alternate navigation preset.
- [ ] **[ui]** Wire the interference overlap solid and mate free-motion display (GUI_NEEDS #18/#19).
- [ ] **[ui]** History strip hint when the active root has no features of its own.
- [ ] **[ui+eng]** Native sketch offset for chains that contain arcs (GUI_NEEDS #21).

## 5. Known gaps (fix, or consciously drop)
- [ ] Setback fillet corners: OpenCascade has no blend; implement or remove the toggle.
- [ ] Drawings: GD&T frames, section views, hatching, collision-free dimension text, real PDF text widths.
- [x] Parts library rows marked TODO: verified against ISO 4762/7380-1/10642/273, ASME B18.3/B18.22.1/B18.2.8
      and vendor tables (ruthex inserts, EJOT PT); #2-56 rows and the B18.2.8 inch close/normal/loose classes added.
      Still marked: shortest ISO 7380-1 lengths, #6-32/#10-32 inch inserts, M2/M2.5 counterbore depths (no source).
- [ ] Per-face STEP colours: no real fixture yet (the sample PCB has none).
- [ ] Closed loops of mates are checked, not solved.
- [x] Pattern/mirror of whole-body features: fillets, chamfers and drafts follow the copies (selectors moved
      onto each instance); whole-body selectors, shell, split and joints get a precise "why not" error.
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
