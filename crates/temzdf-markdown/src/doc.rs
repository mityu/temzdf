//! A Markdown document as temzdf draws it: blocks and inlines read with
//! comrak (GitHub-flavoured, with alerts, footnotes and math), each block
//! with the source lines it came from, and the headings with the anchors
//! links name them by.

use comrak::nodes::{AlertType, AstNode, ListType, NodeValue, TableAlignment};
use comrak::{Arena, Options, parse_document};

/// Source lines, 0-based, both ends included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        anchor: String,
        lines: Lines,
    },
    Paragraph {
        inlines: Vec<Inline>,
        lines: Lines,
    },
    /// `start`: the first number of an ordered list.
    List {
        start: Option<usize>,
        tight: bool,
        items: Vec<Item>,
        lines: Lines,
    },
    Code {
        lang: String,
        text: String,
        lines: Lines,
    },
    Quote {
        blocks: Vec<Block>,
        lines: Lines,
    },
    Alert {
        kind: AlertKind,
        title: Option<String>,
        blocks: Vec<Block>,
        lines: Lines,
    },
    Table {
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        lines: Lines,
    },
    /// Display math (`$$ ... $$`, ```` ```math ````).
    Math {
        tex: String,
        lines: Lines,
    },
    Html {
        html: String,
        lines: Lines,
    },
    Rule {
        lines: Lines,
    },
    /// A footnote's text, numbered as it is first referred to.
    Footnote {
        label: String,
        number: u32,
        blocks: Vec<Block>,
        lines: Lines,
    },
}

impl Block {
    pub fn lines(&self) -> Lines {
        match self {
            Block::Heading { lines, .. }
            | Block::Paragraph { lines, .. }
            | Block::List { lines, .. }
            | Block::Code { lines, .. }
            | Block::Quote { lines, .. }
            | Block::Alert { lines, .. }
            | Block::Table { lines, .. }
            | Block::Math { lines, .. }
            | Block::Html { lines, .. }
            | Block::Rule { lines }
            | Block::Footnote { lines, .. } => *lines,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// A task: whether it is done.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn name(self) -> &'static str {
        match self {
            AlertKind::Note => "Note",
            AlertKind::Tip => "Tip",
            AlertKind::Important => "Important",
            AlertKind::Warning => "Warning",
            AlertKind::Caution => "Caution",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { url: String, children: Vec<Inline> },
    Image { url: String, alt: String },
    Math(String),
    Html(String),
    FootnoteRef { label: String, number: u32 },
    SoftBreak,
    HardBreak,
}

/// A heading: its level, text and anchor (`#anchor` links go to it), and
/// its source lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub anchor: String,
    pub lines: Lines,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub headings: Vec<Heading>,
}

/// comrak's options: GitHub-flavoured Markdown, alerts, footnotes, math.
pub fn options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.alerts = true;
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.front_matter_delimiter = Some("---".into());
    o
}

impl Document {
    pub fn parse(source: &str) -> Document {
        let arena = Arena::new();
        let root = parse_document(&arena, source, &options());
        let mut anchors = Anchors::default();
        let mut blocks = Vec::new();
        let mut footnotes = Vec::new();
        for node in root.children() {
            match block(node, &mut anchors) {
                Some(b @ Block::Footnote { .. }) => footnotes.push(b),
                Some(b) => blocks.push(b),
                None => {}
            }
        }
        // Footnotes at the end, as they are numbered.
        footnotes.sort_by_key(|b| match b {
            Block::Footnote { number, .. } => *number,
            _ => 0,
        });
        blocks.extend(footnotes);
        let mut headings = Vec::new();
        collect_headings(&blocks, &mut headings);
        Document { blocks, headings }
    }

