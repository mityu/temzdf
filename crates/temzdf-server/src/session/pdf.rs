//! PDF documents of the session: opening them, the panes waiting for one to
//! open, and forward search.

use super::*;

pub(super) struct DocEntry {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) host: Option<DocumentHost>,
    pub(super) opening: Option<Opening>,
    /// Panes to create once it opened, and the tabs they were asked for.
    pub(super) waiting: Vec<(Spot, tabs::TabId)>,
    forward: Option<forward::Server>,
    /// A forward search that came while it opened (its socket listens from
    /// the start: `temzdf --forward` sends one to the session it started).
    pub(super) forward_waiting: Option<Forward>,
    /// How the last pane showed it, for the next one.
    pub(super) view: Option<ViewState>,
}

/// A file opens as a PDF by its extension, otherwise as text.
fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

impl Session {
    /// Open `path` in a new pane: the first one, or beside the focused pane.
    /// A file already open is shared, not opened again.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        self.open_at(path, self.next_spot())
    }

    pub(super) fn open_at(&mut self, path: &Path, spot: Spot) -> Result<()> {
        if !is_pdf(path) {
            return self.open_text(path, spot, true).map(drop);
        }
        let canonical = crate::paths::canonical(path);
        if let Some((&id, entry)) = self.docs.iter_mut().find(|(_, d)| d.path == canonical) {
            if entry.host.is_some() {
                self.add_pdf_pane(id, spot);
            } else {
                let spot = self.pin_spot(spot);
                self.docs.get_mut(&id).expect("found").waiting.push(spot);
            }
            return Ok(());
        }
        let id = self.alloc_id();
        let events = self.events.clone();
        let sink: temzdf_pdfcore::workers::Sink = Arc::new(move |e| events(SessionEvent::Doc(id, e)));
        let opening = Opening::start(path, sink)?;
        let forward_server = if self.settings.serve_forward {
            let events = self.events.clone();
            // A second viewer of the same file goes without, until the
            // first closes it (serve_free_forward_sockets).
            forward::serve(path, move |req| events(SessionEvent::Forward(id, req))).ok()
        } else {
            None
        };
        let name = crate::paths::display_name(path);
        let spot = self.pin_spot(spot);
        self.docs.insert(
            id,
            DocEntry {
                path: canonical,
                name,
                host: None,
                opening: Some(opening),
                waiting: vec![spot],
                forward: forward_server,
                forward_waiting: None,
                view: None,
            },
        );
        Ok(())
    }

    /// `spot` as asked for now, to wait for a document: a float goes to the
    /// current tab rather than the one current once the file opened, and a
    /// pane in the layout to this tab's layout.
    pub(super) fn pin_spot(&self, spot: Spot) -> (Spot, tabs::TabId) {
        let spot = match spot {
            Spot::Float(spec) if spec.tab.is_none() => {
                Spot::Float(floats::FloatSpec { tab: Some(tabs::FloatTab::Only(self.tab_id())), ..spec })
            }
            spot => spot,
        };
        (spot, self.tab_id())
    }

    /// A pane that waited for document `doc` to open: in the tab it was
    /// asked for (see `pin_spot`), without taking the focus from this one.
    pub(super) fn add_waiting_pane(&mut self, doc: DocId, spot: Spot, tab: tabs::TabId) {
        if !matches!(spot, Spot::Split { .. }) || tab == self.tab_id() {
            self.add_pdf_pane(doc, spot);
            return;
        }
        // Its tab closed meanwhile: so did the pane.
        let Some(i) = self.tab_index(tab) else { return };
        let here = self.tab;
        self.swap_tab(i);
        if let Some(p) = self.add_pdf_pane_focused(doc, spot, false) {
            // The pane that tab shows focused when entered.
            (self.focus, self.tiled_focus) = (p, p);
        }
        self.swap_tab(here);
        self.relayout();
    }

    /// Create a pane on an open document (or show it in a pane).
    pub(super) fn add_pdf_pane(&mut self, doc: DocId, spot: Spot) -> Option<PaneId> {
        let focus = self.takes_focus(&spot);
        self.add_pdf_pane_focused(doc, spot, focus)
    }

    /// `add_pdf_pane`, the new pane focused if `focus`. Unlike `add_pane`,
    /// what goes wrong is a message (the document opened in the
    /// background, nobody waits for an answer).
    fn add_pdf_pane_focused(&mut self, doc: DocId, spot: Spot, focus: bool) -> Option<PaneId> {
        self.docs.get(&doc)?.host.as_ref()?;
        if let Spot::Replace(p) = spot {
            if let Err(e) = self.show_in(p, doc) {
                self.message(&e);
            }
            return Some(p);
        }
        match self.open_pane(doc, spot, focus) {
            Ok(id) => Some(id),
            Err(e) => {
                self.message(&e);
                None
            }
        }
    }

    /// Listen for forward searches about documents another viewer had the
    /// socket of, once it is free (that viewer closed the file or ended):
    /// else `temzdf --forward` would find nobody and start a new session.
    /// The server asks every few seconds.
    pub fn serve_free_forward_sockets(&mut self) {
        if !self.settings.serve_forward {
            return;
        }
        for (&id, d) in self.docs.iter_mut().filter(|(_, d)| d.forward.is_none()) {
            let events = self.events.clone();
            d.forward = forward::serve_if_free(&d.path, move |req| events(SessionEvent::Forward(id, req)));
        }
    }

    /// Ask for a forward search in the focused pane's document.
    pub fn forward(&mut self, req: Forward) {
        if let Some(doc) = self.panes.get(&self.focus).and_then(PaneEntry::doc) {
            self.handle(SessionEvent::Forward(doc, req), Instant::now());
        }
    }
}
