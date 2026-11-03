# easyCAD icon

An isometric enclosure: the base open, the lid lifted with its orange tongue (the
parting joint), a green PCB inside. `easycad.svg` is generated; edit the geometry
in `tools/render-icon/make_svg.py`, then:

```
python3 tools/render-icon/make_svg.py assets/icon/easycad.svg
cargo run --release --manifest-path tools/render-icon/Cargo.toml -- assets/icon/easycad.svg assets/icon
```

`easycad-256.png` is the window icon (`crates/app/src/lib.rs`), `easycad-120.png`
the Google OAuth consent-screen logo, the rest for desktop launchers
(`assets/easycad.desktop`, install as `~/.local/share/icons/hicolor/<N>x<N>/apps/easycad.png`).
