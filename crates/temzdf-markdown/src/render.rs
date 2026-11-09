//! The text mode: a [`Document`] drawn as lines of text at a width, with
//! decorations (highlight groups, links, images) as character ranges, the
//! source line of every line drawn, and where each anchor went.
//!
//! Paragraphs are wrapped here (so quote bars and list indents continue
//! on their wrapped lines); code blocks are left to the pane's wrapping.

use unicode_segmentation::UnicodeSegmentation;

use crate::doc::{Align, Block, Document, Inline, Item, Lines};
use crate::highlight::highlight;

/// A decoration over characters `start..end` of [`Rendered::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deco {
    Style {
        start: usize,
        end: usize,
        group: &'static str,
    },
    /// A link: a URL, `#anchor` or a relative path, as written.
    Link {
        start: usize,
        end: usize,
        target: String,
    },
    /// An image shown over its alt text (`block`: below its line).
    Image {
        start: usize,
        end: usize,
        url: String,
        alt: String,
        block: bool,
    },
}

impl Deco {
    fn shifted(mut self, by: usize) -> Deco {
        match &mut self {
            Deco::Style { start, end, .. } | Deco::Link { start, end, .. } | Deco::Image { start, end, .. } => {
                *start += by;
                *end += by;
            }
        }
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    /// Lines joined by `\n`.
    pub text: String,
    pub decos: Vec<Deco>,
    /// The source line each line drawn came from.
    pub sources: Vec<usize>,
    /// Anchors (headings', `fn-LABEL` for footnotes) and the line drawn
    /// they are on.
    pub anchors: Vec<(String, usize)>,
}

impl Rendered {
    /// The line drawn nearest to source line `line` (the first one from it
    /// or after it).
    pub fn line_of_source(&self, line: usize) -> usize {
        self.sources.iter().position(|&s| s >= line).unwrap_or(self.sources.len().saturating_sub(1))
    }

