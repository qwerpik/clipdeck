//! Offscreen screenshots of the UI with demo data, drawn by Slint's software renderer.
//! No window is opened. Used for docs and visual checks: `cargo run -p clipdeck-ui --example snapshot`.

use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

use crate::{AppWindow, SoundItem};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn settle(window: &MinimalSoftwareWindow) {
    // Let springs and fades finish.
    for _ in 0..40 {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(20));
    }
    window.request_redraw();
}

fn save(window: &MinimalSoftwareWindow, w: u32, h: u32, path: &Path) -> Result<(), String> {
    settle(window);
    let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });
    let mut img = image::RgbaImage::new(w, h);
    for (px, c) in img.pixels_mut().zip(&buf) {
        *px = image::Rgba([c.red, c.green, c.blue, 255]);
    }
    img.save(path).map_err(|e| e.to_string())
}

/// Write `bar.png` (library with a sound playing and a recording running) and `settings.png`
/// into `dir`.
pub fn run(dir: &Path) -> Result<(), String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))
        .map_err(|e| e.to_string())?;
    let app = AppWindow::new().map_err(|e| e.to_string())?;
    crate::theme::apply(&app, crate::theme::SEEDS[0], true);
    let names = [
        "Air horn",
        "Bruh",
        "Sad trombone",
        "Recording 2026-11-27 21-09-59",
        "Vine boom",
        "Wow",
    ];
    let sounds: Vec<SoundItem> = names
        .iter()
        .enumerate()
        .map(|(i, n)| SoundItem {
            id: i as i32 + 1,
            index: i as i32 + 1,
            name: (*n).into(),
            hotkey: if i == 0 {
                "Ctrl+Alt+1".into()
            } else {
                "".into()
            },
            duration: "0:03".into(),
            playing: i == 2,
            progress: if i == 2 { 0.4 } else { 0.0 },
            ..Default::default()
        })
        .collect();
    app.set_sounds(ModelRc::new(VecModel::from(sounds)));
    app.set_library_count(names.len() as i32);
    app.set_page_title("Library".into());
    app.set_np_active(true);
    app.set_np_title("Sad trombone".into());
    app.set_np_progress(0.4);
    app.set_np_time("1.2 s / 3.0 s".into());
    app.set_np_count(1);
    app.set_recording(true);
    app.set_rec_time("0:12".into());
    app.set_rec_level(0.4);
    app.set_mic_ok(true);
    app.set_after_end(1);
    let s = app.global::<crate::Settings>();
    s.set_rec_folder("/home/you/Music/Clipdeck".into());
    s.set_rec_add(true);
    s.set_rec_category("Recordings".into());
    s.set_rec_trim(true);
    s.set_rec_normalize(true);
    s.set_rec_max(300.0);
    s.set_record_hotkey("Ctrl+Alt+R".into());
    s.set_version(env!("CARGO_PKG_VERSION").into());

    let (w, h) = (1180, 760);
    window.set_size(PhysicalSize::new(w, h));
    app.show().map_err(|e| e.to_string())?;
    save(&window, w, h, &dir.join("bar.png"))?;

    app.set_page(4);
    let (w, h) = (1180, 3400);
    window.set_size(PhysicalSize::new(w, h));
    save(&window, w, h, &dir.join("settings.png"))?;
    app.hide().map_err(|e| e.to_string())?;
    Ok(())
}
