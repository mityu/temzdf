//! The command registry: every command mappings, scripts (`temzdf.cmd.*`,
//! `temzdf.action.*`) and `temzdf action` can run, by name.

use std::time::Instant;

use temzdf_pdfview::keys::Command;
use temzdf_protocol::script::{CommandSpec, ScriptValue};

use super::{Kind, Outcome, PaneEntry, Session, Spot};
use crate::action::Action;
use crate::keymap::notation;
use crate::layout::{Direction, Split};

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
        "(how: \"left\" | \"right\" | \"up\" | \"down\" | { width?: number; height?: number }, cells?: number): void",
        "Move the focused pane's right (bottom) border, else its left (top) one, `cells` cells that way (1 times a mapping's count); or give the pane a size",
    ),
    ("close-pane", "(): void", "Close the focused pane (its buffer stays, hidden)"),
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
        "feedkeys",
        "(keys: string, remap?: boolean): void",
        "Keys as if typed, after the running mapping; remap false skips mappings",
    ),
    ("switch-mode", "(mode: string): void", "Enter a mode (pushed on the mode history)"),
    ("exit-mode", "(): void", "Back to the previous mode"),
    ("current-mode", "(): string", "The current mode's name"),
    ("panes", "(): { id: number; kind: \"pdf\" | \"terminal\"; buffer: number; focused: boolean }[]", "The panes"),
    ("focused-pane", "(): number | null", "The focused pane's id"),
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
                        ScriptValue::map([
                            ("id", ScriptValue::Num(id as f64)),
                            ("kind", ScriptValue::Str(kind.into())),
                            ("buffer", ScriptValue::Num(e.buffer() as f64)),
                            ("focused", ScriptValue::Bool(id == self.focus)),
                        ])
                    })
                    .collect();
                Ok((ScriptValue::List(list), Outcome::None))
            }
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

    fn resize_pane(&mut self, args: &[ScriptValue], count: Option<usize>) -> Result<(ScriptValue, Outcome), String> {
        let area = self.area();
        let pane = self.focus;
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
            dir => {
                let cells = num(args.get(1)).unwrap_or(1.0) as i32 * count.unwrap_or(1) as i32;
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

    /// `temzdf action NAME ARGS...`: a command by name, its arguments as
    /// strings.
    pub fn run_cli(&mut self, words: &[String]) -> anyhow::Result<Outcome> {
        let Some((name, rest)) = words.split_first() else { anyhow::bail!("which action?") };
        let args: Vec<ScriptValue> = rest.iter().map(|w| ScriptValue::Str(w.clone())).collect();
        let (_, out) = self.run_command(name, &args, None, Instant::now()).map_err(anyhow::Error::msg)?;
        Ok(out.merge(self.flush_events()))
    }
}
