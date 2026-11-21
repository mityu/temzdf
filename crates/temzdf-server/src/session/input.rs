//! Input of the session: keys through the matcher and the modes, invocations,
//! typed text, pastes and the mouse.

use super::*;

/// What an invocation runs.
pub(super) enum Invoked {
    /// A mapping's right-hand side (or `invoke`'s target).
    Target { target: Target, count: Option<usize> },
    /// Text typed into a text or input pane (keys no mapping took, input
    /// methods, pastes).
    Text(String),
    /// A mode's fallback function, given the key.
    Fallback { func: crate::keymap::Cond, key: ScriptValue },
    /// A `temzdf action` request.
    Cli { name: String, args: Vec<ScriptValue> },
}

impl Invoked {
    /// What `InvokePre`/`InvokePost` call the target: a function's id, a
    /// `temzdf action` command as `[{ name, args }]` (what `invoke` takes
    /// back; tests' mappings of commands give theirs so too), or
    /// `{ textInput }`.
    fn value(&self) -> ScriptValue {
        let act = |name: &str, args: &[ScriptValue]| {
            ScriptValue::map([("name", ScriptValue::Str(name.into())), ("args", ScriptValue::List(args.to_vec()))])
        };
        match self {
            #[cfg(test)]
            Invoked::Target { target: Target::Actions(acts), .. } => {
                ScriptValue::List(acts.iter().map(|a| act(&a.name, &a.args)).collect())
            }
            Invoked::Target { target: Target::Func(f), .. } | Invoked::Fallback { func: f, .. } => {
                ScriptValue::Num(*f as f64)
            }
            Invoked::Text(t) => ScriptValue::map([("textInput", ScriptValue::Str(t.clone()))]),
            Invoked::Cli { name, args } => ScriptValue::List(vec![act(name, args)]),
        }
    }
}

