# Casework GUI (`crates/app`)

Desktop front end on eframe/egui 0.37 with the wgpu backend.

```
cargo run -p casework --release              # empty document
cargo run -p casework --release -- --sample  # the sensor-node enclosure
cargo run -p casework --release -- path/to/Enclosure.cwk
cargo run -p casework --release -- --smoke /tmp/cw-smoke   # file I/O smoke test, exit code 1 on failure
cargo run -p casework --release -- --sample-board /tmp/b   # the sample PCB as STEP + DXF, for the wizard
```

Display units: the `mm` chip in the title bar switches to inches (display only; the
document stays mm). Fields accept `0.125in`, `1/8"`, `1 1/2"`, `3mm`; in inch
mode a bare number means inches.

## Architecture

| module | role |
|---|---|
| `app.rs` | State, frame loop, shortcuts, file ops, regeneration plumbing, `UiCache` (derived data recomputed only when the revision or model changes). |
| `worker.rs` | Background thread: owns the `Regenerator`, an `AssemblyCache` and a `DrawingCache`, tessellates, builds the pick BVH and face/edge topology, computes drawing sheets. Latest request per purpose wins (Model, Preview, Drawing). The UI never regenerates. For assemblies it solves the mates, runs `regenerate_assembly`, returns the active component's bodies as `bodies` (its frame is the view frame) and every other placed body as a `CtxBody` (shared mesh, per-mesh picker, `xf` = component → view frame, explode applied). |
| `viewport/gpu.rs` + `*.wgsl` | wgpu renderer behind an `egui_wgpu` paint callback: `prepare` renders an offscreen 4×MSAA colour+depth target, `paint` blits it into egui's pass. Meshes are cached per `Arc<Mesh>` (uploaded once per shape), triangles sorted by face so highlights / x-ray are ranged draws. Grid, edges, translucent reference bodies. |
| `camera.rs` | Quaternion orbit camera: Shift+MMB orbit, MMB pan, wheel zoom to cursor, fit, 26 view-cube regions, 15° steps (world or screen axes), home view, 250 ms eased transitions. |
| `viewcube.rs` | The cube: 6 faces × 3×3 hit zones with narrow bands, hover highlight, click, drag-orbit, context menu (home, projection, fit on change, step frame), step buttons. |
| `pickstack.rs` | Filtered, de-duplicated candidates front to back; Tab / Shift+Tab / Alt+wheel cycle; index survives small moves. |
| `ui_main.rs` | Title bar, toolbar, browser, Parameters panel, history strip, status bar, new-document prompt. |
| `ui_process.rs` | PROCESS section: profile, fit allowances, build plate picker + preview, teardrops, calibration. |
| `ui_viewport.rs` | Viewport input, pick popover, overlays, floating dialogs. |
| `dialogs/` | Feature dialogs (fillet/chamfer with self-heal report, extrude, revolve, shell, draft, split, boss, rib, pattern, mirror, standoffs, parting joint, export). |
| `sketch_mode.rs` | 2D sketch editor with tools, constraints, live solver, DXF layers. |
| `ui_parts.rs` | Parts library, placement on hole edges, BOM. |
| `ui_drawing.rs` | Drawing sheets on the engine's `drawing` module. |
| `adapters.rs` | Workarounds for engine gaps (see `GUI_NEEDS.md`). |
| `units.rs` | mm / in display and input normalization. |
| `files.rs` | File choosers: the system dialog (rfd / XDG portal) first; when none can open (no D-Bus session or portal, remote / headless) an in-app path prompt prefilled with the last folder. `CASEWORK_FILE_DIALOG=builtin` forces the prompt. |
| `e2e_state.rs` | `CASEWORK_STATE_FILE`: JSON dump of what the user sees, for the E2E scripts. |
| `ui_welcome.rs` / `wizard.rs` | Empty-document welcome (New enclosure from PCB… / Open / Sample / recent files) and the PCB wizard: reads an EasyEDA STEP or DXF (outline, mounting holes, board z) and builds ordinary features: offset profile → extrude → top-edge fillet → closed shell → board + standoffs → parting joint, all on named parameters. |
| `settings.rs` / `ui_settings.rs` | Per-user settings (navigation preset: Fusion/Casework, SolidWorks, Onshape, Blender; default process for new documents; units; mesh quality; autosave interval) and their window (File ▸ Settings…, Ctrl+,), the `?` cheat sheet, the recovery offer. |
| `recovery.rs` | Autosave: a dirty document is copied every N minutes (off the UI thread) to `$XDG_DATA_HOME/casework/recovery/<doc id>/` (a `.cwk` + `info.json` with the pid). Saving removes it; closing unsaved writes a last one. At start, copies whose process is gone are offered (Restore / Discard / Decide later; Enter restores the newest). `CASEWORK_AUTOSAVE_SECS` overrides the interval (E2E). |
| `ui_assembly.rs` | Assemblies: the component tree in the browser (nested occurrences, expand/collapse, ×N instance badge, eye, ground pin, linked-file status with Update / Break link, lock on references, drag a row onto another to reparent, context menu: activate, isolate, move, ground, new / insert component, place another, make independent copy, remove), activation (double-click / Activate / breadcrumb; Esc goes up), the Mates folder (orange when unsolved or over-constrained). |
| `scope.rs` | Editing inside the active component: dialogs, sketch mode, history strip and Parameters panel see `DocData::component_view`; `scope::edit` runs a change on that view in one undoable edit and writes the component back. |
| `ui_mates.rs` | Mates dialog: type pills, snap glyphs under the cursor (face centre + normal, circle / cylinder axis, straight-edge axis, component origin), live solve (the draft rides along with every model request), jog slider for the free motion, OK = mate + solved placements in one undo step. |
| `ui_gizmo.rs` | Move gizmo (G) on an unconstrained occurrence: translate arrows and rotation rings in the view frame, 1 mm / 15° snapping (Shift: free); live through a model-matrix override, one undo step on release. |
| `ui_interference.rs` / `ui_bom.rs` | Interference (I): pairs with volumes off the UI thread, the pair tinted red, zoom-to. BOM window: indented / flat, made vs purchased, CSV. |
| `smoke.rs` | `--smoke`: save/reopen, import STEP/DXF, export STEP/STL/3MF and drawing PDF/DXF/SVG through the dialogs' own functions. |

