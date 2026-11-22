//! The session's frame: the panes' layers composed with borders, floats and
//! the tab line.

use super::*;

/// Placement keys of the panes' layers, made unique across panes and kept
/// stable from frame to frame (so the compositor moves rather than re-creates).
/// A layer cut by floats gets a key per piece; the first piece keeps the key
/// the whole layer had.
#[derive(Default)]
pub(super) struct Keys {
    map: HashMap<(u32, u32, u32), u32>,
    next: u32,
    /// The keys placed this frame; the others are forgotten after it.
    used: HashSet<(u32, u32, u32)>,
}

impl Keys {
    fn get(&mut self, k: (u32, u32, u32)) -> u32 {
        self.used.insert(k);
        let next = &mut self.next;
        *self.map.entry(k).or_insert_with(|| {
            *next = next.wrapping_add(1).max(1);
            *next
        })
    }

    /// Add `layer` (`key` of `owner`) to `out`, cut around `covers` (pixels).
    fn place(&mut self, out: &mut Vec<Layer>, owner: u32, key: u32, layer: Layer, covers: &[Rect]) {
        for (i, piece) in clip::layer_pieces(&layer, covers).into_iter().enumerate() {
            out.push(Layer { key: self.get((owner, key, i as u32)), ..piece });
        }
    }

    /// Forget the keys not placed since the last call.
    fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.map.retain(|k, _| used.contains(k));
    }
}

/// What a text pane's frame is drawn from ([`Session::text_frame`]): the
/// frame cached with it is shown again while it is equal. The buffer's
/// text and marks, the session's highlight groups, patterns and images
/// by their stamps (`temzdf_texteditor::Stamped`: any mutable access
/// takes a new one), the rest by value.
#[derive(PartialEq)]
pub(super) struct TextFrameKey {
    buf: BufferId,
    stamps: (u64, u64),
    /// The buffer's word characters (patterns' `\b`).
    wordchars: String,
    /// The buffer's name and flags.
    title: String,
    view: temzdf_texteditor::View,
    shape: CursorShape,
    focused: bool,
    session: (u64, u64, u64),
    cell: termpix_core::geom::CellSize,
}

#[cfg(test)]
thread_local! {
    /// Text panes' frames drawn on this thread, not taken from the cache
    /// (tests count them).
    pub(super) static TEXT_FRAMES_DRAWN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a cached text pane's frame is drawn again to check it is the
/// same: in tests, and in debug builds with `TEMZDF_CHECK_FRAME_CACHE` set
/// (`temzdf test`, the simulated terminal's tests).
fn check_text_frames() -> bool {
    static CHECK: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        cfg!(test) || cfg!(debug_assertions) && std::env::var_os("TEMZDF_CHECK_FRAME_CACHE").is_some()
    });
    *CHECK
}

