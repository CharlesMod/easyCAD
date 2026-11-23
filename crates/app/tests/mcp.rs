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
    // the viewport has it
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

/// Decode the image block of a screenshot reply.
fn shot_image(c: &[Value]) -> image::RgbaImage {
    assert_eq!(c[0]["type"], "image");
    image::load_from_memory(&base64_decode(c[0]["data"].as_str().unwrap())).expect("PNG").to_rgba8()
}

/// Pixels of the highlight orange.
fn orange(img: &image::RgbaImage) -> usize {
    img.pixels().filter(|p| p.0[0] > 190 && p.0[1] > 50 && p.0[1] < 150 && p.0[2] < 90).count()
}

#[test]
fn screenshots_that_help_claude_see() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("Seeing"));
    h.state_mut().mcp = Some(Bridge::new());
    // a hollow box with a block inside it, as two bodies
    call(&mut h, "set_param", json!({"params": [{"name": "w", "expr": 60}, {"name": "wall", "expr": 2}]}));
    let sk = call(&mut h, "add_sketch", json!({"name": "Base", "plane": "XY", "elements": [{"type": "rect", "center": [0, 0], "width": "w", "height": 40}]}));
    call(&mut h, "add_feature", json!({"name": "Box", "kind": {"Extrude": {"sketch": sk["sketch_id"], "profiles": "All", "distance": 30, "op": "NewBody"}}}));
    let shell = call(&mut h, "add_feature", json!({"name": "Hollow", "kind": {"Shell": {"open_faces": [], "thickness": "wall"}}}));
    assert_eq!(shell["feature"]["status"], "ok", "{shell}");
    let sk2 = call(&mut h, "add_sketch", json!({"name": "Inner", "plane": "XY", "offset": 5, "elements": [{"type": "rect", "center": [0, 0], "width": 20, "height": 10}]}));
    call(&mut h, "add_feature", json!({"name": "Servo", "kind": {"Extrude": {"sketch": sk2["sketch_id"], "profiles": "All", "distance": 10, "op": "NewBody"}}}));

    // the user's camera, selection and hidden bodies come back unchanged
    let pose = h.state().camera.pose();
    let sel = h.state().selection.clone();
    let c = call_raw(&mut h, "screenshot", json!({"view": "front"})).expect("front");
    assert_eq!(h.state().camera.pose(), pose, "camera put back");
    assert_eq!(h.state().selection, sel);
    assert!(h.state().shot.is_none() && h.state().hidden.is_empty());
    assert!(c[1]["text"].as_str().unwrap().starts_with("Front"), "{}", c[1]["text"]);
    let plain = shot_image(&c);

    // a contact sheet of four views
    let c = call_raw(&mut h, "screenshot", json!({"views": ["front", "top", "right", "iso"], "max_width": 800})).expect("sheet");
    let sheet = shot_image(&c);
    assert!(sheet.width() <= 800 && sheet.width() > 700, "{}", sheet.width());
    assert!(c[1]["text"].as_str().unwrap().contains("4 views, 2 per row"), "{}", c[1]["text"]);
    assert_eq!(h.state().camera.pose(), pose);

    // the box is closed: from outside the block is invisible; x-ray and isolate show it
    let c = call_raw(&mut h, "screenshot", json!({"view": "front", "highlight": {"bodies": ["Servo"]}})).expect("ghost");
    let ghost = shot_image(&c);
    assert!(orange(&ghost) > 500 && orange(&plain) < 50, "ghosted {} plain {}", orange(&ghost), orange(&plain));
    let c = call_raw(&mut h, "screenshot", json!({"view": "iso", "isolate": ["Servo"]})).expect("isolate");
    assert!(c[1]["text"].as_str().unwrap().contains("hidden for this picture"));
    assert!(h.state().hidden.is_empty(), "hidden bodies put back");

    // highlight what a selector picks: the top face
    let c = call_raw(&mut h, "screenshot", json!({"view": "top front right", "highlight": {"faces": [{"Extreme": {"normal": [0, 0, 1], "which": "Max"}}], "body": "Box"}})).expect("faces");
    assert!(orange(&shot_image(&c)) > 2000, "{}", orange(&shot_image(&c)));
    assert!(c[1]["text"].as_str().unwrap().contains("highlighted (orange): 1 faces"), "{}", c[1]["text"]);
    // the faces a feature made (the shell's inner skin is behind the outer one: drawn through)
    let c = call_raw(&mut h, "screenshot", json!({"view": "iso", "highlight": {"features": [shell["feature"]["id"]]}})).expect("feature");
    assert!(c[1]["text"].as_str().unwrap().contains("6 faces"), "{}", c[1]["text"]);
    let err = call_err(&mut h, "screenshot", json!({"highlight": {"faces": [{"Cylinders": {"radius": 99.0, "axis": null}}]}}));
    assert!(err.contains("matched no face"), "{err}");

    // a section through the middle shows the wall and the block inside, looking at the cut
    let c = call_raw(&mut h, "screenshot", json!({"section": {"axis": "y", "at": 0}})).expect("section");
    let section = shot_image(&c);
    let t = c[1]["text"].as_str().unwrap();
    assert!(t.starts_with("Back") && t.contains("plane y=0.00"), "{t}");
    assert!(h.state().section.is_none(), "section put back");
    let err = call_err(&mut h, "screenshot", json!({"section": {"axis": "z", "at": 500}}));
    assert!(err.contains("outside the model"), "{err}");
    assert!(h.state().shot.is_none() && h.state().section.is_none());

    // keep_view leaves the camera at the shot's view
    call_raw(&mut h, "screenshot", json!({"view": "top", "keep_view": true})).expect("keep");
    assert_ne!(h.state().camera.pose(), pose);
    let dir = tmp("see");
    for (name, img) in [("plain", &plain), ("sheet", &sheet), ("ghost", &ghost), ("section", &section)] {
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
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