## Assemblies

A document with occurrences shows the component tree in the browser. Double-click a
component (or Activate) to edit it in place: the rest dims to 30 %, the history strip,
Parameters (its local params first, then the document's), dialogs and sketches work
on that component, the title bar shows `sensor-node / Base`, Esc goes back up. The
view frame is the active component's frame, so overlays and picks need no changes;
the camera keeps its world pose across activation. "New enclosure from PCB" makes an
assembly by default (Enclosure master, hidden and out of the BOM → Base (grounded) +
Lid in context; a STEP board as a locked, purchased reference). See GUI_NEEDS #17
for the design-position anchors that let in-context components move.

## Keyboard

| key | action |
|---|---|
| Ctrl+Z / Ctrl+Shift+Z (Ctrl+Y) | undo / redo |
| Ctrl+N / O / S / Shift+S / I | new / open / save / save as / import |
| S, E, R, H, B, J, M, P, D | sketch, extrude, revolve, shell, boss, joint, mirror, parts, drawing |
| F | fit (or fillet when edges are selected) |
| Tab / Shift+Tab, Alt+wheel | cycle the pick stack |
| Home | home view |
| Alt+↑/↓, Alt+PgUp/PgDn, Alt+←/→ | 15° steps about X / Y / Z (world by default; cube menu switches to screen axes) |
| 1 / 2 / 3 | shaded / shaded + edges / wireframe |
| Enter / Esc | dialog OK / cancel |
| ? | keyboard cheat sheet |
| A / G / X / I | assemblies: new mate / move gizmo / explode slider / interference check |
| Esc (nothing selected) | leave the active component (up one level) |
| Ctrl+, | settings |
| sketch: L R C A F O P T D | tools; type a number or `=expr` while drawing |

## Tests

```
cargo test -p casework                 # unit + snapshot + GPU tests
```

Snapshots (written, not compared) land in `crates/app/tests/snapshots/`. They need
a wgpu adapter; in a container install `mesa-vulkan-drivers` (lavapipe). The
snapshot tests serialize themselves (one GPU, one kernel).

## Click-through E2E (`crates/app/e2e/`)

The release binary under Xvfb, driven by xdotool, asserting on the state dump with
jq and saving screenshots to `e2e/shots/` (a selection is committed).

```
cargo build --release -p casework
crates/app/e2e/run_all.sh          # or one scenario: bash crates/app/e2e/d_sketch_extrude.sh
```

| scenario | covers |
|---|---|
| a | navigation: orbit, pan, zoom, view cube, steps |
| b | pick stack: Tab cycling, hidden faces |
| c | parameter edit, live regen, Ctrl+Z / Ctrl+Shift+Z, bad expression |
| d | sketch on the floor (picked through the PCB), typed rectangle, Fix, extrude-cut |
| e | fillet: pick edges, radius too big, "Use r_max", OK |
| f | parting joint replacing the sample's Split: drag the plane handle, O-ring, OK |
| g | process FDM ↔ molded, teardrop toggle / style, undo |
| h | drawing: add a dimension, Export PDF through the in-app path prompt |
| i | first run: welcome card, New enclosure from PCB (DXF via Browse…), Lip, Create, edit `wall` |
| j | assemblies: wizard with the STEP board → Enclosure master, Base, Lid, board; select the Lid, G, drag the Z arrow, undo; A, snap a revolute hinge on the front seam (Tab to the Base's edge, then the Lid's), jog it open on the slider, OK; I (clear); edit the mate from the Mates folder, jog it into the base, I (one Base/Lid pair) |
| k | `?` cheat sheet, Ctrl+, settings, edit `wall`, wait for the recovery copy, `kill -9`, relaunch, Enter restores the edit (dirty) |

Coordinates assume the 1440×900 window the harness sets.
