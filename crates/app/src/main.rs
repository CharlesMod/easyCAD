//! `casework [path.cwk | --sample]`

use casework_app::{native_options, CaseworkApp};
use casework_model::document::Document;

fn main() -> eframe::Result<()> {
    let arg = std::env::args().nth(1);
    let (doc, path) = match arg.as_deref() {
        Some("--sample") => (Some(casework_app::sample::sensor_node()), None),
        Some(p) => match Document::load_from_dir(std::path::Path::new(p)) {
            Ok(d) => (Some(d), Some(std::path::PathBuf::from(p))),
            Err(e) => {
                eprintln!("casework: cannot open {p}: {e}");
                (None, None)
            }
        },
        None => (None, None),
    };
    eframe::run_native(
        "Casework",
        native_options(),
        Box::new(move |cc| {
            let mut app = CaseworkApp::new(cc, doc);
            app.path = path;
            Ok(Box::new(app))
        }),
    )
}
