//! Widget kit that renders UI layers to RGBA images for termpix.
//!
//! Drawing uses `vello_cpu`, text uses `parley` with system fonts. Widgets render
//! into their own images so the compositor only re-uploads what changed.

pub mod canvas;
pub mod widgets;

pub use canvas::{Canvas, Color, Text, TextLayout, TextStyle};
pub use widgets::{EditResult, LineInput, ListItem, ListView, StatusBar, Theme};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_text_including_cjk() {
        let mut text = Text::new();
        let style = TextStyle { size: 18.0, color: Color::rgb(20, 20, 20), bold: false, family: "sans-serif" };
        let layout = text.layout("Search: 数式 needle", &style, None);
        assert!(layout.width() > 100.0, "width {}", layout.width());
        let mut c = Canvas::new(320, 40);
        c.fill_rect(0.0, 0.0, 320.0, 40.0, Color::rgb(250, 250, 250));
        c.text(&layout, 8.0, 8.0);
        let px = c.finish();
        let dark = px.as_chunks::<4>().0.iter().filter(|p| p[0] < 100).count();
        assert!(dark > 100, "text was drawn ({dark} dark pixels)");
        if let Ok(dir) = std::env::var("TERMPIX_UI_SNAPSHOT_DIR") {
            let img = termpix::raster::RgbaImage::from_raw(320, 40, px).unwrap();
            img.save_png(&std::path::Path::new(&dir).join("text.png")).unwrap();
        }
    }

    #[test]
    fn ellipsis_fits_width() {
        let mut text = Text::new();
        let style = TextStyle { size: 14.0, color: Color::rgb(0, 0, 0), bold: false, family: "sans-serif" };
        let l = text.line("A very long section title that does not fit in the sidebar", &style, 120.0);
        assert!(l.width() <= 120.0);
    }
}
