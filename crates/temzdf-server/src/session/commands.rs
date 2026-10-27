//! The command registry: every command mappings, scripts (`temzdf.cmd.*`,
//! `temzdf.action.*`) and `temzdf action` can run, by name.

use std::time::Instant;

use temzdf_pdfview::keys::Command;
use temzdf_protocol::script::{CommandSpec, ScriptValue};

use temzdf_term::Spawn;

use super::floats::{self, FloatSpec, FloatWhat};
use super::{BufferId, Kind, Outcome, PaneEntry, Session, Spot};
use crate::action::{Action, PaneSpec};
use crate::keymap::notation;
use crate::layout::{Direction, PaneId, Split};

/// The session's own commands; the page-view commands follow (see
/// [`command_specs`]).
const SESSION: &[(&str, &str, &str)] = &[
    (
        "split",
        "(side?: \"right\" | \"left\" | \"bottom\" | \"top\", scope?: \"pane\" | \"screen\"): void",
        "Another pane like the focused one (the same document, or a new shell); `screen` puts it at that edge of the whole screen",
    ),
    ("new-pane", "(...words: string[]): void", "As `temzdf action new-pane`: a file, or \"--\" and a command"),
    ("move-focus", "(dir: \"left\" | \"right\" | \"up\" | \"down\"): void", "Focus the pane in that direction"),
    ("focus-next-pane", "(): void", "Focus the next pane"),
    (
        "resize-pane",
        "(how: \"left\" | \"right\" | \"up\" | \"down\" | \"wider\" | \"narrower\" | \"taller\" | \"shorter\" | { width?: number; height?: number }, cells?: number): void",
        "Move the focused pane's right (bottom) border, else its left (top) one, `cells` cells that way (1 times a mapping's count); or make the pane wider, narrower, taller, shorter by `cells` (Vim's <C-w>> < + -); or give it a size",
    ),
    (
        "equalize-panes",
        "(only?: \"width\" | \"height\"): void",
        "Make the tiled panes the same size (only their widths, or heights), as Vim's <C-w>=",
    ),
    (
        "maximize-pane",
        "(axis: \"width\" | \"height\"): void",
        "Make the focused pane as wide (high) as the others' minimum sizes allow, as Vim's <C-w>| and <C-w>_; a mapping's count N makes it N cells instead",
    ),
    (
        "move-pane",
        "(side: \"left\" | \"right\" | \"top\" | \"bottom\"): void",
        "Move the focused pane to that edge of the screen, the full height or width there (Vim's <C-w>H J K L)",
    ),
    (
        "rotate-panes",
        "(reverse?: boolean): void",
        "Rotate the panes in the focused pane's row or column, each to the next place (Vim's <C-w>r; reverse: <C-w>R); a mapping's count repeats it",
    ),
    ("close-pane", "(): void", "Close the focused pane (its buffer stays, hidden)"),
    (
        "open-float",
        "(what?: { buffer: number } | { file: string } | { command: string[] } | null, opts?: { width?: number | string; height?: number | string; col?: number | string; row?: number | string; title?: string; focus?: boolean; tabpage?: number }): number | null",
        "Open a floating pane (a new shell by default); sizes in cells (the content's) or \"N%\" of the screen, centred unless col/row; tabpage as Vim's popups (0 the current tab page, -1 all, N tab page N); returns the pane (null for a file still opening)",
    ),
    (
        "float-config",
        "(pane: number | null, opts: { width?: number | string; height?: number | string; col?: number | string; row?: number | string; title?: string; focus?: boolean; tabpage?: number }): void",
        "Move or resize a floating pane (the focused one for null), change its title, or the tab pages it shows on (tabpage)",
    ),
    ("toggle-floats", "(): void", "Show or hide every floating pane; with none, open a floating shell"),
    ("toggle-floating", "(pane?: number): void", "Turn a pane (the focused one) floating, or tiled again"),
    (
        "buffers",
        "(): { id: number; kind: \"pdf\" | \"terminal\"; name: string; panes: number[]; exited: boolean }[]",
        "The buffers (documents and terminals), shown or hidden",
    ),
    (
        "show-buffer",
        "(buffer: number, pane?: number): void",
        "Show a buffer in a pane (the focused one), as Vim's :buffer",
    ),
    ("edit", "(path: string): void", "Open a PDF in the focused pane, as Vim's :edit"),
    (
        "delete-buffer",
        "(buffer?: number): void",
        "Delete a buffer (the focused pane's): end its program or close its document, and close its panes",
    ),
    ("kill-session", "(): void", "End the session"),
    ("detach", "(): void", "The operating client detaches"),
    ("reload-config", "(): void", "Restart the script host and load the configuration again"),
    ("message", "(text: string): void", "Show a message"),
    ("send-text", "(text: string): void", "Type text into the focused terminal pane (Vim's chansend)"),
    (
        "paste",
        "(from?: \"clipboard\" | \"primary\"): void",
        "Paste the client's clipboard (or the primary selection) into the focused pane; terminals paste with their own keys",
    ),
    (
        "font-size",
        "(size?: number | string): void",
        "GUI clients: the font size in points (guifont's :h), \"+N\"/\"-N\" points more or less (times a mapping's count), none: back to the configured size",
    ),
    (
        "feedkeys",
        "(keys: string, remap?: boolean): void",
        "Keys as if typed, after the running mapping; remap false skips mappings",
    ),
    ("switch-mode", "(mode: string): void", "Enter a mode (pushed on the mode history)"),
    ("exit-mode", "(): void", "Back to the previous mode"),
    ("current-mode", "(): string", "The current mode's name"),
    (
        "panes",
        "(): { id: number; kind: \"pdf\" | \"terminal\"; buffer: number; focused: boolean; floating: boolean; tab: number | null; rect: { col: number; row: number; width: number; height: number } }[]",
        "The panes of every tab (tab: its index from 1, null for a float on all tabs; rect: the content, in cells)",
    ),
    ("focused-pane", "(): number | null", "The focused pane's id"),
    (
        "new-tab",
        "(what?: { buffer: number } | { file: string } | { command: string[] } | null): number | null",
        "Open a tab page after the current one, its pane showing `what` (a new shell by default), as Vim's :tabnew; returns the pane (null for a file still opening)",
    ),
    (
        "close-tab",
        "(tab?: number): void",
        "Close a tab page (the current one; counted from 1) and its panes; their buffers stay, hidden. The last tab page stays (kill-session ends the session)",
    ),
    (
        "next-tab",
        "(): void",
        "Go to the next tab page, after the last to the first; with a mapping's count N, to tab page N (Vim's gt)",
    ),
    (
        "prev-tab",
        "(): void",
        "Go to the previous tab page, before the first to the last; a mapping's count goes that many back (Vim's gT)",
    ),
    ("goto-tab", "(tab: number): void", "Go to tab page `tab` (counted from 1)"),
    (
        "move-tab",
        "(to?: number | string): void",
        "Move the current tab page after tab page `to` (0: first; none: last), or \"+N\"/\"-N\" places, as Vim's :tabmove",
    ),
    (
        "rename-tab",
        "(name?: string): void",
        "Name the current tab page in the tab line (no name: the focused pane's buffer again)",
    ),
    ("break-pane", "(pane?: number): void", "Move a pane (the focused one) to a new tab page, as Vim's <C-w>T"),
    (
        "tabs",
        "(): { index: number; name: string | null; label: string; current: boolean; panes: number[] }[]",
        "The tab pages in order (index from 1; panes: tiled, and floats on that tab only)",
    ),
    ("current-tab", "(): number", "The current tab page's index (from 1, as Vim's tabpagenr())"),
    ("session-name", "(): string", "The session's name"),
];

