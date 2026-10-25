//! What all views of one PDF share: the tile cache, page links and page text.
//! Panes showing the same file hold the same [`SharedDocument`], so a page is
//! loaded, rendered and its text extracted once however many panes show it.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use temzdf_scene::Images;

use crate::doc_cache::DocCache;
use crate::pdf::{Link, PageBounds};
use crate::selection::PageText;

pub type SharedDocument = Rc<RefCell<Document>>;

pub struct Document {
    pub cache: DocCache,
    /// Links per page, filled as pages load.
    links: Vec<Option<Vec<Link>>>,
    /// Page text for selection, extracted on demand.
    pub(crate) texts: Vec<Option<PageText>>,
}

impl Document {
    pub fn new(bounds: Vec<PageBounds>) -> Self {
        let pages = bounds.len();
        Self { cache: DocCache::new(bounds), links: vec![None; pages], texts: vec![None; pages] }
    }

    pub fn shared(bounds: Vec<PageBounds>) -> SharedDocument {
        Rc::new(RefCell::new(Self::new(bounds)))
    }

    pub fn set_links(&mut self, page: usize, links: Vec<Link>) {
        if let Some(slot) = self.links.get_mut(page) {
            *slot = Some(links);
        }
    }

    /// A page's links, once it loaded.
    pub fn links(&self, page: usize) -> Option<&[Link]> {
        self.links.get(page)?.as_deref()
    }

    /// The link at a page-space point.
    pub fn link_at(&self, page: usize, x: f32, y: f32) -> Option<&Link> {
        self.links.get(page)?.as_ref()?.iter().find(|l| {
            let r = &l.rect;
            x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
        })
    }

    /// The document is closed: drop its tiles from the image store.
    pub fn release(&mut self, images: &mut Images) {
        self.replace(images, Vec::new());
    }

    /// A new version of the file: forget tiles, lists, links and text.
    pub fn replace(&mut self, images: &mut Images, bounds: Vec<PageBounds>) {
        let pages = bounds.len();
        self.cache.replace(images, bounds);
        self.links = vec![None; pages];
        self.texts = vec![None; pages];
    }
}

/// Text of a page, extracted on first use from its display list.
pub fn text_of_page(doc: &SharedDocument, page: usize) -> Option<Ref<'_, PageText>> {
    if doc.borrow().texts.get(page)?.is_none() {
        let mut d = doc.borrow_mut();
        let list = d.cache.display_list(page)?;
        let text = PageText::extract(&list, d.cache.bounds[page]).unwrap_or_default();
        d.texts[page] = Some(text);
    }
    Ref::filter_map(doc.borrow(), |d| d.texts[page].as_ref()).ok()
}
