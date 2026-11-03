# Casework engine

Headless kernel bridge and document model for Casework. The GUI (`crates/app`, a
stub for now) is built against the APIs below. Units are mm and degrees throughout,
right-handed, Z up.

## Crate layout

| crate | role |
|---|---|
| `crates/occt` | cxx bridge (namespace `cw`) over a narrow C++ shim (`cpp/shim.{h,cpp}`) on static OpenCascade 7.8.1 (`occt-sys`). Safe API in `src/lib.rs`, raw bridge in `src/ffi.rs`, fallback fillet/chamfer pipeline in `src/dressup.rs`. |
| `crates/model` | Pure Rust apart from calling `occt`: params, sketch + solver, DXF, selectors, features, regeneration, document, parts library, export, picking. Part tables in `data/parts/*.json`. |
| `crates/app` | GUI (not built yet). |

Build: the first `cargo build` compiles OCCT with CMake (about 18 min on 4 cores,
`CMAKE_BUILD_PARALLEL_LEVEL=4`), installing to `target/OCCT` (259 MB of static
libs). Later builds reuse it. A debug test binary that links the kernel is about 82 MB
(30 MB stripped). Most of that is OCCT, and release + LTO will shrink it further.

## `occt`: kernel API

`occt::Shape` is an immutable handle (cheap `Clone`, `Send + Sync`). Every
operation returns a new shape as `occt::Result<T>` (`Error(String)` carries the
OCCT exception text). The shim catches every `Standard_Failure` and C++ exception
through a custom `rust::behavior::trycatch`, so a kernel failure never aborts the
process. Meshing mutates OCCT's triangulation cache, so it runs behind a global mutex.
Booleans run in non-destructive mode.

- Construction: `Shape::make_box`, `cylinder`, `cone`, `compound`,
  `Shape::face(&Frame, outer: &[Edge], holes: &[Vec<Edge>])` with
  `Edge::{Line, Arc3, Circle}`.
- Sweeps: `extrude(v)`, `extrude_ext(frame, dir, dist, symmetric, draft_deg)`
  (a positive draft tapers inward), `revolve(origin, dir, deg)`.
