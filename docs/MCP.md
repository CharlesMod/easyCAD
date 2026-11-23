# Claude bridge (MCP)

easyCAD can be driven live by Claude Code (or any MCP client). Claude's tool calls
act on the document open in the window: you watch each sketch and feature appear,
can edit at the same time, click faces for Claude, and undo its steps one at a time.

## Setup

1. **Turn the bridge on** in the app: run `easycad --mcp` (this run only; also
   `--mcp-port N`), or tick **Settings → Claude bridge** (every start, saved in
   `settings.json` as `"mcp": true`, port in `"mcp_port"`). A toast shows the
   address, by default `http://127.0.0.1:47311/mcp`.
2. **Build the shim** once: `cargo install --path crates/mcp` (seconds; it does not
   link OpenCascade). It installs `easycad-mcp` into `~/.cargo/bin`.
3. **Install the plugin** in Claude Code:
   ```
   /plugin marketplace add /path/to/easyCAD/tools/claude-plugin
   /plugin install easycad@easycad
   ```
   It registers the `easycad` MCP server (the shim, through
   `bin/easycad-mcp-launch`, which looks in `$EASYCAD_MCP_BIN`, `PATH` and
   `~/.cargo/bin`) and the `easycad` skill that teaches the workflow.
   Restart Claude Code afterwards so it starts the server.
4. Check: `easycad-mcp --check` prints whether the app is reachable.

Without the plugin, register the shim directly:
`claude mcp add easycad -- easycad-mcp`. Or skip the shim and talk HTTP:
`claude mcp add --transport http easycad http://127.0.0.1:47311/mcp --header "Authorization: Bearer <token>"`
with the token from the endpoint file below (it is kept across restarts).

## How it works

```
Claude Code ──stdio──▶ easycad-mcp (shim) ──HTTP POST /mcp, Bearer token──▶ easyCAD app
                                                   (127.0.0.1 only)          │
                                                                             ▼
                                         server thread ──channel──▶ UI thread (frame loop)
                                                                      Document::edit("Claude: …")
                                                                      wait for the regen worker
                                                                      reply with regen reports
```

- **Transport.** MCP "streamable HTTP" with plain JSON responses (no SSE stream;
  `GET` answers 405), JSON-RPC batches accepted, protocol versions 2024-11-05
  through 2025-11-25. `crates/mcp` (std + serde_json only) holds the tool schemas
  (`src/tools.json`), the dispatcher, the server and the shim.
- **Security.** The server binds 127.0.0.1 only, needs `Authorization: Bearer
  <token>` (or `X-Easycad-Token`), and refuses requests with a non-loopback
  `Origin` (browsers on other sites). The token and URL are in
  `~/.config/easycad/mcp-endpoint.json` (`$XDG_CONFIG_HOME/easycad`, `%APPDATA%\easycad`
  on Windows; mode 0600; `EASYCAD_MCP_ENDPOINT` overrides the path). The app
  removes it when the bridge stops. Off by default.
- **The shim** re-reads the endpoint file for every message, so restarting the app
  is fine. When the app is not running it still answers `initialize` and
  `tools/list` (Claude Code lists the tools) and every tool call returns
  "easyCAD is not running with the Claude bridge on ...".
- **Threading** (ENGINE.md, "Concurrency contract"). The `Document` belongs to the
  UI thread. Calls are queued and run one at a time, in order, from the frame loop
  (`mcp::poll`). Mutations go through `Document::edit` on the UI thread, so each is
  one undo step labelled `Claude: add Extrude 'Bin body'` and shows at once (plus a
  toast). The call then waits until the regeneration worker has returned the model
  of that exact revision, and answers with its reports. Exports and interference
  checks run on their own threads from a snapshot.
- **No stale state.** Every call reads the live document. The app keeps the
  revision Claude last saw. When the user changed anything since, `get_document`
  (and the next edit's reply, as `user_changes_before`) lists those steps from the
  undo history. Edits take an optional `expected_revision` and are refused if the
  document moved.
- **The user stays in charge.** Edits are refused while the user has a feature
  dialog, a sketch, the parts library, a drawing or the wizard open, or is editing a
  component in place. The reply says so, so Claude can ask them. Claude's `undo`
  tool only undoes Claude's own steps.
- **Failed features leave no trace.** A new feature, or an edit that breaks a
  working feature, is taken back out with `Document::discard_last` (no undo or redo
  entry). The error carries the full report. `keep_on_error` keeps it.

## Tools

| tool | does |
|---|---|
| `get_document` | name, path, revision, unsaved flag, params (expr + value), sketches (DOF, regions), features (definition JSON, status, fillet summary), bodies, undo history, app state, `user_changes` |
| `set_param` | create / change / delete parameters, several in one undo step; reports what the change broke |
| `add_sketch` | `rect`, `rounded_rect`, `circle`, `line`, `arc`, `point` elements + constraints and dimensions as expressions; solves first, refuses non-converging or conflicting sketches; returns DOF, regions, element names. `replace` edits a sketch in place |
| `add_feature` | any `FeatureKind` as the document's own JSON (numbers accepted for dimensions); checks sketch / body references; optional `position` |
| `import_mesh` | STL / OBJ / GLB / glTF from an absolute path as a mesh body (`ImportMesh`: up axis, scale, rotation, translation); repairs or refuses meshes that are not closed. Combine with `MeshBoolean`, move/copy with `TransformBody`, flat bottom with `Split` `keep: Above` |
| `edit_feature` | patch (`{"Fillet":{"radius":"3"}}` changes only the radius), rename, suppress |
| `delete_feature` | lists what broke downstream |
| `regenerate` | all feature reports: status, fillet/chamfer summary and per-edge problems with the largest workable size, sketch solves, joint reports, timing |
| `measure` | per body: bbox, volume, area; optionally faces and edges with what made them and a ready selector (`Extreme` / `Cylinders` / `Between` when unique, else `Hint`) |
| `get_selection` | the user's viewport selection, same format |
| `interference` | overlapping body pairs with common volume (parts or assemblies) |
| `screenshot` | viewport PNG after regeneration; named view-cube directions (`iso`, `front`, `top front right`, ...) or `current`; downscaled to `max_width` |
| `export` | STL / 3MF / STEP to an absolute path (active process profile, build orientation) |
| `open_document`, `new_document`, `save_document` | refuse to drop unsaved changes unless `discard_unsaved` |
| `undo` | Claude's last step only, unless `allow_user_steps` |

Errors come back as tool results with `isError: true` and a sentence saying what to
do. Examples: unknown parameters list the known ones, sketch errors name the known
points, failed fillet edges give their largest workable size and the selector that
asked for them, unresolved references give the best candidate selector, and bad
feature JSON gives serde's message plus an example.

## Tests

- `cargo test -p easycad-mcp`: JSON-RPC over HTTP, token, origin check, the shim
  forwarding to a server and answering alone when none runs.
- `cargo test -p easycad --test mcp -- --test-threads=1`: the real app under
  egui_kittest. Claude builds a parametric box (params → sketch → extrude → fillet →
  shell), a failing fillet is taken back out, the screenshot is a PNG of the box,
  a hand edit shows in `user_changes`, a stale `expected_revision` and an open dialog
  are refused, the selection gives a selector, STL export works, and the history
  undoes one Claude step at a time. A second test goes through HTTP with the token.