    pub fn anchor_line(&self, anchor: &str) -> Option<usize> {
        self.anchors.iter().find(|(a, _)| a == anchor).map(|(_, l)| *l)
    }
}

/// Math drawn as lines of text (`None`: show the source).
pub type MathFn<'a> = &'a dyn Fn(&str) -> Option<Vec<String>>;

/// Draw `doc` at `width` columns.
pub fn render(doc: &Document, width: usize, math: MathFn<'_>) -> Rendered {
    let mut r = Renderer { out: Out::default(), width: width.max(8), math, list_depth: 0 };
    let mut footnote_rule = false;
    let mut first = true;
    for b in &doc.blocks {
        if !first {
            r.out.blank(b.lines().start, &Vec::new());
        }
        first = false;
        if matches!(b, Block::Footnote { .. }) && !footnote_rule {
            footnote_rule = true;
            let w = r.width;
            r.out.emit(Line::styled(&"─".repeat(w), Some("MarkdownRule")), &Vec::new(), b.lines().start);
            r.out.blank(b.lines().start, &Vec::new());
        }
        r.block(b, &Vec::new(), &Vec::new());
    }
    r.out.finish()
}

/// Text before a line's content (quote bars, list markers, indents).
type Prefix = Vec<(String, Option<&'static str>)>;

fn prefix_width(p: &Prefix) -> usize {
    p.iter().map(|(s, _)| width(s)).sum()
}

fn width(s: &str) -> usize {
    s.graphemes(true).map(temzdf_texteditor::grapheme_width).sum()
}

/// One line being drawn: its text and decorations (offsets in it).
#[derive(Clone, Debug, Default)]
struct Line {
    text: String,
    chars: usize,
    cols: usize,
    decos: Vec<Deco>,
}

impl Line {
    fn styled(s: &str, group: Option<&'static str>) -> Line {
        let mut l = Line::default();
        l.push(s, group);
        l
    }

    fn push(&mut self, s: &str, group: Option<&'static str>) {
        self.push_with(s, &group.into_iter().collect::<Vec<_>>(), None);
    }

    fn push_with(&mut self, s: &str, groups: &[&'static str], link: Option<&str>) {
        if s.is_empty() {
            return;
        }
        let n = s.chars().count();
        let (start, end) = (self.chars, self.chars + n);
        for g in groups {
            // Continue the same style rather than start another.
            match self
                .decos
                .iter_mut()
                .rev()
                .find(|d| matches!(d, Deco::Style { group, end: e, .. } if group == g && *e == start))
            {
                Some(Deco::Style { end: e, .. }) => *e = end,
                _ => self.decos.push(Deco::Style { start, end, group: g }),
            }
        }
        if let Some(target) = link {
            match self
                .decos
                .iter_mut()
                .rev()
                .find(|d| matches!(d, Deco::Link { target: t, end: e, .. } if t == target && *e == start))
            {
                Some(Deco::Link { end: e, .. }) => *e = end,
                _ => self.decos.push(Deco::Link { start, end, target: target.to_owned() }),
            }
        }
        self.text.push_str(s);
        self.chars = end;
        self.cols += width(s);
    }

    fn append(&mut self, other: Line) {
        let by = self.chars;
        self.decos.extend(other.decos.into_iter().map(|d| d.shifted(by)));
        self.text.push_str(&other.text);
        self.chars += other.chars;
        self.cols += other.cols;
    }
}

#[derive(Default)]
struct Out {
    text: String,
    chars: usize,
    decos: Vec<Deco>,
    sources: Vec<usize>,
    anchors: Vec<(String, usize)>,
}

impl Out {
    /// `content` after `prefix`, as the next line.
    fn emit(&mut self, content: Line, prefix: &Prefix, source: usize) {
        let mut line = Line::default();
        for (s, g) in prefix {
            line.push(s, *g);
        }
        line.append(content);
        let text = line.text.trim_end();
        let keep = text.chars().count();
        if !self.sources.is_empty() {
            self.text.push('\n');
            self.chars += 1;
        }
        let by = self.chars;
        self.decos.extend(line.decos.into_iter().filter_map(|d| {
            // Nothing past the trimmed end.
            let d = d.shifted(by);
            match d {
                Deco::Style { start, end, group } => {
                    let end = end.min(by + keep);
                    (start < end).then_some(Deco::Style { start, end, group })
                }
                d => Some(d),
            }
        }));
        self.text.push_str(text);
        self.chars += keep;
        self.sources.push(source);
    }

    /// An empty line (quote bars go on through it), of the block before
    /// it (so a source line finds its block's first line, not the gap).
    fn blank(&mut self, source: usize, prefix: &Prefix) {
        let source = self.sources.last().copied().unwrap_or(source);
        self.emit(Line::default(), prefix, source);
    }

    fn lines(&self) -> usize {
        self.sources.len()
    }

    fn finish(self) -> Rendered {
        Rendered { text: self.text, decos: self.decos, sources: self.sources, anchors: self.anchors }
    }
}

/// A piece of inline text with its styles, link or image.
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    groups: Vec<&'static str>,
    link: Option<String>,
    image: Option<(String, String)>,
}

/// What wrapping works on: words (pieces that stay together), spaces, and
/// forced breaks.
#[derive(Clone, Debug)]
enum Atom {
    /// `brk`: a line may break before it without a space (between wide
    /// characters).
    Word {
        pieces: Vec<Piece>,
        brk: bool,
    },
    Space(Piece),
    Break,
}

struct Renderer<'a> {
    out: Out,
    width: usize,
    math: MathFn<'a>,
    list_depth: usize,
}

impl Renderer<'_> {
    fn block(&mut self, b: &Block, first: &Prefix, rest: &Prefix) {
        let avail = |p: &Prefix| self.width.saturating_sub(prefix_width(p)).max(4);
        match b {
            Block::Heading { level, inlines, anchor, lines } => {
                self.out.anchors.push((anchor.clone(), self.out.lines()));
                let group = ["MarkdownH1", "MarkdownH2", "MarkdownH3", "MarkdownH4", "MarkdownH5", "MarkdownH6"]
                    [(*level as usize).clamp(1, 6) - 1];
                let atoms = self.atoms(inlines, &[group]);
                let wrapped = self.wrap(&atoms, avail(rest));
                let w = wrapped.iter().map(|l| l.cols).max().unwrap_or(0);
                self.emit_lines(wrapped, first, rest, *lines);
                let under = match level {
                    1 => "═",
                    2 => "─",
                    _ => "",
                };
                if !under.is_empty() {
                    self.out.emit(Line::styled(&under.repeat(w.max(1)), Some(group)), rest, lines.end);
                }
            }
            Block::Paragraph { inlines, lines } => {
                let atoms = self.atoms(inlines, &[]);
                let wrapped = self.wrap(&atoms, avail(rest));
                self.emit_lines(wrapped, first, rest, *lines);
                // A paragraph of one image: shown below its line.
                if let [Inline::Image { .. }] = inlines.as_slice()
                    && let Some(Deco::Image { block, .. }) =
                        self.out.decos.iter_mut().rev().find(|d| matches!(d, Deco::Image { .. }))
                {
                    *block = true;
                }
            }
            Block::List { start, tight, items, lines } => {
                self.list_depth += 1;
                let last = start.map(|s| s + items.len().saturating_sub(1));
                let num_w = last.map_or(0, |n| n.to_string().len());
                for (i, item) in items.iter().enumerate() {
                    if i > 0 && !tight {
                        self.out.blank(lines.start, &trimmed(rest));
                    }
                    // A task's box in place of the bullet (as GitHub shows it).
                    let marker = match (start, item.task) {
                        (Some(s), _) => format!("{:>num_w$}. ", s + i),
                        (None, Some(true)) => "☑ ".to_owned(),
                        (None, Some(false)) => "☐ ".to_owned(),
                        (None, None) => format!("{} ", ["•", "◦", "▪"][(self.list_depth - 1) % 3]),
                    };
                    let task = match (start, item.task) {
                        (Some(_), Some(true)) => "☑ ",
                        (Some(_), Some(false)) => "☐ ",
                        _ => "",
                    };
                    let mut item_first = if i == 0 { first.clone() } else { rest.clone() };
                    let marker_group =
                        if item.task.is_some() && start.is_none() { "MarkdownTask" } else { "MarkdownListMarker" };
                    item_first.push((marker.clone(), Some(marker_group)));
                    if !task.is_empty() {
                        item_first.push((task.to_owned(), Some("MarkdownTask")));
                    }
                    let mut item_rest = rest.clone();
                    item_rest.push((" ".repeat(width(&marker) + width(task)), None));
                    self.item(item, &item_first, &item_rest, *tight, *lines);
                }
                self.list_depth -= 1;
            }
            Block::Code { lang, text, lines } => {
                for (i, pieces) in highlight(lang, text).into_iter().enumerate() {
                    let mut l = Line::default();
                    l.push("  ", None);
                    for (t, g) in pieces {
                        let groups: Vec<&'static str> = std::iter::once("MarkdownCodeBlock").chain(g).collect();
                        l.push_with(&t, &groups, None);
                    }
                    let p = if i == 0 { first } else { rest };
                    self.out.emit(l, p, (lines.start + 1 + i).min(lines.end));
                }
            }
            Block::Math { tex, lines } => {
                let drawn = (self.math)(tex).unwrap_or_else(|| tex.lines().map(str::to_owned).collect());
                for (i, t) in drawn.iter().enumerate() {
                    let mut l = Line::default();
                    l.push("  ", None);
                    l.push(t, Some("MarkdownMath"));
                    self.out.emit(l, if i == 0 { first } else { rest }, lines.start);
                }
            }
            Block::Quote { blocks, .. } => {
                let bar = ("│ ".to_owned(), Some("MarkdownQuote"));
                let (mut f, mut r) = (first.clone(), rest.clone());
                f.push(bar.clone());
                r.push(bar);
                self.blocks(blocks, &f, &r, false);
            }
            Block::Alert { kind, title, blocks, lines } => {
                let group = match kind {
                    crate::doc::AlertKind::Note => "MarkdownAlertNote",
                    crate::doc::AlertKind::Tip => "MarkdownAlertTip",
                    crate::doc::AlertKind::Important => "MarkdownAlertImportant",
                    crate::doc::AlertKind::Warning => "MarkdownAlertWarning",
                    crate::doc::AlertKind::Caution => "MarkdownAlertCaution",
                };
                let bar = ("│ ".to_owned(), Some(group));
                let mut f = first.clone();
                f.push(bar.clone());
                let mut r = rest.clone();
                r.push(bar);
                let name = title.clone().unwrap_or_else(|| kind.name().to_owned());
                self.out.emit(Line::styled(&name, Some(group)), &f, lines.start);
                self.blocks(blocks, &r, &r, false);
            }
            Block::Table { aligns, header, rows, lines } => self.table(aligns, header, rows, first, rest, *lines),
            Block::Html { html, lines } => {
                for (i, l) in html.trim_end().lines().enumerate() {
                    self.out.emit(
                        Line::styled(l, Some("MarkdownHtml")),
                        if i == 0 { first } else { rest },
                        lines.start + i,
                    );
                }
            }
            Block::Rule { lines } => {
                let w = avail(rest);
                self.out.emit(Line::styled(&"─".repeat(w), Some("MarkdownRule")), first, lines.start);
            }
            Block::Footnote { label, number, blocks, lines } => {
                self.out.anchors.push((format!("fn-{label}"), self.out.lines()));
                let marker = format!("[{number}] ");
                let mut f = first.clone();
                f.push((marker.clone(), Some("MarkdownFootnote")));
                let mut r = rest.clone();
                r.push((" ".repeat(width(&marker)), None));
                self.item(&Item { task: None, blocks: blocks.clone() }, &f, &r, true, *lines);
            }
        }
    }