/// Every command, for the script host.
pub fn command_specs() -> Vec<CommandSpec> {
    let session = SESSION.iter().map(|(name, ts, doc)| CommandSpec {
        name: (*name).into(),
        doc: (*doc).into(),
        ts: (*ts).into(),
    });
    let pdf = Command::NAMES.iter().map(|(_, name)| CommandSpec {
        name: (*name).into(),
        doc: format!("Page view: {name} in the focused PDF pane (a mapping's count applies)"),
        ts: "(): void".into(),
    });
    session.chain(pdf).collect()
}

/// What a new pane shows (`openFloat`, `newTab`): `{ buffer }`, `{ file }`,
/// `{ command }`, nothing (a shell), or the words of `temzdf action
/// open-float FILE.pdf` or `-- htop`, as `new-pane`.
fn read_what(args: &[ScriptValue]) -> Result<FloatWhat, String> {
    Ok(match args.first() {
        None | Some(ScriptValue::Null) => FloatWhat::Spawn(Spawn::default()),
        Some(o @ ScriptValue::Map(_)) => {
            if let Some(b) = o.get("buffer").and_then(ScriptValue::as_num) {
                FloatWhat::Buffer(b as BufferId)
            } else if let Some(f) = o.get("file").and_then(ScriptValue::as_str) {
                FloatWhat::File(f.into())
            } else if let Some(ScriptValue::List(argv)) = o.get("command") {
                let argv = argv.iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
                FloatWhat::Spawn(Spawn { argv: Some(argv), ..Spawn::default() })
            } else {
                return Err("what is { buffer }, { file } or { command }".into());
            }
        }
        Some(_) => {
            let mut words = vec!["new-pane".to_owned()];
            words.extend(args.iter().filter_map(|a| a.as_str().map(str::to_owned)));
            match Action::parse(&words).map_err(|e| format!("{e:#}"))? {
                Action::NewPane { spec: PaneSpec::Pdf(p), cwd } => {
                    FloatWhat::File(cwd.map_or_else(|| p.clone(), |d| d.join(&p)))
                }
                Action::NewPane { spec: PaneSpec::Command(argv), cwd } => {
                    FloatWhat::Spawn(Spawn { argv: Some(argv), cwd, ..Spawn::default() })
                }
                Action::NewPane { cwd, .. } => FloatWhat::Spawn(Spawn { cwd, ..Spawn::default() }),
                _ => return Err("what to open?".into()),
            }
        }
    })
}