    /// The heading `#anchor` names.
    pub fn heading(&self, anchor: &str) -> Option<&Heading> {
        self.headings.iter().find(|h| h.anchor == anchor)
    }
}

fn collect_headings(blocks: &[Block], out: &mut Vec<Heading>) {
    for b in blocks {
        match b {
            Block::Heading { level, inlines, anchor, lines } => {
                out.push(Heading { level: *level, text: plain(inlines), anchor: anchor.clone(), lines: *lines });
            }
            Block::Quote { blocks, .. } | Block::Alert { blocks, .. } => collect_headings(blocks, out),
            Block::List { items, .. } => items.iter().for_each(|i| collect_headings(&i.blocks, out)),
            _ => {}
        }
    }
}

fn lines_of(node: &AstNode<'_>) -> Lines {
    let p = node.data.borrow().sourcepos;
    Lines {
        start: p.start.line.saturating_sub(1),
        end: p.end.line.saturating_sub(1).max(p.start.line.saturating_sub(1)),
    }
}

fn children<'a>(node: &'a AstNode<'a>, anchors: &mut Anchors) -> Vec<Block> {
    node.children().filter_map(|n| block(n, anchors)).collect()
}

fn block<'a>(node: &'a AstNode<'a>, anchors: &mut Anchors) -> Option<Block> {
    let lines = lines_of(node);
    let value = node.data.borrow().value.clone();
    Some(match value {
        NodeValue::Heading(h) => {
            let inlines = inlines(node);
            let anchor = anchors.make(&plain(&inlines));
            Block::Heading { level: h.level, inlines, anchor, lines }
        }
        NodeValue::Paragraph => {
            let inlines = inlines(node);
            // A paragraph of display math only is the math block.
            if let [Inline::Math(tex)] = inlines.as_slice()
                && is_display_math(node)
            {
                return Some(Block::Math { tex: tex.trim().to_owned(), lines });
            }
            Block::Paragraph { inlines, lines }
        }
        NodeValue::List(l) => {
            let items = node
                .children()
                .map(|item| {
                    let task = match &item.data.borrow().value {
                        NodeValue::TaskItem(t) => Some(t.symbol.is_some_and(|c| c != ' ')),
                        _ => None,
                    };
                    Item { task, blocks: children(item, anchors) }
                })
                .collect();
            let start = (l.list_type == ListType::Ordered).then_some(l.start);
            Block::List { start, tight: l.tight, items, lines }
        }
        NodeValue::CodeBlock(c) => {
            let lang = c.info.split_whitespace().next().unwrap_or("").to_owned();
            let text = c.literal.strip_suffix('\n').unwrap_or(&c.literal).to_owned();
            if lang == "math" {
                return Some(Block::Math { tex: text, lines });
            }
            Block::Code { lang, text, lines }
        }
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
            Block::Quote { blocks: children(node, anchors), lines }
        }
        NodeValue::Alert(a) => {
            let kind = match a.alert_type {
                AlertType::Note => AlertKind::Note,
                AlertType::Tip => AlertKind::Tip,
                AlertType::Important => AlertKind::Important,
                AlertType::Warning => AlertKind::Warning,
                AlertType::Caution => AlertKind::Caution,
            };
            Block::Alert { kind, title: a.title.clone(), blocks: children(node, anchors), lines }
        }
        NodeValue::Table(t) => {
            let aligns = t
                .alignments
                .iter()
                .map(|a| match a {
                    TableAlignment::None => Align::None,
                    TableAlignment::Left => Align::Left,
                    TableAlignment::Center => Align::Center,
                    TableAlignment::Right => Align::Right,
                })
                .collect();
            let mut header = Vec::new();
            let mut rows = Vec::new();
            for row in node.children() {
                let cells: Vec<Vec<Inline>> = row.children().map(inlines).collect();
                if matches!(row.data.borrow().value, NodeValue::TableRow(true)) {
                    header = cells;
                } else {
                    rows.push(cells);
                }
            }
            Block::Table { aligns, header, rows, lines }
        }
        NodeValue::HtmlBlock(h) => Block::Html { html: h.literal.clone(), lines },
        NodeValue::ThematicBreak => Block::Rule { lines },
        NodeValue::FootnoteDefinition(f) => {
            // Numbered as referred to: the first reference's number.
            let number = footnote_number(node, &f.name).unwrap_or(0);
            Block::Footnote { label: f.name.clone(), number, blocks: children(node, anchors), lines }
        }
        // Front matter is not shown; description lists and the like read
        // as their paragraphs.
        NodeValue::FrontMatter(_) => return None,
        NodeValue::DescriptionList
        | NodeValue::DescriptionItem(_)
        | NodeValue::DescriptionTerm
        | NodeValue::DescriptionDetails => Block::Quote { blocks: children(node, anchors), lines },
        _ => return None,
    })
}