- Booleans: `fuse`, `cut`, `common`, `Shape::fuse_all(&[&Shape])`, `unify`, `heal`.
- Dress-up: `dressup::fillet / chamfer(&shape, &[(edge, size)], &DressUpOptions)`
  returns `(Shape, DressUpReport)`. It never returns an invalid solid. The pipeline
  is: pre-heal (ShapeFix + UnifySameDomain), tangent-chain expansion, all at once,
  then chain by chain (convex first, larger first, re-resolving edges geometrically
  after each step), then edge by edge with a binary search for the largest workable
  size. The report gives a per-edge `EdgeStatus::{Applied, AppliedReduced, Failed{max_size}}`,
  the `Strategy` used, and `summary()` (e.g. "3 of 4 edges done · edge 7 maxes out
  at 1.80 mm"). `auto_reduce` applies the max size automatically. `fillet_raw` and
  `chamfer_raw` make a single attempt.
- `shell(open_faces, thickness, outward)` and
  `draft(faces, pull, deg, &neutral_frame)`.
- Transforms: `translated`, `rotated`, `mirrored`, `linear_pattern`,
  `circular_pattern`, `split(origin, normal) -> (below, above)`.
- Topology: `faces() -> Vec<FaceInfo>` (kind, centroid, outward normal, area,
  cylinder axis/radius), `edges() -> Vec<EdgeInfo>` (kind, midpoint, tangent,
  length, ends, circle, adjacent faces), `face_edges`, `tangent_chains`,
  `edge_convexity`, `match_edge`. Indices follow `TopExp::MapShapes` and are only
  stable within one shape.
- `tessellate(lin_defl, ang_deg) -> Mesh` (positions, normals, indices,
  `tri_face` per triangle, edge polylines tagged with edge index).
- Measurement: `volume`, `area`, `center_of_mass`, `bbox`, `is_valid`, `solids`.
- I/O: `read_step(path)`, `read_step_str`, `write_step`, `to_step_string`,
  `write_stl`.

## `model`: document API

```rust
let mut doc = Document::new("Enclosure");
doc.set_param("wall", "2")?;                       // undoable
let sk = doc.add_sketch(sketch);                   // -> SketchId
let f  = doc.add_feature("Body", FeatureKind::Extrude { .. });  // -> FeatureId
let out: &RegenResult = doc.regenerate();          // bodies + per-feature reports
doc.undo(); doc.redo(); doc.undo_label();
doc.edit("Rename", |d: &mut DocData| { ..; Ok::<_, E>(()) })?; // any change, atomic
doc.save_to_dir(Path::new("Enclosure.cwk"))?; Document::load_from_dir(..)?;
doc.bom(); doc.blob_refs();
```

- `params`: `Params::set / evaluate` (cycle detection, unknown refs), `Expr` (a
  string, used for every dimension). Expressions support `+ - * /`, parentheses,
  unary minus, units `mm cm m in " mil deg rad`, `pi`, and the functions
  `sqrt abs min max sin cos tan`.
- `sketch`: `Sketch` (points, `Geom::{Line, Arc, Circle}` with a construction
  flag, `Constraint` enum covering every requested constraint, dimensions as
  `Expr`). Helpers are `rect`, `rounded_rect`, `line` and `circle`.
  `solve(&values) -> SolveReport` runs Levenberg–Marquardt and reports converged,
  DOF, rank and `ConstraintStatus::{Under, Fully, Over, Conflicting}`. `analyze`
  does DOF analysis only. `profiles() -> SketchProfiles { profiles, open }` returns
  nested regions (outer CCW, holes CW) plus open chains.
- `dxf`: `DxfDrawing::parse` reads LINE, ARC, CIRCLE, LWPOLYLINE with bulges, and
  POLYLINE/VERTEX. It applies `$INSUNITS` and groups segments by layer.
  `layer_names`, `segments(layers)`, `circles(layers)` and
  `to_sketch(name, plane, layers)` are available; `to_sketch` adds coincident
  constraints at shared endpoints.
- `select`: features store `FaceSelector::{Hint, Extreme, AllWithNormal, Cylinders}`
  and `EdgeSelector::{Hint, OfFace, Parallel, Between, Circles}`, never raw indices.
  Build picks with `FaceSelector::from_pick(&FaceInfo)` and
  `EdgeSelector::from_pick(&EdgeInfo)`. They are resolved by best match at regen.
- `feature::FeatureKind`: `Extrude`, `Revolve` (NewBody/Join/Cut/Intersect),
  `Fillet`, `Chamfer`, `Shell`, `Draft`, `Rib`, `ScrewBoss`, `StandoffsFromPcb`
  (from DXF circles or the hole edges of a STEP board), `Pattern` (linear or
  circular, over features or a body), `Mirror`, `Split`, `ImportStep` (optionally a
  reference body), and `InsertPart`. Bodies are `BodyId { feature, index }`. Split
  keeps the lower side in the original id and makes the upper side `BodyId::of(split)`.
- `regen`: `RegenResult { bodies: Vec<Body>, reports: Vec<FeatureReport>, recomputed, reused }`.
  `Body { id, name, reference, part, geom }` exposes `shape()` and `mesh()`. The mesh
  is lazy and computed once per shape, then shared across cached states.
  `FeatureReport.status` is one of `Ok`, `Warning`, `Error` or `Suppressed`. A failed
  feature does not stop the features after it. The report also carries the
  `dressup` report and the `sketch` solve report.
  **Caching:** the state after each feature is cached under a chained hash of
  (previous key, the feature's JSON, its sketch, its import blob, and the values of
  the params it names). Undo/redo and re-edits reuse every unchanged prefix (LRU of
  512 states).
- `pick`: `Picker::new([(BodyId, &Mesh)])`.
  `pick_all(origin, dir, EdgeTolerance) -> Vec<Hit>` returns every face and edge hit
  sorted front to back, for the Tab / Alt+wheel pick stack. Faces are tested against
  a BVH over the triangles. Edges are tested against polylines with
  `tol(t) = constant + per_unit_depth·t`; use `EdgeTolerance::perspective(px, fov, h)`
  or `ortho(tol)`.
- `library`: `PartSource` trait (`parts`, `sizes`, `build`, `search`,
  `step_file`). `BuiltinSource` covers ISO 4762/7380/10642/4026, PT-style screws,
  ISO 4032/4035, DIN 557/562, ISO 7089/7092, DIN 127B, metric standoffs and inserts,
  magnets, feet, and ASME B18.3 SHCS/button/flat heads, B18.2.2/B18.6.3 nuts, SAE/USS
  washers, B18.21.1 lock washers, and inch standoffs and inserts. Sizes are written
  `M3x10` or `#4-40x1/2`. Each table is JSON, cites its standard and is interpreted
  by generic generators, so new sizes are data-only. Imperial values stay in inches
  until geometry is built. `LocalStepSource::open(name, dir)` reads a folder of STEP
  files described by `library.json`. `Library::default()` contains the built-in
  source; add others with `add`.
  `hole_rules("M3")` gives close/normal/loose clearance (ISO 273 for metric; the
  close/free drill chart for inch), tap drill, the 0.8·d pilot for thread-forming
  screws in plastic, counterbore/countersink per head, and the insert hole.
  `PartInstance::hole(HoleChoice)` returns a `HoleSpec` in mm, with `tool()` for
  cutting. `InsertPart { part: PartRef, placements: Vec<PlacementRef>, auto_hole, .. }`
  adds reference bodies (never booleaned) and optionally cuts the matching hole.
  `PlacementRef` is one of `CircularEdge`, `PointOnFace` or `Explicit`.
  `Document::part_ref(&PartInfo, size)` embeds local STEP parts as blobs.
- `export`: `write_stl` (binary, from a print-quality mesh), `write_3mf` (zip, one
  object per body, welded and watertight), `write_step` (AP214, via the kernel),
  `weld` and `open_edges`.

## Parting joints (`joint`, `FeatureKind::PartingJoint`)

`PartingJoint { body, plane, offset, kind, rim, upper_name, class, closing_radius }`
splits a hollow body at `plane` (moved along its normal by `offset`). The body keeps
the lower half, and the upper half becomes `BodyId::of(feature)`. The results are in
`FeatureReport.joint: JointReport`:
- `wall_thickness` and `effective_wall`;
- `rim_added`;
- `clearance_per_side`, the result after fits;
- `checks` (name, value, limit, ok);
- `warnings`;
- `oring`, with squeeze, fill, min corner radius, and the suggested depth/width;
- `snap`, with strain, allowable, forces, pass and the suggested length;
- `snaps`, one placement per requested position: where it went and why it moved or
  was skipped.

A case that cannot be supported ends the feature with `FeatureStatus::Error(reason)`.
Joint warnings show up as `FeatureStatus::Warning`. Neither leaves an invalid solid.

**Recipe.** Everything is built from planar regions at the split plane, using
uniform 2D offsets (`BRepOffsetAPI_MakeOffset`) and prisms. Nothing is swept.

- `R`: the wall section ring, i.e. the section face that holds the outer contour.
  Separate sections (free bosses) are ignored and simply split.
- `t`: the wall thickness, `t ≈ 2·area(R) / perimeter(R)`.
- `E` and the cavity. For an unbroken ring, `E` is `R` with holes filled and the
  cavity is `E − R`. For a ring broken by a cutout (USB slot), cutouts are bridged
  with a closing of radius ρ ≥ 12 built from single-boundary offsets:
  `E = fill(R⊕ρ)⊖ρ`. The gaps `G` are the pieces of `closing − R` that reach the
  outside, and the cavity is `E − R − G`.
- Rim thickening: `R' = (R ⊕ (rim_width − t)) ∩ E − G`. It grows the wall inward
  only and never fills a cutout. It is prismed `rim.height` up and down from the
  plane, and extended on the window half for snap fits.
- `band(w) = R' ⊖ ((t' − w)/2)` is a centred band of width `w`. Bosses or ribs merged
  into the wall make it bulge there; the clearance stays constant.
- A section with no cavity (the plane cuts the floor or lid), a cut that would leave
  less than `MIN_WALL` (0.4 mm) of floor or lid, or a band that does not fit each
  give an error with a suggestion (e.g. the rim width needed).

```
Tongue & groove (tongue_on = Lower)     Lip (lip_on = Lower)
   upper  |▓▓▓|   |▓▓▓|                    upper |▓▓▓▓▓|  ·
          |▓▓▓|___|▓▓▓|  groove = band(Hole(w+2c))    |▓▓▓▓▓|__·  recess = R'∩(cav⊕Hole(w+c))
          |▓▓▓| ┌─┐ |▓▓▓|  depth = h + Gap(extra)       |▓▓▓▓▓|┌─┐
 ─ plane ─┴───┘ │ │ └───┴─                ─ plane ─────┘     │ │  lip = R'∩(cav⊕Pin(w)), height h
          |▓▓▓▓▓│ │▓▓▓▓▓|  tongue = band(Pin(w)), h  lower |▓▓▓▓▓▓▓▓|
   lower  |▓▓▓▓▓▓▓▓▓▓▓▓▓|                         outside ←      → cavity

O-ring face seal (gland_on = Lower)     Snap fit (beam_on = Lower)
   upper  |▓▓▓▓▓▓▓▓▓▓▓|                  upper |▓▓| window (through wall) ┐
          |▓▓▓▓▓┌┐▓▓▓▓|  optional compression rib  |  |◄hook  ledge at z1, ramp α   │ pocket
 ─ plane ─┴─────┘└────┴─                          |▓▓| ║ beam t×b, length L     ┘
          |▓▓┌──────┐▓▓|  gland = band(Hole(w)), depth d(1−squeeze)+rib   ─ plane ─┴──┴─╫─
          |▓▓│  ()  │▓▓|  w = πd²/4 /(fill·depth)        lower |▓▓▓▓|╫ root bonded to the wall
   lower  |▓▓▓▓▓▓▓▓▓▓▓|                                         outside   → cavity
```

- **Tongue & groove.** Tongue = band(Pin(w)) of height h, with an optional lead-in
  chamfer. Groove = band(Hole(w + 2c)) of depth h + Gap(extra). The per-side
  clearance is (groove − tongue)/2.
- **Lip.** The lip is the inner `Pin(w)` of the wall (measured from the cavity), of
  height h. The recess is the inner `Hole(w + c)`, of depth h + Gap(c). The outer
  wall at the recess must keep ≥ `MIN_WALL`.
- **O-ring gland.** The cord comes from `data/joints/orings.json` (AS568 and metric
  cross-sections, Parker-style face-seal guidance with TODOs). Depth is
  `d(1 − squeeze) + rib_h`, and width keeps fill at the target (≤ 85 %). An optional
  compression rib sits on the mating half. Warnings, never failures, cover squeeze
  outside 15–30 % (with suggested depth × width), fill above 85 %, and a gland corner
  radius below 3 × cord (sharp corners report 0).
- **Snap fit.** Positions come from `Count(n)` or `At(fractions)` along the outline.
  Each position is moved along the outline until the wall is straight over the beam
  span ± margin, has uniform thickness (no boss or rib merged in), and is not a
  cutout. The beam's keep-out volumes must also be free of material in both halves:
  bosses, ribs, floor and lid. The beam sits against the inner wall (or the inner
  rim), with its root bonded below the plane. The hook protrudes outward into a
  window through the other half's wall, which is clearance `c` all round, plus a
  beam pocket. The calculation uses ε = 1.5·t·y/L², P = b·t²·E·ε/(6L) and
  W = P(μ + tan α)/(1 − μ·tan α). Materials (PLA, PETG, ABS, PC, PA6, PP) are in
  `data/joints/snap_materials.json`. An over-strained beam is still built, with a
  warning giving the passing length.

The test matrix in `crates/model/tests/joints.rs` covers rounded rects
(r 0.5/3/6, with 0–3° draft), an L-shape, a DXF outline with convex and concave
arcs, a plane crossing a boss, a rib and a USB slot, 2 mm walls with rim vs 4 mm
without, planes near the floor or lid, and the O-ring and snap checks. Each kind
must give two valid solids with no interference, watertight meshes, and the reported
clearance, checked by shifting one half 0.8× (no contact) and 1.6× (contact) of it.

## Process profiles, fits, teardrops, build orientation

- `process::ProcessProfile` has `name`, `kind` (`Fdm/Sla/Sls/Molded/Cnc`),
  per-side `fits` for `FitClass::{Press, Snug, Slide, Loose}`,
  `hole_comp { base, per_mm }`, `xy_comp`, `first_layer_comp`,
  `first_layer_chamfer` and `teardrop`. The built-ins are "FDM 0.4 generic", "SLA
  generic" and "Injection molded (nominal)" (all zeros). Their numbers are starting
  points to calibrate. User presets are loaded with `load_presets(dir)` and written
  with `save_preset(dir)`.
- Dimensions are `Expr(text, Option<Fit { role, class }>)`. Resolution is per side:
  `Hole` gives `x + 2·allow + hole_comp(x)`, `Pin` gives `x − 2·allow − 2·xy_comp`,
  `Gap` gives `x + allow`, and `Nominal` gives `x`. In a printed-to-printed mate,
  each part takes its own share. Features use default roles: boss/standoff holes and
  auto-holes are Hole/Snug, joint grooves/glands/pockets are Hole, tongues/lips/hooks
  are Pin, and the parting-line clearance is Gap. Set a fit on the `Expr` to
  override, or `Fit::NOMINAL` to opt out. Expressions can call `hole(x[, class])`,
  `pin(x[, class])`, `gap([class])` and `tol([class])`; the class defaults to slide.
- `DocData.active_profile` (new documents default to the nominal molded profile) and
  `DocData.processes`. Use `Document::set_active_profile`, `add_profile`,
  `regenerate_with_profile`, and `DocData::values()` (params under the active
  profile). Every cached state is keyed on the profile's fit numbers, so switching
  back hits the cache.
- Build orientation is `BuildOrientation { down: CubeFace, quarter_turns }`, with
  `quat()` mapping model to build coordinates (plate face to −Z). It is set per
  document or per body with `Document::set_build_orientation(Option<BodyId>, _)`.
- Teardrops run as a final process stage after all features. They apply to full
  round internal holes whose axis is within `angle_threshold_deg` of horizontal
  (relative to build-up) and whose diameter is at least `min_diameter`. The stage
  cuts a prism over the hole's axial extent: 45° tangent lines meet r·√2 above the
  centre (`Pointed45`), or are cut flat at `fraction·r` (`Flat`). The lower half
  stays exactly round. Overrides are `DocData.teardrop_overrides`. The outcome is in
  `RegenResult.process` (`teardropped`, `skipped` with reasons, `warnings`). The
  stage is skipped entirely for profiles with teardrops disabled (molded, SLA), and
  changing orientation re-runs only this stage.
- `Document::export(ExportFormat::{Stl, ThreeMf, Step}, path, &ExportOptions { profile, orient_to_plate, include_reference })`.
- `process::calibration_coupon(nominal, offsets)` builds a plate of
  `nominal + offset` holes plus nominal pins, with index notches. Default offsets
  run −0.10…+0.50 in 0.05 steps. Feed the results to
  `ProcessProfile::from_calibration(&CalibrationResult)`.

## File format (`.cwk`), sync-friendly

A `.cwk` is a folder containing `doc.json` and `blobs/<sha256>`. `doc.json` holds
`DocData`: `format`, a UUID `doc_id`, a monotonically increasing `revision` (bumped
on every change, including undo/redo), `modified_by_device` (from
`CASEWORK_DEVICE_ID`, then `HOSTNAME`), params, sketches, features, imports and meta.
It is written deterministically as pretty JSON with fixed field order. Lists replace
maps, so diffs and three-way merges are practical. Imported STEP/DXF files and
embedded library STEP parts are content-addressed blobs referenced by hash
(`Document::blob_refs()`). They are written atomically, only when missing, and
verified on load.

## Known limitations

- Topological naming is geometric. `Hint` selectors accept the best candidate only
  if it is within 25 % of the part diagonal, within 20° of the stored
  normal/tangent, and of the same surface/curve type. Otherwise the feature reports
  `FeatureStatus::Unresolved`.
- OCCT's limits apply. Shell fails when an inward thickness exceeds a concave
  fillet radius, and draft on faces adjacent to fillets can fail. Both return
  `FeatureStatus::Error`; nothing panics.
- Ribs have an explicit height and do not extend to meet the body. Joints between
  segments are rounded.
- Pattern/Mirror of features re-applies recorded tool bodies (extrude, revolve, rib,
  boss, auto-hole and pattern). Dress-up features (e.g. boss base fillets) are not
  replayed, and InsertPart reference bodies are not copied (BOM counts placements).
- Threads are cosmetic. Several data rows carry `TODO`s (surfaced in
  `PartInfo::notes`): imperial inserts and standoffs, PT screw heads, some ISO 7380
  lengths, M2/M2.5 counterbores, the inch loose-fit class (ASME B18.2.8) and #2
  button/flat heads. Counterbores for non-socket heads and all countersinks are
  derived from head dimensions (`derived: true`).
- STEP import is geometry only (no XCAF colours or names).
- The DXF reader has no SPLINE, ELLIPSE, blocks/INSERT or binary DXF; skipped
  entities are counted in `DxfDrawing::skipped`.
- The solver uses a numeric Jacobian, which is fine up to a few hundred variables.
  Arc–arc tangency picks internal or external from the configuration at solve start.
- 2D drawings are not implemented yet.
- Drafts that would shrink a convex corner radius below zero over the height now fail
  ("draft angle too large"); OCCT would otherwise build a degenerate cone.
- `Shape::offset_solid` (hollowing by 3D offset) fails when the offset exceeds a
  convex radius. It errors and never crashes.

## Changelog for GUI

- **Selector confidence (breaking).** `FaceSelector::Hint` and `EdgeSelector::Hint`
  gained `kind: Option<FaceKind / EdgeKind>`. It is serde-defaulted, so old files
  load; build hints with `from_pick`. `SelectError::NoMatch` was replaced by
  `SelectError::Unresolved { selector, best, best_index, score }`. There is a new
  `FeatureStatus::Unresolved(Box<Unresolved { selector, best_candidate, score }>)`:
  show it orange with "re-pick", and accept the candidate with
  `Document::accept_candidate(feature, &selector, &best_candidate)` (undoable,
  label "Re-pick reference"). `select::replace_selector` does the same on any
  value. A score ≤ 1 is accepted.
- **Process profiles (breaking).**
  - `params::Expr` is now `Expr(String, Option<Fit>)`. Reading `.0` still works;
    construct it with `Expr::new`, `From<&str/f64/i32>` and `.fit(..)`. Its JSON is
    still a plain string unless a fit is set.
  - New `Expr::eval_or` and `eval_nominal`, and `Params::evaluate_seeded`.
  - `AutoHole` gained `fit: Option<Fit>`.
  - `DocData` gained `processes`, `active_profile`, `build_orientation`,
    `body_orientations` and `teardrop_overrides` (all serde-defaulted).
  - `RegenResult` gained `process: ProcessReport`.
  - `occt::FaceInfo` gained `point`.
  - New module `process`; see the section above.
- **Parting joints.**
  - New `FeatureKind::PartingJoint` and module `joint` (`JointKind`, `Half`, `Rim`,
    `SnapPositions`, `JointReport`).
  - `FeatureReport` gained `joint: Option<JointReport>`.
  - New `occt::Shape` methods: `section`, `offset_region`, `fill_holes`,
    `region_contains`, `outer_wire_length`, `outer_wire_point`, `offset_solid`.
  - `Shape::split` now returns kernel errors instead of silently returning `None`.