/// The font sizes `font-size` keeps to, in points.
const MIN_FONT_POINTS: f64 = 4.0;
const MAX_FONT_POINTS: f64 = 200.0;

/// The size in a `guifont` value ("Family:h14"; 14 when it names none, as
/// the GUI client takes it).
fn font_points(font: &str) -> f64 {
    font.split(':').skip(1).find_map(|p| p.strip_prefix('h')?.parse().ok()).unwrap_or(14.0)
}

/// `font` with its size set to `points` (added when it names none).
fn with_font_points(font: &str, points: f64) -> String {
    let size = format!("h{}", (points * 100.0).round() / 100.0);
    let mut parts: Vec<String> = font.split(':').map(str::to_owned).collect();
    match parts.iter_mut().skip(1).find(|p| p.strip_prefix('h').is_some_and(|n| n.parse::<f64>().is_ok())) {
        Some(p) => *p = size,
        None => parts.push(size),
    }
    parts.join(":")
}

/// `cells` times a mapping's `count`, kept within what any screen has
/// (counts saturate at usize::MAX; scripts pass any number).
fn times(cells: f64, count: Option<usize>) -> i32 {
    const MOST: f64 = 100_000.0;
    let cells = if cells.is_nan() { 0.0 } else { cells.clamp(-MOST, MOST) };
    (cells * count.unwrap_or(1).min(100_000) as f64).clamp(-MOST, MOST) as i32
}