    /// A list item's blocks: the first line after `first`.
    fn item(&mut self, item: &Item, first: &Prefix, rest: &Prefix, tight: bool, lines: Lines) {
        if item.blocks.is_empty() {
            self.out.emit(Line::default(), first, lines.start);
        } else {
            self.blocks(&item.blocks, first, rest, tight);
        }
    }

    fn blocks(&mut self, blocks: &[Block], first: &Prefix, rest: &Prefix, tight: bool) {
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 && !(tight && matches!(b, Block::List { .. } | Block::Paragraph { .. })) {
                self.out.blank(b.lines().start, &trimmed(rest));
            }
            self.block(b, if i == 0 { first } else { rest }, rest);
        }
    }

    fn emit_lines(&mut self, lines: Vec<Line>, first: &Prefix, rest: &Prefix, src: Lines) {
        if lines.is_empty() {
            self.out.emit(Line::default(), first, src.start);
        }
        for (i, l) in lines.into_iter().enumerate() {
            self.out.emit(l, if i == 0 { first } else { rest }, (src.start + i).min(src.end));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        aligns: &[Align],
        header: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        first: &Prefix,
        rest: &Prefix,
        lines: Lines,
    ) {
        let n = aligns.len().max(header.len()).max(1);
        let cells = |row: &[Vec<Inline>], groups: &[&'static str], this: &Self| -> Vec<Vec<Atom>> {
            (0..n).map(|i| row.get(i).map_or_else(Vec::new, |c| this.atoms(c, groups))).collect()
        };
        let head = cells(header, &["MarkdownTableHeader"], self);
        let body: Vec<Vec<Vec<Atom>>> = rows.iter().map(|r| cells(r, &[], self)).collect();
        // Natural widths, then the widest columns narrowed until it fits.
        let mut widths = vec![1usize; n];
        for row in std::iter::once(&head).chain(&body) {
            for (i, c) in row.iter().enumerate() {
                let w: usize = self.wrap(c, usize::MAX).iter().map(|l| l.cols).max().unwrap_or(0);
                widths[i] = widths[i].max(w);
            }
        }
        let avail = self.width.saturating_sub(prefix_width(rest)).saturating_sub(3 * n + 1).max(n * 3);
        while widths.iter().sum::<usize>() > avail {
            let (i, &w) = widths.iter().enumerate().max_by_key(|(_, w)| **w).expect("columns");
            if w <= 3 {
                break;
            }
            widths[i] -= 1;
        }
        let border = |l: &str, m: &str, r: &str| {
            let mut s = l.to_owned();
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if i + 1 == n { r } else { m });
            }
            Line::styled(&s, Some("MarkdownTableBorder"))
        };
        self.out.emit(border("┌", "┬", "┐"), first, lines.start);
        let row_out = |this: &mut Self, row: &[Vec<Atom>], src: usize| {
            let wrapped: Vec<Vec<Line>> = row.iter().enumerate().map(|(i, c)| this.wrap(c, widths[i])).collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for k in 0..height {
                let mut line = Line::styled("│", Some("MarkdownTableBorder"));
                for (i, cell) in wrapped.iter().enumerate() {
                    let content = cell.get(k).cloned().unwrap_or_default();
                    let pad = widths[i].saturating_sub(content.cols);
                    let left = match aligns.get(i).copied().unwrap_or(Align::None) {
                        Align::Right => pad,
                        Align::Center => pad / 2,
                        Align::Left | Align::None => 0,
                    };
                    line.push(&" ".repeat(1 + left), None);
                    line.append(content);
                    line.push(&" ".repeat(pad - left + 1), None);
                    line.push("│", Some("MarkdownTableBorder"));
                }
                this.out.emit(line, rest, src);
            }
        };
        row_out(self, &head, lines.start);
        self.out.emit(border("├", "┼", "┤"), rest, lines.start);
        for (i, row) in body.iter().enumerate() {
            row_out(self, row, (lines.start + 2 + i).min(lines.end));
        }
        self.out.emit(border("└", "┴", "┘"), rest, lines.end);
    }

    /// `inlines` as atoms, with `groups` on all of them.
    fn atoms(&self, inlines: &[Inline], groups: &[&'static str]) -> Vec<Atom> {
        let mut out = Vec::new();
        self.flatten(inlines, &mut groups.to_vec(), None, &mut out);
        out
    }

    fn flatten(&self, inlines: &[Inline], groups: &mut Vec<&'static str>, link: Option<&str>, out: &mut Vec<Atom>) {
        let piece = |text: &str, groups: &[&'static str]| Piece {
            text: text.to_owned(),
            groups: groups.to_vec(),
            link: link.map(str::to_owned),
            image: None,
        };
        let nested =
            |g: &'static str, children: &[Inline], groups: &mut Vec<&'static str>, link, out: &mut Vec<Atom>| {
                groups.push(g);
                self.flatten(children, groups, link, out);
                groups.pop();
            };
        for i in inlines {
            match i {
                Inline::Text(t) => words(t, &piece("", groups), out),
                Inline::Code(c) => {
                    let mut g = groups.clone();
                    g.push("MarkdownCode");
                    words(c, &piece("", &g), out);
                }
                Inline::Emph(c) => nested("MarkdownItalic", c, groups, link, out),
                Inline::Strong(c) => nested("MarkdownBold", c, groups, link, out),
                Inline::Strike(c) => nested("MarkdownStrike", c, groups, link, out),
                Inline::Link { url, children } => {
                    groups.push("MarkdownLink");
                    self.flatten(children, groups, Some(url), out);
                    groups.pop();
                }
                Inline::Image { url, alt } => {
                    let text = if alt.is_empty() { "image".to_owned() } else { alt.clone() };
                    let mut p = piece(&format!("[{text}]"), groups);
                    p.groups.push("MarkdownImage");
                    p.image = Some((url.clone(), alt.clone()));
                    out.push(Atom::Word { pieces: vec![p], brk: true });
                }
                Inline::Math(tex) => {
                    let mut g = groups.clone();
                    g.push("MarkdownMath");
                    match (self.math)(tex).filter(|l| l.len() == 1) {
                        // One line: as text; taller math reads as its source.
                        Some(lines) => out.push(Atom::Word { pieces: vec![piece(&lines[0], &g)], brk: true }),
                        None => words(&format!("${tex}$"), &piece("", &g), out),
                    }
                }
                Inline::Html(h) => {
                    let tag = h.trim_start_matches('<').trim_start_matches('/').to_ascii_lowercase();
                    if tag.starts_with("br") {
                        out.push(Atom::Break);
                    }
                }
                Inline::FootnoteRef { label, number } => {
                    let mut g = groups.clone();
                    g.push("MarkdownFootnote");
                    let mut p = piece(&format!("[{number}]"), &g);
                    p.link = Some(format!("#fn-{label}"));
                    out.push(Atom::Word { pieces: vec![p], brk: false });
                }
                Inline::SoftBreak => out.push(Atom::Space(piece(" ", groups))),
                Inline::HardBreak => out.push(Atom::Break),
            }
        }
    }

    /// Atoms wrapped into lines of at most `width` columns (a word wider
    /// than that is cut).
    fn wrap(&self, atoms: &[Atom], width: usize) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut line = Line::default();
        let mut space: Option<&Piece> = None;
        let put = |line: &mut Line, p: &Piece, text: &str| {
            line.push_with(text, &p.groups, p.link.as_deref());
            if let Some((url, alt)) = &p.image {
                let end = line.chars;
                let start = end - text.chars().count();
                line.decos.push(Deco::Image { start, end, url: url.clone(), alt: alt.clone(), block: false });
            }
        };
        // Words joined without a space (styled parts of one word) stay
        // together: wrap whole groups of them.
        let mut i = 0;
        while i < atoms.len() {
            match &atoms[i] {
                Atom::Break => {
                    lines.push(std::mem::take(&mut line));
                    space = None;
                    i += 1;
                }
                Atom::Space(p) => {
                    if line.cols > 0 {
                        space = Some(p);
                    }
                    i += 1;
                }
                Atom::Word { .. } => {
                    let mut j = i;
                    let mut group: Vec<&Piece> = Vec::new();
                    while let Some(Atom::Word { pieces, brk }) = atoms.get(j) {
                        if j > i && *brk {
                            break;
                        }
                        group.extend(pieces);
                        j += 1;
                    }
                    let w: usize = group.iter().map(|p| width_of(&p.text)).sum();
                    let sw = space.map_or(0, |p| width_of(&p.text));
                    if line.cols > 0 && line.cols + sw + w > width {
                        lines.push(std::mem::take(&mut line));
                        space = None;
                    }
                    if let Some(p) = space.take() {
                        put(&mut line, p, &p.text);
                    }
                    for p in group {
                        if line.cols + width_of(&p.text) <= width {
                            put(&mut line, p, &p.text);
                            continue;
                        }
                        // Too wide for a line: cut by graphemes.
                        for g in p.text.graphemes(true) {
                            if line.cols > 0 && line.cols + width_of(g) > width {
                                lines.push(std::mem::take(&mut line));
                            }
                            put(&mut line, p, g);
                        }
                    }
                    i = j;
                }
            }
        }
        if line.cols > 0 || lines.is_empty() {
            lines.push(line);
        }
        lines
    }
}

