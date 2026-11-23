---
name: easycad
description: Model parts live in the user's running easyCAD app through the easycad MCP tools (get_document, set_param, add_sketch, add_feature, edit_feature, measure, screenshot, export, ...). Use whenever the user asks to design, model, change, check or export a part, enclosure, bracket, box, case or other 3D-printable geometry in easyCAD, or refers to what is open or selected in easyCAD.
---

# Driving easyCAD

easyCAD is a parametric CAD kernel app (OpenCascade) the user has open next to
you. Every tool call acts on the document in their window: they watch each step
appear, can click faces for you, edit parameters by hand, and press Ctrl/Cmd+Z to
undo your steps one at a time (each is labelled "Claude: ..."). You are modelling
*with* them, not for them.

Units are mm and degrees. Z is up; the front of the part faces -Y (the "front"
view looks along +Y); right is +X.

## The loop

1. **Look first.** `get_document` before every change. If it has `user_changes`,
   the user edited by hand: build on their edit, never revert it silently. Pass
   `expected_revision` on edits when it matters that nothing moved underneath you.
2. **Parameters first.** `set_param` for every meaningful size (`width`, `wall`,
   `lid_h`, `corner_r`, `servo_w`...) in one call. Use them by name in sketches
   and features. Derived sizes are expressions: `"wall*2 + 0.2"`, `"width - 2*wall"`.
   Literals are for things nobody will tweak.
3. **Sketch, then feature.** `add_sketch` then `add_feature` (`Extrude`,
   `Revolve`, ...) using the returned `sketch_id`. Prefer fully constrained
   sketches (DOF 0): `rect`/`rounded_rect` with `center`+`width`+`height`, circles
   with `radius`/`diameter`. A non-zero DOF is a warning, not an error.
4. **Read every report.** Each mutating call waits for regeneration and returns
   the changed feature's `status` (`ok` / `warning` / `error` / `unresolved`) plus
   `problems` elsewhere in the history. A new feature that fails is taken back out
   automatically. Read the message, fix the cause, retry; do not pile features on
   top of an error.
5. **Look at it.** `screenshot` after each significant step (`"iso"` or
   `"top front right"`, `"front"`, `"top"`...). It moves the user's camera, so use
   `"current"` when they are looking at something.
6. **Small steps, narrated.** Say in one line what you are about to do, do one
   feature per call, and show the result. The user may interrupt and steer.

## Selectors, never indices

Features reference geometry by selectors that are re-resolved at every regen:

| Face selector | Meaning |
|---|---|
| `{"Extreme":{"normal":[0,0,1],"which":"Max"}}` | the outermost planar face facing +Z (top). Bottom: normal `[0,0,-1]`, Max |
| `{"AllWithNormal":{"normal":[1,0,0]}}` | every planar face facing +X |
| `{"Cylinders":{"radius":1.5,"axis":[0,0,1]}}` | cylindrical faces (radius/axis optional) |
| `{"Hint":{"centroid":[..],"normal":[..],"kind":"Plane"}}` | the face nearest a picked point (fallback) |

| Edge selector | Meaning |
|---|---|
| `{"OfFace": <face selector>}` | every edge bounding those faces (e.g. the top rim) |
| `{"Between":[<face>,<face>]}` | edges shared by the two faces |
| `{"Parallel":{"dir":[0,0,1]}}` | straight edges along a direction (vertical corners) |
| `{"Circles":{"radius":1.5,"near":[[x,y,z],5]}}` | circular edges of a radius, optionally near a point |
| `{"Hint":{"midpoint":[..],"tangent":[..],"kind":"Line"}}` | nearest picked edge (fallback) |

`measure` with `faces:true` / `edges:true` lists each face and edge with what
made it (`"Bin body · end face"`, `"Shell · floor"`) and a ready `selector`
(semantic when one identifies it uniquely). `get_selection` does the same for
what the user clicked. When the user says "this face" or "these edges", call
`get_selection`.

## Sketches (`add_sketch`)

```json
{"name": "Base", "plane": "XY",
 "elements": [
   {"type": "rounded_rect", "id": "outline", "center": [0, 0], "width": "width", "height": "depth", "radius": "corner_r"},
   {"type": "circle", "id": "hole", "center": [12, 0], "diameter": "m3_clear"}
 ]}
```

- Planes: `"XY"` (sketch x→X, y→Y, extrudes +Z), `"XZ"` (front plane: x→X,
  y→Z, extrudes toward -Y), `"YZ"` (x→Y, y→Z, extrudes +X), or
  `{origin, x_dir, normal}`; `offset` moves it along the normal.
- Nested closed loops become holes: an outline plus inner circles extrudes as a
  plate with holes. `profiles: "All"` takes every region; `{"Containing":[[x,y]]}`
  picks the regions around sketch points (stable), `{"Indices":[0]}` by index.
