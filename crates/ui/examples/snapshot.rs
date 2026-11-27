//! Offscreen UI screenshots (no window): `cargo run -p clipdeck-ui --example snapshot [dir]`.

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    if let Err(e) = clipdeck_ui::snapshot::run(std::path::Path::new(&dir)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
