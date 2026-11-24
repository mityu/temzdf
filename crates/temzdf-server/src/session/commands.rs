//! The command registry: every command scripts (`temzdf.action.*`, mapped
//! functions among them) and `temzdf action` can run, by name.

use std::time::Instant;

use temzdf_pdfview::keys::Command;
use temzdf_protocol::script::ScriptValue;
use temzdf_scene::FontEntry;

use temzdf_term::Spawn;
use temzdf_term::visual::{Op as VisualOp, Outcome as VisualOutcome};

use super::floats::{self, FloatSpec, FloatWhat};
use super::{BufferId, Outcome, PaneEntry, Session, Spot};
use crate::action::{self, PaneSpec};
use crate::keymap::notation;
use crate::layout::{Direction, PaneId, Split};

/// How a name no action has fails (and only that): a test tells it from
/// an action's own errors.
pub(super) const NO_ACTION: &str = "no action";

/// The session's actions (`action.*` in scripts, `temzdf action`) by name,
/// as crates/temzdf-script/ts-types/temzdf-actions.d.ts declares them
/// (build.rs reads them from there).
pub fn action_names() -> &'static [&'static str] {
    include!(concat!(env!("OUT_DIR"), "/actions.rs"))
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
            let words: Vec<String> = args.iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
            let action::NewPane { spec, cwd } = action::parse_new_pane(&words).map_err(|e| format!("{e:#}"))?;
            match spec {
                PaneSpec::File(p) => FloatWhat::File(cwd.map_or_else(|| p.clone(), |d| d.join(&p))),
                PaneSpec::Command(argv) => FloatWhat::Spawn(Spawn { argv: Some(argv), cwd, ..Spawn::default() }),
                PaneSpec::Shell => FloatWhat::Spawn(Spawn { cwd, ..Spawn::default() }),
            }
        }
    })
}

/// The font sizes `font-size` and `o.guifont` keep to, in points (larger
/// glyphs would not fit GUI clients' glyph atlases).
pub(super) const MIN_FONT_POINTS: f64 = 4.0;
pub(super) const MAX_FONT_POINTS: f64 = 200.0;

/// Every size in `font` and `wide` changed by `change`: the first font's
/// always (it sets the size of fonts that name none), the others' where
/// they name one.
fn resize_fonts(font: &mut [FontEntry], wide: &mut [FontEntry], change: impl Fn(f64) -> f64) {
    let base = temzdf_scene::base_points(font);
    let resized = |p: f32| ((change(p as f64).clamp(MIN_FONT_POINTS, MAX_FONT_POINTS) * 100.0).round() / 100.0) as f32;
    if let Some(first) = font.first_mut() {
        first.size = Some(resized(first.size.unwrap_or(base)));
    }
    for f in font.iter_mut().skip(1).chain(wide.iter_mut()) {
        f.size = f.size.map(resized);
    }
}

/// `cells`, kept within what any screen has (scripts pass any number).
fn cells(cells: f64) -> i32 {
    const MOST: f64 = 100_000.0;
    if cells.is_nan() { 0 } else { cells.clamp(-MOST, MOST) as i32 }
}

/// A count given as a command's argument `v` (a number, or a string from
/// `temzdf action`): none for anything else, at least 1.
fn count_arg(v: Option<&ScriptValue>) -> Option<usize> {
    let n = match v {
        Some(ScriptValue::Num(n)) => *n,
        Some(ScriptValue::Str(s)) => s.parse().ok()?,
        _ => return None,
    };
    (n >= 1.0).then(|| n.min(usize::MAX as f64) as usize)
}

