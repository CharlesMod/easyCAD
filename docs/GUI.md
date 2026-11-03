# Casework GUI (`crates/app`)

Desktop front end on eframe/egui 0.37 with the wgpu backend.

```
cargo run -p casework --release              # empty document
cargo run -p casework --release -- --sample  # the sensor-node enclosure
cargo run -p casework --release -- path/to/Enclosure.cwk
cargo run -p casework --release -- --smoke /tmp/cw-smoke   # file I/O smoke test, exit code 1 on failure
```

Display units: the `mm` chip in the title bar switches to inches (display only; the
document stays mm). Fields accept `0.125in`, `1/8"`, `1 1/2"`, `3mm`; in inch
mode a bare number means inches.

## Architecture

| module | role |
|---|---|
| `app.rs` | State, frame loop, shortcuts, file ops, regeneration plumbing, `UiCache` (derived data recomputed only when the revision or model changes). |
| `worker.rs` | Background thread: owns the `Regenerator` and a `DrawingCache`, tessellates, builds the pick BVH and face/edge topology, computes drawing sheets. Latest request per purpose wins (Model, Preview, Drawing). The UI never regenerates. |
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
| `smoke.rs` | `--smoke`: save/reopen, import STEP/DXF, export STEP/STL/3MF and drawing PDF/DXF/SVG through the dialogs' own functions. |

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
| sketch: L R C A F O P T D | tools; type a number or `=expr` while drawing |

## Tests

```
cargo test -p casework                 # unit + snapshot + GPU tests
```

Snapshots (written, not compared) land in `crates/app/tests/snapshots/`. They need
a wgpu adapter; in a container install `mesa-vulkan-drivers` (lavapipe). The
snapshot tests serialize themselves (one GPU, one kernel).
