//! `cargo run --release --manifest-path tools/render-icon/Cargo.toml -- assets/icon/easycad.svg assets/icon`
//! writes `easycad-<N>.png` for each size below.

const SIZES: &[u32] = &[16, 24, 32, 48, 64, 120, 128, 256, 512];

fn main() {
    let mut args = std::env::args().skip(1);
    let (svg, out) = match (args.next(), args.next()) {
        (Some(s), Some(o)) => (s, std::path::PathBuf::from(o)),
        _ => {
            eprintln!("usage: render-icon SVG OUT_DIR");
            std::process::exit(2);
        }
    };
    let data = std::fs::read(&svg).expect("read svg");
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).expect("parse svg");
    std::fs::create_dir_all(&out).expect("out dir");
    for &n in SIZES {
        let mut pix = resvg::tiny_skia::Pixmap::new(n, n).expect("pixmap");
        let sc = n as f32 / tree.size().width();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(sc, sc), &mut pix.as_mut());
        let p = out.join(format!("easycad-{n}.png"));
        pix.save_png(&p).expect("write png");
        println!("{}", p.display());
    }
}