/// Whether the paragraph `node` holds `$$` math (its only child).
fn is_display_math<'a>(node: &'a AstNode<'a>) -> bool {
    node.first_child().is_some_and(|c| matches!(&c.data.borrow().value, NodeValue::Math(m) if m.display_math))
}

/// The number the references to footnote `name` show.
fn footnote_number<'a>(node: &'a AstNode<'a>, name: &str) -> Option<u32> {
    let root = node.ancestors().last()?;
    root.descendants().find_map(|n| match &n.data.borrow().value {
        NodeValue::FootnoteReference(r) if r.name == name => Some(r.ref_num),
        _ => None,
    })
}

fn inlines<'a>(node: &'a AstNode<'a>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for n in node.children() {
        let value = n.data.borrow().value.clone();
        let inline = match value {
            NodeValue::Text(t) => {
                // Neighbouring text (split by escapes and the like) as one.
                if let Some(Inline::Text(prev)) = out.last_mut() {
                    prev.push_str(&t);
                    continue;
                }
                Inline::Text(t.into_owned())
            }
            NodeValue::Code(c) => Inline::Code(c.literal.clone()),
            NodeValue::Emph | NodeValue::Underline => Inline::Emph(inlines(n)),
            NodeValue::Strong => Inline::Strong(inlines(n)),
            NodeValue::Strikethrough => Inline::Strike(inlines(n)),
            NodeValue::Link(l) => Inline::Link { url: l.url.clone(), children: inlines(n) },
            NodeValue::Image(l) => Inline::Image { url: l.url.clone(), alt: plain(&inlines(n)) },
            NodeValue::Math(m) => Inline::Math(m.literal.clone()),
            NodeValue::HtmlInline(h) => Inline::Html(h),
            NodeValue::FootnoteReference(r) => Inline::FootnoteRef { label: r.name.clone(), number: r.ref_num },
            NodeValue::SoftBreak => Inline::SoftBreak,
            NodeValue::LineBreak => Inline::HardBreak,
            // Anything else: its text.
            _ => {
                out.extend(inlines(n));
                continue;
            }
        };
        out.push(inline);
    }
    out
}

/// The text of `inlines` without formatting.
pub fn plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) | Inline::Math(t) => s.push_str(t),
            Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) | Inline::Link { children: c, .. } => {
                s.push_str(&plain(c))
            }
            Inline::Image { alt, .. } => s.push_str(alt),
            Inline::FootnoteRef { number, .. } => s.push_str(&number.to_string()),
            Inline::SoftBreak | Inline::HardBreak => s.push(' '),
            Inline::Html(_) => {}
        }
    }
    s
}

/// Heading anchors as GitHub makes them: lower case, spaces as `-`,
/// punctuation dropped, `-1`, `-2` after a repeat.
#[derive(Default)]
struct Anchors {
    seen: std::collections::HashMap<String, usize>,
}

impl Anchors {
    fn make(&mut self, text: &str) -> String {
        let base: String = text
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                '-' | '_' => Some(c),
                c if c.is_alphanumeric() => Some(c),
                _ => None,
            })
            .collect();
        let n = self.seen.entry(base.clone()).or_insert(0);
        let anchor = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        anchor
    }
}

