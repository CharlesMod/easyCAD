//! The Claude bridge end to end, on the real app (worker thread, GPU viewport)
//! with egui_kittest: Claude builds a parametric box with a fillet and a shell, a
//! failing fillet is taken back out, screenshots come back as PNG, the user's own
//! edits show up in `get_document`, and the steps undo one at a time. A second test
//! goes through the loopback HTTP server with a bearer token.
//! Run with `cargo test -p easycad --test mcp -- --test-threads=1`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use easycad_app::mcp::Bridge;
use easycad_app::EasyCadApp;
use egui_kittest::Harness;
use serde_json::{json, Value};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("easycad-mcp-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness(doc: easycad_model::document::Document) -> Harness<'static, EasyCadApp> {
    // the user's crash-recovery copies would open the Recover prompt over the viewport,
    // and the user's endpoint file must not be touched
    std::env::set_var("XDG_DATA_HOME", std::env::temp_dir().join(format!("easycad-test-data-{}", std::process::id())));
    std::env::set_var("EASYCAD_MCP_ENDPOINT", tmp("endpoint").join("mcp-endpoint.json"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .wgpu()
        .build_eframe(move |cc| EasyCadApp::new(cc, Some(doc)));
    for _ in 0..4 {
        h.run_steps(2);
        assert!(h.state_mut().settle(Duration::from_secs(240)));
    }
    h
}

/// Run one tool call through the bridge, stepping frames until it answers.
fn call_raw(h: &mut Harness<'static, EasyCadApp>, name: &str, args: Value) -> Result<Vec<Value>, String> {
    let rx = h.state().mcp.as_ref().expect("bridge on").submit(name, args);
    let t = Instant::now();
    loop {
        h.step();
        match rx.try_recv() {
            Ok(r) => return r,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(t.elapsed() < Duration::from_secs(240), "{name} did not answer");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{name}: {e}"),
        }
    }
}

/// A call that must succeed; returns its JSON.
fn call(h: &mut Harness<'static, EasyCadApp>, name: &str, args: Value) -> Value {
    match call_raw(h, name, args.clone()) {
        Ok(c) => serde_json::from_str(c[0]["text"].as_str().expect("text block")).expect("JSON text"),
        Err(e) => panic!("{name} {args} failed: {e}"),
    }
}

fn call_err(h: &mut Harness<'static, EasyCadApp>, name: &str, args: Value) -> String {
    match call_raw(h, name, args.clone()) {
        Ok(c) => panic!("{name} {args} should fail, got {}", c[0]["text"]),
        Err(e) => e,
    }
}

fn size(v: &Value) -> [f64; 3] {
    let s = &v["bodies"][0]["bbox"]["size"];
    [0, 1, 2].map(|i| s[i].as_f64().unwrap())
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
}

#[test]
fn claude_builds_a_shelled_filleted_box_step_by_step() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Bin"));
    h.state_mut().mcp = Some(Bridge::new());

    let doc = call(&mut h, "get_document", json!({}));
    assert_eq!(doc["name"], "Bin");
    assert!(doc["features"].as_array().unwrap().is_empty());
    assert!(doc.get("user_changes").is_none());

    // parameters first, one undo step
    let p = call(
        &mut h,
        "set_param",
        json!({"params": [
            {"name": "width", "expr": 60}, {"name": "depth", "expr": "40"}, {"name": "height", "expr": "25"},
            {"name": "wall", "expr": "2", "comment": "printed wall"}, {"name": "corner_r", "expr": "wall*2"}
        ]}),
    );
    assert_eq!(p["params"].as_array().unwrap().len(), 5);
    assert_eq!(h.state().doc.undo_label(), Some("Claude: set 5 parameters (width, depth, height, wall, corner_r)"));
    let err = call_err(&mut h, "set_param", json!({"name": "lid", "expr": "heigth/4"}));
    assert!(err.contains("unknown parameter") && err.contains("height"), "{err}");

    // sketch: fully constrained parametric rectangle
    let sk = call(
        &mut h,
        "add_sketch",
        json!({"name": "Base", "plane": "XY", "elements": [{"type": "rect", "id": "outline", "center": [0, 0], "width": "width", "height": "depth"}]}),
    );
    assert_eq!(sk["solve"]["dof"], 0, "{sk}");
    assert_eq!(sk["closed_regions"].as_array().unwrap().len(), 1);
    let sid = sk["sketch_id"].as_u64().unwrap();
    // constraint errors name the known points
    let err = call_err(
        &mut h,
        "add_sketch",
        json!({"name": "Bad", "plane": "XY", "elements": [{"type": "line", "id": "l", "from": [0, 0], "to": [5, 0]}],
               "constraints": [{"type": "distance", "a": "l.a", "b": "l.c", "value": "3"}]}),
    );
    assert!(err.contains("unknown point `l.c`"), "{err}");

    let ex = call(
        &mut h,
        "add_feature",
        json!({"name": "Bin body", "kind": {"Extrude": {"sketch": sid, "profiles": "All", "distance": "height", "op": "NewBody"}}}),
    );
    assert_eq!(ex["feature"]["status"], "ok", "{ex}");
    assert!(close(size(&ex), [60.0, 40.0, 25.0]), "{ex}");
    let body_id = ex["feature"]["id"].as_u64().unwrap();
    assert_eq!(h.state().doc.undo_label(), Some("Claude: add Extrude 'Bin body'"));
    // the user's viewport has it too
    assert!(h.state_mut().settle(Duration::from_secs(60)));
    assert_eq!(h.state().model.as_ref().unwrap().bodies.len(), 1);

    let fi = call(
        &mut h,
        "add_feature",
        json!({"name": "Corners", "kind": {"Fillet": {"edges": [{"Parallel": {"dir": [0, 0, 1]}}], "radius": "corner_r"}}}),
    );
    assert_eq!(fi["feature"]["status"], "ok", "{fi}");
    assert!(fi["feature"]["summary"].as_str().unwrap().starts_with("4 of 4 edges done"), "{fi}");

    // measure offers a ready selector for the top face
    let m = call(&mut h, "measure", json!({"faces": true}));
    let faces = m["bodies"][0]["faces"].as_array().unwrap();
    let top = faces.iter().find(|f| f["selector"] == json!({"Extreme": {"normal": [0.0, 0.0, 1.0], "which": "Max"}})).expect("top face selector");
    assert!(top["made_by"].as_str().unwrap().contains("Bin body"), "{top}");
    let solid = m["bodies"][0]["volume_mm3"].as_f64().unwrap();

    let sh = call(&mut h, "add_feature", json!({"name": "Shell", "kind": {"Shell": {"open_faces": [top["selector"]], "thickness": "wall"}}}));
    assert_eq!(sh["feature"]["status"], "ok", "{sh}");
    let m = call(&mut h, "measure", json!({"body": "Bin body"}));
    let hollow = m["bodies"][0]["volume_mm3"].as_f64().unwrap();
    assert!(hollow < solid * 0.5 && hollow > 1000.0, "{solid} -> {hollow}");

    // a fillet far too big for the 2 mm wall is taken back out, with the reason
    let undo_before = h.state().doc.undo_label().map(String::from);
    let rev_before = h.state().doc.data.revision;
    let err = call_err(
        &mut h,
        "add_feature",
        json!({"name": "Rim", "kind": {"Fillet": {"edges": [{"OfFace": {"Extreme": {"normal": [0, 0, 1], "which": "Max"}}}], "radius": 30}}}),
    );
    assert!(err.contains("taken back out"), "{err}");
    assert_eq!(h.state().doc.undo_label().map(String::from), undo_before);
    assert!(!h.state().doc.can_redo(), "a failed feature leaves no redo step");
    assert!(h.state().doc.data.features.len() == 3);
    assert!(h.state().doc.data.revision > rev_before);
    // bad definitions are explained
    let err = call_err(&mut h, "add_feature", json!({"name": "X", "kind": {"Extrud": {}}}));
    assert!(err.contains("unknown variant `Extrud`"), "{err}");
    let err = call_err(&mut h, "add_feature", json!({"name": "X", "kind": {"Extrude": {"sketch": 99, "profiles": "All", "distance": "1", "op": "Join"}}}));
    assert!(err.contains("sketch 99 does not exist"), "{err}");

    // edit with a patch: only the radius changes
    let fid = fi["feature"]["id"].as_u64().unwrap();
    let ed = call(&mut h, "edit_feature", json!({"id": fid, "kind": {"Fillet": {"radius": "3"}}}));
    assert_eq!(ed["feature"]["status"], "ok", "{ed}");
    let k = &h.state().doc.data.features[1].kind;
    assert_eq!(serde_json::to_value(k).unwrap()["Fillet"]["radius"], "3");

    // screenshots: iso and the user's current view
    let shot = call_raw(&mut h, "screenshot", json!({"view": "top front right", "max_width": 640})).expect("screenshot");
    assert_eq!(shot[0]["type"], "image");
    let png = base64_decode(shot[0]["data"].as_str().unwrap());
    let img = image::load_from_memory(&png).expect("PNG").to_rgba8();
    assert!(img.width() <= 640 && img.width() > 300, "{}", img.width());
    // the box is drawn: not one flat colour
    let first = img.get_pixel(0, 0).0;
    assert!(img.pixels().filter(|p| p.0 != first).count() > 1000);
    std::fs::write(tmp("shot").join("iso.png"), &png).unwrap();
    assert!(shot[1]["text"].as_str().unwrap().contains("Top Front Right"), "{}", shot[1]["text"]);
    let err = call_err(&mut h, "screenshot", json!({"view": "upside"}));
    assert!(err.contains("unknown view word"), "{err}");

    // the user changes a parameter by hand: Claude sees it
    h.state_mut().set_param("height", "30").unwrap();
    let doc = call(&mut h, "get_document", json!({"include_definitions": false}));
    assert_eq!(doc["user_changes"]["steps"], json!(["Set height"]), "{}", doc["user_changes"]);
    assert!(close(size(&doc), [60.0, 40.0, 30.0]), "{doc}");
    let again = call(&mut h, "get_document", json!({}));
    assert!(again.get("user_changes").is_none());
    // a stale revision is refused
    let rev = doc["revision"].as_u64().unwrap();
    h.state_mut().set_param("wall", "1.6").unwrap();
    let err = call_err(&mut h, "set_param", json!({"name": "depth", "expr": "45", "expected_revision": rev}));
    assert!(err.contains("Set wall"), "{err}");

    // the user's selection
    let face = {
        let app = h.state();
        let m = app.model.as_ref().unwrap();
        let mb = &m.bodies[0];
        let i = mb.faces.iter().position(|f| f.normal.z < -0.99 && f.centroid.z < 0.1).unwrap();
        easycad_app::pickstack::PickKey { body: mb.body.id, kind: easycad_app::pickstack::PickKind::Face(i as u32) }
    };
    h.state_mut().selection = vec![face];
    let sel = call(&mut h, "get_selection", json!({}));
    assert_eq!(sel["selection"][0]["type"], "face");
    assert_eq!(sel["selection"][0]["selector"], json!({"Extreme": {"normal": [0.0, 0.0, -1.0], "which": "Max"}}), "{sel}");

    // interference between the bodies of a plain part: none yet, then an overlapping box
    let i = call(&mut h, "interference", json!({}));
    assert_eq!(i["verdict"], "no interference", "{i}");
    let sk2 = call(
        &mut h,
        "add_sketch",
        json!({"name": "Probe", "plane": "XY", "elements": [{"type": "rect", "id": "p", "center": [25, 0], "width": 10, "height": 10}]}),
    );
    let probe = call(
        &mut h,
        "add_feature",
        json!({"name": "Probe", "kind": {"Extrude": {"sketch": sk2["sketch_id"], "profiles": "All", "distance": 10, "op": "NewBody"}}}),
    );
    let i = call(&mut h, "interference", json!({}));
    let pairs = i["interfering_pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{i}");
    // the probe (x 20..30, y ±5, z 0..10) overlaps the 1.6 mm wall (x 28.4..30: 160 mm³)
    // and the floor (x 20..28.4, z 0..1.6: 134.4 mm³)
    assert!((pairs[0]["volume_mm3"].as_f64().unwrap() - 294.4).abs() < 1.0, "{i}");
    let _ = probe;
    // drop the probe again without leaving steps (the undo checks below expect the user's step last)
    for _ in 0..2 {
        h.state_mut().doc.discard_last();
    }
    h.state_mut().after_edit();

    // export
    let out = tmp("export").join("bin.stl");
    let ex = call(&mut h, "export", json!({"path": out.to_str().unwrap()}));
    assert!(ex["bytes"].as_u64().unwrap() > 1000, "{ex}");

    // a dialog open in the app blocks changes
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Extrude);
    let err = call_err(&mut h, "set_param", json!({"name": "depth", "expr": "45"}));
    assert!(err.contains("Extrude dialog"), "{err}");
    h.state_mut().close_dialog(false);

    // undo: Claude's tool refuses the user's step; the user undoes theirs, then Claude's one at a time
    let err = call_err(&mut h, "undo", json!({}));
    assert!(err.contains("Set wall"), "{err}");
    h.state_mut().undo();
    h.state_mut().undo();
    let u = call(&mut h, "undo", json!({}));
    assert_eq!(u["undone"], "Claude: edit Fillet 'Corners'");
    let mut labels = Vec::new();
    while let Some(l) = h.state().doc.undo_label().map(String::from) {
        labels.push(l);
        h.state_mut().undo();
    }
    assert_eq!(
        labels,
        [
            "Claude: add Shell 'Shell'",
            "Claude: add Fillet 'Corners'",
            "Claude: add Extrude 'Bin body'",
            "Claude: add sketch 'Base'",
            "Claude: set 5 parameters (width, depth, height, wall, corner_r)",
        ]
    );
    let doc = call(&mut h, "get_document", json!({}));
    assert!(doc["features"].as_array().unwrap().is_empty());
    let _ = body_id;
}

