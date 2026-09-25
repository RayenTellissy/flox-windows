//! Renders a bare AppWindow (no shell, default properties) with the software renderer
//! to `target/snapshots/scaffold.png`, to check the window, fonts and tokens load.

use std::path::PathBuf;
use std::rc::Rc;

use flox_app::AppWindow;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

struct SnapshotPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for SnapshotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

#[test]
fn scaffold_renders_home_on_canvas() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform {
        window: window.clone(),
    }))
    .unwrap();

    let app = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    app.show().unwrap();

    let mut buffer = vec![Rgb8Pixel::new(0, 0, 0); (WIDTH * HEIGHT) as usize];
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(drawn, "nothing was drawn");

    // The bottom-right corner is plain canvas (#0E0E0E).
    let corner = buffer[(WIDTH * HEIGHT - 1) as usize];
    assert_eq!((corner.r, corner.g, corner.b), (0x0E, 0x0E, 0x0E));
    // Text was drawn: some pixels are much brighter than the canvas.
    assert!(buffer
        .iter()
        .any(|p| p.r > 0xC0 && p.g > 0xC0 && p.b > 0xC0));

    let mut image = image::RgbImage::new(WIDTH, HEIGHT);
    for (i, p) in buffer.iter().enumerate() {
        let i = i as u32;
        image.put_pixel(i % WIDTH, i / WIDTH, image::Rgb([p.r, p.g, p.b]));
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save(dir.join("scaffold.png")).unwrap();
}
