//! `easycad [path.ecad | --sample | --smoke <dir> | --sample-board <dir>]`

use easycad_app::{native_options, EasyCadApp};
use easycad_model::document::Document;

fn main() -> eframe::Result<()> {
    let arg = std::env::args().nth(1);
    if arg.as_deref() == Some("--smoke") {
        let dir = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "easycad-smoke".into());
        match easycad_app::smoke::run(std::path::Path::new(&dir)) {
            Ok(log) => {
                for l in log {
                    println!("{l}");
                }
                println!("smoke test passed");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("smoke test FAILED: {e}");
                std::process::exit(1);
            }
        }
    }
    if arg.as_deref() == Some("--sample-board") {
        // the sample's PCB as EasyEDA-style files, for trying the PCB wizard
        let dir = std::path::PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| ".".into()));
        let r = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join("sensor-node.step"), easycad_app::sample::pcb_step()))
            .and_then(|_| std::fs::write(dir.join("sensor-node.dxf"), easycad_app::sample::board_dxf()));
        match r {
            Ok(()) => println!("wrote {0}/sensor-node.step and {0}/sensor-node.dxf", dir.display()),
            Err(e) => {
                eprintln!("easycad: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let (doc, path) = match arg.as_deref() {
        Some("--sample") => (Some(easycad_app::sample::sensor_node()), None),
        Some(p) => match Document::load_from_dir(std::path::Path::new(p)) {
            Ok(d) => (Some(d), Some(std::path::PathBuf::from(p))),
            Err(e) => {
                eprintln!("easycad: cannot open {p}: {e}");
                (None, None)
            }
        },
        None => (None, None),
    };
    eframe::run_native(
        "easyCAD",
        native_options(),
        Box::new(move |cc| {
            let mut app = EasyCadApp::new(cc, doc);
            app.path = path;
            Ok(Box::new(app))
        }),
    )
}