#[test]
fn over_http_with_a_token() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Http"));
    let ctx = h.ctx.clone();
    h.state_mut().set_mcp(&ctx, true, 0);
    let (url, token) = {
        let b = h.state().mcp.as_ref().expect("bridge started");
        (b.url().unwrap(), b.token().unwrap().to_string())
    };
    let ep = easycad_mcp::read_endpoint().expect("endpoint file");
    assert_eq!((ep.url.as_str(), ep.token.as_str()), (url.as_str(), token.as_str()));
    let client = std::thread::spawn(move || {
        let post = |body: Value, tok: &str| easycad_mcp::post(&url, tok, &body.to_string(), Duration::from_secs(120)).unwrap();
        assert_eq!(post(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), "wrong").0, 401);
        let (s, b) = post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), &token);
        assert_eq!(s, 200, "{b}");
        let (_, b) = post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &token);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert!(v["result"]["tools"].as_array().unwrap().len() >= 16);
        let (_, b) = post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "set_param", "arguments": {"name": "w", "expr": "12"}}}), &token);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["isError"], false, "{v}");
        let (_, b) = post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "undo", "arguments": {}}}), &token);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["isError"], false, "{v}");
        let (_, b) = post(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "undo", "arguments": {}}}), &token);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["isError"], true, "{v}");
    });
    let t = Instant::now();
    while !client.is_finished() {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(t.elapsed() < Duration::from_secs(240));
    }
    client.join().unwrap();
    assert!(h.state().doc.data.params.list.is_empty());
    h.state_mut().set_mcp(&ctx, false, 0);
    assert!(easycad_mcp::read_endpoint().is_none(), "the endpoint file is removed when the bridge stops");
}

