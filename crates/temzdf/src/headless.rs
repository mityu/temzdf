//! `--render-frame`: render one screen of the viewer into a PNG without a
//! terminal. Runs the same `Viewer` code as the interactive mode, does the work
//! inline, and replays the compositor output through termpix's mock terminal, so
//! the image is what a kitty-compatible terminal would show.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use temzdf_scene::Images;
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::input::KeyEvent;
use termpix::mock::MockTerminal;

use crate::keys::Key;
use crate::pane::{PdfPane, Request};
use crate::pdf;
use crate::viewer::{Need, Rendered};
use temzdf_tty::Presenter;

pub struct FrameOptions {
    pub geometry: Geometry,
    /// 0-based page to scroll to.
    pub page: Option<usize>,
    /// Zoom (pixels per point); fit width when `None`.
    pub scale: Option<f32>,
    /// Keys to press before rendering, space separated (see [`parse_keys`]).
    pub keys: Option<String>,
    /// Blend translucent layers in linear light (as kitty does).
    pub linear_blending: bool,
}

/// Parse a key list like `"Tab j j Enter C-f 3 G"`, in the notation of
/// [`Key::parse`].
pub fn parse_keys(spec: &str) -> Result<Vec<KeyEvent>> {
    spec.split_whitespace().map(|tok| Ok(Key::parse(tok)?.event())).collect()
}

pub fn render_frame(pdf_path: &Path, out: &Path, opts: &FrameOptions) -> Result<()> {
    let doc = pdf::open(pdf_path)?;
    let name =
        pdf_path.file_name().map_or_else(|| pdf_path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut session = PdfPane::new(name, pdf::doc_info(&doc)?, opts.geometry);
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, opts.geometry);
    let mut images = Images::new();
    let mut presenter = Presenter::new();
    if let Some(scale) = opts.scale {
        session.viewer.set_scale(scale, (0.0, 0.0));
    }
    if let Some(page) = opts.page {
        session.viewer.goto_page(page, false, Instant::now());
    }
    // Simulated clock, so animations started by keys settle deterministically.
    let mut now = Instant::now();
    for key in parse_keys(opts.keys.as_deref().unwrap_or(""))? {
        session.key(&key, now);
        for req in session.take_requests() {
            match req {
                Request::OpenUri(_) | Request::Copy(_) | Request::Inverse { .. } => {}
                Request::Search { generation, query, start_page, .. } => {
                    let n = session.info.bounds.len();
                    for i in 0..n {
                        let p = (start_page + i) % n;
                        let rects = pdf::search_page(&doc, p, session.info.bounds[p], &query)?;
                        session.search_hits(generation, p, rects, now);
                    }
                }
            }
        }
        for _ in 0..200 {
            if !session.viewer.is_animating() {
                break;
            }
            now += Duration::from_millis(16);
            session.viewer.tick(now);
        }
    }
    loop {
        let (layers, needs) = session.frame(&mut images);
        presenter.sync(&mut comp, &mut images);
        if needs.is_empty() {
            let mut bytes = Vec::new();
            comp.present(&presenter.layers(&layers), &mut bytes)?;
            let mut term = MockTerminal::new(opts.geometry, [0, 0, 0, 255]);
            if opts.linear_blending {
                term.set_blending(termpix::raster::Blending::Linear);
            }
            term.feed(&bytes);
            term.render().save_png(out)?;
            return Ok(());
        }
        for need in needs {
            match need {
                Need::LoadPage(p) => {
                    let generation = session.viewer.generation();
                    session.viewer.page_loaded(generation, p, pdf::display_list(&doc, p)?);
                    session.page_links(p, pdf::page_links(&doc, p, session.info.bounds[p]).unwrap_or_default());
                }
                Need::Render(job) => {
                    let pixels = pdf::render(&job.list, job.bounds, job.scale, job.area)?;
                    let r = Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels };
                    session.viewer.rendered(&mut images, r);
                }
            }
        }
    }
}