fn width_of(s: &str) -> usize {
    width(s)
}

/// A prefix for an empty line: without trailing blanks.
fn trimmed(p: &Prefix) -> Prefix {
    let mut p = p.clone();
    while let Some((s, _)) = p.last() {
        if s.trim().is_empty() {
            p.pop();
        } else {
            break;
        }
    }
    p
}

/// `text` as words and spaces, each piece like `like`; wide characters
/// are words of their own a line may break between.
fn words(text: &str, like: &Piece, out: &mut Vec<Atom>) {
    let with = |t: &str| Piece { text: t.to_owned(), ..like.clone() };
    let mut word = String::new();
    let mut brk = true;
    let flush = |word: &mut String, brk: &mut bool, out: &mut Vec<Atom>| {
        if !word.is_empty() {
            out.push(Atom::Word { pieces: vec![with(word)], brk: *brk });
            word.clear();
        }
        *brk = false;
    };
    // A word right after another piece (no space between) stays with it.
    if !matches!(out.last(), Some(Atom::Space(_)) | Some(Atom::Break) | None) {
        brk = false;
    }
    for g in text.graphemes(true) {
        if g.chars().all(char::is_whitespace) {
            flush(&mut word, &mut brk, out);
            if !matches!(out.last(), Some(Atom::Space(_))) {
                out.push(Atom::Space(with(" ")));
            }
            brk = true;
        } else if temzdf_texteditor::grapheme_width(g) == 2 {
            flush(&mut word, &mut brk, out);
            out.push(Atom::Word { pieces: vec![with(g)], brk: true });
            brk = true;
        } else {
            if word.is_empty()
                && matches!(out.last(), Some(Atom::Word { pieces, .. }) if pieces.last().is_some_and(|p| width_of(&p.text) == 2 && p.text.graphemes(true).count() == 1))
            {
                brk = true;
            }
            word.push_str(g);
        }
    }
    flush(&mut word, &mut brk, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_math(_: &str) -> Option<Vec<String>> {
        None
    }

    fn draw(src: &str, width: usize) -> Rendered {
        render(&Document::parse(src), width, &no_math)
    }

    fn text(src: &str, width: usize) -> Vec<String> {
        draw(src, width).text.split('\n').map(str::to_owned).collect()
    }

    /// The text under each style decoration of `group`.
    fn styled(r: &Rendered, group: &str) -> Vec<String> {
        let chars: Vec<char> = r.text.chars().collect();
        r.decos
            .iter()
            .filter_map(|d| match d {
                Deco::Style { start, end, group: g } if *g == group => Some(chars[*start..*end].iter().collect()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paragraphs_wrap_at_the_width_and_keep_styles() {
        assert_eq!(text("one two three four five", 12), ["one two", "three four", "five"]);
        let r = draw("a **bold** and *it*.\n", 40);
        assert_eq!(r.text, "a bold and it.");
        assert_eq!(styled(&r, "MarkdownBold"), ["bold"]);
        assert_eq!(styled(&r, "MarkdownItalic"), ["it"]);
        // A styled word and the punctuation after it stay together.
        assert_eq!(text("aaaa **bbbb**, cc", 9), ["aaaa", "bbbb, cc"]);
        // Wide characters break anywhere; a long word is cut.
        assert_eq!(text("日本語の文です", 8), ["日本語の", "文です"]);
        assert_eq!(text("abcdefghijkl", 8), ["abcdefgh", "ijkl"]);
    }

    #[test]
    fn headings_lists_quotes_and_code() {
        let src = "# Title\n\nText.\n\n- one\n- two that wraps here\n  - nested\n\n1. a\n2. b\n\n> quoted\n> more\n\n```\ncode  line\n```\n";
        assert_eq!(
            text(src, 16),
            [
                "Title",
                "═════",
                "",
                "Text.",
                "",
                "• one",
                "• two that wraps",
                "  here",
                "  ◦ nested",
                "",
                "1. a",
                "2. b",
                "",
                "│ quoted more",
                "",
                "  code  line",
            ]
        );
    }

    #[test]
    fn tasks_show_their_box_in_place_of_the_bullet() {
        assert_eq!(text("- [x] done\n- [ ] open\n\n1. [x] first\n", 40), ["☑ done", "☐ open", "", "1. ☑ first"]);
    }

    #[test]
    fn tables_fit_the_width_with_borders() {
        let src = "| a | long header |\n|:-|-:|\n| 1 | 2 |\n";
        assert_eq!(
            text(src, 40),
            [
                "┌───┬─────────────┐",
                "│ a │ long header │",
                "├───┼─────────────┤",
                "│ 1 │           2 │",
                "└───┴─────────────┘"
            ]
        );
        // Narrow: the widest column wraps in its cell.
        assert_eq!(
            text(src, 14),
            [
                "┌───┬────────┐",
                "│ a │   long │",
                "│   │ header │",
                "├───┼────────┤",
                "│ 1 │      2 │",
                "└───┴────────┘"
            ]
        );
    }

    #[test]
    fn links_images_footnotes_and_anchors() {
        let r = draw("# Top\n\nSee [the docs](https://x.org) and ![pic](a.png)[^n].\n\n[^n]: Note.\n", 60);
        let lines: Vec<&str> = r.text.split('\n').collect();
        assert_eq!(lines[3], "See the docs and [pic][1].");
        assert!(r.decos.contains(&Deco::Link { start: 13, end: 21, target: "https://x.org".into() }), "{:?}", r.decos);
        assert!(
            r.decos
                .iter()
                .any(|d| matches!(d, Deco::Image { url, alt, block: false, .. } if url == "a.png" && alt == "pic"))
        );
        let alone = draw("Text.\n\n![fig](f.svg)\n", 40);
        assert!(
            alone.decos.iter().any(|d| matches!(d, Deco::Image { url, block: true, .. } if url == "f.svg")),
            "a paragraph of its own"
        );
        assert!(r.decos.iter().any(|d| matches!(d, Deco::Link { target, .. } if target == "#fn-n")));
        assert_eq!(r.anchor_line("top"), Some(0));
        let fn_line = r.anchor_line("fn-n").unwrap();
        assert_eq!(lines[fn_line], "[1] Note.");
    }

    #[test]
    fn math_draws_through_the_given_function() {
        let math = |tex: &str| -> Option<Vec<String>> {
            Some(if tex.contains("frac") {
                vec!["a".into(), "─".into(), "b".into()]
            } else {
                vec![format!("<{tex}>")]
            })
        };
        let r = render(&Document::parse("Inline $x$ and $\\frac{a}{b}$.\n\n$$\n\\frac{a}{b}\n$$\n"), 40, &math);
        let lines: Vec<&str> = r.text.split('\n').collect();
        assert_eq!(lines, ["Inline <x> and $\\frac{a}{b}$.", "", "  a", "  ─", "  b"]);
    }

    #[test]
    fn every_line_knows_its_source_line() {
        let r = draw("# A\n\npara\n\n```\nx\ny\n```\n", 20);
        // Blank lines belong to the block before them.
        assert_eq!(r.sources, [0, 0, 0, 2, 2, 5, 6]);
        assert_eq!(r.line_of_source(2), 3);
        assert_eq!(r.line_of_source(5), 5);
        assert_eq!(r.line_of_source(3), 5, "the next line drawn");
    }
}