impl Session {
    /// Run command `name`.
    pub(super) fn run_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
        now: Instant,
    ) -> Result<(ScriptValue, Outcome), String> {
        // What it shows counts from here (a notice's 3 s, `message`).
        self.clock = self.clock.max(now);
        let text = |i: usize| args.get(i).and_then(ScriptValue::as_str).map(str::to_owned);
        // Numbers come as strings from `temzdf action`.
        let id = |i: usize| match args.get(i) {
            Some(ScriptValue::Num(n)) => Some(*n as u32),
            Some(ScriptValue::Str(s)) => s.parse().ok(),
            _ => None,
        };
        let ok = |o: Outcome| Ok((ScriptValue::Null, o));
        if let Some(result) = self.text_command(name, args) {
            return result;
        }
        if let Some(result) = self.register_command(name, args) {
            return result;
        }
        if let Some(result) = self.test_report_command(name, args) {
            return result;
        }
        if let Some(result) = self.markdown_command(name, args, now) {
            return result;
        }
        if let "search-for" | "search-preview" | "search-cancel" = name {
            let query = if name == "search-cancel" {
                String::new()
            } else {
                text(0).ok_or(format!("{name}: what to search for?"))?
            };
            let which = id(if name == "search-cancel" { 0 } else { 1 });
            let target = which.unwrap_or(self.focus);
            // A Markdown viewer's graphics mode searches its page.
            if self.md_search(target, name, &query).is_some() {
                return ok(Outcome::Redraw);
            }
            let Some(pane) = self.panes.get_mut(&target).and_then(PaneEntry::pdf) else {
                return Err(match which {
                    Some(p) => format!("{name}: pane {p} is not a PDF"),
                    None => format!("{name}: the focused pane is not a PDF"),
                });
            };
            let action = match name {
                "search-for" => pane.search_for(&query),
                "search-preview" => pane.search_preview(&query),
                _ => pane.search_cancel(),
            };
            return ok(if action == temzdf_pdfview::pane::Action::None { Outcome::None } else { Outcome::Redraw });
        }
        if let Some(cmd) = Command::from_name(name) {
            let Some(pane) = self.panes.get_mut(&self.focus).and_then(PaneEntry::pdf) else {
                return Err(format!("{name}: the focused pane is not a PDF"));
            };
            let action = pane.command(cmd, count_arg(args.first()), now);
            return ok(self.pdf_outcome(action));
        }
        if let "visual.search-for" | "visual.search-preview" | "visual.search-cancel" = name {
            let cancel = name == "visual.search-cancel";
            let query = if cancel { String::new() } else { text(0).ok_or(format!("{name}: what to search for?"))? };
            let which = id(if cancel { 0 } else { 1 });
            let backward = !cancel && matches!(args.get(2), Some(ScriptValue::Bool(true)));
            let target = which.unwrap_or(self.focus);
            let Some(t) = self
                .panes
                .get(&target)
                .and_then(PaneEntry::term)
                .and_then(|b| self.terms.get_mut(&b))
                .filter(|t| t.term.visual_kept())
            else {
                return Err(format!("{name}: no terminal in the visual mode there"));
            };
            match name {
                "visual.search-for" => {
                    if !t.term.visual_search(&query, backward, false) && !query.is_empty() {
                        self.not_found(&query);
                    }
                }
                "visual.search-preview" => {
                    t.term.visual_search(&query, backward, true);
                }
                _ => {
                    t.term.visual_search_cancel();
                }
            }
            return ok(Outcome::Redraw);
        }
        if let Some(op) = VisualOp::from_name(name) {
            let Some(t) = self.focused_term().filter(|t| t.visual_on()) else {
                return Err(format!("{name}: the focused pane is not in the visual mode"));
            };
            return match t.visual(op, count_arg(args.first())) {
                VisualOutcome::Leave => {
                    self.leave_mode();
                    ok(Outcome::Redraw)
                }
                VisualOutcome::Stay(true) => ok(Outcome::Redraw),
                VisualOutcome::Stay(false) => ok(Outcome::None),
            };
        }
        match name {
            "split" => {
                // split("right"), split({ side, scope, ... }), split("right", "screen")
                let none = ScriptValue::Null;
                let o = args.first().filter(|o| matches!(o, ScriptValue::Map(_))).unwrap_or(&none);
                let (side, scope) = match o {
                    ScriptValue::Map(_) => {
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
                let (split, before) = action::split_side(side).map_err(|e| format!("{e:#}"))?;
                let num = |k: &str| o.get(k).filter(|v| !matches!(v, ScriptValue::Null));
                let target = match num("pane") {
                    Some(p) => {
                        let p = p.as_num().ok_or("split: pane is a number")? as PaneId;
                        if !self.panes.contains_key(&p) {
                            return Err(format!("split: no pane {p}"));
                        }
                        Some(p)
                    }
                    None => Some(self.focus),
                };
                let target = if root { None } else { target };
                let buffer = match num("buffer") {
                    Some(b) => Some(b.as_num().ok_or("split: buffer is a number")? as BufferId),
                    None => None,
                };
                let area = self.area();
                let size = match (split, num("width"), num("height")) {
                    (_, Some(_), Some(_)) => return Err("split: width or height, not both".into()),
                    (Split::Horizontal, Some(w), None) => {
                        Some(floats::Extent::parse(w).ok_or("split: bad width")?.of(area.w))
                    }
                    (Split::Vertical, None, Some(h)) => {
                        Some(floats::Extent::parse(h).ok_or("split: bad height")?.of(area.h))
                    }
                    (_, None, None) => None,
                    (Split::Horizontal, None, Some(_)) => return Err("split: a split to the side takes a width".into()),
                    (Split::Vertical, Some(_), None) => return Err("split: a split up or down takes a height".into()),
                };
                let pane = self.split_pane(target, split, before, size, buffer).map_err(|e| format!("split: {e}"))?;
                Ok((pane.map_or(ScriptValue::Null, |p| ScriptValue::Num(p as f64)), Outcome::Redraw))
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
                    f.focusable = spec.focusable;
                    (f.spec, f.title) = (spec, title);
                    if let Some(tab) = tab {
                        f.tab = tab;
                    }
                }
                // Moved off the current tab, or no longer taking the focus:
                // the focus goes to the tiled pane.
                if self.focus == pane && !self.focusable_floats().iter().any(|f| f.pane == pane) {
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
                let moved = self
                    .layout
                    .as_mut()
                    .is_some_and(|l| l.rotate(pane, reverse, count_arg(args.get(1)).unwrap_or(1), area));
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
                let words: Vec<String> = args.iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
                let action::NewPane { spec, cwd } = action::parse_new_pane(&words).map_err(|e| format!("{e:#}"))?;
                self.new_pane(&spec, cwd.as_deref()).map_err(|e| format!("{e:#}"))?;
                ok(Outcome::Redraw)
            }
            "move-focus" => {
                let dir = match text(0).as_deref() {
                    Some("left") => Direction::Left,
                    Some("right") => Direction::Right,
                    Some("up") => Direction::Up,
                    Some("down") => Direction::Down,
                    other => return Err(format!("moveFocus: {other:?} is not left, right, up or down")),
                };
                // Within the floats or within the tiled panes.
                let next = if self.is_float(self.focus) {
                    self.float_neighbor(dir)
                } else {
                    self.layout.as_ref().and_then(|l| l.neighbor(self.focus, dir, self.area()))
                };
                if let Some(p) = next {
                    self.set_focus(p);
                }
                ok(Outcome::Redraw)
            }
            "focus-next-pane" => {
                let order = self.focus_order();
                if let Some(i) = order.iter().position(|p| *p == self.focus) {
                    self.set_focus(order[(i + 1) % order.len()]);
                }
                ok(Outcome::Redraw)
            }
            "focus-pane" => {
                let pane = id(0).ok_or("focusPane: which pane?")?;
                let tab = self.tab_of(pane).ok_or_else(|| format!("focusPane: no pane {pane}"))?;
                // Refused before going to its tab page (else a failed call
                // would have changed tabs).
                if let Some(f) = self.floats.iter().find(|f| f.pane == pane) {
                    if !f.focusable {
                        return Err(format!("focusPane: pane {pane} never takes the focus"));
                    }
                    if self.tab_floats_hidden(tab) {
                        return Err(format!("focusPane: pane {pane} is a hidden float"));
                    }
                }
                self.enter_tab(tab);
                self.set_focus(pane);
                ok(Outcome::Redraw)
            }
            "resize-pane" => self.resize_pane(args),
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
                let changed = match count_arg(args.get(1)) {
                    Some(n) => layout.set_pane_size(pane, split, n.min(u16::MAX as usize) as u32, area),
                    None => layout.maximize(pane, split, area),
                };
                if changed {
                    self.relayout();
                }
                ok(if changed { Outcome::Redraw } else { Outcome::None })
            }
            "close-pane" => {
                let pane = id(0).unwrap_or(self.focus);
                if !self.panes.contains_key(&pane) {
                    return Err(format!("closePane: no pane {pane}"));
                }
                ok(self.request_close(pane))
            }
            "buffers" => {
                let all = args.first().and_then(|o| o.get("all")) == Some(&ScriptValue::Bool(true));
                Ok((self.buffer_list(all), Outcome::None))
            }
            "show-buffer" => {
                let buf = id(0).ok_or("showBuffer: which buffer?")?;
                let pane = id(1).unwrap_or(self.focus);
                if !self.has_buffer(buf) {
                    return Err(format!("showBuffer: no buffer {buf}"));
                }
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
                    None => self.pane_buffer(self.focus).ok_or("deleteBuffer: which buffer?")?,
                };
                ok(self.request_delete_buffer(buf).map_err(|e| format!("deleteBuffer: {e}"))?)
            }
            "kill-session" => {
                let force = match args.first() {
                    None => false,
                    Some(ScriptValue::Str(s)) if s == "--force" => true,
                    Some(v) => v.get("force") == Some(&ScriptValue::Bool(true)),
                };
                ok(self.request_end(force))
            }
            "detach" => ok(Outcome::Detach),
            "reload-config" => self.reload_scripts().map(|o| (ScriptValue::Null, o)).map_err(|e| format!("{e:#}")),
            // Not in the registry (no type, no documentation): for tests.
            "debug-stop-script-host" => {
                self.script.stop_host();
                ok(Outcome::None)
            }
            "message" => {
                let error = args.get(1).and_then(|o| o.get("error")) == Some(&ScriptValue::Bool(true));
                let kind = if error { super::notices::Kind::Error } else { super::notices::Kind::Info };
                self.notify(kind, &text(0).unwrap_or_default());
                ok(Outcome::Redraw)
            }
            "font-size" => {
                self.font_size(args.first()).map_err(|e| format!("fontSize: {e}"))?;
                ok(Outcome::None)
            }
            "paste" => {
                let primary = match text(0).as_deref() {
                    None | Some("clipboard") => false,
                    Some("primary") => true,
                    Some(other) => return Err(format!("paste: {other:?} is not clipboard or primary")),
                };
                self.ask_paste(primary);
                ok(Outcome::None)
            }
            "send-text" => match self.focused_term() {
                Some(t) => ok(Outcome::None.or(t.send_text(&text(0).unwrap_or_default()))),
                None => Err("sendText: the focused pane is not a terminal".into()),
            },
            "feedkeys" => {
                let keys = notation::parse(&text(0).unwrap_or_default()).map_err(|e| format!("feedkeys: {e:#}"))?;
                let remap = !matches!(args.get(1), Some(ScriptValue::Bool(false)));
                if self.script.hooks_on("KeyTyped") {
                    for k in &keys {
                        self.key_typed(k.notation(), None, "feedkeys");
                    }
                }
                // One deeper than the mapping running (if any): Vim's maxmapdepth.
                self.matcher.feed_front(&keys, remap, self.map_depth + 1);
                ok(Outcome::None)
            }
            "beep" => {
                self.matcher.drop_fed();
                ok(Outcome::None)
            }
            // An invocation's target again (as InvokePre/InvokePost give it),
            // as part of what runs now.
            "invoke" => {
                let count = args.get(1).and_then(|o| o.get("count")).and_then(ScriptValue::as_num).map(|c| c as usize);
                let what = match args.first() {
                    Some(ScriptValue::Num(f)) => {
                        super::Invoked::Target { target: crate::keymap::Target::Func(*f as u32), count }
                    }
                    // A `temzdf action` request (as Invoked::Cli gives it):
                    // the command runs now, its error the caller's.
                    Some(ScriptValue::List(acts)) => {
                        let [act] = acts.as_slice() else {
                            return Err("invoke: a command target is one action, as InvokePre gives it".into());
                        };
                        let name =
                            act.get("name").and_then(ScriptValue::as_str).ok_or("invoke: an action needs a name")?;
                        let args = match act.get("args") {
                            Some(ScriptValue::List(l)) => l.clone(),
                            _ => Vec::new(),
                        };
                        return self.run_command(name, &args, now);
                    }
                    Some(v) if let Some(t) = v.get("textInput").and_then(ScriptValue::as_str) => {
                        super::Invoked::Text(t.to_owned())
                    }
                    _ => return Err("invoke: a target as InvokePre gives it".into()),
                };
                ok(self.run_invoked(what))
            }
            "switch-mode" => {
                let mode = text(0).ok_or("switchMode: which mode?")?;
                if !self.modes.exists(&mode) {
                    return Err(format!("switchMode: no mode {mode:?}"));
                }
                if mode == "visual" {
                    if self.history.current() == "visual" {
                        return ok(Outcome::None);
                    }
                    if self.panes.get(&self.focus).and_then(PaneEntry::term).is_none() {
                        return Err("switchMode: the visual mode is for terminal panes".into());
                    }
                }
                let old = self.history.current().to_owned();
                self.history.push(&mode);
                self.last_key = now;
                self.mode_changed(&old);
                ok(Outcome::Redraw)
            }
            "exit-mode" => {
                self.leave_mode();
                ok(Outcome::Redraw)
            }
            "current-mode" => Ok((ScriptValue::Str(self.history.current().to_owned()), Outcome::None)),
            "screen-size" => {
                let area = self.area();
                let size = [("width", area.w), ("height", area.h)].map(|(k, n)| (k, ScriptValue::Num(n.into())));
                Ok((ScriptValue::map(size), Outcome::None))
            }
            "panes" => {
                let mut ids: Vec<_> = self.panes.keys().copied().collect();
                ids.sort();
                let list = ids
                    .into_iter()
                    .map(|id| {
                        let e = &self.panes[&id];
                        let (buffer, kind) = self.kind_buffer(&e.kind);
                        let r = e.rect;
                        ScriptValue::map([
                            ("id", ScriptValue::Num(id as f64)),
                            ("kind", ScriptValue::Str(kind.into())),
                            ("buffer", ScriptValue::Num(buffer as f64)),
                            ("focused", ScriptValue::Bool(id == self.focus)),
                            ("floating", ScriptValue::Bool(self.is_float(id))),
                            ("focusable", ScriptValue::Bool(self.floats.iter().all(|f| f.pane != id || f.focusable))),
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
                ok(self.request_close_tab(i))
            }
            "next-tab" => {
                let i = match count_arg(args.first()) {
                    Some(n) => self.tab_number(n).map_err(|e| format!("nextTab: {e}"))?,
                    None => (self.tab + 1) % self.tabs.len(),
                };
                self.enter_tab(i);
                ok(Outcome::Redraw)
            }
            "prev-tab" => {
                let len = self.tabs.len();
                let back = count_arg(args.first()).unwrap_or(1) % len;
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
            "status-line" => {
                let show = match args.first() {
                    Some(ScriptValue::Bool(b)) => Some(*b),
                    // From `temzdf action`: words.
                    Some(ScriptValue::Str(s)) if s == "on" || s == "true" => Some(true),
                    Some(ScriptValue::Str(s)) if s == "off" || s == "false" => Some(false),
                    None | Some(ScriptValue::Null) => None,
                    Some(other) => return Err(format!("statusLine: show is a boolean, not {other:?}")),
                };
                let pane = id(1).unwrap_or(self.focus);
                self.set_status_line(pane, show).map_err(|e| format!("statusLine: {e}"))?;
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
            other => Err(format!("{NO_ACTION} {other:?}")),
        }
    }

    /// `font-size`: change the sizes in `guifont` and `guifontwide` (a size
    /// sets the first font's and scales the others'; +N adds to each),
    /// remembering the configured fonts for going back.
    fn font_size(&mut self, size: Option<&ScriptValue>) -> Result<(), String> {
        let o = &self.options;
        let current = temzdf_scene::base_points(&o.guifont) as f64;
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
                if s.trim_start().starts_with(['+', '-']) { current + n } else { n }
            }
            Some(other) => return Err(format!("{other:?} is not a size")),
        };
        if !points.is_finite() {
            return Err("the size is not a number".into());
        }
        let points = points.clamp(MIN_FONT_POINTS, MAX_FONT_POINTS);
        self.configured_fonts.get_or_insert_with(|| (o.guifont.clone(), o.guifontwide.clone()));
        let o = &mut self.options;
        let relative = matches!(size, Some(ScriptValue::Str(s)) if s.trim_start().starts_with(['+', '-']));
        if relative {
            let delta = points - current;
            resize_fonts(&mut o.guifont, &mut o.guifontwide, |p| p + delta);
        } else {
            resize_fonts(&mut o.guifont, &mut o.guifontwide, |p| p * points / current);
        }
        Ok(())
    }

    fn resize_pane(&mut self, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let area = self.area();
        let pane = self.focus;
        if self.is_float(pane) {
            return self.resize_float_pane(pane, args);
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
                let cells = cells(num(args.get(1)).unwrap_or(1.0));
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
                let cells = cells(num(args.get(1)).unwrap_or(1.0));
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
    fn resize_float_pane(&mut self, pane: PaneId, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let num = |v: Option<&ScriptValue>| match v {
            Some(ScriptValue::Num(n)) => Some(*n as i64),
            Some(ScriptValue::Str(s)) => s.parse().ok(),
            _ => None,
        };
        match args.first() {
            Some(o @ ScriptValue::Map(_)) => self.resize_float(pane, num(o.get("width")), num(o.get("height")), true),
            dir => {
                let cells = cells(num(args.get(1)).unwrap_or(1) as f64) as i64;
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
        self.invocation += 1;
        // Why a "before" hook cancelled what it asked for, if one did
        // (answered once the scripts it started are done: take_cli_answer).
        self.cli_cancel = Some(None);
        let Some((name, rest)) = words.split_first() else { anyhow::bail!("which action?") };
        let args: Vec<ScriptValue> = rest.iter().map(|w| ScriptValue::Str(w.clone())).collect();
        self.map_depth = 0;
        let out = self.invoke(super::Invoked::Cli { name: name.clone(), args }, "cli", String::new());
        let mut out = out.merge(self.flush_events());
        // Keys it fed (feedkeys) are matched now, not when the next key is
        // typed; with scripts running, when they are done.
        if !self.holds_input() {
            out = out.merge(self.drive(self.now()));
        }
        // Done already and it failed: say so now (else the answer waits).
        if !self.holds_input()
            && let Some(Some(_)) = &self.cli_cancel
        {
            anyhow::bail!(self.take_cli_answer().unwrap_err());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{cells, count_arg};
    use temzdf_protocol::script::ScriptValue;

    #[test]
    fn counts_and_cells_stay_in_range() {
        assert_eq!(cells(-2.0), -2);
        assert_eq!(cells(f64::INFINITY), 100_000);
        assert_eq!(cells(f64::NEG_INFINITY), -100_000);
        assert_eq!(cells(f64::NAN), 0);
        assert_eq!(count_arg(Some(&ScriptValue::Num(3.0))), Some(3));
        assert_eq!(count_arg(Some(&ScriptValue::Str("4".into()))), Some(4));
        assert_eq!(count_arg(Some(&ScriptValue::Num(0.0))), None);
        assert_eq!(count_arg(Some(&ScriptValue::Null)), None);
        assert_eq!(count_arg(None), None);
    }
}