impl Session {
    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        self.clock = self.clock.max(now);
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Outcome::None;
        }
        self.invocation += 1;
        // A cancelled operation a key asked for is told in a message.
        self.cli_cancel = None;
        // A key dismisses error messages (and is handled as usual).
        let dismissed = Outcome::None.or(self.dismiss_errors());
        if self.hold_key(k) {
            return dismissed;
        }
        // KeyTyped hooks hear it before it is matched: while they run, it
        // waits (first in line).
        if self.script.hooks_on("KeyTyped") {
            let typed = self.typed_text(k);
            self.key_typed(Stroke::of(k).notation(), typed, "typed");
            let heard = self.flush_events();
            if self.script.busy() {
                self.script.hold_announced(k.clone());
                return dismissed.merge(heard);
            }
        }
        dismissed.merge(self.key_announced(k, now))
    }

    /// A key whose KeyTyped hooks ran: matched and handled.
    pub(super) fn key_announced(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        self.last_key = now;
        let out = self.key_inner(k, now);
        out.merge(self.flush_events())
    }

    fn key_inner(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        // The script host should run but does not: the fixed escape keys.
        if self.settings.script.is_some() && !self.script.running() {
            return self.escape_key(k);
        }
        // <C-c> cancels pending keys and the count; otherwise it is an
        // ordinary (unmappable) key.
        let ctrl_c = script::is_interrupt(k);
        if ctrl_c && (!self.matcher.pending().is_empty() || self.matcher.count().is_some()) {
            self.matcher.cancel();
            return Outcome::None;
        }
        let mut out = Outcome::None;
        // A PDF pane's own keys (its search prompt, outline, link preview)
        // come before mappings, unless a key sequence is under way.
        let focus = self.focus;
        if self.matcher.pending().is_empty()
            && !self.matcher.has_input()
            && let Some(Kind::Pdf { pane, .. }) = self.panes.get_mut(&focus).map(|e| &mut e.kind)
        {
            let mut redraw = false;
            if let Some(action) = pane.key_first(k, now, &mut redraw) {
                return self.pdf_outcome(action);
            }
            out = out.or(redraw);
        }
        self.matcher.push(k);
        out.merge(self.drive(now))
    }

    /// Keys while the script host is down (no configured mappings work):
    /// `<C-b>d` detaches, `<C-b>q` ends the session, `<C-b>r` restarts the
    /// host; other keys go to a focused terminal pane as typed and do
    /// nothing in other panes.
    fn escape_key(&mut self, k: &KeyEvent) -> Outcome {
        let stroke = Stroke::of(k);
        let is = |s: &str| notation::parse(s).is_ok_and(|v| v[0] == stroke);
        if std::mem::take(&mut self.escape_prefix) {
            return if is("d") {
                Outcome::Detach
            } else if is("q") {
                Outcome::Quit
            } else if is("r") {
                match self.reload_scripts() {
                    Ok(o) => o,
                    Err(e) => {
                        self.message(&format!("{e:#}"));
                        Outcome::Redraw
                    }
                }
            } else {
                Outcome::None
            };
        }
        if is("<C-b>") {
            self.escape_prefix = true;
            return Outcome::None;
        }
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.key(k));
        }
        // Other panes' keys do nothing while the host is down (decided with
        // the user on 2026-11-22): their keys are all mapped by the scripts.
        Outcome::None
    }

    /// Let the matcher decide what queued keys do, until it waits (for more
    /// keys, or while a script function runs).
    pub(super) fn drive(&mut self, now: Instant) -> Outcome {
        let mut out = Outcome::None;
        while !self.script.busy() {
            let layers = self.layers();
            match self.matcher.step(&self.modes, &layers, self.options.timeoutlen, now) {
                Step::Wait(_) => {
                    // The keys and what they expanded to are used up: keys
                    // fed later (by a hook, a timer) start from the top.
                    if !self.matcher.has_input() && self.matcher.pending().is_empty() {
                        self.map_depth = 0;
                    }
                    break;
                }
                Step::NeedConditions(funcs) => {
                    let keys = self.matcher.pending().iter().map(|k| k.notation()).collect();
                    let ctx = self.key_context(None, keys);
                    self.eval(funcs, vec![ctx]);
                }
                Step::Leave => {
                    self.leave_mode();
                    out = out.merge(Outcome::Redraw);
                }
                Step::Run { target, count, keys } => {
                    if self.too_deep() {
                        out = out.merge(Outcome::Redraw);
                        break;
                    }
                    let keys = keys.iter().map(|k| k.notation()).collect();
                    out = out.merge(self.invoke(Invoked::Target { target, count }, "key", keys));
                }
                Step::Unmapped(stroke, ev) => {
                    let ev = ev.unwrap_or_else(|| stroke.event());
                    match self.fallback() {
                        Fallback::Drop => {}
                        // Typing into a text or input pane is an
                        // invocation; a key for a terminal's program is not.
                        Fallback::Passthrough => match self.typed_text(&ev) {
                            Some(t) if self.focused_takes_text() => {
                                out = out.merge(self.invoke(Invoked::Text(t), "key", stroke.notation()));
                            }
                            _ => out = out.merge(self.send_to_pane(&ev)),
                        },
                        Fallback::Func(func) => {
                            if self.too_deep() {
                                out = out.merge(Outcome::Redraw);
                                break;
                            }
                            let text = ev.text.clone().or_else(|| match ev.code {
                                KeyCode::Char(c) => Some(c.to_string()),
                                _ => None,
                            });
                            let key = ScriptValue::map([
                                ("keys", ScriptValue::Str(stroke.notation())),
                                ("text", text.map_or(ScriptValue::Null, ScriptValue::Str)),
                            ]);
                            out = out.merge(self.invoke(Invoked::Fallback { func, key }, "key", stroke.notation()));
                        }
                    }
                }
            }
        }
        out
    }

    /// Mappings (and fallback functions) that feed keys mapping to
    /// themselves (`a` to `a`, or to `aa`) would run forever, as in Vim:
    /// keys fed more than `MAX_MAP_DEPTH` mappings deep stop with an error
    /// (Vim's maxmapdepth, E223). Else what runs now has that depth, and the
    /// keys it feeds one more.
    fn too_deep(&mut self) -> bool {
        let depth = self.matcher.depth();
        if depth > MAX_MAP_DEPTH {
            self.matcher.cancel();
            self.map_depth = 0;
            self.message("recursive mapping (mappings fed keys more than 1000 deep)");
            return true;
        }
        self.map_depth = depth;
        false
    }

    /// Start an invocation (a mapping, typed text, a fallback function, a
    /// CLI request): `InvokePre` hooks first (they may cancel it), then it
    /// runs; `InvokePost` comes at the safe point after it and its hooks.
    pub(super) fn invoke(&mut self, what: Invoked, source: &str, keys: String) -> Outcome {
        self.invocation_keys.clone_from(&keys);
        self.inv_seq += 1;
        let count = match &what {
            Invoked::Target { count: Some(c), .. } => ScriptValue::Num(*c as f64),
            _ => ScriptValue::Null,
        };
        let info = ScriptValue::map([
            ("seq", ScriptValue::Num(self.inv_seq as f64)),
            ("target", what.value()),
            ("keys", ScriptValue::Str(keys)),
            ("count", count),
            ("mode", ScriptValue::Str(self.history.current().to_owned())),
            ("pane", ScriptValue::Num(self.focus as f64)),
            ("source", ScriptValue::Str(source.to_owned())),
        ]);
        self.current_invocation = Some(self.inv_seq);
        if self.script.hooks_on("InvokePost") {
            self.script.pending_post = Some(info.clone());
        }
        self.request_op(vec![("InvokePre".to_owned(), vec![info])], script::Deferred::Run(Box::new(what)))
    }

    /// Run what an invocation invokes (also `invoke`, again).
    pub(super) fn run_invoked(&mut self, what: Invoked) -> Outcome {
        match what {
            Invoked::Target { target, count } => self.run_target(target, count),
            Invoked::Text(t) => self.text_type(&t).unwrap_or(Outcome::None),
            Invoked::Fallback { func, key } => {
                let ctx = self.key_context(None, self.invocation_keys.clone());
                self.call(func, vec![ctx, key], false);
                Outcome::None
            }
            Invoked::Cli { name, args } => match self.run_command(&name, &args, self.now()) {
                Ok((_, o)) => o,
                Err(e) => {
                    match &mut self.cli_cancel {
                        Some(slot) => {
                            slot.get_or_insert(e);
                        }
                        None => self.message(&e),
                    }
                    Outcome::Redraw
                }
            },
        }
    }

    /// `KeyTyped({ keys, text, source })`: a key arrived (typed, or fed by
    /// `feedkeys`), before it is matched.
    pub(super) fn key_typed(&mut self, keys: String, text: Option<String>, source: &str) {
        let info = ScriptValue::map([
            ("keys", ScriptValue::Str(keys)),
            ("text", text.map_or(ScriptValue::Null, ScriptValue::Str)),
            ("source", ScriptValue::Str(source.into())),
        ]);
        self.event("KeyTyped", vec![info]);
    }

    /// The text a key types (no modifier but Shift), if any.
    fn typed_text(&self, k: &KeyEvent) -> Option<String> {
        let typed = k.text.clone().or_else(|| match k.code {
            KeyCode::Char(c) => Some(c.to_string()),
            _ => None,
        });
        let held = [Modifiers::ALT, Modifiers::CTRL, Modifiers::SUPER, Modifiers::HYPER, Modifiers::META];
        let plain = !held.iter().any(|m| k.modifiers.contains(*m));
        typed.filter(|t| plain && !t.chars().any(char::is_control))
    }

    /// What a PDF pane's action asks of the session (the focused pane's).
    pub(super) fn pdf_outcome(&mut self, action: pane::Action) -> Outcome {
        match action {
            pane::Action::Quit => self.request_close(self.focus),
            pane::Action::Redraw => Outcome::Redraw,
            pane::Action::None => Outcome::None,
        }
    }

    /// The focused pane takes typed text (a text or input pane).
    fn focused_takes_text(&self) -> bool {
        matches!(self.panes.get(&self.focus).map(|e| &e.kind), Some(Kind::Text { .. }))
    }

    /// An `InvokePost` argument with the cursor and selection after it (the
    /// focused text pane's; null elsewhere).
    pub(super) fn with_cursor(&self, info: ScriptValue) -> ScriptValue {
        let ScriptValue::Map(mut fields) = info else { return info };
        let (cursor, selection) = match self.panes.get(&self.focus).map(|e| &e.kind) {
            Some(Kind::Text { buf, view }) if let Some(t) = self.texts.get(buf) => {
                let text = t.buf.text();
                let c = view.view.cursor;
                let sel = c.selection().map_or(ScriptValue::Null, |(a, b, _)| {
                    ScriptValue::map([
                        ("start", text::position_value(text.position(a))),
                        ("end", text::position_value(text.position(b))),
                    ])
                });
                (text::position_value(text.position(c.pos)), sel)
            }
            _ => (ScriptValue::Null, ScriptValue::Null),
        };
        fields.push(("cursor".into(), cursor));
        fields.push(("selection".into(), selection));
        ScriptValue::Map(fields)
    }

    /// Now, as the latest input or event said (every one comes with the
    /// time; a headless render's is simulated).
    pub(super) fn now(&self) -> Instant {
        self.clock
    }

    /// What a mapping runs: a script function, or (tests' mappings only)
    /// commands right here.
    fn run_target(&mut self, target: Target, count: Option<usize>) -> Outcome {
        match target {
            #[cfg(test)]
            Target::Actions(acts) => {
                let mut out = Outcome::None;
                for a in acts {
                    match self.run_command(&a.name, &a.args, self.now()) {
                        Ok((_, o)) => out = out.merge(o),
                        Err(e) => {
                            self.message(&e);
                            out = out.merge(Outcome::Redraw);
                            break;
                        }
                    }
                }
                out
            }
            Target::Func(func) => {
                let ctx = self.key_context(Some(count), self.invocation_keys.clone());
                self.call(func, vec![ctx], false);
                Outcome::None
            }
        }
    }

    /// What mapped functions and conditions get: the focused pane, its
    /// kind, whether it floats, the mode, `keys` (conditions: those of the
    /// sequence before the key asked about; functions: those that ran
    /// them), and (mapped functions) the count.
    pub(super) fn key_context(&self, count: Option<Option<usize>>, keys: String) -> ScriptValue {
        let kind = match self.panes.get(&self.focus) {
            Some(e) => ScriptValue::Str(self.kind_buffer(&e.kind).1.into()),
            None => ScriptValue::Null,
        };
        let mut fields = vec![
            ("pane", ScriptValue::Num(self.focus as f64)),
            ("paneKind", kind),
            ("floating", ScriptValue::Bool(self.is_float(self.focus))),
            ("mode", ScriptValue::Str(self.history.current().to_owned())),
            ("keys", ScriptValue::Str(keys)),
        ];
        if let Some(count) = count {
            fields.push(("count", count.map_or(ScriptValue::Null, |c| ScriptValue::Num(c as f64))));
        }
        ScriptValue::map(fields)
    }

    /// Where keys are looked up now: the current mode; below a transient
    /// mode with `keepalive: "always"` the modes under it too, down to the
    /// first that is not transient.
    fn layers(&self) -> Layers {
        let mut modes = Vec::new();
        let mut keep = None;
        for (i, name) in self.history.modes().enumerate() {
            let def = self.modes.get(name);
            modes.push(name.to_owned());
            match def {
                Some(d) if d.transient => match d.keepalive {
                    Keepalive::Always => continue,
                    // Its own fallback function takes the keys it does not
                    // map, rather than leaving (reading one key).
                    _ if i == 0 && matches!(d.fallback, Some(Fallback::Func(_))) => break,
                    k if i == 0 => {
                        keep = Some(k);
                        break;
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        let count = self.mode_property(|d| d.count).unwrap_or(false);
        Layers { modes, count, keep }
    }

    /// A property of the current mode, or of the modes below it where it
    /// is not given.
    fn mode_property<P>(&self, get: impl Fn(&crate::keymap::ModeDef<Target>) -> Option<P>) -> Option<P> {
        self.history.modes().find_map(|m| self.modes.get(m).and_then(&get))
    }

    /// What unmapped keys do now.
    fn fallback(&self) -> Fallback {
        self.mode_property(|d| d.fallback).unwrap_or(Fallback::Drop)
    }

    /// The current mode, or the first below it that is not transient.
    fn base_mode(&self) -> &str {
        self.history.modes().find(|m| !self.modes.get(m).is_some_and(|d| d.transient)).unwrap_or("normal")
    }

    /// When the transient mode on top leaves for lack of keys.
    pub(super) fn submode_deadline(&self) -> Option<Instant> {
        let d = self.modes.get(self.history.current())?;
        Some(self.last_key + d.timeout.filter(|_| d.transient)?)
    }

    /// An unmapped key in a pass-through mode goes to the focused pane: a
    /// terminal's program, or an input pane, which takes the characters it
    /// types (with no modifier but Shift) and drops the rest.
    fn send_to_pane(&mut self, k: &KeyEvent) -> Outcome {
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.key(k));
        }
        match self.typed_text(k) {
            Some(t) if self.text_type(&t).is_ok() => Outcome::Redraw,
            _ => Outcome::None,
        }
    }

    /// A pane's base mode, the bottom of the history while it has the
    /// focus: its buffer's (text buffers made with one), else its kind's
    /// (`o.basemode`), else the kind's own mode; one not defined falls back
    /// to the kind's.
    pub(super) fn pane_mode(&self, id: PaneId) -> String {
        let Some(e) = self.panes.get(&id) else { return "normal".into() };
        let kind = e.mode_kind();
        let chosen = match &e.kind {
            Kind::Text { buf, .. } => self.texts.get(buf).and_then(|t| t.basemode.clone()),
            _ => None,
        };
        chosen
            .or_else(|| self.options.basemode.get(kind).cloned())
            .filter(|m| self.modes.get(m).is_some())
            .unwrap_or_else(|| kind.into())
    }

    /// Announce a mode change (if `old` differs from the current mode).
    ///
    /// The `visual` mode drives the focused terminal's visual mode: entering
    /// it begins one (or goes on with a suspended one); leaving it ends it,
    /// unless the focus went to another pane (the search's input line, a
    /// click) or `visual` is still deeper in the history: then it is
    /// suspended, to go on when the mode comes back.
    pub(super) fn mode_changed(&mut self, old: &str) {
        let new = self.history.current().to_owned();
        if new == "visual" && old != "visual" {
            let buf = self.panes.get(&self.focus).and_then(PaneEntry::term);
            if let Some(other) = self.visual.filter(|b| Some(*b) != buf)
                && let Some(t) = self.terms.get_mut(&other)
            {
                t.term.visual_end();
            }
            self.visual = None;
            if let Some(b) = buf
                && let Some(t) = self.terms.get_mut(&b)
                && t.term.visual_begin()
            {
                self.visual = Some(b);
            }
        } else if old == "visual"
            && new != "visual"
            && let Some(b) = self.visual
        {
            let here = self.panes.get(&self.focus).and_then(PaneEntry::term) == Some(b);
            let deeper = self.history.modes().any(|m| m == "visual");
            match self.terms.get_mut(&b) {
                Some(t) if here && !deeper => {
                    t.term.visual_end();
                    self.visual = None;
                }
                Some(t) => t.term.visual_suspend(),
                None => self.visual = None,
            }
        }
        if new != old {
            self.event("ModeChanged", vec![ScriptValue::Str(old.to_owned()), ScriptValue::Str(new)]);
        }
    }

    /// Text an input method committed: typed into the focused terminal in a
    /// pass-through mode (`terminal`), else each character as a key, so
    /// mappings stay reachable with an input method on (as Vim takes IME
    /// input in Normal mode).
    pub fn text(&mut self, text: &str, now: Instant) -> Outcome {
        self.clock = self.clock.max(now);
        self.invocation += 1;
        if self.hold_text(text) {
            return Outcome::None;
        }
        let out = if self.fallback() == Fallback::Passthrough
            && let Some(t) = self.focused_term()
        {
            Outcome::None.or(t.send_text(text))
        } else if self.fallback() == Fallback::Passthrough && self.focused_takes_text() {
            self.invoke(Invoked::Text(text.to_owned()), "key", String::new())
        } else {
            let mut out = Outcome::None;
            for c in text.chars() {
                let key = KeyEvent { text: Some(c.to_string()), ..KeyEvent::new(KeyCode::Char(c), Modifiers::NONE) };
                out = out.merge(self.key(&key, now));
            }
            out
        };
        out.merge(self.flush_events())
    }

    /// The client's window (or terminal) gained or lost the focus: the
    /// focused terminal pane tells its program, if it asked (Vim reloads
    /// files on focus).
    pub fn client_focus(&mut self, gained: bool) -> Outcome {
        if let Some(t) = self.focused_term() {
            t.focus(gained);
        }
        Outcome::None
    }

    pub fn paste(&mut self, text: &str) -> Outcome {
        self.invocation += 1;
        if self.hold_paste(text) {
            return Outcome::None;
        }
        let out = self.paste_inner(text);
        out.merge(self.flush_events())
    }

    fn paste_inner(&mut self, text: &str) -> Outcome {
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.paste(text));
        }
        // A text or input pane takes it (an invocation); elsewhere it goes
        // nowhere.
        if self.focused_takes_text() {
            return self.invoke(Invoked::Text(text.to_owned()), "key", String::new());
        }
        Outcome::None
    }

    /// The pane at a screen pixel.
    /// The shown floats come first, top down, with their frames.
    pub(super) fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        let cell = self.geometry.cell;
        let inside = |r: &Rect| {
            let (x0, y0) = (r.x as f32 * cell.width as f32, r.y as f32 * cell.height as f32);
            x >= x0 && y >= y0 && x < x0 + (r.w * cell.width) as f32 && y < y0 + (r.h * cell.height) as f32
        };
        if let Some(f) = self.shown_floats().iter().rev().find(|f| inside(&f.frame)) {
            return Some(f.pane);
        }
        let tiled = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        tiled.into_iter().find(|id| self.panes.get(id).is_some_and(|e| inside(&e.rect)))
    }

    pub fn mouse(&mut self, m: &MouseEvent, now: Instant) -> Outcome {
        self.clock = self.clock.max(now);
        if self.hold_mouse(m) {
            return Outcome::None;
        }
        let out = self.mouse_inner(m, now);
        out.merge(self.flush_events())
    }

    fn mouse_inner(&mut self, m: &MouseEvent, now: Instant) -> Outcome {
        let cell = self.geometry.cell;
        let (px, py) = match m.unit {
            CoordUnit::Pixel => (m.x as f32, m.y as f32),
            CoordUnit::Cell => ((m.x as f32 + 0.5) * cell.width as f32, (m.y as f32 + 0.5) * cell.height as f32),
        };
        let under = self.pane_at(px, py);
        let target = self.captured.or(under);
        let mut out = Outcome::None;
        // The pointer left a pane: let it forget hover state (link previews).
        if self.hovered != under {
            if let Some(old) = self.hovered.take()
                && let Some(p) = self.panes.get_mut(&old).and_then(PaneEntry::pdf)
            {
                let away = MouseEvent { kind: MouseEventKind::Move, x: -100_000, y: -100_000, ..*m };
                out = out.or(p.mouse(&away, now));
            }
            self.hovered = under;
        }
        let (col, row) = ((px / cell.width as f32).floor() as i32, (py / cell.height as f32).floor() as i32);
        if let Some(tabs) = self.tabline_mouse(m, col, row) {
            return out.merge(tabs);
        }
        // Floats' frames lie over the borders (unless one is being dragged).
        if self.dragging.is_none()
            && let Some(frame) = self.float_mouse(m, col, row)
        {
            return out.merge(frame);
        }
        if let Some(border) = self.border_mouse(m, under, px, py) {
            return out.merge(border);
        }
        // (A release ends the capture, and still belongs to it.)
        let captured = self.captured.is_some();
        match m.kind {
            MouseEventKind::Press(_) => {
                self.captured = under;
                if let Some(p) = under
                    && p != self.focus
                {
                    // A click on another pane only focuses it: the press, its
                    // drags and its release stay out of the pane (a program
                    // there would act on a click meant to switch panes).
                    self.set_focus(p);
                    self.focus_click = true;
                    return Outcome::Redraw;
                }
            }
            MouseEventKind::Release(_) => {
                self.captured = None;
                if std::mem::take(&mut self.focus_click) {
                    return out;
                }
            }
            MouseEventKind::Drag(_) if self.focus_click => return out,
            _ => {}
        }
        let Some(id) = target else { return out };
        // In the operating mode, the left button selects in terminals even
        // when their program tracks the mouse; in the visual mode the mouse
        // is the visual mode's.
        let select = self.base_mode() == "normal";
        let visual = self.history.current() == "visual";
        let Some(e) = self.panes.get_mut(&id) else { return out };
        let content = e.content();
        // A pane's status line is not its content: the pointer there goes to
        // the pane only to end what began in it (a drag, a selection).
        if !captured && row >= content.y + content.h as i32 {
            return out;
        }
        let (dx, dy) = match m.unit {
            CoordUnit::Pixel => ((content.x * cell.width as i32), (content.y * cell.height as i32)),
            CoordUnit::Cell => (content.x, content.y),
        };
        let local = MouseEvent { x: m.x - dx, y: m.y - dy, ..*m };
        match &mut e.kind {
            Kind::Markdown { pane, doc } => {
                let px = match local.unit {
                    CoordUnit::Pixel => local,
                    CoordUnit::Cell => MouseEvent {
                        x: local.x * cell.width as i32,
                        y: local.y * cell.height as i32,
                        unit: CoordUnit::Pixel,
                        ..local
                    },
                };
                let page = self.md_docs.get(doc).and_then(|d| d.page(pane.page_key()));
                let changed = page.is_some_and(|page| pane.mouse(page, &px, cell.height, now));
                let out = if changed { out.merge(Outcome::Redraw) } else { out };
                out.merge(self.md_requests(id))
            }
            Kind::Pdf { pane, .. } => out.or(pane.mouse(&local, now)),
            Kind::Term { buf, view } => match self.terms.get_mut(buf) {
                Some(t) if visual && t.term.visual_on() => out.or(view.visual_mouse(&mut t.term, &local)),
                Some(t) => out.or(view.mouse(&mut t.term, &local, select)),
                None => out,
            },
            Kind::Text { buf, view } => match self.texts.get_mut(buf) {
                Some(t) => {
                    // A click (press and release on one character) on a
                    // viewer's link follows it.
                    let click = t.markdown.is_some().then(|| text::click(t, view, &local, cell)).flatten();
                    let out = out.merge(text::mouse(t, view, &local, cell));
                    match click {
                        Some(at) => match self.md_follow_at(id, at) {
                            Ok(_) => out.merge(Outcome::Redraw),
                            Err(e) => {
                                self.message(&e);
                                out.merge(Outcome::Redraw)
                            }
                        },
                        None => out,
                    }
                }
                None => out,
            },
        }
    }

    /// Borders: the pointer shape over them, and dragging them with the left
    /// button. `Some` when the event was the border's.
    fn border_mouse(&mut self, m: &MouseEvent, under: Option<PaneId>, px: f32, py: f32) -> Option<Outcome> {
        let cell = self.geometry.cell;
        let (col, row) = ((px / cell.width as f32).floor() as i32, (py / cell.height as f32).floor() as i32);
        let area = self.area();
        let layout = self.layout.as_mut()?;
        if let Some(id) = &self.dragging {
            return Some(match m.kind {
                MouseEventKind::Release(_) => {
                    self.dragging = None;
                    self.over_border = layout.border_at(area, col, row).map(|(_, s)| s);
                    self.fit_terms();
                    Outcome::Redraw
                }
                MouseEventKind::Drag(_) if layout.drag_border(id, col, row, area) => {
                    self.relayout();
                    Outcome::Redraw
                }
                _ => Outcome::None,
            });
        }
        // Over a pane, or a button held for one: not a border's.
        let border = if under.is_none() && self.captured.is_none() { layout.border_at(area, col, row) } else { None };
        self.over_border = border.as_ref().map(|(_, s)| *s);
        let (id, _) = border?;
        Some(match m.kind {
            MouseEventKind::Press(MouseButton::Left) => {
                self.dragging = Some(id);
                Outcome::Redraw
            }
            _ => Outcome::None,
        })
    }
}
