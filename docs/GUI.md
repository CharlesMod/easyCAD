# easyCAD GUI (`crates/app`)

Desktop front end on eframe/egui 0.37 with the wgpu backend.

```
cargo run -p easycad --release              # empty document
cargo run -p easycad --release -- --sample  # the sensor-node enclosure
cargo run -p easycad --release -- path/to/Enclosure.ecad
cargo run -p easycad --release -- --smoke /tmp/cw-smoke   # file I/O smoke test, exit code 1 on failure
cargo run -p easycad --release -- --sample-board /tmp/b   # the sample PCB as STEP + DXF, for the wizard
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
| `files.rs` | File choosers: the system dialog (rfd / XDG portal) first; when none can open (no D-Bus session or portal, remote / headless) an in-app path prompt prefilled with the last folder. `EASYCAD_FILE_DIALOG=builtin` forces the prompt. |
| `e2e_state.rs` | `EASYCAD_STATE_FILE`: JSON dump of what the user sees, for the E2E scripts. |
| `ui_welcome.rs` / `wizard.rs` | Empty-document welcome (New enclosure from PCB… / Open / Sample / recent files) and the PCB wizard: reads an EasyEDA STEP or DXF (outline, mounting holes, board z) and builds ordinary features: offset profile → extrude → top-edge fillet → closed shell → board + standoffs → parting joint, all on named parameters. |
| `settings.rs` / `ui_settings.rs` | Per-user settings (navigation preset: Fusion/easyCAD, SolidWorks, Onshape, Blender; default process for new documents; units; mesh quality; autosave interval) and their window (File ▸ Settings…, Ctrl+,), the `?` cheat sheet, the recovery offer. |
| `recovery.rs` | Autosave: a dirty document is copied every N minutes (off the UI thread) to `$XDG_DATA_HOME/easycad/recovery/<doc id>/` (a `.ecad` + `info.json` with the pid). Saving removes it; closing unsaved writes a last one. At start, copies whose process is gone are offered (Restore / Discard / Decide later; Enter restores the newest). `EASYCAD_AUTOSAVE_SECS` overrides the interval (E2E). |
| `ui_assembly.rs` | Assemblies: the component tree in the browser (nested occurrences, expand/collapse, ×N instance badge, eye, ground pin, linked-file status with Update / Break link, lock on references, drag a row onto another to reparent, context menu: activate, isolate, move, ground, new / insert component, place another, make independent copy, remove), activation (double-click / Activate / breadcrumb; Esc goes up), the Mates folder (orange when unsolved or over-constrained). |
| `scope.rs` | Editing inside the active component: dialogs, sketch mode, history strip and Parameters panel see `DocData::component_view`; `scope::edit` runs a change on that view in one undoable edit and writes the component back. |
| `ui_mates.rs` | Mates dialog: type pills, snap glyphs under the cursor (face centre + normal, circle / cylinder axis, straight-edge axis, component origin), live solve (the draft rides along with every model request), jog slider for the free motion, OK = mate + solved placements in one undo step. |
| `ui_gizmo.rs` | Move gizmo (G) on an unconstrained occurrence: translate arrows and rotation rings in the view frame, 1 mm / 15° snapping (Shift: free); live through a model-matrix override, one undo step on release. |
| `ui_interference.rs` / `ui_bom.rs` | Interference (I): pairs with volumes off the UI thread, the pair tinted red, zoom-to. BOM window: indented / flat, made vs purchased, CSV. |
| `cloud.rs` | Google Drive sync on `easycad-sync`: the Google client lookup, the keychain sign-in check at start, Sign in / Sign out (browser, off the UI thread), the background `SyncHandle` over the Documents, Parts Library and Settings roots, and the status-bar indicator / conflict notices it derives from the sync events. Inert unless `main.rs` starts it, so tests never touch the keychain or the network. |
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

## Idle cost

The app draws only when something asks: input, a worker or sync result, an
animation. At rest it draws no frames. A frame whose 3D scene is unchanged (same
camera, meshes, colours, highlights, size) reuses the last offscreen image, so hover
and status-bar repaints cost a blit, not a 4×MSAA pass (`ViewportGpu::render_cached`).
While the window is minimized or covered the viewport neither renders nor blits.
Busy spinners (`widgets::spinner`) animate at full rate only in the focused window:
2 frames/s when unfocused, none when hidden. Also: the offscreen target is allocated
with headroom (rounded up to 256 px) so dragging the window edge reuses it; bodies
wholly outside the view are not drawn; imported STEP colours keep one `Arc` per
mesh so a regenerated, unchanged model stays cached; the hover pick skips its ray
cast while pointer, camera, model and visibility are unchanged (`PickStack::key`);
"Regenerating…" has no repaint timer (the worker wakes the UI). Sync ignores watcher
events for files it wrote itself and Drive changes it made itself; `settings.json` is
rewritten only when it changed; recovery copies hard-link the previous copy's blobs.
`EASYCAD_DEBUG_REPAINT=1` prints frames per second, 3D renders and what requested
each frame.

## Performance overlay

F12 (or `EASYCAD_PROFILE=1`) shows, top left of the viewport: the GPU adapter and API
(an orange "software rendering" line means llvmpipe, not the GPU), the present mode, frames in
the last second, a graph of frame gaps against 144 Hz and 60 Hz, and per frame: UI logic
(`EasyCadApp::frame`), eframe's whole update, the 3D pass's CPU encode and its GPU time
(timestamp queries, requested at device creation when the adapter has them), 3D renders vs
image-cache hits, draws and triangles of the last render (`profiler.rs`). The overlay itself
never asks for frames. `EASYCAD_PRESENT=immediate|mailbox|fifo` overrides vsync to see
uncapped frame times. `tests/pan_perf.rs` times a pan of the sample at 2560×1440.

Started inside a distrobox/toolbox container without a GPU Vulkan driver (wgpu would
fall back to llvmpipe), the binary re-runs itself on the host through
`distrobox-host-exec`, passing `EASYCAD_*`, `XDG_*` and `RUST_*`; `EASYCAD_NO_HOST=1`
keeps it in the container.

## Google Drive sync

File ▸ Settings ▸ Google Drive has **Sign in with Google**. It opens the browser
(OAuth with PKCE, a loopback redirect, scope `drive.file`: easyCAD sees only the
files it created) and keeps the refresh token in the OS keychain. While signed in,
these folders sync with `easyCAD/` in My Drive:

| local | Drive |
|---|---|
| `~/Documents/easyCAD` (Save starts here for a new document) | `easyCAD/Documents` |
| `$XDG_DATA_HOME/easycad/library` (parts library) | `easyCAD/Parts Library` |
| `settings.json` in the settings folder (nothing else there) | `easyCAD/Settings` |

A pass runs at start, 2 s after a local change (file watcher), right after a save,
when Drive's change feed has something (polled every minute) and every 15 minutes.
The right end of the status bar shows Synced / Syncing n/N / Offline / Sign-in
needed / Sync conflict / Sign in to sync; a click opens Settings. A document changed
on two computers is never overwritten: this computer's version stays and is pushed,
the other one is saved beside it as `Name (conflict <device> <date time>).ecad`, a
warning toast names it, and Settings lists it with *Open their version* and *Dismiss*.
Deleted files go to the Drive trash; files deleted locally by sync go to
`.cwsync-trash/` in the root. Sync is off when no Google client file is installed
(`~/.config/easycad/google_client.json`, see `crates/sync/README.md`) or with
`EASYCAD_SYNC=off`. The state is in `$XDG_DATA_HOME/easycad/sync/state.json`.

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
cargo test -p easycad                 # unit + snapshot + GPU tests
```

Snapshots (written, not compared) land in `crates/app/tests/snapshots/`. They need
a wgpu adapter; in a container install `mesa-vulkan-drivers` (lavapipe). The
snapshot tests serialize themselves (one GPU, one kernel).

## Click-through E2E (`crates/app/e2e/`)

The release binary under Xvfb, driven by xdotool, asserting on the state dump with
jq and saving screenshots to `e2e/shots/` (a selection is committed).

```
cargo build --release -p easycad
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
