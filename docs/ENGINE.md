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

- Topological naming is geometric. A `Hint` selector always takes the best match,
  so after drastic edits it can land on the wrong face or edge. Semantic selectors
  (`Extreme`, `Parallel`, `OfFace`, ...) are more robust; there is no "could not
  find" threshold yet.
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
- 2D drawings are not implemented yet (phase 2).
