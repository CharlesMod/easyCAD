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
process. No call writes into its input shapes (see "Concurrency contract"), and
booleans run in non-destructive mode.

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
- `shell(open_faces, thickness, outward)` (with no open faces: a closed hollow solid,
  i.e. the outer skin plus an internal void shell, or an error naming the failed
  offset) and
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
  `sqrt abs min max sin cos tan`. A fraction of integer literals binds to a
  following unit: `1/8"` = 3.175, `3/4 in`, mixed `1 1/2"` = 38.1, and `0.125in`.
  Without a unit (`10/4`), or with a non-literal (`w/2in` = w ÷ 2 in), `/` is plain
  division.
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

## 2D drawings (`drawing`)

- `DocData.drawings: Vec<Drawing>`. A `Drawing { id, name, sheet, views, annotations }`
  has `add_view` and `annotate`. Document helpers are `Document::add_drawing`,
  `edit_drawing(id, label, |d| ..)` (undoable), `drawing_result(id) -> DrawingResult`
  and `export_drawing(id, path)` (format by extension: `.svg`, `.dxf` or `.pdf`).
- **Sheet:** `PaperSize::{A4..A0, AnsiA..AnsiE}` × `PaperOrientation` plus a margin.
  The `TitleBlock` fields are templates using `{param}`, `{meta.key}`, `{doc.name}`,
  `{doc.revision}` and `{sheet.scale}`. They are resolved live with
  `drawing::substitute`, and so is note text.
- **View:**
  - `orientation` is `Face(CubeFace)` (front = `NegY`, top = `PosZ`),
    `Iso(IsoCorner { x, y, z: ±1 })` or `Rotation(DQuat)`. The quaternion maps model
    to view, with view +Z toward the viewer.
  - Also `rotation_deg` (in-sheet), `scale`, `position` (the sheet mm the view's
    bounding box is centred on), `hidden_lines`, `exact` (HLRBRep_Algo, or
    PolyAlgo; the exact algorithm falls back to PolyAlgo automatically and reports
    `poly_fallback`), `smooth_edges`, and `bodies` (empty = all solids).
  - Projection uses `ViewFrame` (`sheet(p3)`, `local`, `sheet_dir`), and
    `ViewOrientation::basis()` gives (toward viewer, right, up).
- **HLR:** `occt::Shape::hlr(dir, x_dir, exact, defl)`. Each `ViewEdge` has sheet
  points, a visible flag, a kind (`Sharp`, `Smooth` or `Outline`) and
  `source: Option<(BodyId, edge index)>`. Sources are found by matching against
  analytically projected model edges (lines, circles/arcs, mesh polylines for other
  curves). When edges coincide in projection, visible output takes the frontmost
  and hidden output the backmost. Results are cached per (body geometry,
  orientation, algorithm) in the document.
