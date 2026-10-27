//! Offscreen rendering on a software GPU (Mesa's lavapipe, from the dev
//! shell's `TEMZDF_LAVAPIPE_ICD`): image layers against a CPU reference,
//! text by its properties (no stored images to compare with). Skipped when
//! no adapter is found (the `ci` shell has none).

use temzdf_gui::drawlist::{self, Grid};
use temzdf_gui::palette::Colors;
use temzdf_gui::render::{Renderer, offscreen_device};
use temzdf_scene::{Attrs, Cell, Color, CursorShape, Frame, Images, Layer, Rect, TextGrid};
use termpix_core::raster::{Blending, RgbaImage, RgbaRef, blit_blended};

fn main() {
    if let Ok(icd) = std::env::var("TEMZDF_LAVAPIPE_ICD") {
        // SAFETY: the first thing this test program does, before any other
        // thread exists (no test harness: `harness = false`).
        unsafe { std::env::set_var("VK_DRIVER_FILES", icd) };
    }
    let Ok(fonts_dir) = std::env::var("TEMZDF_TEST_FONTS") else {
        eprintln!("SKIPPED: no TEMZDF_TEST_FONTS (the dev shell sets it)");
        return;
    };
    let (device, queue) = match offscreen_device() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SKIPPED: {e:#}");
            return;
        }
    };
    let fonts = temzdf_gui::fonts::from_dir(std::path::Path::new(&fonts_dir));
    let mut r = Renderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, fonts, 1.0);
    r.set_font(&[temzdf_scene::FontEntry { family: "UDEV Gothic NF".into(), size: Some(14.0) }], &[], 1.0);
    layers_match_the_cpu_reference(&mut r);
    text_lands_in_its_cells(&mut r);
    println!("gpu: 2 tests passed");
}

fn layers_match_the_cpu_reference(r: &mut Renderer) {
    let (w, h) = (64u32, 48u32);
    let mut images = Images::new();
    let opaque: Vec<u8> = (0..16 * 16).flat_map(|i| [(i * 7) as u8, (i * 3) as u8, 200, 255]).collect();
    let translucent = [255u8, 0, 0, 128].repeat(20 * 10);
    let a = images.add(16, 16, opaque.clone());
    let b = images.add(20, 10, translucent.clone());
    for (k, i) in [(a, &opaque), (b, &translucent)] {
        let _ = i;
        r.set_image(k, images.get(k).unwrap());
    }
    let frame = Frame {
        layers: vec![
            Layer { key: 1, image: a, src: Rect::new(0, 0, 16, 16), dst: Rect::new(5, 7, 16, 16), z: -1_073_741_900 },
            Layer { key: 2, image: a, src: Rect::new(4, 4, 8, 8), dst: Rect::new(30, 2, 8, 8), z: -1_073_741_900 },
            Layer { key: 3, image: b, src: Rect::new(0, 0, 20, 10), dst: Rect::new(10, 15, 20, 10), z: -10 },
        ],
        ..Frame::default()
    };
    let colors = Colors::default();
    let grid = Grid { cols: 8, rows: 3, cell_w: 8, cell_h: 16 };
    let got = r.draw_offscreen(&drawlist::build(&frame, grid, &colors), grid, (w, h)).unwrap();
    let bg = colors.background;
    let mut want = RgbaImage::new(w, h, [bg[0], bg[1], bg[2], 255]);
    blit_blended(
        &mut want,
        Rect::new(5, 7, 16, 16),
        RgbaRef { width: 16, height: 16, data: &opaque },
        Rect::new(0, 0, 16, 16),
        Blending::Srgb,
    );
    blit_blended(
        &mut want,
        Rect::new(30, 2, 8, 8),
        RgbaRef { width: 16, height: 16, data: &opaque },
        Rect::new(4, 4, 8, 8),
        Blending::Srgb,
    );
    blit_blended(
        &mut want,
        Rect::new(10, 15, 20, 10),
        RgbaRef { width: 20, height: 10, data: &translucent },
        Rect::new(0, 0, 20, 10),
        Blending::Srgb,
    );
    let worst = got.iter().zip(&want.data).map(|(g, w)| g.abs_diff(*w)).max().unwrap();
    assert!(worst <= 2, "layers differ from the CPU reference by up to {worst}");
}

fn text_lands_in_its_cells(r: &mut Renderer) {
    let m = r.metrics();
    let grid = Grid { cols: 8, rows: 2, cell_w: m.width, cell_h: m.height };
    let size = (grid.cols as u32 * m.width, grid.rows as u32 * m.height);
    let cell = |text: &str, width, fg, bg| Cell { text: text.into(), width, fg, bg, attrs: Attrs::default() };
    let frame = Frame {
        text: vec![TextGrid {
            col: 0,
            row: 0,
            rows: vec![vec![
                cell("", 1, Color::Default, Color::Default),
                cell("A", 1, Color::Rgb([255, 0, 0]), Color::Rgb([0, 0, 255])),
                cell("数", 2, Color::Rgb([0, 255, 0]), Color::Default),
                cell("", 0, Color::Default, Color::Default),
                cell("x", 1, Color::Default, Color::Default),
            ]],
        }],
        cursor: Some((4, 0)),
        cursor_shape: CursorShape::Block { blink: false },
        ..Frame::default()
    };
    let colors = Colors::default();
    let px = r.draw_offscreen(&drawlist::build(&frame, grid, &colors), grid, size).unwrap();
    let at = |x: u32, y: u32| -> [u8; 3] {
        let i = ((y * size.0 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    let cell_px = |col: u32, cols: u32| {
        (col * m.width..(col + cols) * m.width).flat_map(move |x| (0..m.height).map(move |y| (x, y)))
    };
    let bg = colors.background;
    assert!(cell_px(0, 1).all(|(x, y)| at(x, y) == bg), "an empty cell shows the background exactly");
    let blue = cell_px(1, 1).filter(|&(x, y)| at(x, y) == [0, 0, 255]).count();
    let red = cell_px(1, 1)
        .filter(|&(x, y)| {
            let c = at(x, y);
            c[0] > 150 && c[2] < 100
        })
        .count();
    assert!(blue > (m.width * m.height / 2) as usize && red > 5, "A: red on blue ({red} red, {blue} blue)");
    for half in [2, 3] {
        let green = cell_px(half, 1).filter(|&(x, y)| at(x, y)[1] > 120 && at(x, y)[0] < 100).count();
        assert!(green > 3, "数 spans both of its cells (cell {half}: {green})");
    }
    // The block cursor: the foreground colour, its character in the
    // background colour.
    let fg = colors.foreground;
    let on = cell_px(4, 1).filter(|&(x, y)| at(x, y) == fg).count();
    let dark = cell_px(4, 1)
        .filter(|&(x, y)| {
            let c = at(x, y);
            c[0] < 100
        })
        .count();
    assert!(on > (m.width * m.height / 2) as usize && dark > 3, "cursor over x ({on} lit, {dark} dark)");
    assert!(cell_px(5, 3).all(|(x, y)| at(x, y) == bg), "nothing beyond");
}
