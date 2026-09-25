//! Renders the component gallery (`ui/gallery.slint`) with the software renderer to
//! `target/snapshots/gallery.png` for review against DESIGN.dark.md.

use std::path::PathBuf;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

slint::slint! {
    export { Gallery } from "../ui/gallery.slint";
}

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
fn gallery_snapshot() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform {
        window: window.clone(),
    }))
    .unwrap();

    let gallery = Gallery::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    gallery.show().unwrap();

    let mut buffer = vec![Rgb8Pixel::new(0, 0, 0); (WIDTH * HEIGHT) as usize];
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(drawn, "nothing was drawn");
    assert_eq!(buffer.len(), (WIDTH * HEIGHT) as usize);

    let rgb = |p: &Rgb8Pixel| (p.r, p.g, p.b);
    // The bottom-right corner is plain canvas (#0E0E0E).
    assert_eq!(
        rgb(&buffer[(WIDTH * HEIGHT - 1) as usize]),
        (0x0E, 0x0E, 0x0E)
    );
    // Not empty: a good share of the page differs from the canvas.
    let painted = buffer
        .iter()
        .filter(|p| rgb(p) != (0x0E, 0x0E, 0x0E))
        .count();
    assert!(
        painted > (WIDTH * HEIGHT / 10) as usize,
        "only {painted} painted pixels"
    );
    // Bright text and the inverted button surface are present.
    assert!(buffer.iter().any(|p| rgb(p) == (0xFA, 0xFA, 0xFA)));
    // The filled button's terminal-green ring is drawn.
    let green = buffer
        .iter()
        .filter(|p| rgb(p) == (0x29, 0x7A, 0x3A))
        .count();
    assert!(green > 200, "only {green} terminal-green pixels");

    let mut image = image::RgbImage::new(WIDTH, HEIGHT);
    for (i, p) in buffer.iter().enumerate() {
        let i = i as u32;
        image.put_pixel(i % WIDTH, i / WIDTH, image::Rgb([p.r, p.g, p.b]));
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gallery.png");
    image.save(&path).unwrap();
    let saved = image::open(&path).unwrap();
    assert_eq!((saved.width(), saved.height()), (WIDTH, HEIGHT));
}