impl Session {
    /// Run command `name`. `count` comes from a mapping (`3j`).
    pub(super) fn run_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
        count: Option<usize>,
        now: Instant,
    ) -> Result<(ScriptValue, Outcome), String> {
        let text = |i: usize| args.get(i).and_then(ScriptValue::as_str).map(str::to_owned);
        // Numbers come as strings from `temzdf action`.
        let id = |i: usize| match args.get(i) {
            Some(ScriptValue::Num(n)) => Some(*n as u32),
            Some(ScriptValue::Str(s)) => s.parse().ok(),
            _ => None,
        };
        let ok = |o: Outcome| Ok((ScriptValue::Null, o));
        let act = |s: &mut Self, a: Action| {
            s.action_inner(a, now).map(|o| (ScriptValue::Null, o)).map_err(|e| format!("{e:#}"))
        };
        if let Some(cmd) = Command::from_name(name) {
            let Some(pane) = self.panes.get_mut(&self.focus).and_then(PaneEntry::pdf) else {
                return Err(format!("{name}: the focused pane is not a PDF"));
            };
            return match pane.command(cmd, count, now) {
                temzdf_pdfview::pane::Action::Quit => ok(self.request_close(self.focus)),
                temzdf_pdfview::pane::Action::Redraw => ok(Outcome::Redraw),
                temzdf_pdfview::pane::Action::None => ok(Outcome::None),
            };
        }
        match name {
            "split" => {
                // split("right"), split({ side, scope }), split("right", "screen")
                let (side, scope) = match args.first() {
                    Some(ScriptValue::Map(_)) => {
                        let o = &args[0];
                        (o.get("side").and_then(ScriptValue::as_str), o.get("scope").and_then(ScriptValue::as_str))
                    }
                    _ => (args.first().and_then(ScriptValue::as_str), args.get(1).and_then(ScriptValue::as_str)),
                };
                let root = match scope {
                    None | Some("pane") => false,
                    Some("screen") => true,
                    Some(other) => return Err(format!("split: scope {other:?} is not pane or screen")),
                };
                let side = side.unwrap_or(if self.options.splitright { "right" } else { "left" });
                let side = match side {
                    "bottom" | "down" if !self.options.splitbelow => "top",
                    s => s,
                };
                let a = Action::split(side, root).map_err(|e| format!("{e:#}"))?;
                act(self, a)
            }
            "open-float" => {
                let what = read_what(args).map_err(|e| format!("openFloat: {e}"))?;
                let mut spec = FloatSpec::default();
                if let Some(o) = args.get(1).filter(|o| matches!(o, ScriptValue::Map(_))) {
                    let mut title = None;
                    floats::read_opts(o, &mut spec, &mut title).map_err(|e| format!("openFloat: {e}"))?;
                    spec.title = title;
                    if let Some(t) = o.get("tabpage") {
                        spec.tab = Some(self.float_tab(t).map_err(|e| format!("openFloat: {e}"))?);
                    }
                    spec.focus = !matches!(o.get("focus"), Some(ScriptValue::Bool(false)));
                }
                let pane = self.open_float(what, spec).map_err(|e| format!("openFloat: {e}"))?;
                Ok((pane.map_or(ScriptValue::Null, |p| ScriptValue::Num(p as f64)), Outcome::Redraw))
            }
            "float-config" => {
                let pane = id(0).unwrap_or(self.focus);
                let o = args.get(1).ok_or("floatConfig: which options?")?;
                let Some(f) = self.float_mut(pane) else {
                    return Err(format!("floatConfig: pane {pane} does not float"));
                };
                let (mut spec, mut title) = (f.spec.clone(), f.title.clone());
                floats::read_opts(o, &mut spec, &mut title).map_err(|e| format!("floatConfig: {e}"))?;
                let tab = match o.get("tabpage") {
                    Some(t) => Some(self.float_tab(t).map_err(|e| format!("floatConfig: {e}"))?),
                    None => None,
                };
                if let Some(f) = self.float_mut(pane) {
                    (f.spec, f.title) = (spec, title);
                    if let Some(tab) = tab {
                        f.tab = tab;
                    }
                }
                // Moved off the current tab: the focus stays on this one.
                if tab.is_some() && self.focus == pane && !self.shown_floats().iter().any(|f| f.pane == pane) {
                    self.set_focus(self.tiled_focus);
                }
                self.relayout();
                ok(Outcome::Redraw)
            }
            "move-pane" => {
                let side = match text(0).as_deref() {
                    Some("left") => Direction::Left,
                    Some("right") => Direction::Right,
                    Some("top" | "up") => Direction::Up,
                    Some("bottom" | "down") => Direction::Down,
                    other => return Err(format!("movePane: {other:?} is not left, right, top or bottom")),
                };
                if self.is_float(self.focus) {
                    return Err("movePane: the pane floats".into());
                }
                let (pane, area) = (self.focus, self.area());
                let moved = self.layout.as_mut().is_some_and(|l| l.move_to_edge(pane, side, area));
                if moved {
                    self.dragging = None;
                    self.relayout();
                }
                ok(if moved { Outcome::Redraw } else { Outcome::None })
            }
            "rotate-panes" => {
                let reverse = matches!(args.first(), Some(ScriptValue::Bool(true)));
                let (pane, area) = (self.focus, self.area());
                let moved = self.layout.as_mut().is_some_and(|l| l.rotate(pane, reverse, count.unwrap_or(1), area));
                if moved {
                    self.dragging = None;
                    self.relayout();
                }
                ok(if moved { Outcome::Redraw } else { Outcome::None })
            }
            "toggle-floats" => {
                self.toggle_floats().map_err(|e| format!("toggleFloats: {e:#}"))?;
                ok(Outcome::Redraw)
            }
            "toggle-floating" => {
                let pane = id(0).unwrap_or(self.focus);
                self.toggle_floating(pane).map_err(|e| format!("toggleFloating: {e}"))?;
                ok(Outcome::Redraw)
            }
            "new-pane" => {
                let mut words = vec!["new-pane".to_owned()];
                words.extend(args.iter().filter_map(|a| a.as_str().map(str::to_owned)));
                let a = Action::parse(&words).map_err(|e| format!("{e:#}"))?;
                act(self, a)
            }
            "move-focus" => {
                let dir = match text(0).as_deref() {
                    Some("left") => Direction::Left,
                    Some("right") => Direction::Right,
                    Some("up") => Direction::Up,
                    Some("down") => Direction::Down,
                    other => return Err(format!("moveFocus: {other:?} is not left, right, up or down")),
                };
                act(self, Action::MoveFocus(dir))
            }
            "focus-next-pane" => act(self, Action::FocusNextPane),
            "resize-pane" => self.resize_pane(args, count),
            "equalize-panes" => {
                let only = match text(0).as_deref() {
                    None => None,
                    Some("width") => Some(Split::Horizontal),
                    Some("height") => Some(Split::Vertical),
                    Some(other) => return Err(format!("equalizePanes: {other:?} is not width or height")),
                };
                let area = self.area();
                let changed = self.layout.as_mut().is_some_and(|l| l.equalize(only, area));
                if changed {
                    self.relayout();
                }
                ok(if changed { Outcome::Redraw } else { Outcome::None })
            }
            "maximize-pane" => {
                let split = match text(0).as_deref() {
                    Some("width") => Split::Horizontal,
                    Some("height") => Split::Vertical,
                    other => return Err(format!("maximizePane: {other:?} is not width or height")),
                };
                let pane = self.focus;
                if self.is_float(pane) {
                    return Err("maximizePane: the pane is floating (resizePane sizes it)".into());
                }
                let area = self.area();
                let Some(layout) = self.layout.as_mut() else { return ok(Outcome::None) };
                let changed = match count {
                    Some(n) => layout.set_pane_size(pane, split, n.min(u16::MAX as usize) as u32, area),
                    None => layout.maximize(pane, split, area),
                };
                if changed {
                    self.relayout();
                }
                ok(if changed { Outcome::Redraw } else { Outcome::None })
            }
            "close-pane" => ok(self.request_close(self.focus)),
            "buffers" => Ok((self.buffer_list(), Outcome::None)),
            "show-buffer" => {
                let buf = id(0).ok_or("showBuffer: which buffer?")?;
                let pane = id(1).unwrap_or(self.focus);
                ok(self.show_in(pane, buf).map_err(|e| format!("showBuffer: {e}"))?)
            }
            "edit" => {
                let path = text(0).ok_or("edit: which file?")?;
                self.open_at(std::path::Path::new(&path), Spot::Replace(self.focus))
                    .map_err(|e| format!("edit: {e:#}"))?;
                ok(Outcome::Redraw)
            }
            "delete-buffer" => {
                let buf = match id(0) {
                    Some(b) => b,
                    None => self.panes.get(&self.focus).map(PaneEntry::buffer).ok_or("deleteBuffer: which buffer?")?,
                };
                ok(self.delete_buffer(buf).map_err(|e| format!("deleteBuffer: {e}"))?)
            }
            "kill-session" => ok(Outcome::Quit),
            "detach" => ok(Outcome::Detach),
            "reload-config" => act(self, Action::ReloadConfig),
            "message" => {
                self.notify(super::notices::Kind::Info, &text(0).unwrap_or_default());
                ok(Outcome::Redraw)
            }
            "font-size" => {
                self.font_size(args.first(), count).map_err(|e| format!("fontSize: {e}"))?;
                ok(Outcome::None)
            }
            "paste" => {
                let primary = match text(0).as_deref() {
                    None | Some("clipboard") => false,
                    Some("primary") => true,
                    Some(other) => return Err(format!("paste: {other:?} is not clipboard or primary")),
                };
                self.effects.push(temzdf_scene::Effect::Paste { primary });
                ok(Outcome::None)
            }
            "send-text" => match self.focused_term() {
                Some(t) => ok(Outcome::None.or(t.send_text(&text(0).unwrap_or_default()))),
                None => Err("sendText: the focused pane is not a terminal".into()),
            },
            "feedkeys" => {
                let keys = notation::parse(&text(0).unwrap_or_default()).map_err(|e| format!("feedkeys: {e:#}"))?;
                let remap = !matches!(args.get(1), Some(ScriptValue::Bool(false)));
                self.matcher.feed_front(&keys, remap);
                ok(Outcome::None)
            }
            "switch-mode" => {
                let mode = text(0).ok_or("switchMode: which mode?")?;
                if !self.modes.exists(&mode) {
                    return Err(format!("switchMode: no mode {mode:?}"));
                }
                let old = self.history.current().to_owned();
                self.history.push(&mode);
                self.mode_changed(&old);
                ok(Outcome::Redraw)
            }
            "exit-mode" => {
                let old = self.history.current().to_owned();
                self.history.pop();
                self.mode_changed(&old);
                ok(Outcome::Redraw)
            }
            "current-mode" => Ok((ScriptValue::Str(self.history.current().to_owned()), Outcome::None)),
            "panes" => {
                let mut ids: Vec<_> = self.panes.keys().copied().collect();
                ids.sort();
                let list = ids
                    .into_iter()
                    .map(|id| {
                        let e = &self.panes[&id];
                        let kind = match e.kind {
                            Kind::Pdf { .. } => "pdf",
                            Kind::Term { .. } => "terminal",
                        };
                        let r = e.rect;
                        ScriptValue::map([
                            ("id", ScriptValue::Num(id as f64)),
                            ("kind", ScriptValue::Str(kind.into())),
                            ("buffer", ScriptValue::Num(e.buffer() as f64)),
                            ("focused", ScriptValue::Bool(id == self.focus)),
                            ("floating", ScriptValue::Bool(self.is_float(id))),
                            (
                                "tab",
                                match self.floats.iter().find(|f| f.pane == id).map(|f| f.tab) {
                                    Some(super::tabs::FloatTab::All) => ScriptValue::Null,
                                    _ => {
                                        self.tab_of(id).map_or(ScriptValue::Null, |i| ScriptValue::Num((i + 1) as f64))
                                    }
                                },
                            ),
                            (
                                "rect",
                                ScriptValue::map([
                                    ("col", ScriptValue::Num(r.x as f64)),
                                    ("row", ScriptValue::Num(r.y as f64)),
                                    ("width", ScriptValue::Num(r.w as f64)),
                                    ("height", ScriptValue::Num(r.h as f64)),
                                ]),
                            ),
                        ])
                    })
                    .collect();
                Ok((ScriptValue::List(list), Outcome::None))
            }
            "new-tab" => {
                let what = read_what(args).map_err(|e| format!("newTab: {e}"))?;
                let pane = self.open_what(what, Spot::NewTab).map_err(|e| format!("newTab: {e}"))?;
                Ok((pane.map_or(ScriptValue::Null, |p| ScriptValue::Num(p as f64)), Outcome::Redraw))
            }
            "close-tab" => {
                let i = match id(0) {
                    Some(n) => self.tab_number(n).map_err(|e| format!("closeTab: {e}"))?,
                    None => self.tab,
                };
                if self.tabs.len() <= 1 {
                    return Err("closeTab: the last tab page stays (killSession ends the session)".into());
                }
                ok(self.remove_tab(i))
            }
            "next-tab" => {
                let i = match count {
                    Some(n) => self.tab_number(n as u32).map_err(|e| format!("nextTab: {e}"))?,
                    None => (self.tab + 1) % self.tabs.len(),
                };
                self.enter_tab(i);
                ok(Outcome::Redraw)
            }
            "prev-tab" => {
                let len = self.tabs.len();
                let back = count.unwrap_or(1) % len;
                self.enter_tab((self.tab + len - back) % len);
                ok(Outcome::Redraw)
            }
            "goto-tab" => {
                let n = id(0).ok_or("gotoTab: which tab page?")?;
                let i = self.tab_number(n).map_err(|e| format!("gotoTab: {e}"))?;
                self.enter_tab(i);
                ok(Outcome::Redraw)
            }
            "move-tab" => {
                let to = match args.first() {
                    Some(ScriptValue::Num(n)) => Some(format!("{n}")),
                    Some(ScriptValue::Str(s)) => Some(s.clone()),
                    _ => None,
                };
                self.move_tab(to.as_deref()).map_err(|e| format!("moveTab: {e}"))?;
                ok(Outcome::Redraw)
            }
            "rename-tab" => {
                let tab = self.tab;
                self.tabs[tab].name = text(0).filter(|s| !s.is_empty());
                ok(Outcome::Redraw)
            }
            "break-pane" => {
                let pane = id(0).unwrap_or(self.focus);
                self.break_pane(pane).map_err(|e| format!("breakPane: {e}"))?;
                ok(Outcome::Redraw)
            }
            "tabs" => Ok((self.tab_list(), Outcome::None)),
            "current-tab" => Ok((ScriptValue::Num((self.tab + 1) as f64), Outcome::None)),
            "focused-pane" => Ok((
                if self.panes.contains_key(&self.focus) {
                    ScriptValue::Num(self.focus as f64)
                } else {
                    ScriptValue::Null
                },
                Outcome::None,
            )),
            "session-name" => Ok((ScriptValue::Str(self.name.clone()), Outcome::None)),
            other => Err(format!("no command {other:?}")),
        }
    }

    /// `font-size`: change the size in `guifont` (and in `guifontwide`
    /// when it names one), remembering the configured fonts for going back.
    fn font_size(&mut self, size: Option<&ScriptValue>, count: Option<usize>) -> Result<(), String> {
        let o = &self.options;
        let current = font_points(&o.guifont);
        let points = match size {
            None | Some(ScriptValue::Null) => {
                if let Some((font, wide)) = self.configured_fonts.take() {
                    self.options.guifont = font;
                    self.options.guifontwide = wide;
                }
                return Ok(());
            }
            Some(ScriptValue::Num(n)) => *n,
            Some(ScriptValue::Str(s)) => {
                let n: f64 = s.trim().parse().map_err(|_| format!("{s:?} is not a size or +N/-N"))?;
                if s.trim_start().starts_with(['+', '-']) {
                    current + n * count.unwrap_or(1).min(1000) as f64
                } else {
                    n
                }
            }
            Some(other) => return Err(format!("{other:?} is not a size")),
        };
        if !points.is_finite() {
            return Err("the size is not a number".into());
        }
        let points = points.clamp(MIN_FONT_POINTS, MAX_FONT_POINTS);
        self.configured_fonts.get_or_insert_with(|| (o.guifont.clone(), o.guifontwide.clone()));
        let o = &mut self.options;
        o.guifont = with_font_points(&o.guifont, points);
        if let Some(wide) = &mut o.guifontwide {
            *wide = with_font_points(wide, points);
        }
        Ok(())
    }

    fn resize_pane(&mut self, args: &[ScriptValue], count: Option<usize>) -> Result<(ScriptValue, Outcome), String> {
        let area = self.area();
        let pane = self.focus;
        if self.is_float(pane) {
            return self.resize_float_pane(pane, args, count);
        }
        let Some(layout) = self.layout.as_mut() else { return Ok((ScriptValue::Null, Outcome::None)) };
        let num = |v: Option<&ScriptValue>| match v {
            Some(ScriptValue::Num(n)) => Some(*n),
            Some(ScriptValue::Str(s)) => s.parse().ok(),
            _ => None,
        };
        let moved = match args.first() {
            Some(o @ ScriptValue::Map(_)) => {
                let mut moved = false;
                for (key, split) in [("width", Split::Horizontal), ("height", Split::Vertical)] {
                    if let Some(n) = num(o.get(key)) {
                        moved |= layout.set_pane_size(pane, split, n.max(0.0) as u32, area);
                    }
                }
                moved
            }
            Some(ScriptValue::Str(how)) if matches!(how.as_str(), "wider" | "narrower" | "taller" | "shorter") => {
                let cells = times(num(args.get(1)).unwrap_or(1.0), count);
                let (split, sign) = match how.as_str() {
                    "wider" => (Split::Horizontal, 1),
                    "narrower" => (Split::Horizontal, -1),
                    "taller" => (Split::Vertical, 1),
                    _ => (Split::Vertical, -1),
                };
                let Some((_, r)) = layout.rects(area).into_iter().find(|(p, _)| *p == pane) else {
                    return Ok((ScriptValue::Null, Outcome::None));
                };
                let now = if split == Split::Horizontal { r.w } else { r.h } as i32;
                layout.set_pane_size(pane, split, (now + sign * cells).max(0) as u32, area)
            }
            dir => {
                let cells = times(num(args.get(1)).unwrap_or(1.0), count);
                let (split, sign) = match dir.and_then(ScriptValue::as_str) {
                    Some("left") => (Split::Horizontal, -1),
                    Some("right") => (Split::Horizontal, 1),
                    Some("up") => (Split::Vertical, -1),
                    Some("down") => (Split::Vertical, 1),
                    other => return Err(format!("resizePane: {other:?} is not left, right, up, down or a size")),
                };
                layout.move_border(pane, split, sign * cells, area)
            }
        };
        if moved {
            self.relayout();
        }
        Ok((ScriptValue::Null, if moved { Outcome::Redraw } else { Outcome::None }))
    }

    /// `resizePane` on a float: its right or bottom edge moves, or it gets a size.
    fn resize_float_pane(
        &mut self,
        pane: PaneId,
        args: &[ScriptValue],
        count: Option<usize>,
    ) -> Result<(ScriptValue, Outcome), String> {
        let num = |v: Option<&ScriptValue>| match v {
            Some(ScriptValue::Num(n)) => Some(*n as i64),
            Some(ScriptValue::Str(s)) => s.parse().ok(),
            _ => None,
        };
        match args.first() {
            Some(o @ ScriptValue::Map(_)) => self.resize_float(pane, num(o.get("width")), num(o.get("height")), true),
            dir => {
                let cells = times(num(args.get(1)).unwrap_or(1) as f64, count) as i64;
                let (w, h) = match dir.and_then(ScriptValue::as_str) {
                    Some("left" | "narrower") => (Some(-cells), None),
                    Some("right" | "wider") => (Some(cells), None),
                    Some("up" | "shorter") => (None, Some(-cells)),
                    Some("down" | "taller") => (None, Some(cells)),
                    other => return Err(format!("resizePane: {other:?} is not left, right, up, down or a size")),
                };
                self.resize_float(pane, w, h, false);
            }
        }
        Ok((ScriptValue::Null, Outcome::Redraw))
    }

    /// `temzdf action NAME ARGS...`: a command by name, its arguments as
    /// strings.
    pub fn run_cli(&mut self, words: &[String]) -> anyhow::Result<Outcome> {
        let Some((name, rest)) = words.split_first() else { anyhow::bail!("which action?") };
        let args: Vec<ScriptValue> = rest.iter().map(|w| ScriptValue::Str(w.clone())).collect();
        self.map_depth = 0;
        let (_, out) = self.run_command(name, &args, None, Instant::now()).map_err(anyhow::Error::msg)?;
        Ok(out.merge(self.flush_events()))
    }
}

#[cfg(test)]
mod tests {
    use super::times;

    #[test]
    fn counts_and_cells_stay_in_range() {
        assert_eq!(times(3.0, Some(4)), 12);
        assert_eq!(times(-2.0, None), -2);
        // Counts saturate at usize::MAX; scripts pass anything.
        assert_eq!(times(1.0, Some(usize::MAX)), 100_000);
        assert_eq!(times(-1.0, Some(usize::MAX)), -100_000);
        assert_eq!(times(f64::INFINITY, None), 100_000);
        assert_eq!(times(f64::NAN, Some(5)), 0);
    }
}
