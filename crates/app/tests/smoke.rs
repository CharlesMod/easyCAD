//! The `--smoke` I/O path (save/reopen, import STEP/DXF, export STEP/STL/3MF and
//! drawing PDF/DXF/SVG) as a test.

#[test]
fn file_io_smoke() {
    let dir = std::env::temp_dir().join(format!("easycad-smoke-{}", std::process::id()));
    let log = easycad_app::smoke::run(&dir).unwrap_or_else(|e| panic!("smoke failed: {e}"));
    assert!(log.len() >= 10, "{log:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