- Free-form shapes: `line`/`arc` elements, then `coincident` constraints on the
  shared ends (`"l1.b"`, `"l2.a"`) and dimensions (`length`, `distance_x`,
  `angle`...). Names: `rect` gives `<id>.bottom/.right/.top/.left`,
  `.bl/.br/.tr/.tl`, `.center`; lines `.a/.b`; arcs and circles `.center`.
- Replace a sketch in place with `replace: <sketch_id>`: features using it follow.

## Features (`add_feature`, `edit_feature`)

`kind` is the document's own JSON, one key naming the type. Dimensions are
expression strings or numbers; vectors `[x,y,z]`; bodies
`{"feature": <id>, "index": 0}`; an omitted `body`/`target` means the last solid.

```json
{"Extrude": {"sketch": 1, "profiles": "All", "distance": "height", "op": "NewBody"}}
{"Extrude": {"sketch": 3, "profiles": "All", "distance": "wall", "op": "Cut", "reverse": true}}
{"Extrude": {"sketch": 2, "profiles": "All", "distance": "10", "op": "Join", "symmetric": true, "draft_deg": "1"}}
{"Revolve": {"sketch": 4, "profiles": "All", "axis": {"origin": [0,0,0], "dir": [0,0,1]}, "angle_deg": "360", "op": "NewBody"}}
{"Fillet": {"edges": [{"Parallel": {"dir": [0,0,1]}}], "radius": "corner_r", "auto_reduce": false}}
{"Chamfer": {"edges": [{"OfFace": {"Extreme": {"normal": [0,0,-1], "which": "Max"}}}], "distance": "0.4"}}
{"Shell": {"open_faces": [{"Extreme": {"normal": [0,0,1], "which": "Max"}}], "thickness": "wall"}}
{"Draft": {"faces": [{"AllWithNormal": {"normal": [1,0,0]}}], "neutral": {"origin": [0,0,0], "x_dir": [1,0,0], "normal": [0,0,1]}, "pull": [0,0,1], "angle_deg": "1"}}
{"ScrewBoss": {"plane": {"origin": [0,0,2], "x_dir": [1,0,0], "normal": [0,0,1]}, "points": [[20,15],[-20,15]], "od": "6", "hole_dia": "2.5", "height": "8"}}
{"Rib": {"sketch": 5, "thickness": "1.2", "height": "6"}}
{"Pattern": {"source": {"Features": [7]}, "kind": {"Linear": {"dir": [1,0,0], "spacing": "8", "count": "5"}}}}
{"Mirror": {"source": {"Features": [7]}, "plane": {"origin": [0,0,0], "x_dir": [0,1,0], "normal": [1,0,0]}}}
{"Split": {"plane": {"origin": [0,0,0], "x_dir": [1,0,0], "normal": [0,0,1]}, "offset": "lid_split", "new_name": "Lid"}}
```

Also `PartingJoint` (lid/base split with tongue & groove, lip, O-ring or snap
fits), `StandoffsFromPcb`, `InsertPart` (library screws, inserts, magnets) and
`ImportStep`. For an unfamiliar type, send your best guess: the error names the
missing field or unknown variant; or look at one the user made via
`get_document`.

`edit_feature` takes a patch: `{"id": 4, "kind": {"Fillet": {"radius": "3"}}}`
changes only the radius. `suppressed: true` switches a feature off.

## Fillets and shells, the usual traps

- Fillet the outer vertical corners *before* the shell, so the walls follow them.
  Fillet inner edges with a radius below the wall thickness or they fail.
- A fillet report lists `edge_problems` with each failed edge's
  `largest_workable_size`. Lower the radius, put those edges in a separate set
  (`sets: [{"edges": [...], "size": "1"}]`), set `auto_reduce: true`, or add
  their selectors to `skipped`.
- A shell whose inward offset collapses geometry (a wall thicker than a concave
  radius, a sliver face) fails with the kernel's message. Shrink the wall, grow
  the radius, or shell before the inner details.
- `unresolved` means a stored selector no longer matches confidently (the
  geometry changed a lot). The report carries `best_candidate`: use it in
  `edit_feature` if it is the intended face or edge, or pick again with `measure`.

## Checking

- `measure` gives bbox, volume and area per body. Check sizes against the
  requirement after each feature that should change them.
- `interference` checks bodies (a lid against the base, a servo or PCB reference
  body against the walls) and reports the overlap volume of each pair.
- `regenerate` returns every feature's report without changing anything.

## Etiquette

- Never undo, delete or overwrite the user's own steps unless they ask. The
  `undo` tool only undoes your steps unless `allow_user_steps` is set.
- When a tool says the user has a dialog or sketch open, tell them and wait. Do
  not retry in a loop.
- Ask before `open_document` / `new_document` when there are unsaved changes, and
  before `save_document` to a new place. `export` writes only where the user
  wants files.
- If the tools say easyCAD is not running, ask the user to start it with
  `easycad --mcp` (or Settings → Claude bridge) and keep the window visible.