/// A mesh body end to end: import an STL, snip it against a solid post at two
/// carriage positions with clearance, flat bottom, check it with measure and
/// interference, export a watertight STL. Solid-only features refuse it.
#[test]
fn claude_imports_a_mesh_and_snips_it_at_two_positions() {
    use easycad_model::meshbody;
    let _g = serial();
    let dir = tmp("mesh");
    let pile = meshbody::sample_blob(glam::DVec3::new(0.0, 0.0, 10.0), 10.0, 4);
    let stl = dir.join("pile.stl");
    std::fs::write(&stl, meshbody::write_stl_bytes(&pile)).unwrap();
    let mut h = harness(easycad_model::document::Document::new("Pile"));
    h.state_mut().mcp = Some(Bridge::new());

    // a static post through the pile's side
    let sk = call(
        &mut h,
        "add_sketch",
        json!({"name": "Post", "plane": "XY", "elements": [{"type": "rect", "id": "p", "center": [14.5, 0], "width": 11, "height": 40}]}),
    );
    let sid = sk["sketch_id"].as_u64().unwrap();
    let post = call(&mut h, "add_feature", json!({"name": "Post", "kind": {"Extrude": {"sketch": sid, "profiles": "All", "distance": 30, "op": "NewBody"}}}));
    let post_id = post["feature"]["id"].as_u64().unwrap();

    let err = call_err(&mut h, "import_mesh", json!({"path": "pile.stl"}));
    assert!(err.contains("absolute"), "{err}");
    let imp = call(&mut h, "import_mesh", json!({"path": stl.display().to_string(), "name": "Trash pile"}));
    let fid = imp["feature"]["id"].as_u64().unwrap();
    assert_eq!(h.state().doc.undo_label(), Some("Claude: import mesh 'pile.stl'"));
    let pb = imp["bodies"].as_array().unwrap().iter().find(|b| b["name"] == "Trash pile").expect("pile body").clone();
    assert_eq!(pb["mesh_body"], true, "{pb}");
    let pile_body = json!({"feature": fid, "index": 0});

    // snip at rest and 14 mm up, 0.5 mm clearance
    let snip = call(
        &mut h,
        "add_feature",
        json!({"name": "Snip", "kind": {"MeshBoolean": {"target": pile_body, "tools": [{"feature": post_id, "index": 0}], "op": "Cut", "clearance": "0.5", "positions": [[0, 0, 0], [0, 0, 14]]}}}),
    );
    assert_eq!(snip["feature"]["status"], "ok", "{snip}");
    let flat = call(
        &mut h,
        "add_feature",
        json!({"name": "Flat bottom", "kind": {"Split": {"body": pile_body, "plane": {"origin": [0, 0, 0], "x_dir": [1, 0, 0], "normal": [0, 0, 1]}, "offset": "1.5", "new_name": "", "keep": "Above"}}}),
    );
    assert_eq!(flat["feature"]["status"], "ok", "{flat}");

    let m = call(&mut h, "measure", json!({"body": pile_body}));
    let b = &m["bodies"][0];
    assert_eq!(b["mesh"]["watertight"], true, "{b}");
    assert!((b["bbox"]["min"][2].as_f64().unwrap() - 1.5).abs() < 1e-6, "{b}");
    assert!(b["bbox"]["max"][0].as_f64().unwrap() <= 9.0 - 0.5 + 1e-6, "{b}");
    assert!(b["volume_mm3"].as_f64().unwrap() < pile.volume());

    // the pile and the post do not touch (and the copy 14 mm up would not either)
    let i = call(&mut h, "interference", json!({}));
    assert!(i["pairs"].as_array().map_or(true, |p| p.is_empty()), "{i}");

    // solid-only features refuse a mesh body, and are taken back
    let err = call_err(&mut h, "add_feature", json!({"name": "Round", "kind": {"Shell": {"body": pile_body, "open_faces": [], "thickness": "1"}}}));
    assert!(err.contains("mesh body"), "{err}");

    // watertight STL out
    let out = dir.join("pile_fit.stl");
    call(&mut h, "export", json!({"path": out.display().to_string()}));
    let raw = meshbody::read_mesh(&std::fs::read(&out).unwrap(), "stl").unwrap();
    let w = easycad_model::export::weld(&easycad_model::occt::Mesh {
        positions: raw.positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
        indices: raw.tris.clone(),
        ..Default::default()
    });
    assert_eq!(easycad_model::export::open_edges(&w), 0);

    // Move the pile up 14 mm with a copy: still a mesh body
    let mv = call(
        &mut h,
        "add_feature",
        json!({"name": "Pile at B", "kind": {"TransformBody": {"body": pile_body, "translate": [0, 0, 14], "copy": true, "new_name": "Pile B"}}}),
    );
    let bb = mv["bodies"].as_array().unwrap().iter().find(|b| b["name"] == "Pile B").expect("copy").clone();
    assert_eq!(bb["mesh_body"], true);
    assert!((bb["bbox"]["min"][2].as_f64().unwrap() - 15.5).abs() < 1e-6, "{bb}");
}

