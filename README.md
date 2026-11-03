# easyCAD

A fast, focused parametric CAD app for designing electronics enclosures, for 3D
printing and injection molding. It does what an enclosure needs and nothing else,
with a Fusion-360-style workflow, and it reads PCBs straight from EasyEDA.

Built in Rust on the OpenCascade B-rep kernel (statically linked), with an egui/wgpu UI.
Fully offline.

## What it does

- **Sketch → extrude/revolve**, with a constraint solver (DOF status), offsets and trim, and
  `=expression` input everywhere.
- **Shell, fillet, chamfer, draft, ribs, bosses, patterns, mirror, split.** Fillets and
  chamfers *self-heal*: they retry by tangent chain, re-order failed sets and find the largest
  radius that fits, and you always get a valid solid plus a report ("1 edge maxes out at
  1.99 → Use 1.99 / Skip").
- **Parting joints** from a draggable split plane: tongue & groove, lip, O-ring gland (squeeze
  and fill checks against AS568/metric cords), snap fits (strain checked per material).
- **Process profiles:** FDM / SLA / molded fit allowances. Mark a dimension `hole · slide` or
  `pin · snug` instead of typing `wall - printtol/2`. Teardrop ("V-cut") horizontal holes for
  FDM, set against your build-plate orientation; a calibration coupon generator.
- **EasyEDA in:** board STEP (with colors and part names) as a locked reference, board outline
  DXF by layer, and "Standoffs from PCB" from the board's mounting holes. A **New enclosure
  from PCB** wizard builds the whole parametric enclosure in one go.
- **Parts library:** metric and imperial screws, nuts, washers, inserts, standoffs, magnets
  and feet, each with its clearance, counterbore, tap and pilot hole rules.
- **Assemblies:** components and subcomponents, instanced occurrences, linked `.ecad` files,
  in-context design, mates (rigid, revolute, slider, planar, cylindrical, ball),
  interference check, and an indented BOM.
- **2D drawings:** views by cube face, iso corner or any rotation; associative dimensions,
  notes, balloons, BOM table and title block; PDF / DXF / SVG.
- **Export:** STEP (parts or assembly), STL, 3MF.
- **Selection that works:** a pick stack under the cursor (Tab cycles hidden faces, no
  press-and-hold), a full view cube (26 regions, 15° steps, home), and Fusion navigation.

## Build

```bash
cargo run -p easycad --release -- --sample
```

You need Rust, CMake and a C++17 compiler. The first build compiles OpenCascade (~20 min).
Release zips for macOS, Linux and Windows: `scripts/build-release.sh` (run on a Mac). See
[docs/BUILDING.md](docs/BUILDING.md).

## Layout

| path | what |
|---|---|
| `crates/occt` | cxx bridge to a narrow C++ shim over OpenCascade 7.8 |
| `crates/model` | documents, params/expressions, sketch solver, features, regen cache, joints, process profiles, parts library, assemblies, drawings, import/export |
| `crates/app` | the desktop app (`easycad` binary) plus snapshot and click-through (`e2e/`) tests |
| `crates/sync` | Google Drive sync, tabled work in progress, outside the workspace |
| `docs/` | `ENGINE.md` (engine API), `GUI.md`, `GUI_NEEDS.md`, `BUILDING.md` |

Documents are `.ecad` folders (`doc.json` plus content-addressed `blobs/`). Older `.cwk`
folders still open.

## License

[LGPL-2.1-only](LICENSE), matching OpenCascade (which is LGPL-2.1 with an additional exception,
see its sources).