- **Annotations** (`AnnotationKind`) reference geometry by `(ViewId, DimRef)`, where
  `DimRef::Edge { body, edge: EdgeSelector }` or `DimRef::Vertex { body, near }`.
  The kinds are:
  - linear `Horizontal`/`Vertical`/`Aligned` (two refs, or one straight edge);
  - `Ordinate`;
  - `Radius` / diameter (the circle must be face-on);
  - `Angle` (two straight edges);
  - `Chamfer` ("leg × angle°");
  - `Note`;
  - `Leader`;
  - `Balloon` (numbered by the body's row in `Document::bom()`);
  - `CenterMark`;
  - `Centerline`;
  - `PartingLine` (from a `PartingJoint`/`Split` feature's plane, which must be seen
    edge-on).

  Each `AnnotationResult` has `status` (`Ok` or `Dangling(reason)`), `value` (model
  mm or degrees, already divided by the view scale), `text` and `prims` (polylines
  with a `LineStyle`, circles, texts, arrows in sheet mm). Dangling annotations stay
  in the document and recover if the geometry returns (e.g. on undo).
  `text_override` replaces the shown text.
- **Export:** `drawing::primitives(result, drawing)` gives everything to draw;
  `to_svg` (mm viewBox), `to_dxf` (R12 LINE/CIRCLE/TEXT/SOLID on the layers
  VISIBLE, HIDDEN, DIM, CENTER, PARTING, FRAME and TEXT, `$INSUNITS` = mm) and
  `to_pdf` (one vector page via the `pdf-writer` crate, base-14 Helvetica with
  WinAnsi for ⌀ ° ×). The GUI can render `DrawingResult` directly.

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

## Performance

Timings for the test enclosure in `crates/model/tests/timing.rs` (6 features: shell
cavity, 4 filleted bosses, rib, tongue & groove joint with rim; best of 5, release,
4-core container shared with a GUI build). Run them with
`cargo test -p casework-model --release --test timing -- --nocapture`.

| step | before | after |
|---|---|---|
| full regen (cold) | 443 ms | 301 ms |
| param edit `wall` → regen | 481 ms | 345 ms |
| edit of the last feature only | 345 ms | 225–254 ms |
| undo → regen (cache) | 0.2 ms | 0.13 ms |
| tessellation, Display (12k tris) | 79 ms | 29 ms |
| tessellation, Final (24k tris) | 148 ms | 46 ms |
| tessellation, Preview (4.5k tris) | — | 19 ms |

What changed:
- Joints defer their booleans to one fuse and one cut per half, and run the split,
  the ring analysis and the two halves on two threads.
- Booleans and BRepMesh use OCCT's parallel mode and oriented bounding boxes; set
  `CW_SERIAL_BOP=1` to compare against serial.
- Meshing starts from a clean triangulation, so each `MeshQuality` (Preview,
  Display, Final) gets its own density, computed once per shape.
- `FeatureReport.elapsed_ms` gives per-feature cost (the joint dominates).
- Unchanged bodies keep their geometry `Arc`, and with it their meshes, across edits.

Regenerating independent bodies in parallel is not done. Features form one cached
chain, and most time sits in single large booleans.

## Robustness

- Hardware signals inside the kernel (null dereferences in OCCT algorithms on
  degenerate input) are converted to errors. The shim installs OCCT's signal handler
  (`OSD::SetSignal`) and every entry point runs under `OCC_CATCH_SIGNALS`; OCCT is
  built with `OCC_CONVERT_SIGNALS`. A crash in, for example, MAT2d inside
  `BRepOffsetAPI_MakeOffset` therefore becomes `FeatureStatus::Error`. The 2D offset
  also cleans faces first and retries, which removed the one crash the randomized
  test found. Faults outside kernel calls are forwarded to the previously installed
  handler, so a Rust stack overflow is still reported by Rust (see "Concurrency
  contract").
- `crates/model/tests/fuzz.rs` runs 200 random enclosures with a fixed seed,
  varying size, wall, fillet radius, draft, bosses, joint kind and params, rim,
  fit class and profile. It checks for no panic, valid solids, and a reason for every
  refusal. In the current run, 48 joints are built and 152 are refused, all with a
  reason.

## Concurrency contract

Everything in `occt` and `casework-model` may run on any number of threads at once.
The GUI's global kernel mutex is no longer needed.

- **Shapes** (`occt::Shape`, `Body`, `RegenResult`) are `Send + Sync` and may be used
  from several threads at the same time: mesh a body on the render thread while the
  regen worker booleans it and an export thread writes it.
- **Documents** (`Document`, `Regenerator`) are plain `&mut` values. One thread per
  document at a time (Rust enforces this). Separate documents regenerate in
  parallel.
- **Internal lock:** STEP/XCAF reading and writing (`read_step*`,
  `write_step`, `to_step_string*`, `Document::import_step`'s regen, STEP export)
  is serialised by one process-wide mutex in the shim. OCCT keeps translator
  parameters (`Interface_Static`) and the XCAF application in unsynchronised globals.
  Other kernel work is not blocked while a STEP file is being read.
- **One-time setup** (signal handlers, silencing OCCT's messenger) runs before the
  first kernel call on any thread, under a C++11 static initialiser.
- **Signals.** A hardware fault inside a kernel call becomes `occt::Error`
  ("OSD_SIGSEGV ..."), on whichever thread it happens. Faults outside kernel calls go
  to the handler that was installed before OCCT's: Rust's stack overflow report, or
  the default crash. OCCT's own handler would call `exit(1)` from inside the
  signal handler. Handlers run on the alternate signal stack, so a stack overflow
  inside the kernel is converted as well.

**Root cause of the parallel SIGSEGV (GUI_NEEDS #10).** OCCT keeps mutable caches inside the
shared topology objects (TShapes). Examples are triangulations and edge polygons
(written by `BRepMesh` and `BRepTools::Clean`), and tolerances and pcurves that
ShapeFix, offsets and the thick-solid algorithm update in place. Every boolean reads
triangulations through `BRepBndLib::Add` (in `BOPDS_DS::InitShapes`), as do HLR
PolyAlgo and the STL writer. Meshing a shape while another thread booleaned, meshed,
hid or STL-exported the same shape freed a `Poly_Triangulation` under that thread. The
shared-shape stress test reproduced it in 5 of 6 runs (SIGSEGV converted to errors,
"cannot write STL", `BOPAlgo_AlertIntersectionFailed`). Separate documents shared
nothing, so they were not the trigger; the regen worker's shapes being meshed for
display on another thread were. Every operation that could write into topology now
works on a private `BRepBuilderAPI_Copy` of its input (new TShapes, shared immutable
geometry). Index order is preserved, so face and edge indices are unchanged. A side
effect is that boolean bounding boxes no longer depend on whether a body had been
displayed, so a regen is deterministic. Each mesh call also meshes at exactly its own
density. In an A/B run of `tests/timing.rs` the copy made no difference beyond noise.

**The last writer: exact HLR.** With the copies above, the shared-shape test still
failed in about 2 of 5 runs with `OSD_SIGSEGV` (small addresses, i.e. a null
member) inside `Shape::hlr(exact = true)`. `HLRBRep_Algo::Add` was the one remaining
algorithm that ran directly on the shared input, and its outliner/edge data build
writes into the input topology. It now also loads a private copy.

**What is still locked, and why.** Only STEP/XCAF translation (global translator
parameters, see above). No other lock exists in `occt` or `casework-model`; the old
mesh mutex is gone. If a future kernel entry point is found to write into its input,
the fix is the same `privateCopy` in the shim, not a caller-side lock.

**Evidence.** `crates/model/tests/concurrency.rs` has two tests.
`parallel_documents_regen_mesh_hlr_export` runs 6 threads × N rounds of
regen, 3 mesh levels, exact, hidden-line and poly HLR, STEP/STL/3MF/SVG/PDF export, STEP
re-import and assembly re-import, with every result compared to a serial reference.
`shared_shapes_across_threads` runs 8 threads on the same `Shape`s, doing mesh, STL,
HLR, boolean, STEP round trip, topology queries and section. Before the fix the
shared test failed in 5 of 6 runs; with the copies but exact HLR still on the input,
in 2 of 5. After the fix it passed 20 of 20 runs at the default rounds and 20 of 20
with `CW_STRESS_ROUNDS=6`, and the whole file (both tests) passed 10 of 10 runs. ASan or TSan
was not run: both need the static OCCT build and the Rust side instrumented together
(nightly `-Zsanitizer` plus an OCCT rebuild in a separate target directory).

**Rules for callers (the GUI).**
- Drop the global kernel mutex. Run regen, meshing for display, drawings and exports
  on whatever threads are convenient.
- Never hold `&mut Document` on two threads (Rust stops you anyway). To regen in the
  background, move or clone the `DocData` + `BlobStore` into the worker (or own the
  `Document` there) and send `RegenResult`s back; they are cheap `Arc` clones.
- A converted kernel fault (`occt::Error("OSD_SIGSEGV ...")`) leaves the process
  healthy; report it like any other feature error.

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
- Ribs use `extent: RibExtent::{Height, ToBody}`. `ToBody` grows the rib along its
  direction and trims it where it first meets the body; with draft it is rebuilt to
  that height. A rib that never meets the body, or starts inside it, is an error.
  Joints between segments are rounded.
- Pattern/Mirror re-runs the source feature's recipe under the transform: extrude,
  revolve, rib, screw boss, standoffs, inserted parts and nested patterns. Boss
  fillets, part copies (counted in the BOM), fits and teardrops therefore all follow.
  Whole-body operations (fillet, shell, draft, split, joint) cannot be patterned and
  give a clear error.
- Threads are cosmetic. Several data rows carry `TODO`s (surfaced in
  `PartInfo::notes`): imperial inserts and standoffs, PT screw heads, some ISO 7380
  lengths, M2/M2.5 counterbores, the inch loose-fit class (ASME B18.2.8) and #2
  button/flat heads. Counterbores for non-socket heads and all countersinks are
  derived from head dimensions (`derived: true`).
- STEP import goes through XCAF and keeps per-solid names and colours. A reference
  import is one locked body whose `Body::appearance()` lists sub-parts with their
  names and colours. Non-reference imports give one named body per solid. STEP
  export writes body and sub-part names and colours. Layers, face-level colours and
  PMI are not read.
- The DXF reader has no SPLINE, ELLIPSE, blocks/INSERT or binary DXF; skipped
  entities are counted in `DxfDrawing::skipped`.
- The solver uses a numeric Jacobian, which is fine up to a few hundred variables.
  Arc–arc tangency picks internal or external from the configuration at solve start.
- Drawings have no GD&T frames, projected/auxiliary view relationships, section views or
  hatching. Dimension text placement is basic (no collision avoidance). PDF text
  width is estimated.
- Drafts that would shrink a convex corner radius below zero over the height now fail
  ("draft angle too large"); OCCT would otherwise build a degenerate cone.
- `Shape::offset_solid` (hollowing by 3D offset) fails when the offset exceeds a
  convex radius. It errors and never crashes.

## Changelog for GUI

- **Fractional inches (GUI_NEEDS #12a).** The parser reads `1/8"`, `3/4 in` and
  `1 1/2"` as fractional inches (previously `1/8"` was 1 ÷ 8 in).
  `units::normalize_input`'s rewrite still evaluates the same, so it can be dropped.

- **Shell with no open faces (GUI_NEEDS #5).** `FeatureKind::Shell { open_faces: [] }`
  now gives a closed hollow solid (one solid, two shells: skin + void). When the
  inner offset fails (wall thicker than the body or a convex radius) the feature is
  `FeatureStatus::Error("shell with no open faces: ...")`. The sample's "shell open
  bottom + floor plate" workaround is no longer needed.

- **Concurrency (GUI_NEEDS #10).** Kernel and model calls are safe on any number
  of threads; see "Concurrency contract". The GUI's global kernel mutex can go.
  Meshing no longer writes into the shape (each `MeshQuality` meshes a private copy),
  so boolean results no longer depend on whether a body was displayed first.

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
- **Drawings.** New module `drawing`.
  - `DocData.drawings` (serde-defaulted).
  - New `Document::{add_drawing, edit_drawing, drawing_result, export_drawing}`.
  - New `occt::Shape::hlr` with `occt::{HlrEdge, HlrKind}`.
- **Phase 3.**
  - `FeatureKind::Rib` gained `extent: RibExtent` (serde default `Height`). I added
    `extent: Default::default()` to the two GUI struct literals (dialogs/mod.rs,
    sample.rs) but did not commit them.
  - `Document::bom()` now takes `&mut self` and counts the placed part bodies of the
    regenerated model.
  - `Body::{mesh_quality(MeshQuality), appearance(), face_colors(), mesh_colors(q)}`,
    plus `regen::{MeshQuality, Appearance, SubPart, DEFAULT_COLOR}` and
    `BodyGeom::with_appearance`.
  - `FeatureReport.elapsed_ms`.
  - `occt::{StepPart, Shape::read_step_xcaf, to_step_string_named, face_owners}`.
  - Patterns no longer use `ToolOp.tool` (internal).