/// Decode the image block of a screenshot reply.
fn shot_image(c: &[Value]) -> image::RgbaImage {
    assert_eq!(c[0]["type"], "image");
    image::load_from_memory(&base64_decode(c[0]["data"].as_str().unwrap())).expect("PNG").to_rgba8()
}

/// Pixels of the highlight orange.
fn orange(img: &image::RgbaImage) -> usize {
    img.pixels().filter(|p| p.0[0] > 190 && p.0[1] > 50 && p.0[1] < 150 && p.0[2] < 90).count()
}

/// A closed hollow box with a block inside it, built through the bridge.
fn box_with_block(h: &mut Harness<'static, EasyCadApp>) -> (Value, u64) {
    call(h, "set_param", json!({"params": [{"name": "w", "expr": 60}, {"name": "wall", "expr": 2}]}));
    let sk = call(h, "add_sketch", json!({"name": "Base", "plane": "XY", "elements": [{"type": "rect", "center": [0, 0], "width": "w", "height": 40}]}));
    call(h, "add_feature", json!({"name": "Box", "kind": {"Extrude": {"sketch": sk["sketch_id"], "profiles": "All", "distance": 30, "op": "NewBody"}}}));
    let shell = call(h, "add_feature", json!({"name": "Hollow", "kind": {"Shell": {"open_faces": [], "thickness": "wall"}}}));
    assert_eq!(shell["feature"]["status"], "ok", "{shell}");
    let sk2 = call(h, "add_sketch", json!({"name": "Inner", "plane": "XY", "offset": 5, "elements": [{"type": "rect", "center": [0, 0], "width": 20, "height": 10}]}));
    call(h, "add_feature", json!({"name": "Servo", "kind": {"Extrude": {"sketch": sk2["sketch_id"], "profiles": "All", "distance": 10, "op": "NewBody"}}}));
    let id = shell["feature"]["id"].as_u64().unwrap();
    (shell, id)
}