impl Session {
    /// Compose this frame from every pane and hand their work to the documents.
    pub fn frame(&mut self) -> Frame {
        let cell = self.geometry.cell;
        // No screen-wide background: terminal panes show the terminal's own
        // background, so only the gaps between panes get one (border_layers).
        let mut layers = Vec::new();
        let mut text = Vec::new();
        let mut order = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        order.extend(self.shown_floats().iter().map(|f| f.pane));
        let mut needs: HashMap<DocId, Vec<Need>> = HashMap::new();
        let mut md_jobs = Vec::new();
        let mut cursor = None;
        let mut cursor_shape = CursorShape::Default;
        let mut title = self.name.clone();
        let mut complete = true;
        let mut regions = Vec::new();
        let mut fallback_text = Vec::new();
        // The focused pane's work first.
        let mut ids = order.clone();
        ids.sort_by_key(|id| *id != self.focus);
        for id in ids {
            // What lies above this pane, in cells and in pixels.
            let covers = self.covers(Some(id));
            let covers_px: Vec<Rect> = covers.iter().map(|r| self.pixels(r)).collect();
            let Some(e) = self.panes.get_mut(&id) else { continue };
            let frame = match &mut e.kind {
                Kind::Text { buf, view } => {
                    let t = self.texts.get(buf);
                    let draw = || {
                        Self::text_frame(
                            t,
                            view,
                            id,
                            id == self.focus,
                            &self.highlights,
                            &self.patterns,
                            &self.loaded_images,
                            cell,
                        )
                    };
                    // Drawn again only when something it is drawn from
                    // changed (most frames change one pane, or none).
                    let key = t.map(|t| TextFrameKey {
                        buf: *buf,
                        stamps: t.buf.stamps(),
                        wordchars: t.buf.wordchars.class().to_owned(),
                        title: Self::text_title(t),
                        view: view.view.clone(),
                        shape: view.shape,
                        focused: id == self.focus,
                        session: (self.highlights.stamp(), self.patterns.stamp(), self.loaded_images.stamp()),
                        cell,
                    });
                    match self.text_frames.get(&id).filter(|(k, _)| Some(k) == key.as_ref()) {
                        Some((_, cached)) => {
                            if check_text_frames() {
                                assert!(
                                    *cached == draw(),
                                    "a text pane's cached frame is stale: its key misses something"
                                );
                            }
                            cached.clone()
                        }
                        None => {
                            #[cfg(test)]
                            TEXT_FRAMES_DRAWN.set(TEXT_FRAMES_DRAWN.get() + 1);
                            let frame = draw();
                            if let Some(key) = key {
                                self.text_frames.insert(id, (key, frame.clone()));
                            }
                            frame
                        }
                    }
                }
                Kind::Markdown { pane, doc } => {
                    match self.md_docs.get_mut(doc).and_then(|d| d.page_and_strips(pane.page_key())) {
                        Some((page, strips)) => {
                            let (frame, jobs) = pane.frame(u64::from(id), page, strips, *doc, &mut self.images);
                            md_jobs.extend(jobs);
                            complete &= pane.is_complete();
                            frame
                        }
                        None => Frame::default(),
                    }
                }
                Kind::Pdf { pane, doc } => {
                    let (frame, pane_needs) = pane.frame(&mut self.images);
                    needs.entry(*doc).or_default().extend(pane_needs);
                    complete &= pane.viewer.is_complete();
                    frame
                }
                Kind::Term { buf, view } => match self.terms.get_mut(buf) {
                    Some(t) => {
                        let mut frame = view.frame(&mut t.term, &mut self.images);
                        for l in &mut frame.layers {
                            l.z = l.z.max(Z_PROGRAMS_MIN);
                        }
                        frame
                    }
                    None => Frame::default(),
                },
            };
            let content = e.content();
            let (ox, oy) = (content.x * cell.width as i32, content.y * cell.height as i32);
            let clip = Rect::new(ox, oy, content.w * cell.width, content.h * cell.height);
            // A pane one row high shows its status line only.
            let frame = if content.h == 0 { Frame { title: frame.title, ..Frame::default() } } else { frame };
            // For frontends without images: where it is, and a PDF pane's
            // line saying it cannot show.
            if content.h > 0 {
                let kind = match &e.kind {
                    Kind::Pdf { .. } | Kind::Markdown { .. } => RegionKind::Pdf,
                    Kind::Term { .. } => RegionKind::Terminal,
                    Kind::Text { .. } => RegionKind::Input,
                };
                let images = kind == RegionKind::Pdf || !frame.layers.is_empty();
                regions.push(Region { pane: id, rect: content, kind, images });
                if kind == RegionKind::Pdf {
                    let what =
                        if matches!(e.kind, Kind::Markdown { .. }) { NO_GRAPHICS_MARKDOWN } else { NO_GRAPHICS_PDF };
                    let line = statusline::centred(what, content);
                    fallback_text.extend(clip::text_pieces(line, &covers));
                }
            }
            for l in frame.layers {
                let moved = Layer { dst: Rect::new(l.dst.x + ox, l.dst.y + oy, l.dst.w, l.dst.h), ..l };
                if let Some(c) = moved.clipped(&clip) {
                    self.keys.place(&mut layers, id, l.key, c, &covers_px);
                }
            }
            for g in frame.text {
                let g = TextGrid { col: g.col + content.x as u16, row: g.row + content.y as u16, ..g };
                text.extend(clip::text_pieces(g, &covers));
            }
            // A text pane's images' alt text, where images cannot be shown.
            for g in frame.fallback_text {
                let g = TextGrid { col: g.col + content.x as u16, row: g.row + content.y as u16, ..g };
                fallback_text.extend(clip::text_pieces(g, &covers));
            }
            if id == self.focus {
                cursor = frame
                    .cursor
                    .map(|(c, r)| (c + content.x as u16, r + content.y as u16))
                    .filter(|&(c, r)| !covers.iter().any(|k| k.contains(c as i32, r as i32)));
                cursor_shape = frame.cursor_shape;
                title = frame.title;
            }
        }
        self.complete = complete;
        self.text_frames.retain(|id, _| order.contains(id));
        // Status lines, under floats over them.
        for id in &order {
            let Some(e) = self.panes.get(id).filter(|e| e.status_line && e.rect.h > 0) else { continue };
            let bar = match &e.kind {
                Kind::Pdf { pane, .. } => pane.status(),
                Kind::Markdown { pane, doc } => self.md_status(pane, *doc),
                Kind::Text { buf, view } => Self::text_status(self.texts.get(buf), view, self.history.current()),
                Kind::Term { .. } => termpix_ui::StatusBar::default(),
            };
            let grid = statusline::grid(&bar, e.rect, *id == self.focus);
            text.extend(clip::text_pieces(grid, &self.covers(Some(*id))));
        }
        text.extend(self.border_grids());
        self.float_frames(&mut text);
        text.extend(self.tabline().map(|(grid, _)| grid));
        text.extend(self.window_statusline());
        self.keys.end_frame();
        for (doc, needs) in needs {
            if let Some(host) = self.docs.get_mut(&doc).and_then(|d| d.host.as_mut()) {
                host.dispatch(needs);
            }
        }
        self.md_dispatch(md_jobs);
        // Floating messages over everything.
        text.extend(self.notice_grids());
        Frame { layers, text, cursor, cursor_shape, pointer: self.pointer(), title, regions, fallback_text }
    }

    /// The borders between the tiled panes, as text (see `borders`), cut by
    /// what lies above them.
    fn border_grids(&self) -> Vec<TextGrid> {
        let Some(layout) = self.layout.as_ref() else { return Vec::new() };
        let grids = borders::gaps(self.area(), &layout.rects(self.area()), self.focus);
        let covers = self.covers(None);
        grids.into_iter().flat_map(|g| clip::text_pieces(g, &covers)).collect()
    }

    /// A rectangle of cells in screen pixels.
    pub(super) fn pixels(&self, r: &Rect) -> Rect {
        let cell = self.geometry.cell;
        Rect::new(r.x * cell.width as i32, r.y * cell.height as i32, r.w * cell.width, r.h * cell.height)
    }
}
