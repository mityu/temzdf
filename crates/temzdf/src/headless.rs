//! `--render-frame`: render one screen of the viewer into a PNG without a
//! terminal. Runs the same `Viewer` code as the interactive mode, does the work
//! inline, and replays the compositor output through termpix's mock terminal, so
//! the image is what a kitty-compatible terminal would show.

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::mock::MockTerminal;

use crate::pdf;
use crate::viewer::{Need, Rendered, Viewer};

pub struct FrameOptions {
    pub geometry: Geometry,
    /// 0-based page to scroll to.
    pub page: Option<usize>,
    /// Zoom (pixels per point); fit width when `None`.
    pub scale: Option<f32>,
}

pub fn render_frame(pdf_path: &Path, out: &Path, opts: &FrameOptions) -> Result<()> {
    let doc = pdf::open(pdf_path)?;
    let mut viewer = Viewer::new(pdf::page_bounds(&doc)?, opts.geometry);
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, opts.geometry);
    if let Some(scale) = opts.scale {
        viewer.set_scale(scale, (0.0, 0.0));
    }
    if let Some(page) = opts.page {
        viewer.goto_page(page, false, Instant::now());
    }
    loop {
        let (layers, needs) = viewer.frame(&mut comp);
        if needs.is_empty() {
            let mut bytes = Vec::new();
            comp.present(&layers, &mut bytes)?;
            let mut term = MockTerminal::new(opts.geometry, [0, 0, 0, 255]);
            term.feed(&bytes);
            term.render().save_png(out)?;
            return Ok(());
        }
        for need in needs {
            match need {
                Need::LoadPage(p) => viewer.page_loaded(p, pdf::display_list(&doc, p)?),
                Need::Render(job) => {
                    let pixels = pdf::render(&job.list, job.bounds, job.scale, job.area)?;
                    let r = Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels };
                    viewer.rendered(&mut comp, r);
                }
            }
        }
    }
}