/// Share of pixels that differ noticeably.
fn diff(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions());
    let n = a.pixels().zip(b.pixels()).filter(|(p, q)| (0..3).any(|i| (p.0[i] as i32 - q.0[i] as i32).abs() > 24)).count();
    n as f64 / (a.width() * a.height()) as f64
}

#[test]
fn screenshots_that_help_claude_see() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Seeing"));
    h.state_mut().mcp = Some(Bridge::new());
    let (shell, _) = box_with_block(&mut h);

    let c = call_raw(&mut h, "screenshot", json!({"view": "front"})).expect("front");
    assert!(c[1]["text"].as_str().unwrap().starts_with("Front"), "{}", c[1]["text"]);
    let plain = shot_image(&c);
    assert_eq!(plain.width(), 1024);

    // a contact sheet of four views does not move Claude's camera
    let c = call_raw(&mut h, "screenshot", json!({"views": ["front", "top", "right", "iso"], "max_width": 800})).expect("sheet");
    let sheet = shot_image(&c);
    // the front cell is the same picture as the front view (60 wide, 30 high)
    let front = image::imageops::resize(&plain, 398, 299, image::imageops::FilterType::Triangle);
    let cell = image::imageops::crop_imm(&sheet, 0, 0, 398, 299).to_image();
    let wide = |img: &image::RgbaImage| {
        let body: Vec<(u32, u32)> = img.enumerate_pixels().filter(|(_, y, p)| *y > 40 && p.0[0] > 140 && p.0[1] > 140).map(|(x, y, _)| (x, y)).collect();
        let (x0, x1) = (body.iter().map(|p| p.0).min().unwrap(), body.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (body.iter().map(|p| p.1).min().unwrap(), body.iter().map(|p| p.1).max().unwrap());
        (x1 - x0) as f64 / (y1 - y0) as f64
    };
    assert!(wide(&cell) > 1.7 && wide(&front) > 1.7, "front views are 2:1 wide, got {} / {}", wide(&cell), wide(&front));
    assert!(sheet.width() <= 800 && sheet.width() > 700, "{}", sheet.width());
    assert!(c[1]["text"].as_str().unwrap().contains("4 views, 2 per row"), "{}", c[1]["text"]);
    let again = shot_image(&call_raw(&mut h, "screenshot", json!({})).expect("current"));
    assert!(diff(&plain, &again) < 0.001, "current = the last single view");

    // the box is closed: highlighting the block shows it through the faded box
    let c = call_raw(&mut h, "screenshot", json!({"highlight": {"bodies": ["Servo"]}})).expect("ghost");
    let ghost = shot_image(&c);
    assert!(orange(&ghost) > 500 && orange(&plain) < 50, "ghosted {} plain {}", orange(&ghost), orange(&plain));

    // a selector, the faces a feature made, a selector that matches nothing
    let c = call_raw(&mut h, "screenshot", json!({"view": "top front right", "highlight": {"faces": [{"Extreme": {"normal": [0, 0, 1], "which": "Max"}}], "body": "Box"}})).expect("faces");
    assert!(orange(&shot_image(&c)) > 2000, "{}", orange(&shot_image(&c)));
    assert!(c[1]["text"].as_str().unwrap().contains("highlighted (orange): 1 faces"), "{}", c[1]["text"]);
    let c = call_raw(&mut h, "screenshot", json!({"highlight": {"features": [shell["feature"]["id"]]}})).expect("feature");
    assert!(c[1]["text"].as_str().unwrap().contains("6 faces"), "{}", c[1]["text"]);
    let c = call_raw(&mut h, "screenshot", json!({"highlight": {"edges": [{"Parallel": {"dir": [0, 0, 1]}}], "body": "Box"}})).expect("edges");
    assert!(c[1]["text"].as_str().unwrap().contains("8 edges"), "{}", c[1]["text"]);
    assert!(orange(&shot_image(&c)) > 300);
    let err = call_err(&mut h, "screenshot", json!({"highlight": {"faces": [{"Cylinders": {"radius": 99.0, "axis": null}}]}}));
    assert!(err.contains("matched no face"), "{err}");

    // a section stays on until removed, and looks straight at the cut
    let c = call_raw(&mut h, "screenshot", json!({"section": {"axis": "y", "at": 0}})).expect("section");
    let section = shot_image(&c);
    let t = c[1]["text"].as_str().unwrap();
    assert!(t.starts_with("Back") && t.contains("plane y=0.00"), "{t}");
    let c = call_raw(&mut h, "screenshot", json!({"view": "iso"})).expect("still cut");
    assert!(c[1]["text"].as_str().unwrap().contains("section: plane y"), "{}", c[1]["text"]);
    let err = call_err(&mut h, "screenshot", json!({"section": {"axis": "z", "at": 500}}));
    assert!(err.contains("outside the model"), "{err}");
    let c = call_raw(&mut h, "screenshot", json!({"section": null, "hide": ["Servo"], "display": "wireframe"})).expect("uncut");
    let t = c[1]["text"].as_str().unwrap();
    assert!(!t.contains("section: plane") && t.contains("\"hidden_bodies\":[\"Servo\"]") && t.contains("wireframe"), "{t}");
    let st = call(&mut h, "view_state", json!({"reset": true}));
    assert_eq!(st["claude_view"]["hidden_bodies"], json!([]));
    assert_eq!(st["claude_view"]["display"], "edges");

    let dir = tmp("see");
    for (name, img) in [("plain", &plain), ("sheet", &sheet), ("ghost", &ghost), ("section", &section)] {
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

#[test]
fn claude_and_the_user_see_independently() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Together"));
    h.state_mut().mcp = Some(Bridge::new());
    let (_, shell_id) = box_with_block(&mut h);
    let base = shot_image(&call_raw(&mut h, "screenshot", json!({"view": "iso"})).expect("base"));
    let vol = call(&mut h, "measure", json!({"body": "Box"}))["bodies"][0]["volume_mm3"].as_f64().unwrap();
    assert_eq!(call(&mut h, "interference", json!({}))["verdict"], "no interference");
    assert!(h.state_mut().settle(Duration::from_secs(60)));

    // the user changes everything about their own view
    let servo = h.state().model.as_ref().unwrap().bodies.iter().find(|b| b.body.name == "Servo").unwrap().body.id;
    {
        let app = h.state_mut();
        app.hidden.insert(servo);
        app.toggle_section();
        app.display = easycad_app::viewport::gpu::DisplayMode::Wireframe;
        app.camera.orbit(glam::Vec2::new(120.0, -40.0));
        let mb = &app.model.as_ref().unwrap().bodies[0];
        let f = mb.faces.iter().position(|f| f.normal.z > 0.99).unwrap() as u32;
        app.selection = vec![easycad_app::pickstack::PickKey { body: mb.body.id, kind: easycad_app::pickstack::PickKind::Face(f) }];
    }
    assert!(h.state_mut().settle(Duration::from_secs(60)));
    let user_pose = h.state().camera.pose();
    let user_section = h.state().section;

    // Claude's picture, numbers and document are unchanged
    let same = shot_image(&call_raw(&mut h, "screenshot", json!({})).expect("same"));
    assert!(diff(&base, &same) < 0.001, "{}", diff(&base, &same));
    assert_eq!(call(&mut h, "measure", json!({"body": "Box"}))["bodies"][0]["volume_mm3"].as_f64().unwrap(), vol);
    // the selection is described on the whole model
    let sel = call(&mut h, "get_selection", json!({}));
    assert_eq!(sel["selection"][0]["type"], "face", "{sel}");
    // and Claude's calls never touch the user's view
    call_raw(&mut h, "screenshot", json!({"view": "front", "hide": ["Box"], "section": {"axis": "x"}, "display": "shaded"})).expect("claude's own");
    call_raw(&mut h, "screenshot", json!({"views": ["top", "left"]})).expect("sheet");
    assert_eq!(h.state().camera.pose(), user_pose);
    assert_eq!(h.state().section, user_section);
    assert!(h.state().hidden.contains(&servo) && h.state().hidden.len() == 1);
    assert_eq!(h.state().display, easycad_app::viewport::gpu::DisplayMode::Wireframe);
    // Claude's view lasts across calls; "user" borrows the user's camera
    let st = call(&mut h, "view_state", json!({}));
    assert_eq!(st["claude_view"]["hidden_bodies"], json!(["Box"]), "{st}");
    let c = call_raw(&mut h, "screenshot", json!({"view": "user", "reset_view": true})).expect("user view");
    assert!(c[1]["text"].as_str().unwrap().starts_with("The user's view"));

    // a dialog that rolls the user's viewport back to before the shell
    h.state_mut().edit_feature(easycad_model::feature::FeatureId(shell_id as u32));
    assert!(h.state().dialog.is_some());
    assert!(h.state_mut().settle(Duration::from_secs(60)));
    let doc = call(&mut h, "get_document", json!({"include_definitions": false}));
    assert_eq!(doc["bodies"].as_array().unwrap().len(), 2);
    assert!(doc["features"].as_array().unwrap().iter().all(|f| f["status"] == "ok"), "{doc}");
    assert_eq!(call(&mut h, "measure", json!({"body": "Box"}))["bodies"][0]["volume_mm3"].as_f64().unwrap(), vol);
    assert_eq!(call(&mut h, "interference", json!({}))["verdict"], "no interference", "the shell is still there for Claude");
    let iso = shot_image(&call_raw(&mut h, "screenshot", json!({"view": "iso"})).expect("dialog open"));
    assert!(diff(&base, &iso) < 0.001, "{}", diff(&base, &iso));
    h.state_mut().close_dialog(false);

    // and an open sketch
    h.state_mut().open_sketch(None);
    assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    let sk = shot_image(&call_raw(&mut h, "screenshot", json!({})).expect("sketch mode"));
    assert!(diff(&base, &sk) < 0.001, "{}", diff(&base, &sk));
    // Claude keeps working while the user sketches
    let doc = call(&mut h, "get_document", json!({"include_definitions": false}));
    assert!(doc["app"]["state"].as_str().unwrap().contains("new sketch"), "{doc}");
    call(&mut h, "set_param", json!({"name": "w", "expr": 61}));
}

/// Pixels close to a colour.
fn count_rgb(img: &image::RgbaImage, c: [u8; 3]) -> usize {
    img.pixels().filter(|p| (0..3).all(|i| (p.0[i] as i32 - c[i] as i32).abs() <= 12)).count()
}

#[test]
fn sketch_pictures_clearances_and_editing_beside_an_open_sketch() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Sketches"));
    h.state_mut().mcp = Some(Bridge::new());
    box_with_block(&mut h);
    let doc = call(&mut h, "get_document", json!({"include_definitions": false}));
    let base_id = doc["sketches"][0]["id"].as_u64().unwrap();

    // ---- render_sketch: a fully constrained rectangle, dimension w = 60
    let c = call_raw(&mut h, "render_sketch", json!({"id": base_id})).expect("render");
    let img = shot_image(&c);
    let t: Value = serde_json::from_str(c[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(t["solve"]["dof"], 0, "{t}");
    assert_eq!(t["regions"].as_array().unwrap().len(), 1);
    assert!((t["regions"][0]["area_mm2"].as_f64().unwrap() - 2400.0).abs() < 1e-6, "{t}");
    assert!(t["dimensions"].as_array().unwrap().iter().any(|d| d["expr"] == "w" && d["value"] == 60.0), "{t}");
    assert!(t.get("free").is_none(), "{t}");
    assert!(img.width() <= 900 && img.height() > 200);
    assert!(count_rgb(&img, [226, 229, 233]) > 300, "constrained outline in white");
    assert!(count_rgb(&img, [236, 178, 64]) > 50, "dimension text in amber");
    assert_eq!(count_rgb(&img, [80, 160, 255]), 0, "nothing free");
    // a loose line: blue, listed with its coordinates
    let loose = call(&mut h, "add_sketch", json!({"name": "Loose", "plane": "XZ", "elements": [{"type": "line", "id": "l", "from": [0, 0], "to": [30, 10]}]}));
    let c = call_raw(&mut h, "render_sketch", json!({"id": loose["sketch_id"], "labels": "all"})).expect("render loose");
    let t: Value = serde_json::from_str(c[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(t["solve"]["dof"], 4, "{t}");
    assert_eq!(t["free"][0]["entity"], "#e0", "{t}");
    assert_eq!(t["free"][0]["b"], json!([30.0, 10.0]));
    assert_eq!(t["plane_mapping"]["sketch_y"], json!([0.0, 0.0, 1.0]));
    assert!(count_rgb(&shot_image(&c), [80, 160, 255]) > 30, "the free line is blue");
    assert!(call_err(&mut h, "render_sketch", json!({})).contains("Sketches:"));

    // ---- clearance: the block sits 3 mm above the floor (wall 2, block at z 5)
    let cl = call(&mut h, "clearance", json!({"a": "Servo"}));
    assert!((cl["min_distance_mm"].as_f64().unwrap() - 3.0).abs() < 1e-6, "{cl}");
    assert_eq!(cl["closest"]["b"], "'Box'");
    assert!((cl["closest"]["point_on_a"][2].as_f64().unwrap() - 5.0).abs() < 1e-6, "{cl}");
    assert_eq!(cl["closest"]["direction_a_to_b"], json!([0.0, 0.0, -1.0]));
    // wall thickness between two faces of one body: top outside to the ceiling inside
    let wall = call(&mut h, "clearance", json!({
        "a": {"body": "Box", "face": {"Extreme": {"normal": [0, 0, 1], "which": "Max"}}},
        "b": {"body": "Box", "face": {"AllWithNormal": {"normal": [0, 0, -1]}}},
    }));
    assert!((wall["min_distance_mm"].as_f64().unwrap() - 2.0).abs() < 1e-6, "{wall}");
    assert!(call_err(&mut h, "clearance", json!({"a": "Nope"})).contains("bodies:"));
    assert!(call_err(&mut h, "clearance", json!({"a": {"body": "Box", "face": {"Cylinders": {}}}})).contains("matched nothing"));

    // ---- the user opens the Base sketch; Claude keeps editing around it
    h.state_mut().open_sketch(Some(easycad_model::feature::SketchId(base_id as u32)));
    assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    let doc = call(&mut h, "get_document", json!({"include_definitions": false}));
    assert!(doc["app"]["state"].as_str().unwrap().contains(&format!("(id {base_id})")), "{doc}");
    call(&mut h, "set_param", json!({"name": "wall", "expr": 2.5}));
    if let easycad_app::app::Mode::Sketch(s) = &h.state().mode {
        assert_eq!(s.values.get("wall"), Some(&2.5), "the open sketch sees Claude's parameter");
    }
    let err = call_err(&mut h, "add_sketch", json!({"name": "X", "replace": base_id, "plane": "XY", "elements": [{"type": "circle", "center": [0, 0], "radius": 3}]}));
    assert!(err.contains("open"), "{err}");
    // their draft, unfinished: render_sketch shows it
    if let easycad_app::app::Mode::Sketch(s) = &mut h.state_mut().mode {
        s.sketch.circle(glam::DVec2::new(0.0, 0.0), 5.0);
        s.dirty = true;
    }
    let draft: Value = serde_json::from_str(call_raw(&mut h, "render_sketch", json!({})).unwrap()[1]["text"].as_str().unwrap()).unwrap();
    assert!(draft["source"].as_str().unwrap().contains("user's open sketch"), "{draft}");
    let saved: Value = serde_json::from_str(call_raw(&mut h, "render_sketch", json!({"id": base_id, "saved": true})).unwrap()[1]["text"].as_str().unwrap()).unwrap();
    let n = saved["counts"]["entities"].as_u64().unwrap();
    assert_eq!(draft["counts"]["entities"].as_u64().unwrap(), n + 1, "{draft}");
    assert_eq!(draft["free"][0]["kind"], "circle", "their new circle is free: {draft}");
    // they finish: both their sketch and Claude's parameter are in the document
    h.state_mut().finish_sketch();
    assert!(h.state_mut().settle(Duration::from_secs(60)));
    let d = &h.state().doc.data;
    assert_eq!(d.sketch(easycad_model::feature::SketchId(base_id as u32)).unwrap().entities.len() as u64, n + 1);
    assert!(d.params.list.iter().any(|p| p.name == "wall" && p.expr.0 == "2.5"));
}

fn base64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let b: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::new();
    for ch in b.chunks(4) {
        let n = ch.iter().enumerate().fold(0u32, |acc, (i, c)| acc | (val(*c) as u32) << (18 - 6 * i));
        out.push((n >> 16) as u8);
        if ch.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if ch.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}