/// Where line `line` of `old` went in `new` (a line that changed: where
/// its unchanged neighbour above went, plus the distance from it).
pub fn map_line(old: &str, new: &str, line: usize) -> usize {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let (mut o, mut n) = (0usize, 0usize);
    let mut last_equal: Option<(usize, usize)> = None;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if o == line {
                    return n;
                }
                last_equal = Some((o, n));
                o += 1;
                n += 1;
            }
            ChangeTag::Delete => {
                if o == line {
                    break;
                }
                o += 1;
            }
            ChangeTag::Insert => n += 1,
        }
        if o > line {
            break;
        }
    }
    match last_equal {
        Some((lo, ln)) => ln + (line - lo),
        None => line,
    }
    .min(new.lines().count().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.into())
    }

    #[test]
    fn blocks_keep_their_source_lines() {
        let d = Document::parse("# Title\n\nOne\ntwo\n\n```rust\nfn f() {}\n```\n");
        assert_eq!(
            d.blocks,
            [
                Block::Heading {
                    level: 1,
                    inlines: vec![text("Title")],
                    anchor: "title".into(),
                    lines: Lines { start: 0, end: 0 }
                },
                Block::Paragraph {
                    inlines: vec![text("One"), Inline::SoftBreak, text("two")],
                    lines: Lines { start: 2, end: 3 }
                },
                Block::Code { lang: "rust".into(), text: "fn f() {}".into(), lines: Lines { start: 5, end: 7 } },
            ]
        );
    }

    #[test]
    fn anchors_are_githubs() {
        let d = Document::parse("# Hello, World!\n## Hello World\n## 日本語 の見出し\n### a_b-c\n");
        let anchors: Vec<_> = d.headings.iter().map(|h| h.anchor.as_str()).collect();
        assert_eq!(anchors, ["hello-world", "hello-world-1", "日本語-の見出し", "a_b-c"]);
        assert_eq!(d.heading("hello-world-1").map(|h| h.lines.start), Some(1));
    }

    #[test]
    fn gfm_and_extensions_read_as_blocks() {
        let src = "- [x] done\n- [ ] open\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\n> [!WARNING]\n> Careful\n\n$$\nx^2\n$$\n\n\
                   See[^n] and https://example.com.\n\n[^n]: Note.\n";
        let d = Document::parse(src);
        let Block::List { items, start: None, .. } = &d.blocks[0] else { panic!("{:?}", d.blocks[0]) };
        assert_eq!(items.iter().map(|i| i.task).collect::<Vec<_>>(), [Some(true), Some(false)]);
        let Block::Table { aligns, header, rows, .. } = &d.blocks[1] else { panic!() };
        assert_eq!(aligns, &[Align::Left, Align::Right]);
        assert_eq!((header.len(), rows.len()), (2, 1));
        assert!(matches!(&d.blocks[2], Block::Alert { kind: AlertKind::Warning, .. }));
        assert_eq!(d.blocks[3], Block::Math { tex: "x^2".into(), lines: Lines { start: 10, end: 12 } });
        let Block::Paragraph { inlines, .. } = &d.blocks[4] else { panic!() };
        assert!(inlines.contains(&Inline::FootnoteRef { label: "n".into(), number: 1 }));
        assert!(inlines.iter().any(|i| matches!(i, Inline::Link { url, .. } if url == "https://example.com")));
        assert!(matches!(d.blocks.last(), Some(Block::Footnote { number: 1, .. })), "footnotes last");
    }

    #[test]
    fn lines_follow_a_changed_file() {
        let old = "a\nb\nc\nd\n";
        assert_eq!(map_line(old, "new\nnew\na\nb\nc\nd\n", 2), 4, "moved down");
        assert_eq!(map_line(old, "b\nc\nd\n", 3), 2, "moved up");
        assert_eq!(map_line(old, "a\nB\nc\nd\n", 1), 1, "changed: where it was");
        assert_eq!(map_line(old, "x\n", 3), 0, "within the new text");
    }

    #[test]
    fn front_matter_is_not_shown() {
        let d = Document::parse("---\ntitle: x\n---\n\nText\n");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].lines(), Lines { start: 4, end: 4 });
    }
}
