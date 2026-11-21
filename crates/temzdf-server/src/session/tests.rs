use super::*;
use crate::keymap::{Act, notation};
use std::time::Duration;

/// Mappings of commands (the data form, tests only) for the tests of
/// the matcher and the modes, in sessions without a script host: the
/// prefix, a few keys of the normal mode, the visual mode's stacking.
/// The shipped keys themselves are tested with the real script host
/// (session/host_tests.rs); commands alone with `run_command`.
fn default_mappings(m: &mut Modes<Target>) {
    let keys = |s: &str| notation::parse(s).expect("keys");
    let acts = |v: &[(&str, &[ScriptValue])]| Target::Actions(v.iter().map(|(n, a)| Act::new(n, a.to_vec())).collect());
    let s = |x: &str| ScriptValue::Str(x.into());
    let map = |m: &mut Modes<Target>, mode: &str, lhs: &str, target: Target| {
        m.map(mode, keys(lhs), crate::keymap::Mapping::new(target));
    };
    for (lhs, target) in [
        ("%", acts(&[("split", &[s("right")])])),
        ("\"", acts(&[("split", &[s("bottom")])])),
        ("j", acts(&[("move-focus", &[s("down")])])),
        ("k", acts(&[("move-focus", &[s("up")])])),
        ("=", acts(&[("equalize-panes", &[])])),
        ("<C-b>", acts(&[("exit-mode", &[]), ("feedkeys", &[s("<C-b>"), ScriptValue::Bool(false)])])),
        ("<Esc>", acts(&[("exit-mode", &[])])),
        ("[", acts(&[("exit-mode", &[]), ("switch-mode", &[s("visual")])])),
    ] {
        map(m, "normal", lhs, target);
    }
    for mode in ["terminal", "pdf", "visual"] {
        map(m, mode, "<C-b>", acts(&[("switch-mode", &[s("normal")])]));
    }
    map(m, "terminal", "<C-S-v>", acts(&[("paste", &[])]));
    map(m, "visual", "q", acts(&[("exit-mode", &[])]));
}
use std::sync::mpsc::{self, Receiver};
use temzdf_pdfcore::doc_cache::TileKey;
use temzdf_pdfcore::pdf;
use termpix_core::geom::CellSize;
use termpix_core::input::Modifiers;

const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

#[test]
fn font_size_changes_guifont_and_goes_back_to_the_configured_one() {
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    // Each list's sizes, `None` where a font names none.
    let sizes = |s: &Session| {
        let a = s.appearance();
        let of = |l: &[temzdf_scene::FontEntry]| l.iter().map(|f| f.size).collect::<Vec<_>>();
        (of(&a.font), of(&a.font_wide))
    };
    let run = |s: &mut Session, arg: Option<&str>| {
        let args: Vec<ScriptValue> = arg.map(|a| ScriptValue::Str(a.into())).into_iter().collect();
        s.run_command("font-size", &args, Instant::now()).map(|_| ())
    };
    let fonts = |list: &[(&str, Option<f64>)]| {
        ScriptValue::List(
            list.iter()
                .map(|(f, size)| {
                    let mut m = vec![("font".to_owned(), ScriptValue::Str((*f).into()))];
                    m.extend(size.map(|n| ("size".to_owned(), ScriptValue::Num(n))));
                    ScriptValue::Map(m)
                })
                .collect(),
        )
    };
    s.set_option("guifont", &fonts(&[("Mono", Some(11.0)), ("Cjk", Some(12.0)), ("Emoji", None)])).unwrap();
    s.set_option("guifontwide", &fonts(&[("Wide", None)])).unwrap();
    assert_eq!(s.get_option("guifont").unwrap(), fonts(&[("Mono", Some(11.0)), ("Cjk", Some(12.0)), ("Emoji", None)]));
    run(&mut s, Some("+1")).unwrap();
    assert_eq!(
        sizes(&s),
        (vec![Some(12.0), Some(13.0), None], vec![None]),
        "each named size; the rest follow the first"
    );
    run(&mut s, Some("-6")).unwrap();
    assert_eq!(sizes(&s).0, [Some(6.0), Some(7.0), None], "minus six");
    run(&mut s, Some("-100")).unwrap();
    assert_eq!(sizes(&s).0, [Some(4.0), Some(5.0), None], "kept readable, by what the first could take");
    s.run_command("font-size", &[ScriptValue::Num(8.0)], Instant::now()).unwrap();
    assert_eq!(sizes(&s).0, [Some(8.0), Some(10.0), None], "a size scales them");
    assert!(run(&mut s, Some("big")).is_err());
    run(&mut s, None).unwrap();
    assert_eq!(sizes(&s), (vec![Some(11.0), Some(12.0), None], vec![None]), "back to the configuration");
    s.run_command("font-size", &[ScriptValue::Num(22.0)], Instant::now()).unwrap();
    assert_eq!(sizes(&s).0, [Some(22.0), Some(24.0), None], "in proportion");
    // A first font without a size starts from 14; setting the option
    // again makes that the configured font.
    s.set_option("guifont", &fonts(&[("Plain", None)])).unwrap();
    run(&mut s, Some("+2")).unwrap();
    assert_eq!(sizes(&s).0, [Some(16.0)], "from the default of 14");
    s.set_option("guifont", &fonts(&[("Other", Some(8.0))])).unwrap();
    run(&mut s, None).unwrap();
    assert_eq!(sizes(&s).0, [Some(8.0)]);
    // What is not a font list.
    for bad in [ScriptValue::Str("Mono:h11".into()), fonts(&[]), ScriptValue::List(vec![ScriptValue::Num(1.0)])] {
        assert!(s.set_option("guifont", &bad).is_err(), "{bad:?}");
    }
    let sized = |v: ScriptValue| {
        ScriptValue::List(vec![ScriptValue::Map(vec![
            ("font".into(), ScriptValue::Str("X".into())),
            ("size".into(), v),
        ])])
    };
    assert!(s.set_option("guifont", &sized(ScriptValue::Num(-1.0))).is_err());
    assert!(s.set_option("guifont", &sized(ScriptValue::Str("12".into()))).is_err());
    assert!(s.set_option("guifontwide", &fonts(&[])).is_ok(), "no wide fonts");
}

fn session() -> (Session, Receiver<SessionEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
    default_mappings(&mut s.modes);
    s.open(&pdf::fixture("sample.pdf")).unwrap();
    while s.is_starting() {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        s.handle(ev, Instant::now());
    }
    (s, rx)
}

/// Run frames and the workers' results until idle; returns the last frame
/// and the keys of every tile rendered meanwhile.
fn settle(s: &mut Session, rx: &Receiver<SessionEvent>) -> (Frame, Vec<TileKey>) {
    let mut rendered = Vec::new();
    loop {
        s.take_effects();
        let frame = s.frame();
        if s.is_idle() {
            return (frame, rendered);
        }
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        if let SessionEvent::Doc(_, DocEvent::Rendered(r)) = &ev {
            rendered.push(r.key);
        }
        s.handle(ev, Instant::now());
    }
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn search_for_searches_the_focused_pdf() {
    let (mut s, rx) = session();
    // As `temzdf action search-for ...`: words.
    s.run_cli(&["search-for".into(), "Back to Section".into()]).unwrap();
    settle(&mut s, &rx);
    // The jump to the first hit (on page 2) scrolls: let it end.
    s.tick(Instant::now() + Duration::from_secs(5));
    let frame = s.frame();
    assert!(frame.title.contains("2 / 3"), "{}", frame.title);
    let e = s.run_cli(&["search-for".into()]).unwrap_err();
    assert!(e.to_string().contains("what to search for"), "{e}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn searches_go_to_the_pdf_pane_given() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let pdf = s.focus;
    // Typed into an input pane below it: the PDF pane is not focused.
    let prompt = ScriptValue::map([("basemode", ScriptValue::Str("input".into()))]);
    let (buf, _) = s.run_command("new-buffer", &[prompt], Instant::now()).unwrap();
    let o = ScriptValue::map([
        ("side", ScriptValue::Str("bottom".into())),
        ("buffer", buf),
        ("height", ScriptValue::Num(1.0)),
    ]);
    let (input, _) = s.run_command("split", &[o], Instant::now()).unwrap();
    assert_eq!(s.focus, input.as_num().unwrap() as PaneId);
    let search = |s: &mut Session, name: &str, args: &[&str]| cmd(s, name, args);
    search(&mut s, "search-preview", &["needle", &pdf.to_string()]).unwrap();
    settle(&mut s, &rx);
    assert!(s.panes.get_mut(&pdf).and_then(PaneEntry::pdf).unwrap().search.is_active());
    search(&mut s, "search-cancel", &[&pdf.to_string()]).unwrap();
    assert!(!s.panes.get_mut(&pdf).and_then(PaneEntry::pdf).unwrap().search.is_active());
    // The focused input pane is no PDF.
    let e = search(&mut s, "search-for", &["needle"]).unwrap_err();
    assert!(e.contains("not a PDF"), "{e}");
    let focus = s.focus.to_string();
    let e = search(&mut s, "search-cancel", &[&focus]).unwrap_err();
    assert!(e.contains("is not a PDF"), "{e}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn search_for_needs_a_pdf() {
    let (mut s, _rx) = terminal_session(&["sh", "-c", "sleep 5"]);
    let e = s.run_cli(&["search-for".into(), "x".into()]).unwrap_err();
    assert!(e.to_string().contains("not a PDF"), "{e}");
}

fn key(s: &mut Session, spec: &str) -> Outcome {
    let mut out = Outcome::None;
    for k in notation::parse(spec).unwrap() {
        out = out.merge(s.key(&k.event(), Instant::now()));
    }
    out
}

fn click(s: &mut Session, x: i32, y: i32) {
    for kind in [MouseEventKind::Press(MouseButton::Left), MouseEventKind::Release(MouseButton::Left)] {
        let m = MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
        s.mouse(&m, Instant::now());
    }
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn panes_split_focus_close_detach_and_quit() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let first = s.focus();
    assert_eq!(cmd(&mut s, "split", &["right"]), Ok(Outcome::Redraw));
    assert_eq!(s.pane_count(), 2);
    let second = s.focus();
    assert_ne!(first, second, "the new pane has the focus");
    cmd(&mut s, "move-focus", &["left"]).unwrap();
    assert_eq!(s.focus(), first);
    // A click on the right half focuses the pane there.
    click(&mut s, 700, 200);
    assert_eq!(s.focus(), second);
    assert_eq!(cmd(&mut s, "close-pane", &[]), Ok(Outcome::Redraw));
    assert_eq!((s.pane_count(), s.focus()), (1, first));
    assert_eq!(cmd(&mut s, "detach", &[]), Ok(Outcome::Detach));
    assert_eq!(cmd(&mut s, "kill-session", &[]), Ok(Outcome::Quit), "quitting the last pane ends the session");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_float_for_another_tab_page_does_not_take_the_focus_here() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    new_tab(&mut s, &["sleep", "30"]);
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    let f = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(2.0))]);
    assert_eq!(s.focus, one, "keys must not go to a pane nobody sees");
    assert!(s.here_panes().contains(&s.focus));
    assert!(s.shown_floats().is_empty());
    cmd(&mut s, "goto-tab", &["2"]).unwrap();
    assert!(s.shown_floats().iter().any(|x| x.pane == f), "it shows on its tab page");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_tab_opened_while_the_configuration_loads_leaves_no_tab_page_empty() {
    let (tx, _rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    s.shell = Some(vec!["/bin/sh".into()]);
    // config.ts: action.newTab({ command: [...] }) at the top level.
    new_tab(&mut s, &["sleep", "30"]);
    // Then the session's first pane, once the configuration loaded.
    s.script.first = Some((PaneSpec::Shell, None));
    let _ = s.open_first();
    assert_eq!(s.layouts().count(), s.tabs.len(), "every tab page has a pane");
    assert_eq!(s.tab, 1, "the configuration's tab page stays current");
    for n in 1..=s.tabs.len() {
        cmd(&mut s, "goto-tab", &[&n.to_string()]).unwrap();
        assert!(!s.is_starting(), "tab page {n} shows something");
        assert!(s.panes.contains_key(&s.focus), "tab page {n} has a focus");
    }
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn a_forward_search_sent_while_the_document_opens_is_kept() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
    s.open(&pdf::fixture("sample.pdf")).unwrap();
    // Before it has opened (line 44 is on the last page).
    let doc = *s.docs.keys().next().unwrap();
    let req = ForwardRequest { file: "sample.tex".into(), line: 44, column: 0 };
    s.handle(SessionEvent::Forward(doc, req), Instant::now());
    while s.is_starting() {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        s.handle(ev, Instant::now());
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while s.focused_pane().unwrap().viewer.current_page() != 2 {
        assert!(Instant::now() < deadline, "the search was lost");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
            s.handle(ev, Instant::now());
        }
        s.tick(Instant::now());
    }
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn transient_modes_stay_leave_and_time_out() {
    use crate::keymap::Mapping;
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let keys = |x: &str| notation::parse(x).unwrap();
    let act = |n: &str, a: &[&str]| {
        Mapping::new(Target::Actions(vec![Act::new(n, a.iter().map(|x| ScriptValue::Str((*x).into())).collect())]))
    };
    // doc/design/keymaps.md's example: + repeats in a submode of normal.
    let d = s.modes.define("resize");
    d.transient = true;
    d.timeout = Some(Duration::from_millis(1500));
    s.modes.map("resize", keys("+"), act("resize-pane", &["taller"]));
    s.modes.map("normal", keys("s"), act("switch-mode", &["resize"]));
    s.modes.define("win").transient = true;
    s.modes.define("win").keepalive = Keepalive::Always;
    s.modes.map("normal", keys("w"), act("switch-mode", &["win"]));
    assert_eq!(key(&mut s, "<C-b>"), Outcome::Redraw, "the normal mode");
    assert_eq!(s.current_mode(), "normal");
    key(&mut s, "\"");
    let (top, bottom) = {
        let p = s.layout.as_ref().unwrap().panes();
        (p[0], p[1])
    };
    assert_eq!(s.focus(), bottom);
    let height = |s: &Session| s.panes[&bottom].rect.h;
    let before = height(&s);
    key(&mut s, "<C-b>s++");
    assert_eq!((height(&s), s.current_mode()), (before + 2, "resize"), "its keys repeat");
    // keepalive "mapped": another key leaves, and normal handles it.
    key(&mut s, "k");
    assert_eq!(s.focus(), top, "k moved the focus up, as in normal");
    assert_eq!(s.current_mode(), "pdf", "the focus change left the modes");
    // A timeout leaves too.
    key(&mut s, "<C-b>s");
    assert_eq!(s.current_mode(), "resize");
    assert!(s.next_deadline().is_some_and(|d| d <= Instant::now() + Duration::from_millis(1500)));
    s.poll(Instant::now() + Duration::from_millis(1600));
    assert_eq!(s.current_mode(), "normal", "back to the mode below");
    // keepalive "always": the mode below takes what it does not map,
    // and it stays.
    key(&mut s, "w");
    key(&mut s, "=");
    assert_eq!(s.current_mode(), "win");
    // A focus change leaves it, as the normal mode below.
    key(&mut s, "j");
    assert_eq!((s.focus(), s.current_mode()), (bottom, "pdf"));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn equal_and_maximized_panes() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    for _ in 0..2 {
        cmd(&mut s, "split", &["right"]).unwrap();
    }
    let widths = |s: &Session| s.layout.as_ref().unwrap().panes().iter().map(|p| s.panes[p].rect.w).collect::<Vec<_>>();
    assert_eq!(widths(&s), [44, 22, 22], "each split halved the focused pane");
    assert_eq!(cmd(&mut s, "equalize-panes", &[]), Ok(Outcome::Redraw));
    assert_eq!(widths(&s), [29, 30, 29], "90 cells and two borders");
    assert_eq!(cmd(&mut s, "maximize-pane", &["width"]), Ok(Outcome::Redraw));
    assert_eq!(widths(&s), [10, 10, 68], "the focused, last pane takes all it can");
    cmd(&mut s, "maximize-pane", &["width", "30"]).unwrap();
    assert_eq!(widths(&s)[2], 30, "cells are the width");
    assert_eq!(cmd(&mut s, "maximize-pane", &["height"]), Ok(Outcome::None), "nothing stacked with it");
    assert!(s.run_command("maximize-pane", &[], Instant::now()).is_err());
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn panes_are_composed_inside_their_rectangles_with_unique_keys() {
    let (mut s, rx) = session();
    cmd(&mut s, "split", &["right"]).unwrap();
    let (frame, _) = settle(&mut s, &rx);
    let mut keys = HashSet::new();
    for l in &frame.layers {
        assert!(keys.insert(l.key), "duplicate key {}", l.key);
        let in_left = l.dst.right() <= 440;
        let in_right = l.dst.x >= 450;
        let screen = l.dst == Rect::new(0, 0, 900, 600);
        assert!(in_left || in_right || screen, "{:?} crosses the panes", l.dst);
    }
    // The border between them is text: a line down the gap's column,
    // lit beside the focused (new, right) pane.
    let rows = screen_text(&frame, GEOMETRY);
    assert!(rows.iter().take(29).all(|r| r.chars().nth(44) == Some('│')), "{rows:?}");
    let border = frame.text.iter().find(|g| g.col == 44 && g.row == 0).expect("the border's top cell");
    let accent = termpix_ui::Theme::dark().accent.0;
    assert_eq!(border.rows[0][0].fg, temzdf_scene::Color::Rgb([accent[0], accent[1], accent[2]]));
    // The focused pane's title is the window title.
    assert!(frame.title.starts_with("sample.pdf"), "{}", frame.title);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn panes_on_one_document_render_each_tile_once() {
    let (mut s, rx) = session();
    // Stacked panes have the same width, so the same zoom and tiles; both
    // ask for them in the first frame. (The deduplication itself is
    // tested deterministically in temzdf-pdfview's host.rs.)
    cmd(&mut s, "split", &["down"]).unwrap();
    let (_, rendered) = settle(&mut s, &rx);
    assert!(!rendered.is_empty());
    let unique: HashSet<_> = rendered.iter().collect();
    assert_eq!(unique.len(), rendered.len(), "no tile rendered twice: {rendered:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn closing_a_pane_hides_its_document_until_it_is_shown_again() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let first = s.panes[&s.focus].buffer();
    cmd(&mut s, "new-pane", &[pdf::fixture("sample-short.pdf").to_str().unwrap()]).unwrap();
    while s.pane_count() < 2 {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        s.handle(ev, Instant::now());
    }
    settle(&mut s, &rx);
    let pane = s.focus;
    let doc = s.panes[&pane].buffer();
    s.panes.get_mut(&pane).unwrap().pdf().unwrap().viewer.jump_to(0.0, 700.0);
    let before = s.panes.get_mut(&pane).unwrap().pdf().unwrap().viewer.view_state();
    cmd(&mut s, "close-pane", &[]).unwrap();
    assert_eq!((s.pane_count(), s.buffer_count()), (1, 2), "the document stays, hidden");
    {
        let host = s.docs[&doc].host.as_ref().unwrap();
        let d = host.doc.borrow();
        assert!((0..d.cache.page_count()).all(|p| d.cache.display_list(p).is_none()), "its display lists went");
    }
    // Shown again (in the other pane), where it was.
    assert_eq!(s.show_in(s.focus, doc), Ok(Outcome::Redraw));
    settle(&mut s, &rx);
    let focus = s.focus;
    let after = s.panes.get_mut(&focus).unwrap().pdf().unwrap().viewer.view_state();
    assert_eq!(after.top.page, before.top.page);
    assert!((after.top.y.unwrap() - before.top.y.unwrap()).abs() < 1.0, "{after:?} {before:?}");
    // Closing the last pane ends the session, even with a buffer hidden.
    assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
    assert_eq!(s.close(focus), Outcome::Quit);
    // Deleting the hidden buffer leaves the pane alone; deleting the last
    // pane's ends the session.
    let hidden = [ScriptValue::Num(first as f64)];
    assert_eq!(s.run_command("delete-buffer", &hidden, Instant::now()).map(|r| r.1), Ok(Outcome::Redraw));
    assert_eq!((s.buffer_count(), s.panes[&focus].buffer()), (1, doc));
    assert_eq!(s.run_command("delete-buffer", &[], Instant::now()).map(|r| r.1), Ok(Outcome::Quit));
}

#[test]
#[cfg_attr(miri, ignore = "Miri has no inotify")]
fn opening_a_missing_file_fails_the_session() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    s.open(Path::new("/nonexistent/x.pdf")).unwrap();
    let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(s.handle(ev, Instant::now()), Outcome::Quit);
    assert!(s.error().is_some());
}

/// Present `frame` through `comp`, appending the terminal bytes to `out`.
fn present(
    s: &mut Session,
    frame: &Frame,
    comp: &mut termpix::compositor::Compositor,
    presenter: &mut temzdf_tty::Presenter,
    out: &mut Vec<u8>,
) {
    presenter.sync(comp, s.images());
    comp.present(&presenter.layers(&frame.layers), out).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn resizing_split_panes_redraws_like_a_fresh_start() {
    use termpix::compositor::{Backend, Compositor};
    use termpix::graphics::Medium;
    use termpix::mock::MockTerminal;
    let big = Geometry { cols: 100, rows: 34, ..GEOMETRY };
    let (mut s, rx) = session();
    cmd(&mut s, "split", &["right"]).unwrap();
    cmd(&mut s, "split", &["down"]).unwrap();
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, GEOMETRY);
    let mut presenter = temzdf_tty::Presenter::new();
    let mut out = Vec::new();
    let (frame, _) = settle(&mut s, &rx);
    present(&mut s, &frame, &mut comp, &mut presenter, &mut out);
    // The window grows (same cells, more of them): replay everything sent
    // since the start into a terminal of the new size.
    s.resize(big);
    comp.set_geometry(big);
    let (frame, _) = settle(&mut s, &rx);
    present(&mut s, &frame, &mut comp, &mut presenter, &mut out);
    let mut term = MockTerminal::new(big, [0, 0, 0, 255]);
    term.feed(&out);
    let resized = term.render();

    // The same layout (panes keep their sizes when the screen changes),
    // drawn only at the new size.
    let (mut fresh, rx2) = session();
    cmd(&mut fresh, "split", &["right"]).unwrap();
    cmd(&mut fresh, "split", &["down"]).unwrap();
    fresh.resize(big);
    let mut comp2 = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, big);
    let (frame2, _) = settle(&mut fresh, &rx2);
    let mut out2 = Vec::new();
    present(&mut fresh, &frame2, &mut comp2, &mut temzdf_tty::Presenter::new(), &mut out2);
    let mut term2 = MockTerminal::new(big, [0, 0, 0, 255]);
    term2.feed(&out2);
    let expected = term2.render();
    if let Ok(dir) = std::env::var("TEMZDF_TEST_PNG") {
        resized.save_png(&Path::new(&dir).join("resized.png")).unwrap();
        expected.save_png(&Path::new(&dir).join("expected.png")).unwrap();
    }
    let differing = resized.data.chunks(4).zip(expected.data.chunks(4)).filter(|(a, b)| a != b).count();
    assert_eq!(differing, 0, "{differing} pixels differ");
}

fn terminal_session(argv: &[&str]) -> (Session, Receiver<SessionEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("term".into(), GEOMETRY, Hints::default(), settings, sink);
    // Splits start this instead of the user's $SHELL.
    s.shell = Some(vec!["/bin/sh".into()]);
    default_mappings(&mut s.modes);
    let spec = PaneSpec::Command(argv.iter().map(|a| a.to_string()).collect());
    s.new_pane(&spec, None).unwrap();
    (s, rx)
}

/// Handle events until the composed text satisfies `done`; returns the
/// last outcome and the screen's rows.
fn wait_text(s: &mut Session, rx: &Receiver<SessionEvent>, done: impl Fn(&[String]) -> bool) -> (Outcome, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut outcome = Outcome::None;
    loop {
        let rows = screen_text(&s.frame(), GEOMETRY);
        if done(&rows) || outcome == Outcome::Quit {
            return (outcome, rows);
        }
        assert!(Instant::now() <= deadline, "the screen never showed what was waited for: {rows:?}");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            outcome = s.handle(ev, Instant::now());
        }
    }
}

/// Columns `a..b` of a screen row (characters, not bytes).
fn cols(row: &str, a: usize, b: usize) -> String {
    row.chars().skip(a).take(b - a).collect()
}

/// The frame's text as screen rows (trailing blanks trimmed).
fn screen_text(frame: &Frame, g: Geometry) -> Vec<String> {
    let mut screen = vec![vec![' '; g.cols as usize]; g.rows as usize];
    for grid in &frame.text {
        for (dy, row) in grid.rows.iter().enumerate() {
            for (dx, c) in row.iter().enumerate() {
                let (x, y) = (grid.col as usize + dx, grid.row as usize + dy);
                if let (Some(ch), true) = (c.text.chars().next(), y < screen.len() && x < screen[0].len()) {
                    screen[y][x] = ch;
                }
            }
        }
    }
    screen.into_iter().map(|r| r.into_iter().collect::<String>().trim_end().to_owned()).collect()
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_synchronized_update_left_on_shows_after_its_time() {
    let script = r"printf 'before\n'; sleep 0.3; printf '\033[?2026hduring'; sleep 100";
    let (mut s, rx) = terminal_session(&["sh", "-c", script]);
    wait_text(&mut s, &rx, |rows| rows.iter().any(|r| r.contains("before")));
    // The update began: the session wakes when it stops holding.
    let until = Instant::now() + Duration::from_secs(10);
    while s.next_deadline().is_none() && Instant::now() < until {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            s.handle(ev, Instant::now());
        }
    }
    let due = s.next_deadline().expect("a deadline");
    assert!(due <= Instant::now() + temzdf_term::SYNC_TIMEOUT);
    let rows = screen_text(&s.frame(), GEOMETRY);
    assert!(!rows.iter().any(|r| r.contains("during")), "held: {rows:?}");
    assert_eq!(s.poll(due - Duration::from_millis(1)), Outcome::None);
    assert_eq!(s.poll(due), Outcome::Redraw);
    let rows = screen_text(&s.frame(), GEOMETRY);
    assert!(rows.iter().any(|r| r.contains("during")), "shown: {rows:?}");
    assert_eq!(s.next_deadline(), None);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn terminal_panes_show_output_take_keys_and_close_when_the_program_exits() {
    let (mut s, rx) = terminal_session(&[
        "sh",
        "-c",
        "stty -echo -ixon; echo ready; read l; echo \"got:$(printf %s \"$l\" | od -An -tx1 | tr -d ' ')\"; read l",
    ]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    // The prefix twice sends it itself.
    key(&mut s, "<C-b>");
    key(&mut s, "<C-b>");
    key(&mut s, "a<CR>");
    let (_, rows) = wait_text(&mut s, &rx, |r| r[1].starts_with("got:"));
    assert_eq!(rows[1], "got:0261", "C-b (0x02), then a");
    assert!(s.frame().cursor.is_some());
    key(&mut s, "<CR>");
    let (outcome, _) = wait_text(&mut s, &rx, |_| false);
    assert_eq!(outcome, Outcome::Quit, "the last pane's program exited");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn splitting_a_terminal_starts_another_shell_beside_it() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo left; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "left");
    assert_eq!(cmd(&mut s, "split", &["right"]), Ok(Outcome::Redraw));
    assert_eq!(s.pane_count(), 2);
    for c in "echo right-$((40+2))".chars() {
        s.key(&KeyEvent::new(KeyCode::Char(c), Modifiers::NONE), Instant::now());
    }
    key(&mut s, "<CR>");
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("right-42")));
    // The left pane keeps its text in its half; the new shell is in the right one.
    assert!(rows[0].starts_with("left"), "{rows:?}");
    let line = rows.iter().find(|l| l.contains("right-42")).unwrap();
    assert!(line.find("right-42").unwrap() >= 45, "{line:?}");
    // Closing it gives the first pane the whole screen again.
    cmd(&mut s, "close-pane", &[]).unwrap();
    assert_eq!(s.pane_count(), 1);
    // A bigger window: the pane (and its program's pty) grows with it.
    s.resize(Geometry { cols: 100, rows: 34, ..GEOMETRY });
    let frame = s.frame();
    // (Its status line takes a row.)
    assert_eq!((frame.text[0].rows.len(), frame.text[0].rows[0].len()), (33, 100));
}

/// Handle events until `done` holds (or ten seconds pass); returns the
/// strongest outcome meanwhile.
fn pump(s: &mut Session, rx: &Receiver<SessionEvent>, done: impl Fn(&Session) -> bool) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut out = Outcome::None;
    while !done(s) && Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
            out = out.merge(s.handle(ev, Instant::now()));
        }
    }
    out
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn closed_terminals_keep_running_hidden() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo first; read l; echo \"got:$l\"; read l"]);
    wait_text(&mut s, &rx, |r| r[0] == "first");
    let first = s.panes[&s.focus].buffer();
    cmd(&mut s, "split", &["right"]).unwrap();
    let second = s.panes[&s.focus].buffer();
    cmd(&mut s, "move-focus", &["left"]).unwrap();
    cmd(&mut s, "close-pane", &[]).unwrap();
    assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
    assert_eq!(s.panes[&s.focus].buffer(), second);
    // Shown again, it takes input.
    assert_eq!(s.show_in(s.focus, first), Ok(Outcome::Redraw));
    assert_eq!(s.current_mode(), "terminal");
    key(&mut s, "hi<CR>");
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "got:hi"));
    assert_eq!(rows[0], "first", "{rows:?}");
    // Its program ends: the last pane closes, and the session ends
    // although the other shell still runs hidden.
    key(&mut s, "<CR>");
    let out = pump(&mut s, &rx, |s| s.terms[&first].exited);
    assert_eq!(out, Outcome::Quit);
    assert!(!s.terms[&second].exited);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_hidden_terminal_that_ended_stays_until_it_was_shown() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo started; sleep 1; echo bye"]);
    wait_text(&mut s, &rx, |r| r[0] == "started");
    let first = s.panes[&s.focus].buffer();
    cmd(&mut s, "split", &["right"]).unwrap();
    cmd(&mut s, "move-focus", &["left"]).unwrap();
    cmd(&mut s, "close-pane", &[]).unwrap();
    pump(&mut s, &rx, |s| s.terms[&first].exited);
    assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
    let list = s.buffer_list(false);
    let ScriptValue::List(items) = &list else { panic!() };
    let entry = items.iter().find(|b| b.get("id") == Some(&ScriptValue::Num(first as f64))).unwrap();
    assert_eq!(entry.get("exited"), Some(&ScriptValue::Bool(true)));
    s.show_in(s.focus, first).unwrap();
    let (_, rows) = wait_text(&mut s, &rx, |r| r.get(1).is_some_and(|l| l == "bye"));
    assert_eq!(&rows[..2], ["started", "bye"]);
    // Hidden again, it goes.
    let shown = s.focus;
    cmd(&mut s, "split", &["right"]).unwrap();
    assert_eq!(s.close(shown), Outcome::Redraw);
    assert!(!s.terms.contains_key(&first));
    assert_eq!(s.buffer_count(), 2);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_terminal_in_two_panes_is_as_large_as_the_larger() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do stty size; done"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let first = s.panes[&s.focus].buffer();
    key(&mut s, "<CR>");
    // (Rows less the status line.)
    wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "29 90"));
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    s.show_in(right, first).unwrap();
    let widest = s.panes.values().map(|e| e.rect.w).max().unwrap();
    assert!(widest < 90);
    key(&mut s, "<CR>");
    let want = format!("29 {widest}");
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains(&want)));
    assert!(rows.iter().any(|l| l.contains(&want)), "{rows:?}");
    // One pane left: the whole width again.
    s.close(right);
    key(&mut s, "<CR>");
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().filter(|l| *l == "29 90").count() == 2);
    assert_eq!(rows.iter().filter(|l| *l == "29 90").count(), 2, "{rows:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn dragging_a_border_resizes_panes_and_their_programs_on_release() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let left = s.focus;
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    let (term, width) = (s.panes[&left].buffer(), |s: &Session, p| s.panes[&p].rect.w);
    let cols = |s: &Session| s.terms[&term].term.geometry().cols;
    let border = width(&s, left) as i32;
    let at = |kind, col: i32| MouseEvent {
        kind,
        x: col * 10 + 5,
        y: 105,
        unit: CoordUnit::Pixel,
        modifiers: Modifiers::NONE,
    };
    let now = Instant::now();
    s.mouse(&at(MouseEventKind::Move, border - 3), now);
    assert_eq!(s.pointer(), Pointer::Default);
    s.mouse(&at(MouseEventKind::Move, border), now);
    assert_eq!(s.pointer(), Pointer::ResizeColumns, "over the border");
    s.mouse(&at(MouseEventKind::Press(MouseButton::Left), border), now);
    assert_eq!(s.mouse(&at(MouseEventKind::Drag(MouseButton::Left), 60), now), Outcome::Redraw);
    assert_eq!((width(&s, left), width(&s, right)), (60, 29));
    assert_eq!(cols(&s), border as u16, "the program keeps its size while dragging");
    assert_eq!(s.pointer(), Pointer::ResizeColumns, "anywhere while dragging");
    s.mouse(&at(MouseEventKind::Drag(MouseButton::Left), 70), now);
    s.mouse(&at(MouseEventKind::Release(MouseButton::Left), 70), now);
    assert_eq!((width(&s, left), cols(&s)), (70, 70), "and gets the new size on release");
    assert_eq!(s.focus, right, "the border took the press");
    // resize-pane sizes the focused pane (as Vim's <C-w>> and <C-w><),
    // by the cells given (the shipped keys pass their count).
    assert_eq!(cmd(&mut s, "resize-pane", &["wider", "3"]), Ok(Outcome::Redraw));
    assert_eq!((width(&s, left), cols(&s)), (67, 67));
    cmd(&mut s, "resize-pane", &["narrower"]).unwrap();
    assert_eq!(width(&s, left), 68, "one cell without a count");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn panes_move_to_edges_and_rotate() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let one = s.focus;
    cmd(&mut s, "split", &["right"]).unwrap();
    let two = s.focus;
    cmd(&mut s, "split", &["bottom"]).unwrap();
    let three = s.focus;
    let panes = |s: &Session| s.layout.as_ref().unwrap().panes();
    assert_eq!(panes(&s), [one, two, three]);
    // H: the full height at the left; K: the full width at the top.
    cmd(&mut s, "move-pane", &["left"]).unwrap();
    assert_eq!(panes(&s), [three, one, two]);
    assert_eq!(s.panes[&three].rect.h, s.area().h);
    cmd(&mut s, "move-pane", &["top"]).unwrap();
    assert_eq!((s.panes[&three].rect.y, s.panes[&three].rect.w), (0, s.area().w));
    // r: the row of the others turns; R back.
    s.set_focus(one);
    cmd(&mut s, "rotate-panes", &[]).unwrap();
    assert_eq!(panes(&s), [three, two, one]);
    s.run_command("rotate-panes", &[ScriptValue::Bool(true)], Instant::now()).unwrap();
    assert_eq!(panes(&s), [three, one, two]);
    assert_eq!(s.focus, one, "the focus stays with its pane");
    // Sizes: + and - (one pane in a column: its border moves).
    let h = s.panes[&one].rect.h;
    cmd(&mut s, "resize-pane", &["shorter", "2"]).unwrap();
    assert_eq!(s.panes[&one].rect.h, h - 2);
    cmd(&mut s, "resize-pane", &["taller"]).unwrap();
    assert_eq!(s.panes[&one].rect.h, h - 1);
}

/// Open a floating pane running `argv` with `opts`; returns it.
fn float(s: &mut Session, argv: &[&str], opts: &[(&str, ScriptValue)]) -> PaneId {
    let what = ScriptValue::map([(
        "command",
        ScriptValue::List(argv.iter().map(|a| ScriptValue::Str((*a).into())).collect()),
    )]);
    let opts = ScriptValue::map(opts.iter().cloned());
    let (pane, _) = s.run_command("open-float", &[what, opts], Instant::now()).unwrap();
    pane.as_num().expect("a pane") as PaneId
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn the document's workers")]
fn a_pdf_opened_by_a_relative_path_that_does_not_resolve_is_known_by_its_absolute_one() {
    let (tx, _rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    // Not there (yet): canonicalize fails, the path is made absolute.
    let relative = Path::new("temzdf-no-such-dir").join(format!("missing-{}.pdf", std::process::id()));
    let absolute = std::env::current_dir().unwrap().join(&relative);
    s.open(&relative).unwrap();
    assert_eq!(s.docs.values().map(|d| &d.path).collect::<Vec<_>>(), [&absolute]);
    // So the same file by its absolute path is the same document.
    s.open(&absolute).unwrap();
    assert_eq!(s.docs.len(), 1, "one document, shared");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_float_opened_while_the_first_document_fails_leaves_a_session_that_shows() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    // The configuration opens a float; then the first pane's PDF fails.
    let f = float(&mut s, &["sleep", "30"], &[]);
    let bad = std::env::temp_dir().join(format!("temzdf-bad-{}.pdf", std::process::id()));
    std::fs::write(&bad, "not a PDF").unwrap();
    s.open(&bad).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !s.docs.is_empty() {
        assert!(Instant::now() < deadline, "the document never failed");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
            s.handle(ev, Instant::now());
        }
    }
    std::fs::remove_file(&bad).unwrap();
    assert!(!s.is_starting(), "frames are made again");
    assert!(s.layout.as_ref().is_some_and(|l| l.panes() == [f]), "the float is the tiled pane");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_float_covers_the_panes_below_it() {
    let (mut s, rx) =
        terminal_session(&["sh", "-c", "for i in 1 2 3 4 5 6 7 8; do printf '%080d\\n' 0; done; sleep 30"]);
    let zeros = "0".repeat(80);
    wait_text(&mut s, &rx, |r| r[7] == zeros);
    let n = |v: f64| ScriptValue::Num(v);
    let opts = [
        ("width", n(20.0)),
        ("height", n(4.0)),
        ("col", n(5.0)),
        ("row", n(2.0)),
        ("title", ScriptValue::Str("pop".into())),
    ];
    let pane = float(&mut s, &["sh", "-c", "echo FLOAT; sleep 30"], &opts);
    assert_eq!((s.focus, s.current_mode()), (pane, "terminal"));
    // The frame is 22 x 6 cells at (5, 2); the content inside it.
    assert_eq!(s.panes[&pane].rect, Rect::new(6, 3, 20, 4));
    let (_, rows) = wait_text(&mut s, &rx, |r| cols(&r[3], 6, 11) == "FLOAT");
    assert_eq!(rows[1], zeros, "above the float");
    assert_eq!(cols(&rows[2], 7, 12), " pop ", "the title on the frame");
    for (y, row) in rows.iter().enumerate().take(8).skip(2) {
        let inside: String = row.chars().skip(5).take(22).collect();
        assert!(!inside.contains('0'), "row {y}: the pane below shows through: {row:?}");
        assert_eq!(row.chars().nth(4), Some('0'), "row {y}: left of the float");
        assert_eq!(row.chars().nth(27), Some('0'), "row {y}: right of the float");
    }
    // The frame is a box of text around the title.
    assert_eq!(cols(&rows[2], 5, 27), "╭─ pop ──────────────╮");
    assert_eq!(cols(&rows[4], 5, 6), "│");
    assert_eq!(cols(&rows[7], 5, 27), "╰────────────────────╯");
    // Its terminal is as large as its content, less the status line.
    let buf = s.panes[&pane].buffer();
    assert_eq!((s.terms[&buf].term.geometry().cols, s.terms[&buf].term.geometry().rows), (20, 3));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn floats_cascade_raise_take_turns_and_hide_together() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    let a = float(&mut s, &["sleep", "30"], &[]);
    // Splitting a float opens another float beside it.
    cmd(&mut s, "split", &["right"]).unwrap();
    let b = s.focus;
    assert_ne!(b, a);
    assert_eq!(s.pane_count(), 3);
    let (fa, fb) = (s.floats[0].frame, s.floats[1].frame);
    assert_eq!((fb.x, fb.y, fb.w, fb.h), (fa.x + 2, fa.y + 2, fa.w, fa.h), "two cells right and down");
    // Focus moves among the floats, raising them.
    cmd(&mut s, "move-focus", &["left"]).unwrap();
    assert_eq!((s.focus, s.floats.last().unwrap().pane), (a, a));
    // The cycle: floats from the top, then the tiled panes.
    assert_eq!(s.focus_order(), [a, b, tiled]);
    cmd(&mut s, "focus-next-pane", &[]).unwrap();
    assert_eq!(s.focus, b);
    cmd(&mut s, "focus-next-pane", &[]).unwrap();
    assert_eq!(s.focus, tiled, "a tiled pane takes the focus; the floats stay");
    assert_eq!(s.covers(Some(tiled)).len(), 2);
    // Hidden: the focus goes to the tiled pane, nothing covers it.
    cmd(&mut s, "focus-next-pane", &[]).unwrap();
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!((s.focus, s.covers(Some(tiled)).len()), (tiled, 0));
    assert!(s.pane_at(10.0, 10.0) == Some(tiled), "hidden floats take no clicks");
    // Shown again: the top float has the focus.
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.focus, s.floats.last().unwrap().pane);
    // Moving and resizing by commands.
    s.run_command(
        "float-config",
        &[
            ScriptValue::Num(a as f64),
            ScriptValue::map([("width", ScriptValue::Num(30.0)), ("col", ScriptValue::Num(0.0))]),
        ],
        Instant::now(),
    )
    .unwrap();
    let fa = s.floats.iter().find(|f| f.pane == a).unwrap().frame;
    assert_eq!((fa.x, fa.w), (0, 32));
    s.set_focus(a);
    s.run_command("resize-pane", &[ScriptValue::Str("left".into()), ScriptValue::Num(5.0)], Instant::now()).unwrap();
    assert_eq!(s.panes[&a].rect.w, 25, "the right edge moved left");
    let ScriptValue::List(panes) = s.run_command("panes", &[], Instant::now()).unwrap().0 else { panic!() };
    let floating: Vec<bool> = panes.iter().map(|p| p.get("floating") == Some(&ScriptValue::Bool(true))).collect();
    assert_eq!(floating.iter().filter(|f| **f).count(), 2);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_last_tiled_pane_gives_way_to_a_shown_float() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    // The last tiled pane does not float.
    let err = s.run_command("toggle-floating", &[], Instant::now()).unwrap_err();
    assert!(err.contains("last tiled pane"), "{err}");
    let f = float(&mut s, &["sh", "-c", "echo floating; sleep 30"], &[]);
    // Hidden floats do not keep the session.
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.close(tiled), Outcome::Quit);
    // Shown, the top float takes the tiled pane's place.
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.close(tiled), Outcome::Redraw);
    assert_eq!((s.layout.as_ref().unwrap().panes(), s.floats.len(), s.focus), (vec![f], 0, f));
    assert_eq!(s.panes[&f].rect, s.area(), "the whole screen");
    // A tiled pane floats and comes back beside the focused tiled pane.
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    s.run_command("toggle-floating", &[], Instant::now()).unwrap();
    assert!(s.is_float(right));
    assert_eq!(s.layout.as_ref().unwrap().panes(), [f]);
    s.run_command("toggle-floating", &[], Instant::now()).unwrap();
    assert!(!s.is_float(right));
    assert_eq!(s.layout.as_ref().unwrap().panes(), [f, right]);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_floating_terminal_closes_when_its_program_exits() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    float(&mut s, &["sh", "-c", "sleep 0.2"], &[]);
    let out = pump(&mut s, &rx, |s| s.floats.is_empty());
    assert_ne!(out, Outcome::Quit);
    assert_eq!((s.floats.len(), s.focus), (0, tiled));
    // The tiled pane to return to closes while a float has the focus.
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    float(&mut s, &["sleep", "30"], &[]);
    assert_eq!(s.close(right), Outcome::Redraw);
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.focus, tiled, "hiding the floats focuses a tiled pane that is there");
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.floats.len(), 1);
    assert_eq!(s.focus, s.floats[0].pane);
    // With no float, toggling floats opens a floating shell.
    let only = s.focus;
    s.close(only);
    s.run_command("toggle-floats", &[], Instant::now()).unwrap();
    assert_eq!(s.floats.len(), 1);
    assert_eq!(s.focus, s.floats[0].pane);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn dragging_a_float_moves_and_resizes_it_and_its_program_on_release() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    let n = |v: f64| ScriptValue::Num(v);
    let f =
        float(&mut s, &["sleep", "30"], &[("width", n(20.0)), ("height", n(6.0)), ("col", n(10.0)), ("row", n(5.0))]);
    let frame = |s: &Session| s.floats.iter().find(|x| x.pane == f).unwrap().frame;
    let cols = |s: &Session| s.terms[&s.panes[&f].buffer()].term.geometry().cols;
    assert_eq!(frame(&s), Rect::new(10, 5, 22, 8));
    // Cells to pixels (10 x 20 cells), at the cell's centre.
    let at = |kind, col: i32, row: i32| MouseEvent {
        kind,
        x: col * 10 + 5,
        y: row * 20 + 10,
        unit: CoordUnit::Pixel,
        modifiers: Modifiers::NONE,
    };
    let now = Instant::now();
    let left = MouseButton::Left;
    // A click in the tiled pane focuses it; the float stays.
    s.mouse(&at(MouseEventKind::Press(left), 50, 20), now);
    s.mouse(&at(MouseEventKind::Release(left), 50, 20), now);
    assert_eq!(s.focus, tiled);
    // The pointer over the frame's parts.
    for (col, row, pointer) in [
        (15, 5, Pointer::Move),
        (10, 5, Pointer::ResizeNwse),
        (31, 12, Pointer::ResizeNwse),
        (31, 5, Pointer::ResizeNesw),
        (31, 8, Pointer::ResizeColumns),
        (15, 12, Pointer::ResizeRows),
        (15, 8, Pointer::Default),
    ] {
        s.mouse(&at(MouseEventKind::Move, col, row), now);
        assert_eq!(s.pointer(), pointer, "at ({col}, {row})");
    }
    // Moving by the title edge focuses the float.
    s.mouse(&at(MouseEventKind::Press(left), 15, 5), now);
    assert_eq!(s.focus, f);
    s.mouse(&at(MouseEventKind::Drag(left), 25, 2), now);
    assert_eq!(frame(&s), Rect::new(20, 2, 22, 8));
    s.mouse(&at(MouseEventKind::Release(left), 25, 2), now);
    // Resizing by the bottom right corner: the program gets the size on release.
    s.mouse(&at(MouseEventKind::Press(left), 41, 9), now);
    s.mouse(&at(MouseEventKind::Drag(left), 51, 11), now);
    assert_eq!(frame(&s), Rect::new(20, 2, 32, 10));
    assert_eq!((s.panes[&f].rect.w, cols(&s)), (30, 20), "the program keeps its size while dragging");
    assert_eq!(s.pointer(), Pointer::ResizeNwse);
    s.mouse(&at(MouseEventKind::Release(left), 51, 11), now);
    assert_eq!(cols(&s), 30);
    // Not below the smallest float.
    s.mouse(&at(MouseEventKind::Press(left), 20, 6), now);
    s.mouse(&at(MouseEventKind::Drag(left), 60, 6), now);
    s.mouse(&at(MouseEventKind::Release(left), 60, 6), now);
    assert_eq!((frame(&s).x, frame(&s).w), (40, 12), "the left edge stops at the smallest width");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn floats_open_hide_and_embed() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    cmd(&mut s, "open-float", &[]).unwrap();
    let f = s.focus;
    assert!(s.is_float(f), "open-float: a floating shell");
    assert_eq!(s.current_mode(), "terminal", "in its own mode");
    cmd(&mut s, "toggle-floats", &[]).unwrap();
    assert_eq!((s.focus, s.floats_hidden), (tiled, true), "toggle-floats: hidden");
    cmd(&mut s, "toggle-floats", &[]).unwrap();
    assert_eq!((s.focus, s.floats_hidden), (f, false), "toggle-floats: shown");
    cmd(&mut s, "toggle-floating", &[]).unwrap();
    assert!(!s.is_float(f), "toggle-floating: tiled");
    assert_eq!(s.current_mode(), "terminal", "the pane's mode");
    assert_eq!(s.layout.as_ref().unwrap().panes(), [tiled, f]);
    cmd(&mut s, "toggle-floating", &[]).unwrap();
    assert!(s.is_float(f), "toggle-floating: floating again");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn no_image_lies_under_text_drawn_over_it() {
    // WezTerm draws images over text whatever their z: text over images
    // (messages, a float's title) needs the images cut out below it.
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let pct = |p: &str| ScriptValue::Str(p.into());
    let f =
        float(&mut s, &["sleep", "30"], &[("width", pct("100%")), ("height", pct("100%")), ("title", pct("title"))]);
    assert_eq!(s.floats[0].frame, s.area());
    s.message("a message");
    let frame = s.frame();
    let notice = s.notice_rects()[0];
    let title = Rect::new(2, 0, 7, 1);
    let rows = screen_text(&frame, GEOMETRY);
    assert_eq!(cols(&rows[0], 2, 9), " title ");
    for r in [notice, title] {
        let px = s.pixels(&r);
        let under: Vec<_> = frame.layers.iter().filter(|l| l.dst.intersect(&px).is_some()).collect();
        assert!(under.is_empty(), "images under the text at {r:?}: {under:?}");
    }
    // Around them the frame is drawn.
    assert_eq!(cols(&rows[0], 0, 2), "╭─", "the frame of float {f}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn recursive_mappings_stop_with_an_error() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let feed = |keys: &str| Target::Actions(vec![Act::new("feedkeys", vec![ScriptValue::Str(keys.into())])]);
    let keys = |k: &str| notation::parse(k).unwrap();
    s.modes.map("normal", keys("a"), crate::keymap::Mapping::new(feed("a")));
    s.modes.map("normal", keys("b"), crate::keymap::Mapping::new(feed("bb")));
    for k in ["a", "b"] {
        let start = Instant::now();
        key(&mut s, &format!("<C-b>{k}"));
        assert!(start.elapsed() < Duration::from_secs(5), "{k} ran too long");
        let shown = s.notices.iter().any(|n| n.text.contains("recursive mapping"));
        assert!(shown, "{k}: an error says why");
        assert!(s.matcher.pending().is_empty());
        key(&mut s, "<Esc>");
    }
    // Ordinary keys work again afterwards.
    key(&mut s, "<C-b>%");
    assert_eq!(s.pane_count(), 2);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn keys_fed_without_typing_do_not_add_up_to_a_recursive_mapping() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let act = Target::Actions(vec![Act::new("current-mode", vec![])]);
    s.modes.map("terminal", notation::parse("<F5>").unwrap(), crate::keymap::Mapping::new(act));
    // As a hook or a timer feeds keys, for hours, with nothing typed.
    for _ in 0..=MAX_MAP_DEPTH {
        let fed = ScriptValue::Str("<F5>".into());
        s.run_command("feedkeys", &[fed], Instant::now()).unwrap();
        let _ = s.drive(Instant::now());
    }
    assert!(!s.notices.iter().any(|n| n.text.contains("recursive mapping")));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_terminal_closes_when_its_program_exits_although_jobs_hold_the_pty() {
    // The shell leaves a job behind that keeps the pty open. (Linux hangs
    // the pty up when its session leader exits, so its end of file came
    // anyway there; the exit is now taken from the process itself.)
    let (mut s, rx) = terminal_session(&["sh", "-c", "sleep 1000 & echo started"]);
    let (outcome, rows) = wait_text(&mut s, &rx, |_| false);
    assert_eq!(outcome, Outcome::Quit, "{rows:?}");
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn deleting_a_terminal_ends_a_program_that_ignores_hangups_without_waiting() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "trap '' HUP; echo ready; exec sleep 1000"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let buf = s.panes[&s.focus].buffer();
    let pid = s.terms[&buf].term.pid().unwrap();
    cmd(&mut s, "split", &["right"]).unwrap();
    let start = Instant::now();
    s.run_command("delete-buffer", &[ScriptValue::Num(buf as f64)], Instant::now()).unwrap();
    assert!(start.elapsed() < Duration::from_millis(100), "the session waited {:?}", start.elapsed());
    // Hung up, then killed a second later, and reaped (no zombie).
    let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while proc.exists() {
        assert!(Instant::now() < deadline, "{pid} is still there");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_float_opened_while_the_configuration_loads_leaves_the_first_pane_tiled() {
    let (tx, _rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    s.shell = Some(vec!["/bin/sh".into()]);
    float(&mut s, &["sleep", "30"], &[]);
    assert!(s.is_starting(), "no tiled pane yet");
    s.new_pane(&PaneSpec::Shell, None).unwrap();
    assert!(!s.is_starting());
    assert_eq!(s.layout.as_ref().map(|l| l.panes().len()), Some(1));
    assert_eq!(s.floats.len(), 1);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn floats_on_tiny_screens_and_cells_do_not_overflow() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let big = ScriptValue::Num(f64::from(u32::MAX));
    float(&mut s, &["sleep", "30"], &[("width", big.clone()), ("height", big)]);
    for g in [
        Geometry { cols: 3, rows: 2, cell: CellSize { width: 1, height: 1 } },
        Geometry { cols: 1, rows: 1, cell: CellSize { width: 10, height: 20 } },
    ] {
        s.resize(g);
        s.frame();
    }
    s.run_command("resize-pane", &[ScriptValue::map([("width", ScriptValue::Num(-5.0))])], Instant::now()).unwrap();
    s.frame();
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_float_waiting_for_its_file_keeps_its_title_and_focus() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let tiled = s.focus;
    let what = ScriptValue::map([(
        "file",
        ScriptValue::Str(temzdf_pdfcore::pdf::fixture("sample.pdf").display().to_string()),
    )]);
    let opts = ScriptValue::map([("title", ScriptValue::Str("paper".into())), ("focus", ScriptValue::Bool(false))]);
    let (pane, _) = s.run_command("open-float", &[what, opts], Instant::now()).unwrap();
    assert_eq!(pane, ScriptValue::Null, "opening: no pane yet");
    pump(&mut s, &rx, |s| !s.floats.is_empty());
    assert_eq!(s.floats[0].title.as_deref(), Some("paper"));
    assert_eq!(s.focus, tiled, "focus: false");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn panes_waiting_for_a_document_go_to_the_tab_they_were_asked_for() {
    let sample = temzdf_pdfcore::pdf::fixture("sample.pdf");
    let opened = |s: &Session| s.docs.values().all(|d| d.host.is_some());
    // A split, then another tab entered while the file opens.
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    s.open(&sample).unwrap();
    let other = new_tab(&mut s, &["sleep", "30"]);
    pump(&mut s, &rx, opened);
    assert_eq!((s.tab, s.focus), (1, other), "the current tab stays");
    let first: Vec<_> = s.layouts().filter(|(i, _)| *i == 0).map(|(_, l)| l.panes().len()).collect();
    assert_eq!(first, [2], "the split went into the first tab");
    assert_eq!(s.layout.as_ref().unwrap().panes(), [other]);
    // A float, and a second one on the document still opening.
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "tiled");
    let file = ScriptValue::map([("file", ScriptValue::Str(sample.display().to_string()))]);
    s.run_command("open-float", &[file], Instant::now()).unwrap();
    let doc = *s.docs.keys().next().unwrap();
    let buffer = ScriptValue::map([("buffer", ScriptValue::Num(doc as f64))]);
    s.run_command("open-float", &[buffer], Instant::now()).unwrap();
    let first_tab = s.tab_id();
    let other = new_tab(&mut s, &["sleep", "30"]);
    pump(&mut s, &rx, |s| opened(s) && s.floats.len() == 2);
    assert_eq!(s.floats.len(), 2, "the buffer's float opened too");
    assert!(s.floats.iter().all(|f| f.tab == tabs::FloatTab::Only(first_tab)), "on the first tab");
    assert_eq!((s.tab, s.focus), (1, other));
}

#[test]
fn tab_numbers_out_of_range_do_not_wrap_around() {
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    s.open_tab();
    let huge = (1u64 << 32) + 1;
    assert!(s.float_tab(&ScriptValue::Num(huge as f64)).is_err());
    assert!(s.float_tab(&ScriptValue::Str(huge.to_string())).is_err());
    assert!(s.run_command("next-tab", &[ScriptValue::Num(huge as f64)], Instant::now()).is_err());
    assert_eq!(s.tab, 1);
}

/// `new-tab` with a command; returns its pane.
fn new_tab(s: &mut Session, argv: &[&str]) -> PaneId {
    let what = ScriptValue::map([(
        "command",
        ScriptValue::List(argv.iter().map(|a| ScriptValue::Str((*a).into())).collect()),
    )]);
    let (pane, _) = s.run_command("new-tab", &[what], Instant::now()).unwrap();
    pane.as_num().expect("a pane") as PaneId
}

fn cmd(s: &mut Session, name: &str, args: &[&str]) -> Result<Outcome, String> {
    let args: Vec<ScriptValue> = args.iter().map(|a| ScriptValue::Str((*a).into())).collect();
    s.run_command(name, &args, Instant::now()).map(|(_, o)| o)
}

/// The tab line's labels, in order (`tabs()`).
fn labels(s: &mut Session) -> Vec<String> {
    let (list, _) = s.run_command("tabs", &[], Instant::now()).unwrap();
    let ScriptValue::List(list) = list else { panic!() };
    list.iter().map(|t| t.get("label").and_then(ScriptValue::as_str).unwrap_or_default().to_owned()).collect()
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn tabs_open_switch_move_and_close() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    let two = new_tab(&mut s, &["sh", "-c", "echo two; sleep 30"]);
    assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 1, two));
    let (_, rows) = wait_text(&mut s, &rx, |r| r[1] == "two");
    assert!(!rows.iter().any(|r| r.contains("one")), "only the current tab shows");
    // After the last, the first; the focus is the tab's own.
    cmd(&mut s, "next-tab", &[]).unwrap();
    assert_eq!((s.tab, s.focus), (0, one));
    wait_text(&mut s, &rx, |r| r[1] == "one");
    // A count goes to that tab (Vim's {N}gt), back as many (gT).
    cmd(&mut s, "next-tab", &["2"]).unwrap();
    assert_eq!((s.tab, s.focus), (1, two));
    cmd(&mut s, "prev-tab", &["3"]).unwrap();
    assert_eq!(s.tab, 0);
    assert!(cmd(&mut s, "goto-tab", &["3"]).unwrap_err().contains("no tab page 3"));
    // A new tab goes after the current one.
    let three = new_tab(&mut s, &["sh", "-c", "echo three; sleep 30"]);
    assert_eq!(s.tab, 1);
    for (i, name) in ["a", "c", "b"].iter().enumerate() {
        cmd(&mut s, "goto-tab", &[&(i + 1).to_string()]).unwrap();
        cmd(&mut s, "rename-tab", &[name]).unwrap();
    }
    assert_eq!(labels(&mut s), ["a", "c", "b"]);
    // :tabmove: after tab N (0: first), last, or by places.
    cmd(&mut s, "goto-tab", &["2"]).unwrap();
    cmd(&mut s, "move-tab", &[]).unwrap();
    assert_eq!((labels(&mut s), s.tab), (vec!["a".into(), "b".into(), "c".into()], 2));
    cmd(&mut s, "move-tab", &["0"]).unwrap();
    assert_eq!((labels(&mut s), s.tab), (vec!["c".into(), "a".into(), "b".into()], 0));
    cmd(&mut s, "move-tab", &["+1"]).unwrap();
    assert_eq!(labels(&mut s), ["a", "c", "b"]);
    // After itself (Vim: no effect), then after the last.
    cmd(&mut s, "move-tab", &["2"]).unwrap();
    assert_eq!(labels(&mut s), ["a", "c", "b"]);
    cmd(&mut s, "move-tab", &["3"]).unwrap();
    assert_eq!(labels(&mut s), ["a", "b", "c"]);
    // An empty name shows the focused buffer again.
    cmd(&mut s, "rename-tab", &[]).unwrap();
    assert_eq!(labels(&mut s)[2], "shell");
    // Closing the current tab goes to the one after it, else before; the
    // buffers stay.
    assert_eq!(s.focus, three);
    let buffers = s.buffer_count();
    assert_eq!(cmd(&mut s, "close-tab", &[]), Ok(Outcome::Redraw));
    assert_eq!((s.tabs.len(), s.tab, s.focus, s.buffer_count()), (2, 1, two, buffers));
    assert!(!s.panes.contains_key(&three));
    cmd(&mut s, "close-tab", &["1"]).unwrap();
    assert_eq!((s.tabs.len(), s.tab, s.focus), (1, 0, two));
    assert!(cmd(&mut s, "close-tab", &[]).unwrap_err().contains("last tab page"));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_tabs_last_pane_gives_way_to_its_own_float_else_the_tab_closes() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    let two = new_tab(&mut s, &["sleep", "30"]);
    let own = float(&mut s, &["sleep", "30"], &[]);
    let all = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(-1.0))]);
    // The tab's own float takes the place, not the one on all tabs.
    assert_eq!(s.close(two), Outcome::Redraw);
    assert_eq!(s.layout.as_ref().unwrap().panes(), [own]);
    assert!(s.is_float(all));
    // Without one, the tab closes; the float on all tabs stays.
    assert_eq!(s.close(own), Outcome::Redraw);
    assert_eq!((s.tabs.len(), s.tab), (1, 0));
    assert!(s.shown_floats().iter().any(|f| f.pane == all));
    // The last tab's last pane: the session ends (the float does not
    // keep it).
    assert_eq!(s.close(one), Outcome::Quit);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn panes_of_other_tabs_close_there() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    // A tab whose only program ends while another tab is current goes.
    new_tab(&mut s, &["sh", "-c", "sleep 0.3"]);
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    let out = pump(&mut s, &rx, |s| s.tabs.len() == 1);
    assert_ne!(out, Outcome::Quit);
    assert_eq!((s.tabs.len(), s.tab, s.focus), (1, 0, one));
    // One of two panes there: the tab stays, its focus moves on.
    let a = new_tab(&mut s, &["sleep", "30"]);
    cmd(&mut s, "split", &["right"]).unwrap();
    let b = s.focus;
    assert_ne!(a, b);
    let buf = s.panes[&b].buffer();
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    s.delete_buffer(buf).unwrap();
    assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 0, one));
    cmd(&mut s, "goto-tab", &["2"]).unwrap();
    assert_eq!((s.layout.as_ref().unwrap().panes(), s.focus), (vec![a], a));
    assert_eq!(s.panes[&a].rect, s.area(), "the whole screen again");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn toggle_floating_keeps_to_the_current_tab() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let left = s.focus;
    cmd(&mut s, "split", &["right"]).unwrap();
    new_tab(&mut s, &["sleep", "30"]);
    cmd(&mut s, "split", &["right"]).unwrap();
    let e = cmd(&mut s, "toggle-floating", &[&left.to_string()]).unwrap_err();
    assert!(e.contains("in this tab"), "{e}");
    let places = s.layouts().filter(|(_, l)| l.panes().contains(&left)).count()
        + s.floats.iter().filter(|f| f.pane == left).count();
    assert_eq!(places, 1, "a pane is in one place");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_tab_that_cannot_start_goes_back_where_it_was() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    new_tab(&mut s, &["sleep", "30"]);
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    let what = ScriptValue::map([("command", ScriptValue::List(vec![ScriptValue::Str("/nonexistent/x".into())]))]);
    assert!(s.run_command("new-tab", &[what], Instant::now()).is_err());
    assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 0, one));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn break_pane_moves_a_pane_to_a_new_tab() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let left = s.focus;
    assert!(cmd(&mut s, "break-pane", &[]).unwrap_err().contains("only tiled pane"));
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    cmd(&mut s, "break-pane", &[]).unwrap();
    assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 1, right));
    assert_eq!(s.layout.as_ref().unwrap().panes(), [right]);
    assert_eq!(s.panes[&right].rect, s.area());
    cmd(&mut s, "prev-tab", &[]).unwrap();
    assert_eq!((s.layout.as_ref().unwrap().panes(), s.focus), (vec![left], left));
    assert_eq!(s.panes[&left].rect, s.area());
    // A float becomes the new tab's tiled pane.
    let f = float(&mut s, &["sleep", "30"], &[]);
    cmd(&mut s, "break-pane", &[]).unwrap();
    assert!(!s.is_float(f));
    assert_eq!((s.tab, s.layout.as_ref().unwrap().panes()), (1, vec![f]));
    assert_eq!(s.tabs.len(), 3);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_terminal_in_two_tabs_is_as_large_as_the_larger() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    let buf = s.panes[&one].buffer();
    cmd(&mut s, "split", &["right"]).unwrap();
    let half = s.panes[&one].rect.w as u16;
    assert_eq!(s.terms[&buf].term.geometry().cols, half);
    let what = ScriptValue::map([("buffer", ScriptValue::Num(buf as f64))]);
    s.run_command("new-tab", &[what], Instant::now()).unwrap();
    assert_eq!(s.terms[&buf].term.geometry().cols, GEOMETRY.cols, "the whole screen in the new tab");
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    assert_eq!(s.terms[&buf].term.geometry().cols, GEOMETRY.cols, "the other tab still counts");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn only_the_current_tabs_images_are_drawn() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    assert!(!s.frame().layers.is_empty());
    new_tab(&mut s, &["sleep", "30"]);
    assert!(s.frame().layers.is_empty(), "the PDF is in the other tab");
    cmd(&mut s, "prev-tab", &[]).unwrap();
    assert!(!s.frame().layers.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_tab_line_takes_the_top_row_and_its_clicks_switch_and_close() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let one = s.focus;
    let buf = s.panes[&one].buffer();
    // (Less the pane's status line.)
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1, "one tab: no tab line");
    new_tab(&mut s, &["sh", "-c", "echo two; sleep 30"]);
    let (_, rows) = wait_text(&mut s, &rx, |r| r[1] == "two");
    assert_eq!(rows[0], " 1 shell  2 shell");
    assert_eq!(s.area(), Rect::new(0, 1, GEOMETRY.cols as u32, GEOMETRY.rows as u32 - 1));
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 2, "the other tab's terminal too");
    // A click on a tab goes there; the middle button closes it.
    let cell = GEOMETRY.cell;
    click(&mut s, 2 * cell.width as i32, cell.height as i32 / 2);
    assert_eq!((s.tab, s.focus), (0, one));
    wait_text(&mut s, &rx, |r| r[1] == "one");
    let middle =
        |kind| MouseEvent { kind, x: 12 * cell.width as i32, y: 0, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
    s.mouse(&middle(MouseEventKind::Press(MouseButton::Middle)), Instant::now());
    s.mouse(&middle(MouseEventKind::Release(MouseButton::Middle)), Instant::now());
    assert_eq!(s.tabs.len(), 1);
    // One tab: the line goes, the panes get the row back.
    assert_eq!(s.area().y, 0);
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
    // showtabline: always, never.
    s.set_option("showtabline", &ScriptValue::Str("always".into())).unwrap();
    assert_eq!((s.area().y, screen_text(&s.frame(), GEOMETRY)[0].as_str()), (1, " 1 shell"));
    assert_eq!(s.get_option("showtabline"), Ok(ScriptValue::Str("always".into())));
    s.set_option("showtabline", &ScriptValue::Str("never".into())).unwrap();
    new_tab(&mut s, &["sleep", "30"]);
    assert_eq!(s.area().y, 0);
    assert!(s.set_option("showtabline", &ScriptValue::Str("sometimes".into())).is_err());
    assert!(s.set_option("showtabline", &ScriptValue::Num(2.0)).is_err());
}

/// The colour of a theme colour as a cell's.
fn theme_rgb(c: termpix_ui::Color) -> temzdf_scene::Color {
    temzdf_scene::Color::Rgb([c.0[0], c.0[1], c.0[2]])
}

/// The composed cell at (`col`, `row`).
fn cell_at(frame: &Frame, col: u16, row: u16) -> Option<temzdf_scene::Cell> {
    frame.text.iter().rev().find_map(|g| {
        let r = g.rows.get(row.checked_sub(g.row)? as usize)?;
        r.get(col.checked_sub(g.col)? as usize).cloned()
    })
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn pdf_panes_say_their_file_page_zoom_and_messages_in_their_status_line() {
    let (mut s, rx) = session();
    let (frame, _) = settle(&mut s, &rx);
    let last = GEOMETRY.rows - 1;
    let rows = screen_text(&frame, GEOMETRY);
    assert!(rows[last as usize].starts_with(" sample.pdf"), "{rows:?}");
    assert!(rows[last as usize].contains("1 / 3"), "{rows:?}");
    assert!(rows[last as usize].ends_with('%'), "the zoom: {rows:?}");
    let theme = termpix_ui::Theme::dark();
    let name = cell_at(&frame, 1, last).unwrap();
    assert_eq!((name.fg, name.bg), (theme_rgb(theme.bar_fg), theme_rgb(theme.bar_bg)), "focused");
    // No page lies on the status line.
    let row_px = s.pixels(&Rect::new(0, last as i32, GEOMETRY.cols as u32, 1));
    assert!(frame.layers.iter().all(|l| l.dst.intersect(&row_px).is_none()), "an image on the status line");
    // Messages take the left side.
    s.run_cli(&["search-for".into(), "zzzzqqq".into()]).unwrap();
    let (frame, _) = settle(&mut s, &rx);
    let rows = screen_text(&frame, GEOMETRY);
    assert!(rows[last as usize].starts_with(" Pattern not found: zzzzqqq"), "{rows:?}");
    assert_eq!(cell_at(&frame, 1, last).unwrap().fg, theme_rgb(theme.error));
    // Another pane (the message gone, as the next key takes it): the
    // unfocused one's name is dimmed.
    s.panes.get_mut(&s.focus).and_then(PaneEntry::pdf).unwrap().message = None;
    cmd(&mut s, "split", &["right"]).unwrap();
    let (frame, _) = settle(&mut s, &rx);
    let left = s.panes.iter().find(|(id, _)| **id != s.focus).map(|(_, e)| e.rect).unwrap();
    let right = s.panes[&s.focus].rect;
    let at = |r: Rect| ((r.x + 1) as u16, (r.y + r.h as i32 - 1) as u16);
    let dim = cell_at(&frame, at(left).0, at(left).1).unwrap();
    let lit = cell_at(&frame, at(right).0, at(right).1).unwrap();
    assert_eq!((dim.fg, lit.fg), (theme_rgb(theme.bar_dim), theme_rgb(theme.bar_fg)), "{left:?} {right:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn a_selection_released_over_the_status_line_is_copied() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let at = |kind, x, y| MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
    let left = MouseButton::Left;
    // From the title down onto the status line (the bottom row, 580..600).
    s.mouse(&at(MouseEventKind::Press(left), 318, 188), Instant::now());
    s.mouse(&at(MouseEventKind::Drag(left), 450, 590), Instant::now());
    s.mouse(&at(MouseEventKind::Release(left), 450, 590), Instant::now());
    // The loop's poll after the event takes the copy in: its notice shows
    // in the next frame.
    let _ = s.poll(Instant::now());
    let notice = s.notices.iter().find(|n| n.text.starts_with("Copied ")).map(|n| n.text.clone());
    let copied = s.take_effects().into_iter().find_map(|e| match e {
        temzdf_scene::Effect::Copy(t) => Some(t),
        _ => None,
    });
    assert!(copied.as_deref().is_some_and(|t| t.starts_with("temzdf sample document")), "{copied:?}");
    let n = copied.as_deref().unwrap().chars().count();
    assert_eq!(notice, Some(format!("Copied {n} characters")));
    // It is the copy history's newest entry, from the PDF, as characters.
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let newest = match run(&mut s, "history", &[]) {
        ScriptValue::List(l) => l[0].clone(),
        v => panic!("{v:?}"),
    };
    assert_eq!(newest.get("text").and_then(ScriptValue::as_str), copied.as_deref());
    assert_eq!(newest.get("kind"), Some(&ScriptValue::Str("char".into())));
    assert_eq!(newest.get("how"), Some(&ScriptValue::Str("copy".into())));
    let source = newest.get("source").and_then(|x| x.get("kind"));
    assert_eq!(source, Some(&ScriptValue::Str("pdf".into())));
    // A text pane puts it.
    let buf = run(&mut s, "new-buffer", &[]);
    let o = ScriptValue::map([("side", ScriptValue::Str("right".into())), ("buffer", buf)]);
    run(&mut s, "split", &[o]);
    run(&mut s, "put", &[]);
    assert_eq!(Some(text_of(&mut s)), copied);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn a_pane_copy_notice_says_what_was_left_out() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let buf = s.panes[&s.focus].buffer();
    s.pane_copied(buf, "abc".into(), temzdf_scene::CopyKind::Char, Some("1 page(s) not loaded were left out"));
    assert!(s.notices.iter().any(|n| n.text == "Copied 3 characters; 1 page(s) not loaded were left out"));
}

/// What the session copied (OSC 52 in a terminal client).
fn copied(s: &mut Session) -> Vec<String> {
    s.take_effects()
        .into_iter()
        .filter_map(|e| match e {
            temzdf_scene::Effect::Copy(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_visual_mode_moves_selects_and_copies() {
    // cat echoes what reaches it: keys in the visual mode must not.
    let (mut s, rx) = terminal_session(&["sh", "-c", "printf 'hello world\\nsecond line\\n'; exec cat"]);
    wait_text(&mut s, &rx, |r| r[1] == "second line");
    key(&mut s, "<C-b>[");
    assert_eq!(s.current_mode(), "visual");
    key(&mut s, "x");
    for c in ["visual.top", "visual.word-forward", "visual.charwise", "visual.word-end"] {
        cmd(&mut s, c, &[]).unwrap();
    }
    assert!(rows_after(&mut s, &rx, 300)[2].is_empty(), "x went nowhere");
    cmd(&mut s, "visual.yank", &[]).unwrap();
    assert_eq!(copied(&mut s), ["world"]);
    assert!(s.notices.iter().any(|n| n.text == "Copied 5 characters"), "said so");
    assert_eq!(s.current_mode(), "terminal", "y leaves");
    assert!(!s.focused_term().unwrap().visual_kept());
    // Rows, escaping twice.
    cmd(&mut s, "switch-mode", &["visual"]).unwrap();
    for c in ["visual.top", "visual.linewise", "visual.down", "visual.yank"] {
        cmd(&mut s, c, &[]).unwrap();
    }
    assert_eq!(copied(&mut s), ["hello world\nsecond line\n"], "lines, with their line break");
    let (history, _) = s.run_command("history", &[], Instant::now()).unwrap();
    let ScriptValue::List(history) = history else { panic!() };
    let newest = &history[0];
    assert_eq!(newest.get("kind").and_then(ScriptValue::as_str), Some("line"));
    assert_eq!(newest.get("source").and_then(|x| x.get("kind")).and_then(ScriptValue::as_str), Some("terminal"));
    cmd(&mut s, "switch-mode", &["visual"]).unwrap();
    cmd(&mut s, "visual.charwise", &[]).unwrap();
    cmd(&mut s, "visual.escape", &[]).unwrap();
    assert_eq!(s.current_mode(), "visual", "the first escape drops the selection");
    cmd(&mut s, "visual.escape", &[]).unwrap();
    assert_eq!(s.current_mode(), "terminal");
    assert!(cmd(&mut s, "visual.left", &[]).is_err(), "no visual mode now");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_window_status_line_names_the_mode_and_the_session() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty size; sleep 30"]);
    // The pane's own status line takes a row.
    wait_text(&mut s, &rx, |r| r[0] == format!("{} {}", GEOMETRY.rows - 1, GEOMETRY.cols));
    s.set_option("showstatusline", &ScriptValue::Str("always".into())).unwrap();
    assert_eq!(s.get_option("showstatusline"), Ok(ScriptValue::Str("always".into())));
    let last = GEOMETRY.rows as usize - 1;
    let bottom = |s: &mut Session| screen_text(&s.frame(), GEOMETRY)[last].clone();
    let row = bottom(&mut s);
    assert!(row.starts_with(" TERMINAL ") && row.ends_with("term"), "{row:?}");
    // The pane is a row shorter.
    assert_eq!(s.panes[&s.focus].rect.h, GEOMETRY.rows as u32 - 1);
    cmd(&mut s, "switch-mode", &["normal"]).unwrap();
    assert!(bottom(&mut s).starts_with(" NORMAL "));
    cmd(&mut s, "exit-mode", &[]).unwrap();
    cmd(&mut s, "switch-mode", &["visual"]).unwrap();
    assert!(bottom(&mut s).starts_with(" VISUAL "));
    cmd(&mut s, "visual.linewise", &[]).unwrap();
    assert!(bottom(&mut s).starts_with(" VISUAL LINE "), "the kind of selection");
    let frame = s.frame();
    let cell = &frame.text.last().unwrap().rows[0][1];
    assert!(cell.attrs.bold && cell.bg != frame.text.last().unwrap().rows[0][40].bg, "the mode stands out");
    cmd(&mut s, "visual.escape", &[]).unwrap();
    cmd(&mut s, "visual.escape", &[]).unwrap();
    // With tab pages: which one, of how many.
    new_tab(&mut s, &["sh", "-c", "sleep 30"]);
    assert!(bottom(&mut s).ends_with("2/2  term"), "{:?}", bottom(&mut s));
    // never: the row goes back to the panes.
    s.set_option("showstatusline", &ScriptValue::Str("never".into())).unwrap();
    assert_eq!(s.panes[&s.focus].rect.h, GEOMETRY.rows as u32 - 1, "below the tab line only");
    assert!(!bottom(&mut s).contains("TERMINAL"));
    assert!(s.set_option("showstatusline", &ScriptValue::Str("auto".into())).is_err());
}

/// The screen after handling events for `ms` milliseconds.
fn rows_after(s: &mut Session, rx: &Receiver<SessionEvent>, ms: u64) -> Vec<String> {
    let until = Instant::now() + Duration::from_millis(ms);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if let Ok(ev) = rx.recv_timeout(left) {
            s.handle(ev, Instant::now());
        }
    }
    screen_text(&s.frame(), GEOMETRY)
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn the_visual_mode_is_for_terminals_and_waits_while_another_pane_has_the_focus() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    assert!(cmd(&mut s, "switch-mode", &["visual"]).is_err(), "not on a PDF pane");
    assert_eq!(s.current_mode(), "pdf");

    let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "first");
    let term = s.focus;
    cmd(&mut s, "switch-mode", &["visual"]).unwrap();
    cmd(&mut s, "visual.top", &[]).unwrap();
    cmd(&mut s, "visual.charwise", &[]).unwrap();
    // The search's input line takes the focus: the visual mode waits.
    let prompt = ScriptValue::map([("basemode", ScriptValue::Str("input".into()))]);
    let (buf, _) = s.run_command("new-buffer", &[prompt], Instant::now()).unwrap();
    let spec = ScriptValue::map([
        ("side", ScriptValue::Str("bottom".into())),
        ("buffer", buf.clone()),
        ("height", ScriptValue::Num(1.0)),
    ]);
    s.run_command("split", &[spec], Instant::now()).unwrap();
    assert_eq!(s.current_mode(), "input");
    let t = s.terms.values().next().unwrap();
    assert!(t.term.visual_kept() && !t.term.visual_on(), "suspended");
    // Searching from there, then back.
    cmd(&mut s, "visual.search-preview", &["fir", &term.to_string()]).unwrap();
    cmd(&mut s, "focus-pane", &[&term.to_string()]).unwrap();
    assert_eq!(s.current_mode(), "terminal");
    cmd(&mut s, "switch-mode", &["visual"]).unwrap();
    assert_eq!(s.focused_term().unwrap().visual_label(), Some("VISUAL CHAR"), "it goes on");
    assert_eq!(cmd(&mut s, "switch-mode", &["visual"]), Ok(Outcome::None), "once only");
    // The prefix's normal mode over it, and back.
    key(&mut s, "<C-b>");
    assert!(!s.focused_term().unwrap().visual_on());
    key(&mut s, "<Esc>");
    assert_eq!(s.current_mode(), "visual");
    assert_eq!(s.focused_term().unwrap().visual_label(), Some("VISUAL CHAR"));
    key(&mut s, "q");
    assert!(!s.focused_term().unwrap().visual_kept(), "leaving in its own pane ends it");
}

/// A terminal session with a prompt below it, as openPrompt makes one
/// (a nofile, unlisted text buffer in the mode `input`, one row high,
/// without wrapping, `/` inline virtual text at its start), focused;
/// returns the buffer and the pane.
fn prompt_session() -> (Session, Receiver<SessionEvent>, BufferId, PaneId) {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "first");
    let str = |v: &str| ScriptValue::Str(v.into());
    let o = ScriptValue::map([
        ("basemode", str("input")),
        ("attrs", ScriptValue::List(vec![str("nofile"), str("unlisted")])),
    ]);
    let (buf, _) = s.run_command("new-buffer", &[o], Instant::now()).unwrap();
    let o = ScriptValue::map([("side", str("bottom")), ("buffer", buf.clone()), ("height", ScriptValue::Num(1.0))]);
    let (pane, _) = s.run_command("split", &[o], Instant::now()).unwrap();
    let pane = pane.as_num().unwrap() as PaneId;
    cmd(&mut s, "status-line", &["off"]).unwrap();
    let o = ScriptValue::map([("wrap", ScriptValue::Bool(false)), ("number", str("off"))]);
    s.run_command("text.set-options", &[o], Instant::now()).unwrap();
    let at = ScriptValue::map([("start", pos(0, 0)), ("end", pos(0, 0))]);
    let virt = ScriptValue::map([
        ("virtText", ScriptValue::List(vec![ScriptValue::map([("text", str("/"))])])),
        ("style", ScriptValue::Null),
    ]);
    s.run_command("text.add-decoration", &[at, virt], Instant::now()).unwrap();
    (s, rx, buf.as_num().unwrap() as BufferId, pane)
}

/// The prompt pane's row (29) as text, and the cursor.
fn prompt_row(s: &mut Session) -> (String, Option<(u16, u16)>) {
    let frame = s.frame();
    (screen_text(&frame, GEOMETRY)[29].clone(), frame.cursor)
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn prompts_are_text_panes_taking_typed_text() {
    let (mut s, _rx, buf, pane) = prompt_session();
    assert_eq!((s.current_mode(), s.panes[&pane].mode_kind()), ("input", "edit"));
    assert_eq!(prompt_row(&mut s), ("/".into(), Some((1, 29))));
    // Characters typed go in after the prompt; keys with Ctrl or Alt,
    // and Enter, do not (unmapped here).
    key(&mut s, "ab<C-x><M-c><CR><Tab>C");
    assert_eq!(prompt_row(&mut s), ("/abC".into(), Some((4, 29))));
    // Input-method text, and pastes (lines and all: the cursor's line
    // is what a prompt runs).
    s.text("数式", Instant::now());
    assert_eq!(prompt_row(&mut s), ("/abC数 式".into(), Some((8, 29))));
    s.paste("x\ny");
    assert_eq!(text_of(&mut s), "abC数式x\ny");
    // A script's own buffer: unlisted, never modified.
    let (list, _) = s.run_command("buffers", &[], Instant::now()).unwrap();
    let ScriptValue::List(list) = list else { panic!() };
    assert!(list.iter().all(|b| b.get("id") != Some(&ScriptValue::Num(buf as f64))));
    let info = s.run_command("text.info", &[], Instant::now()).unwrap().0;
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_long_prompt_scrolls_sideways_to_keep_the_cursor_in_sight() {
    let (mut s, _rx, _buf, _pane) = prompt_session();
    let long: String = ('a'..='z').cycle().take(120).collect();
    s.run_command("text.insert", &[ScriptValue::Str(long.clone())], Instant::now()).unwrap();
    let end = ScriptValue::map([("line", ScriptValue::Num(0.0)), ("character", ScriptValue::Num(120.0))]);
    s.run_command("text.set-cursor", &[end], Instant::now()).unwrap();
    let (row, cursor) = prompt_row(&mut s);
    // 121 columns and the cursor's: the last 89 characters and the cursor.
    assert_eq!((row.as_str(), cursor), (&long[31..], Some((89, 29))));
    s.run_command("text.set-cursor", &[pos(0, 0)], Instant::now()).unwrap();
    let (row, cursor) = prompt_row(&mut s);
    assert_eq!((row.as_str(), cursor), (format!("/{}", &long[..89]).as_str(), Some((1, 29))), "the prompt again");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn hidden_buffers_reach_hooks() {
    let (mut s, _rx, buf, pane) = prompt_session();
    s.listen(&["BufferHidden", "BufferDeleted"]);
    let b = ScriptValue::Num(buf as f64);
    // Shown in a second pane: closing one does not hide it.
    let term_pane = *s.panes.iter().find(|(_, e)| e.mode_kind() == "terminal").unwrap().0;
    let o = ScriptValue::map([("buffer", b.clone()), ("pane", ScriptValue::Num(term_pane as f64))]);
    let (second, _) = s.run_command("split", &[o], Instant::now()).unwrap();
    s.script_events.clear();
    s.close(second.as_num().unwrap() as PaneId);
    assert!(s.script_events.iter().all(|(e, _)| e != "BufferHidden"), "{:?}", s.script_events);
    // Deleting it: hidden first, then deleted.
    s.script_events.clear();
    cmd(&mut s, "delete-buffer", &[&buf.to_string()]).unwrap();
    let names: Vec<&str> = s.script_events.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names, ["BufferHidden", "BufferDeleted"]);
    assert_eq!(s.script_events[0].1, [b, ScriptValue::Str("edit".into())]);
    assert!(!s.panes.contains_key(&pane));
    // A terminal hidden by closing its pane too.
    let first = s.focus;
    let term = s.panes[&first].buffer();
    cmd(&mut s, "split", &["right"]).unwrap();
    s.script_events.clear();
    s.close(first);
    assert_eq!(
        s.script_events.iter().find(|(e, _)| e == "BufferHidden").map(|(_, a)| a.clone()),
        Some(vec![ScriptValue::Num(term as f64), ScriptValue::Str("terminal".into())])
    );
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn split_shows_a_buffer_at_a_fixed_size_and_returns_the_pane() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "first");
    let first = s.focus;
    let buf = s.panes[&first].buffer();
    let opts = |kv: &[(&str, ScriptValue)]| ScriptValue::map(kv.iter().cloned());
    let n = |v: f64| ScriptValue::Num(v);
    let side = |v: &str| ScriptValue::Str(v.into());
    let split = |s: &mut Session, o: ScriptValue| s.run_command("split", &[o], Instant::now());
    // The first terminal again, one row high, below it.
    let (pane, _) =
        split(&mut s, opts(&[("side", side("bottom")), ("buffer", n(buf as f64)), ("height", n(1.0))])).unwrap();
    let pane = pane.as_num().expect("the new pane") as PaneId;
    assert_eq!((s.focus, s.panes[&pane].buffer()), (pane, buf));
    assert_eq!(s.panes[&pane].rect, Rect::new(0, 29, 90, 1));
    assert_eq!(s.panes[&first].rect, Rect::new(0, 0, 90, 28), "less the row and the border");
    // Fixed: a taller screen and equalizing leave it one row.
    s.resize(Geometry { rows: 40, ..GEOMETRY });
    assert_eq!(s.panes[&pane].rect, Rect::new(0, 39, 90, 1));
    cmd(&mut s, "equalize-panes", &[]).unwrap();
    assert_eq!(s.panes[&pane].rect.h, 1);
    // Back to the first pane by its id; a pane of another one to split.
    cmd(&mut s, "focus-pane", &[&first.to_string()]).unwrap();
    assert_eq!(s.focus, first);
    let (right, _) =
        split(&mut s, opts(&[("side", side("right")), ("pane", n(first as f64)), ("width", side("50%"))])).unwrap();
    let right = right.as_num().unwrap() as PaneId;
    assert_eq!((s.panes[&right].rect.w, s.panes[&right].rect.h), (45, 38));
    // Beside the one-row pane there is no room (4 rows at least).
    let beside = opts(&[("side", side("right")), ("pane", n(pane as f64)), ("width", n(20.0))]);
    assert!(split(&mut s, beside).is_err());
    // Wrong combinations are refused.
    assert!(split(&mut s, opts(&[("side", side("bottom")), ("width", n(3.0))])).is_err());
    assert!(split(&mut s, opts(&[("buffer", n(9999.0))])).is_err());
    assert!(cmd(&mut s, "focus-pane", &["9999"]).is_err());
}

/// Split the focused pane `side` ("right", "bottom"); the new pane.
fn split_focused(s: &mut Session, side: &str) -> PaneId {
    let (pane, _) = s.run_command("split", &[ScriptValue::Str(side.into())], Instant::now()).unwrap();
    pane.as_num().expect("the new pane") as PaneId
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn a_split_pdf_shows_the_same_place_at_the_same_zoom() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let from = s.focus;
    let viewer = |s: &mut Session, p: PaneId| s.panes.get_mut(&p).unwrap().pdf().unwrap().viewer.view_state();
    {
        let v = &mut s.panes.get_mut(&from).unwrap().pdf().unwrap().viewer;
        v.zoom(1.5, (0.0, 0.0));
        v.jump_to(40.0, 4000.0);
    }
    let before = viewer(&mut s, from);
    assert!(before.top.page > 0 && before.zoom.is_some(), "{before:?}");
    let new = split_focused(&mut s, "right");
    let after = viewer(&mut s, new);
    assert_eq!(after.top.page, before.top.page);
    assert!((after.top.y.unwrap() - before.top.y.unwrap()).abs() < 1.0, "{after:?} {before:?}");
    // The zoom itself, as the pane it came from keeps it, though half
    // as wide: not the zoom relative to the width (half as large).
    let from_after = viewer(&mut s, from);
    assert!((from_after.scale - before.scale).abs() < 1e-4, "{from_after:?} {before:?}");
    assert!((after.scale - before.scale).abs() < 1e-4, "{after:?} {before:?}");
    assert!((after.left - before.left).abs() < 1.0, "{after:?} {before:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn a_split_pdf_fitting_the_width_fits_the_new_width_in_both() {
    let (mut s, rx) = session();
    settle(&mut s, &rx);
    let from = s.focus;
    let viewer = |s: &mut Session, p: PaneId| s.panes.get_mut(&p).unwrap().pdf().unwrap().viewer.view_state();
    let before = viewer(&mut s, from);
    assert_eq!(before.zoom, None, "fits the width");
    let new = split_focused(&mut s, "right");
    let (a, b) = (viewer(&mut s, from), viewer(&mut s, new));
    assert_eq!((a.zoom, b.zoom), (None, None), "both still fit the width");
    assert!(a.scale < before.scale * 0.6, "the narrower width: {a:?} {before:?}");
    assert!((a.scale - b.scale).abs() < 0.05 * a.scale, "{a:?} {b:?}");
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf")]
fn a_split_markdown_viewer_shows_the_same_line_in_the_same_mode() {
    let words = "word ".repeat(30);
    let mut src = String::new();
    for i in 0..30 {
        src.push_str(&format!("## Section {i}\n\n{words}\n\n"));
    }
    let (mut s, _path) = markdown_session("split.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    // Text mode.
    run(&mut s, "markdown.goto", &[str("section-12")]).unwrap();
    let text = s.focus;
    let line = s.md_top_line(text).unwrap();
    assert!(line > 0);
    let new = split_focused(&mut s, "right");
    assert_eq!((s.md_mode(new), s.md_top_line(new)), (Some("text"), Some(line)));
    // Graphics mode.
    run(&mut s, "markdown.set-mode", &[str("graphics")]).unwrap();
    run(&mut s, "markdown.goto", &[str("section-20")]).unwrap();
    run(&mut s, "markdown.zoom", &[str("in")]).unwrap();
    let graphics = s.focus;
    let line = s.md_top_line(graphics).unwrap();
    let zoom = |s: &mut Session, p: PaneId| s.md_graphics(p).map(|(pane, _)| pane.zoom);
    let zoomed = zoom(&mut s, graphics).unwrap();
    assert!(zoomed > 1.0);
    let new = split_focused(&mut s, "right");
    assert_eq!((s.md_mode(new), s.md_top_line(new)), (Some("graphics"), Some(line)));
    assert_eq!(zoom(&mut s, new), Some(zoomed), "the same zoom");
    // A float split from it opens in text mode, at the same line.
    s.toggle_floating(graphics).unwrap();
    s.set_focus(graphics);
    let float = split_focused(&mut s, "right");
    assert!(s.is_float(float));
    assert_eq!((s.md_mode(float), s.md_top_line(float)), (Some("text"), Some(line)));
}

/// The buffer `panes()` says pane `pane` shows.
fn pane_buffer_of(s: &mut Session, pane: PaneId) -> BufferId {
    pane_info(s, pane).get("buffer").and_then(ScriptValue::as_num).unwrap() as BufferId
}

/// The ids and kinds `buffers()` (all of them) lists.
fn listed_buffers(s: &mut Session) -> Vec<(BufferId, String)> {
    let all = ScriptValue::map([("all", ScriptValue::Bool(true))]);
    let ScriptValue::List(list) = s.run_command("buffers", &[all], Instant::now()).unwrap().0 else { panic!() };
    list.iter()
        .map(|b| {
            let id = b.get("id").and_then(ScriptValue::as_num).unwrap() as BufferId;
            (id, b.get("kind").and_then(ScriptValue::as_str).unwrap().to_owned())
        })
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf")]
fn a_markdown_viewer_s_buffer_is_its_document_in_either_mode() {
    let words = "word ".repeat(40);
    let src: String = (0..30).map(|i| format!("## Section {i}\n\n{words}\n\n")).collect();
    let (mut s, path) = markdown_session("buffer.md", &src, &[]);
    s.listen(&["BufferCreated", "BufferHidden", "BufferDeleted"]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let pane = s.focus;
    let md = *s.md_docs.keys().next().unwrap();
    assert_eq!(pane_buffer_of(&mut s, pane), md, "not its drawing");
    let info = run(&mut s, "text.info", &[]).unwrap().0;
    assert_eq!(info.get("buffer").and_then(ScriptValue::as_num), Some(f64::from(md)));
    // Listed as a document; its drawings are not, even with `all`.
    let listed = listed_buffers(&mut s);
    assert!(listed.contains(&(md, "markdown".to_owned())), "{listed:?}");
    let drawings: Vec<BufferId> = s.md_docs[&md].renders.iter().map(|r| r.buf).collect();
    assert!(listed.iter().all(|(b, _)| !drawings.contains(b)), "{listed:?} {drawings:?}");
    // The buffer's variable, the focused pane's buffer, in either mode.
    let var = |s: &mut Session| run(s, "get-var", &[str("buffer"), str("x")]).unwrap().0;
    run(&mut s, "set-var", &[str("buffer"), str("x"), ScriptValue::Num(1.0)]).unwrap();
    s.script_events.clear();
    for mode in ["graphics", "text", "graphics", "text"] {
        run(&mut s, "markdown.set-mode", &[str(mode)]).unwrap();
        assert_eq!(s.md_mode(pane), Some(mode));
        assert_eq!(pane_buffer_of(&mut s, pane), md, "in {mode} mode");
        assert_eq!(var(&mut s), ScriptValue::Num(1.0), "in {mode} mode");
    }
    assert!(listed_buffers(&mut s).contains(&(md, "markdown".to_owned())));
    assert!(s.script_events.is_empty(), "switching modes: {:?}", s.script_events);
    // Narrower: another drawing, the same buffer, no events.
    let drawing = |s: &Session| match &s.panes[&pane].kind {
        Kind::Text { buf, .. } => *buf,
        _ => panic!("text"),
    };
    let before = drawing(&s);
    s.resize(Geometry { cols: 50, ..GEOMETRY });
    assert_ne!(drawing(&s), before, "drawn again for the new width");
    assert_eq!(pane_buffer_of(&mut s, pane), md);
    assert!(s.script_events.is_empty(), "a new width: {:?}", s.script_events);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf")]
fn deleting_a_markdown_document_closes_its_panes_in_either_mode() {
    let (mut s, path) = markdown_session("delete.md", "# Title\n\nText.\n", &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let md = *s.md_docs.keys().next().unwrap();
    let text = s.focus;
    // Another buffer's pane stays (the last pane would end the session).
    let other = run(&mut s, "new-buffer", &[]).unwrap().0;
    run(&mut s, "split", &[ScriptValue::map([("buffer", other)])]).unwrap();
    s.set_focus(text);
    let graphics = split_focused(&mut s, "right");
    run(&mut s, "markdown.set-mode", &[ScriptValue::Str("graphics".into())]).unwrap();
    assert_eq!((s.md_mode(text), s.md_mode(graphics)), (Some("text"), Some("graphics")));
    assert_eq!(s.panes_showing(md), [text, graphics]);
    s.listen(&["BufferHidden", "BufferDeleted"]);
    s.script_events.clear();
    // From the graphics pane: the text one closes too.
    run(&mut s, "delete-buffer", &[]).unwrap();
    assert!(!s.panes.contains_key(&text) && !s.panes.contains_key(&graphics));
    assert!(s.md_docs.is_empty() && s.texts.values().all(|t| t.markdown.is_none()), "drawings gone");
    let events: Vec<(String, Vec<ScriptValue>)> = s.script_events.clone();
    let m = ScriptValue::Num(f64::from(md));
    assert_eq!(
        events,
        [
            ("BufferHidden".to_owned(), vec![m.clone(), ScriptValue::Str("markdown".into())]),
            ("BufferDeleted".to_owned(), vec![m])
        ]
    );
    assert!(run(&mut s, "delete-buffer", &[ScriptValue::Num(f64::from(md))]).is_err(), "gone");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf, Blitz")]
fn a_markdown_document_stays_hidden_with_its_last_pane_and_shows_again_where_it_was() {
    let words = "word ".repeat(40);
    let src: String = (0..30).map(|i| format!("## Section {i}\n\n{words}\n\n")).collect();
    let (mut s, path) = markdown_session("show.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let md = *s.md_docs.keys().next().unwrap();
    let viewer = s.focus;
    let other = run(&mut s, "new-buffer", &[]).unwrap().0;
    let (pane, _) = run(&mut s, "split", &[ScriptValue::map([("buffer", other.clone())])]).unwrap();
    let pane = pane.as_num().unwrap() as PaneId;
    // showBuffer: the viewer, at the pane's width, in the mode the
    // option says.
    s.set_option("markdownmode", &str("graphics")).unwrap();
    run(&mut s, "show-buffer", &[ScriptValue::Num(f64::from(md)), ScriptValue::Num(f64::from(pane))]).unwrap();
    assert_eq!((s.md_mode(pane), pane_buffer_of(&mut s, pane)), (Some("graphics"), md));
    // A drawing is no buffer to scripts.
    let drawing = s.md_docs[&md].renders[0].buf;
    assert!(run(&mut s, "show-buffer", &[ScriptValue::Num(f64::from(drawing))]).is_err());
    s.set_focus(pane);
    run(&mut s, "markdown.goto", &[str("section-12")]).unwrap();
    let line = s.md_top_line(pane).unwrap();
    assert!(line > 0);
    // Hidden with its last pane, not deleted: kept (and listed), its
    // drawings and pages gone.
    s.listen(&["BufferHidden", "BufferDeleted"]);
    s.script_events.clear();
    s.close(viewer);
    assert!(s.script_events.is_empty(), "{:?}", s.script_events);
    let o = other.as_num().unwrap() as BufferId;
    s.show_in(pane, o).unwrap();
    let m = ScriptValue::Num(f64::from(md));
    assert_eq!(s.script_events, [("BufferHidden".to_owned(), vec![m.clone(), str("markdown")])]);
    assert!(s.md_docs.contains_key(&md), "kept");
    assert!(listed_buffers(&mut s).contains(&(md, "markdown".to_owned())));
    assert!(s.md_docs[&md].renders.is_empty() && s.md_docs[&md].pages.is_empty(), "drawings and pages gone");
    assert!(s.texts.values().all(|t| t.markdown.is_none()), "no drawing left");
    // Read again while hidden (no pane to touch), a fetch arriving: the
    // line follows the text.
    std::fs::write(&path, format!("# New top\n\nMore.\n\n{src}")).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    assert_eq!(s.md_file_changed(md), Outcome::Redraw);
    let _ = s.md_image_fetched(md, "http://127.0.0.1:9/x.png".into(), Err("refused".into()));
    let line = line + 4;
    // Opening the file again finds it.
    let p = str(path.to_str().unwrap());
    let at = ScriptValue::map([("pane", ScriptValue::Num(f64::from(pane)))]);
    run(&mut s, "markdown.open", &[p, at]).unwrap();
    assert_eq!(s.md_docs.keys().copied().collect::<Vec<_>>(), [md], "the same document");
    // Shown again where it was (in the mode the option says).
    assert_eq!((s.md_mode(pane), s.md_top_line(pane)), (Some("graphics"), Some(line)));
    s.show_in(pane, o).unwrap();
    s.set_option("markdownmode", &str("text")).unwrap();
    run(&mut s, "show-buffer", &[m.clone(), ScriptValue::Num(f64::from(pane))]).unwrap();
    assert_eq!((s.md_mode(pane), s.md_top_line(pane)), (Some("text"), Some(line)));
    // Only deleteBuffer deletes it.
    s.show_in(pane, o).unwrap();
    s.script_events.clear();
    run(&mut s, "delete-buffer", std::slice::from_ref(&m)).unwrap();
    assert_eq!(s.script_events, [("BufferDeleted".to_owned(), vec![m])]);
    assert!(s.md_docs.is_empty());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf")]
fn a_markdown_viewer_s_pane_kind_is_markdown_in_either_mode() {
    let (mut s, path) = markdown_session("kind.md", "# Title\n\nText.\n", &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let kind =
        |s: &mut Session, p: PaneId| pane_info(s, p).get("kind").and_then(ScriptValue::as_str).map(str::to_owned);
    let pane_kind = |s: &Session| s.key_context(None, String::new()).get("paneKind").cloned();
    let text = s.focus;
    assert_eq!(s.md_mode(text), Some("text"));
    assert_eq!(kind(&mut s, text).as_deref(), Some("markdown"));
    assert_eq!(pane_kind(&s), Some(ScriptValue::Str("markdown".into())));
    // Still a text pane for its base mode (Vim's keys).
    assert_eq!(s.pane_mode(text), "edit");
    s.listen(&["PaneOpened"]);
    s.script_events.clear();
    let new = split_focused(&mut s, "right");
    assert_eq!(s.md_mode(new), Some("text"));
    assert_eq!(
        s.script_events,
        [("PaneOpened".to_owned(), vec![ScriptValue::Num(f64::from(new)), ScriptValue::Str("markdown".into())])]
    );
    run(&mut s, "markdown.set-mode", &[ScriptValue::Str("graphics".into())]).unwrap();
    assert_eq!(kind(&mut s, new).as_deref(), Some("markdown"));
    assert_eq!(pane_kind(&s), Some(ScriptValue::Str("markdown".into())));
    // A text buffer's pane is an edit one.
    let other = run(&mut s, "new-buffer", &[]).unwrap().0;
    let (p, _) = run(&mut s, "split", &[ScriptValue::map([("buffer", other)])]).unwrap();
    assert_eq!(kind(&mut s, p.as_num().unwrap() as PaneId).as_deref(), Some("edit"));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_split_text_pane_has_the_cursor_and_scroll_of_the_pane_it_splits() {
    let content: String = (0..300).map(|i| format!("line {i}\n")).collect();
    let (mut s, _path) = text_session("split.txt", &content);
    let first = s.focus;
    let other = split_focused(&mut s, "right");
    let place = |s: &mut Session, p: PaneId, line: Option<usize>| {
        let Some(Kind::Text { buf, view }) = s.panes.get_mut(&p).map(|e| &mut e.kind) else { panic!("text") };
        let t = &s.texts[buf];
        if let Some(line) = line {
            view.view.cursor.pos = t.buf.text().line_start(line);
            view.view.top = t.buf.text().line_start(line - 5);
            view.view.relayout(&t.buf);
        }
        (view.view.cursor.pos, view.view.top)
    };
    // The panes on the buffer show different places; the split copies
    // the focused one's.
    place(&mut s, other, Some(40));
    let want = place(&mut s, first, Some(200));
    s.set_focus(first);
    let new = split_focused(&mut s, "bottom");
    assert_eq!(place(&mut s, new, None), want);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
fn frames_say_where_panes_and_their_images_are_for_frontends_without_images() {
    let (mut s, rx) = session();
    let (frame, _) = settle(&mut s, &rx);
    let pdf = s.focus;
    // The PDF pane: images, and its line for frontends without them in
    // the middle of its content (29 rows: the status line below).
    assert_eq!(
        frame.regions,
        [Region { pane: pdf, rect: Rect::new(0, 0, 90, 29), kind: RegionKind::Pdf, images: true }]
    );
    let rows = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
    assert_eq!(rows[14].trim(), NO_GRAPHICS_PDF);
    assert!(screen_text(&frame, GEOMETRY).iter().all(|r| !r.contains("disabled")), "not in the text");
    // A float over it cuts the line.
    let n = |v: f64| ScriptValue::Num(v);
    let opts = [("width", n(20.0)), ("height", n(5.0)), ("col", n(30.0)), ("row", n(11.0))];
    let f = float(&mut s, &["sleep", "30"], &opts);
    let frame = s.frame();
    let rows = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
    assert!(!rows[14].contains("graphics protocol"), "{:?}", rows[14]);
    let kinds: Vec<_> = frame.regions.iter().map(|r| (r.pane, r.kind, r.images)).collect();
    assert!(kinds.contains(&(f, RegionKind::Terminal, false)), "{kinds:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_terminal_region_has_images_once_a_program_shows_one() {
    let (mut s, rx) = terminal_session(&[
        "sh",
        "-c",
        "img() { printf '\\033_Ga=T,f=32,s=2,v=2,c=4,r=2,q=2;/wAA/wD/AP8AAP///////w==\\033\\\\'; echo; }; \
         read l; img; read l; img; echo second-done; sleep 30",
    ]);
    let pane = s.focus;
    let images = |s: &mut Session| s.frame().regions.iter().find(|r| r.pane == pane).map(|r| r.images);
    assert_eq!(images(&mut s), Some(false));
    key(&mut s, "<CR>");
    let deadline = Instant::now() + Duration::from_secs(10);
    while images(&mut s) != Some(true) {
        assert!(Instant::now() < deadline, "the image never came");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
            s.handle(ev, Instant::now());
        }
    }
    assert!(s.frame().fallback_text.is_empty(), "terminal panes have no line");
    // A second image: the flag stays set in every frame (a frontend names
    // the pane again whenever it goes false and true).
    key(&mut s, "<CR>");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !screen_text(&s.frame(), GEOMETRY).iter().any(|r| r.contains("second-done")) {
        assert!(Instant::now() < deadline, "the second image never came");
        assert_eq!(images(&mut s), Some(true), "the flag flickered");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
            s.handle(ev, Instant::now());
        }
    }
    assert_eq!(images(&mut s), Some(true));
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn hiding_a_status_line_gives_its_row_to_the_pane() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do stty size; done"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let pane = s.focus;
    let buf = s.panes[&pane].buffer();
    // A terminal's status line is blank, in the bar's colour.
    let theme = termpix_ui::Theme::dark();
    let last = GEOMETRY.rows - 1;
    let bar = cell_at(&s.frame(), 0, last).unwrap();
    assert_eq!((bar.text.as_str(), bar.bg), (" ", theme_rgb(theme.bar_bg)));
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
    cmd(&mut s, "status-line", &["off"]).unwrap();
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows, "the row back");
    assert!(cell_at(&s.frame(), 0, last).is_none_or(|c| c.bg != theme_rgb(theme.bar_bg)), "no status line");
    key(&mut s, "<CR>");
    wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "30 90"));
    // No argument: the other way round; a pane by its id.
    let id = ScriptValue::Num(pane as f64);
    s.run_command("status-line", &[ScriptValue::Null, id], Instant::now()).unwrap();
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
    assert_eq!(cell_at(&s.frame(), 0, last).unwrap().bg, theme_rgb(theme.bar_bg));
    assert!(cmd(&mut s, "status-line", &["maybe"]).is_err());
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn a_pane_one_row_high_shows_only_its_status_line() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do printf SAID; done"]);
    wait_text(&mut s, &rx, |r| r[0] == "ready");
    let pane = s.focus;
    let buf = s.panes[&pane].buffer();
    let one = Geometry { rows: 1, ..GEOMETRY };
    s.resize(one);
    // Its program keeps the size it had (never 0 rows).
    assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
    key(&mut s, "<CR>");
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
            s.handle(ev, Instant::now());
        }
    }
    let frame = s.frame();
    assert_eq!(screen_text(&frame, one), [""], "only the (blank) status line");
    assert_eq!(cell_at(&frame, 0, 0).unwrap().bg, theme_rgb(termpix_ui::Theme::dark().bar_bg));
    assert_eq!(frame.cursor, None, "no cursor in hidden content");
    // Hidden, the content gets its row back.
    cmd(&mut s, "status-line", &["off"]).unwrap();
    assert_eq!(s.terms[&buf].term.geometry().rows, 1);
    // (No newline after it: one row would scroll it away.)
    let (_, rows) = wait_text(&mut s, &rx, |r| r[0] == "SAID");
    assert_eq!(rows[0], "SAID");
}

/// The panes() entry of `pane`.
fn pane_info(s: &mut Session, pane: PaneId) -> ScriptValue {
    let (list, _) = s.run_command("panes", &[], Instant::now()).unwrap();
    let ScriptValue::List(list) = list else { panic!() };
    list.into_iter().find(|p| p.get("id").and_then(ScriptValue::as_num) == Some(pane as f64)).unwrap()
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn floats_show_on_their_tab_or_on_all_and_hide_per_tab() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    let own = float(&mut s, &["sleep", "30"], &[]);
    let all = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(-1.0))]);
    let shown = |s: &Session| s.shown_floats().iter().map(|f| f.pane).collect::<Vec<_>>();
    assert_eq!(shown(&s), [own, all]);
    new_tab(&mut s, &["sleep", "30"]);
    assert_eq!(shown(&s), [all], "only the float on all tabs");
    // The stack is one: raising here raises it there.
    let second = float(&mut s, &["sleep", "30"], &[]);
    assert_eq!(shown(&s), [all, second]);
    s.set_focus(all);
    assert_eq!(shown(&s), [second, all]);
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    assert_eq!(shown(&s), [own, all]);
    // Hiding is per tab; the float on all tabs follows the current one.
    cmd(&mut s, "toggle-floats", &[]).unwrap();
    assert!(shown(&s).is_empty());
    cmd(&mut s, "goto-tab", &["2"]).unwrap();
    assert_eq!(shown(&s), [second, all]);
    // A hidden float is not focused, and its tab page stays where it is.
    assert!(cmd(&mut s, "focus-pane", &[&own.to_string()]).is_err());
    assert_eq!(s.tab, 1, "still on the second tab page");
    // panes() says where each pane is.
    assert_eq!(pane_info(&mut s, own).get("tab"), Some(&ScriptValue::Num(1.0)));
    assert_eq!(pane_info(&mut s, second).get("tab"), Some(&ScriptValue::Num(2.0)));
    assert_eq!(pane_info(&mut s, all).get("tab"), Some(&ScriptValue::Null));
    // tabpage moves a float to another tab.
    let opts = ScriptValue::map([("tabpage", ScriptValue::Num(1.0))]);
    s.run_command("float-config", &[ScriptValue::Num(second as f64), opts], Instant::now()).unwrap();
    assert_eq!(shown(&s), [all]);
    assert_ne!(s.focus, second, "the focus stays on this tab");
    // A float on all tabs becomes a tiled pane of the current tab.
    s.set_focus(all);
    cmd(&mut s, "toggle-floating", &[]).unwrap();
    assert!(!s.is_float(all));
    assert_eq!(s.tab_of(all), Some(1));
    cmd(&mut s, "goto-tab", &["1"]).unwrap();
    assert!(!s.layout.as_ref().unwrap().panes().contains(&all));
    assert!(
        s.run_command(
            "open-float",
            &[ScriptValue::Null, ScriptValue::map([("tabpage", ScriptValue::Num(5.0))])],
            Instant::now()
        )
        .is_err()
    );
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn tabs_switch_and_break_out() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
    wait_text(&mut s, &rx, |r| r[0] == "one");
    cmd(&mut s, "split", &["right"]).unwrap();
    let right = s.focus;
    cmd(&mut s, "break-pane", &[]).unwrap();
    assert_eq!((s.tabs.len(), s.tab, s.focus, s.current_mode()), (2, 1, right, "terminal"));
    new_tab(&mut s, &["sleep", "30"]);
    cmd(&mut s, "next-tab", &[]).unwrap();
    assert_eq!((s.tab, s.current_mode()), (0, "terminal"));
    cmd(&mut s, "prev-tab", &[]).unwrap();
    assert_eq!(s.tab, 2);
    cmd(&mut s, "next-tab", &["2"]).unwrap();
    assert_eq!((s.tab, s.focus), (1, right));
    cmd(&mut s, "prev-tab", &["2"]).unwrap();
    assert_eq!(s.tab, 2);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn committed_text_is_typed_in_terminals_and_keys_elsewhere() {
    let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; read l; echo \"got:$l\"; sleep 30"]);
    // In the terminal mode: typed as it is (not keys, not a paste).
    s.text("日本語", Instant::now());
    key(&mut s, "<CR>");
    // (Wide characters leave a blank spacer cell in the screen's text.)
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
    assert!(rows.iter().any(|l| l.replace(' ', "").contains("got:日本語")), "{rows:?}");
    // In the normal mode: keys, so mappings work with an input method on.
    key(&mut s, "<C-b>");
    s.text("%", Instant::now());
    assert_eq!(s.layout.as_ref().unwrap().panes().len(), 2, "split by the % mapping");
    // paste asks the client for its clipboard.
    s.take_effects();
    s.run_command("paste", &[], Instant::now()).unwrap();
    s.run_command("paste", &[ScriptValue::Str("primary".into())], Instant::now()).unwrap();
    assert_eq!(
        s.take_effects(),
        [temzdf_scene::Effect::Paste { primary: false }, temzdf_scene::Effect::Paste { primary: true }]
    );
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_clients_focus_reaches_programs_that_asked() {
    let script = "printf '\\033[?1004h'; stty raw -echo; printf ready; v=$(dd bs=1 count=6 2>/dev/null | od -An -c | tr -s ' '); printf 'got:%s' \"$v\"; sleep 30";
    let (mut s, rx) = terminal_session(&["sh", "-c", script]);
    wait_text(&mut s, &rx, |r| r[0].contains("ready"));
    s.client_focus(false);
    s.client_focus(true);
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
    assert!(rows.iter().any(|l| l.contains("got: 033 [ O 033 [ I")), "{rows:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn unthemed_terminals_report_the_gui_defaults() {
    // One query at a time: under load two answers may come in two reads.
    let script = "stty raw -echo min 0 time 20; printf ready; \
        printf '\\033]4;1;?\\033\\\\'; a=$(dd bs=128 count=1 2>/dev/null | tr -d '\\033'); \
        printf '\\033]11;?\\033\\\\'; b=$(dd bs=128 count=1 2>/dev/null | tr -d '\\033'); \
        printf 'got:%s%s' \"$a\" \"$b\"; sleep 30";
    let (mut s, rx) = terminal_session(&["sh", "-c", script]);
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
    let all = rows.join("");
    // xterm's red and the GUI's default background (temzdf_scene::theme).
    assert!(all.contains("rgb:cdcd/0000/0000"), "{rows:?}");
    assert!(all.contains("rgb:1e1e/1e1e/1e1e"), "{rows:?}");
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn the_theme_answers_programs_colour_queries() {
    let script = "stty raw -echo min 0 time 20; printf ready; printf '\\033]11;?\\033\\\\'; v=$(dd bs=64 count=1 2>/dev/null | tr -d '\\033'); printf 'got:%s' \"$v\"; sleep 30";
    let (mut s, rx) = terminal_session(&["sh", "-c", "sleep 30"]);
    s.set_option("background", &ScriptValue::Str("#102030".into())).unwrap();
    s.set_option("palette", &ScriptValue::List(vec![ScriptValue::Str("#ff0000".into())])).unwrap();
    // A terminal opened after the theme was set gets it too.
    s.run_command(
        "new-pane",
        &[
            ScriptValue::Str("--".into()),
            ScriptValue::Str("sh".into()),
            ScriptValue::Str("-c".into()),
            ScriptValue::Str(script.into()),
        ],
        Instant::now(),
    )
    .unwrap();
    let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
    assert!(rows.iter().any(|l| l.contains("rgb:1010/2020/3030")), "{rows:?}");
    // Options read back; bad values are refused.
    assert_eq!(s.appearance().background, Some([0x10, 0x20, 0x30]));
    assert_eq!(s.appearance().palette, [[0xff, 0, 0]]);
    let jetbrains = temzdf_scene::FontEntry { family: "JetBrains Mono".into(), size: Some(14.0) };
    assert_eq!(s.appearance().font, [jetbrains], "the default");
    assert!(s.set_option("foreground", &ScriptValue::Str("#12345".into())).is_err());
    assert!(s.set_option("palette", &ScriptValue::List(vec![ScriptValue::Null])).is_err());
    let cjk = ScriptValue::Map(vec![("font".into(), ScriptValue::Str("Noto Sans CJK JP".into()))]);
    s.set_option("guifontwide", &ScriptValue::List(vec![cjk])).unwrap();
    assert_eq!(s.appearance().font_wide, [temzdf_scene::FontEntry { family: "Noto Sans CJK JP".into(), size: None }]);
}

#[test]
#[cfg_attr(miri, ignore = "Miri cannot open a pty")]
fn ctrl_shift_v_asks_the_client_to_paste() {
    let (mut s, _rx) = terminal_session(&["sleep", "30"]);
    s.take_effects();
    // As the GUI reports it: the base letter with Ctrl and Shift.
    let k = KeyEvent::new(KeyCode::Char('v'), Modifiers::CTRL | Modifiers::SHIFT);
    s.key(&k, Instant::now());
    assert_eq!(s.take_effects(), [temzdf_scene::Effect::Paste { primary: false }]);
}

/// A session editing `name` (written with `content` first) in a
/// temporary directory.
fn text_session(name: &str, content: &str) -> (Session, PathBuf) {
    let dir = std::env::temp_dir().join(format!("temzdf-text-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
    default_mappings(&mut s.modes);
    s.start(PaneSpec::File(path.clone()), None).unwrap();
    (s, path)
}

/// A session whose pane shows `name` (Markdown, in a fresh directory
/// with `others` beside it) in the viewer.
fn markdown_session(name: &str, content: &str, others: &[(&str, &str)]) -> (Session, PathBuf) {
    let (mut s, path) = text_session(name, content);
    for (n, c) in others {
        std::fs::write(path.parent().unwrap().join(n), c).unwrap();
    }
    let p = ScriptValue::Str(path.to_str().unwrap().into());
    let pane = ScriptValue::map([("pane", ScriptValue::Num(f64::from(s.focus)))]);
    s.run_command("markdown.open", &[p, pane], Instant::now()).unwrap();
    (s, path)
}

#[test]
#[cfg_attr(miri, ignore = "the file system, libtexprintf")]
fn the_markdown_viewer_draws_text_that_cannot_be_edited() {
    let src = "# Title\n\nSome *text* and $\\alpha^2$.\n";
    let (mut s, path) = markdown_session("view.md", src, &[]);
    let rows = screen_text(&s.frame(), GEOMETRY);
    assert_eq!(rows[0].trim_end(), "Title");
    assert_eq!(rows[1].trim_end(), "═════");
    assert_eq!(rows[3].trim_end(), "Some text and α².", "math by libtexprintf");
    // The buffer is not modifiable, by edits, typing or undo.
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let err = run(&mut s, "text.insert", &[ScriptValue::Str("x".into()), pos(0, 0)]).unwrap_err();
    assert!(err.contains("not modifiable"), "{err}");
    assert!(run(&mut s, "text.undo", &[]).unwrap_err().contains("not modifiable"));
    assert!(s.text_type("x").unwrap_err().contains("not modifiable"));
    assert_eq!(rows[0].trim_end(), screen_text(&s.frame(), GEOMETRY)[0].trim_end());
    // Its styles are decorations.
    let decos = run(&mut s, "text.decorations", &[]).unwrap().0;
    let ScriptValue::List(decos) = decos else { panic!() };
    assert!(decos.iter().any(|d| d.get("style") == Some(&ScriptValue::Str("MarkdownItalic".into()))));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn markdown_links_go_to_anchors_urls_and_files_with_a_way_back() {
    let src = "[down](#far) [web](https://example.com) [other](other.md#end)\n\n# Far\n";
    let (mut s, path) = markdown_session("links.md", src, &[("other.md", "# Other\n\n# End\n")]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let follow = |s: &mut Session, col: usize| {
        run(s, "text.set-cursor", &[pos(0, col)]).unwrap();
        run(s, "markdown.follow", &[]).unwrap().0
    };
    assert_eq!(follow(&mut s, 1), ScriptValue::Bool(true));
    assert_eq!(cursor_of(&mut s), (2, 0), "at the heading");
    assert_eq!(follow(&mut s, 7), ScriptValue::Bool(true));
    assert!(s.take_effects().iter().any(|e| matches!(e, Effect::OpenUri { uri, .. } if uri == "https://example.com")));
    assert_eq!(follow(&mut s, 4), ScriptValue::Bool(false), "a space: no link");
    let pane = s.focus;
    assert_eq!(follow(&mut s, 11), ScriptValue::Bool(true));
    assert_eq!(s.focus, pane, "the same pane");
    let rows = screen_text(&s.frame(), GEOMETRY);
    assert_eq!(rows[0].trim_end(), "End", "the other file, at its anchor");
    assert_eq!(run(&mut s, "markdown.back", &[]).unwrap().0, ScriptValue::Bool(true));
    assert!(screen_text(&s.frame(), GEOMETRY)[0].starts_with("down web other"));
    assert_eq!(run(&mut s, "markdown.back", &[]).unwrap().0, ScriptValue::Bool(false));
    // The other document stays, hidden: drawn no more.
    assert_eq!(s.md_docs.len(), 2);
    assert_eq!(s.md_docs.values().filter(|d| d.renders.is_empty()).count(), 1, "the other");
    let outline = run(&mut s, "markdown.outline", &[]).unwrap().0;
    assert_eq!(
        outline,
        ScriptValue::List(vec![ScriptValue::map([
            ("level", ScriptValue::Num(1.0)),
            ("text", ScriptValue::Str("Far".into())),
            ("anchor", ScriptValue::Str("far".into())),
            ("line", ScriptValue::Num(2.0)),
        ])])
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn markdown_is_drawn_again_for_a_new_width_and_a_changed_file_at_the_same_place() {
    let words = "word ".repeat(40);
    let src: String = (0..30).map(|i| format!("## Section {i}\n\n{words}\n\n")).collect();
    let (mut s, path) = markdown_session("long.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    run(&mut s, "markdown.goto", &[ScriptValue::Str("section-10".into())]).unwrap();
    assert_eq!(screen_text(&s.frame(), GEOMETRY)[0].trim_end(), "Section 10");
    let widths = |s: &Session| s.md_docs.values().next().unwrap().renders.iter().map(|r| r.width).collect::<Vec<_>>();
    assert_eq!(widths(&s), [90]);
    // Narrower: drawn again (paragraphs wrap at the new width), the old
    // drawing gone, the same heading on top.
    s.resize(Geometry { cols: 50, ..GEOMETRY });
    let rows = screen_text(&s.frame(), Geometry { cols: 50, ..GEOMETRY });
    assert_eq!(rows[0].trim_end(), "Section 10");
    assert!(rows[3].trim_end().len() <= 50 && rows[4].starts_with("word"), "{rows:?}");
    assert_eq!(widths(&s), [50], "drawn for the new width, the old drawing gone");
    // The file changes above: still at Section 10.
    let src2 = format!("# New top\n\nMore.\n\n{src}");
    std::fs::write(&path, &src2).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    let md = *s.md_docs.keys().next().unwrap();
    assert_eq!(s.md_file_changed(md), Outcome::Redraw);
    assert_eq!(screen_text(&s.frame(), Geometry { cols: 50, ..GEOMETRY })[0].trim_end(), "Section 10");
    assert_eq!(s.md_file_changed(md), Outcome::None, "unchanged since");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn markdown_images_show_inline_or_below_their_line() {
    let src = "An ![icon](pic.png) inline.\n\n![figure](pic.png)\n\nAfter.\n";
    let (mut s, path) = text_session("img.md", src);
    write_png(&path.parent().unwrap().join("pic.png"), 20, 40);
    let p = ScriptValue::Str(path.to_str().unwrap().into());
    let pane = ScriptValue::map([("pane", ScriptValue::Num(f64::from(s.focus)))]);
    s.run_command("markdown.open", &[p, pane], Instant::now()).unwrap();
    // The cursor elsewhere: line 0 is the cursor's, so move it down.
    s.run_command("text.set-cursor", &[pos(4, 0)], Instant::now()).unwrap();
    let frame = s.frame();
    let mut layers: Vec<_> = frame.layers.iter().map(|l| (l.dst.x, l.dst.y, l.dst.w, l.dst.h)).collect();
    layers.sort();
    // 10x20 cells: inline, one row high where "[icon]" was (column 3);
    // below its line, two rows high (40 px).
    assert_eq!(layers, [(0, 60, 20, 40), (30, 0, 10, 20)], "{layers:?}");
    let rows = screen_text(&frame, GEOMETRY);
    assert!(rows[0].starts_with("An   inline."), "{:?}", rows[0]);
    assert_eq!(rows[2].trim(), "", "the alt text hidden");
    assert_eq!(rows[6].trim_end(), "After.");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "sockets and threads")]
fn markdown_remote_images_come_when_asked_for() {
    use std::io::{Read, Write};
    // A one-request HTTP server with a PNG.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = std::env::temp_dir().join(format!("temzdf-md-remote-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("p.png");
    write_png(&png, 30, 20);
    let body = std::fs::read(&png).unwrap();
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut buf = [0; 4096];
        let _ = c.read(&mut buf).unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        c.write_all(head.as_bytes()).unwrap();
        c.write_all(&body).unwrap();
    });
    let src = format!("Text.\n\n![remote](http://127.0.0.1:{port}/p.png)\n");
    let md = dir.join("r.md");
    std::fs::write(&md, src).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
    s.start(PaneSpec::File(md.clone()), None).unwrap();
    let p = ScriptValue::Str(md.to_str().unwrap().into());
    let pane = ScriptValue::map([("pane", ScriptValue::Num(f64::from(s.focus)))]);
    s.run_command("markdown.open", &[p, pane], Instant::now()).unwrap();
    assert!(s.frame().layers.is_empty(), "not fetched unasked");
    let n = s.run_command("markdown.load-remote-images", &[], Instant::now()).unwrap().0;
    assert_eq!(n, ScriptValue::Num(1.0));
    loop {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        let image = matches!(ev, SessionEvent::MarkdownImage(..));
        s.handle(ev, Instant::now());
        if image {
            break;
        }
    }
    server.join().unwrap();
    let layers = s.frame().layers;
    assert_eq!(layers.len(), 1, "{layers:?}");
    assert_eq!((layers[0].dst.w, layers[0].dst.h), (30, 20));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system, Blitz, RaTeX")]
fn fetched_images_lay_the_markdown_page_out_again_from_the_same_html() {
    let mut src = String::from("Some $x^2 + y^2$ math.\n\n");
    for i in 0..3 {
        src.push_str(&format!("![image {i}](http://127.0.0.1:9/{i}.png)\n\n"));
    }
    let (mut s, path) = markdown_session("f.md", &src, &[]);
    let png = path.parent().unwrap().join("p.png");
    write_png(&png, 30, 20);
    let bytes = std::fs::read(&png).unwrap();
    s.run_command("markdown.set-mode", &[ScriptValue::Str("graphics".into())], Instant::now()).unwrap();
    let md = *s.md_docs.keys().next().unwrap();
    assert_eq!((s.md_docs[&md].bodies, s.md_docs[&md].layouts), (1, 1));
    for i in 0..3 {
        s.md_image_fetched(md, format!("http://127.0.0.1:9/{i}.png"), Ok(bytes.clone()));
    }
    // Laid out again for each, the HTML (and its math) made once.
    assert_eq!((s.md_docs[&md].bodies, s.md_docs[&md].layouts), (1, 4));
    assert_eq!(s.md_docs[&md].pages.len(), 1, "the old pages went");
    // A changed file makes it again.
    std::fs::write(&path, format!("{src}More.\n")).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    s.md_file_changed(md);
    assert_eq!(s.md_docs[&md].bodies, 2);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system, Blitz")]
fn a_pinch_magnifies_the_markdown_page_without_laying_it_out_again() {
    use termpix_core::input::{PinchPhase, ScrollPhase, ScrollUnit};
    let words = "word ".repeat(30);
    let mut src = String::from("[jump](#section-12)\n\n");
    for i in 0..20 {
        src.push_str(&format!("## Section {i}\n\n{words}\n\n"));
    }
    let (mut s, _path) = markdown_session("g.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    s.events = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let painted = move |s: &mut Session| {
        let mut frame = s.frame();
        while !s.is_complete() || !s.is_idle() {
            let ev = rx.recv_timeout(Duration::from_secs(20)).expect("a strip");
            s.handle(ev, Instant::now());
            frame = s.frame();
        }
        frame
    };
    run(&mut s, "markdown.set-mode", &[str("graphics")]).unwrap();
    painted(&mut s);
    let md = *s.md_docs.keys().next().unwrap();
    let page = s.md_docs[&md].pages[0].id;
    let ev = |kind, (x, y)| MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
    let pinch = |s: &mut Session, delta, phase, at| {
        s.mouse(&ev(MouseEventKind::Pinch { delta, phase }, at), Instant::now());
    };
    // Where a pane pixel is on the page as laid out, and the magnification.
    let on_page = |s: &mut Session, (x, y): (i32, i32)| {
        let (p, _) = s.md_graphics(s.focus).unwrap();
        let (ox, oy) = p.viewport.offset();
        let m = p.magnify();
        ((f64::from(x) + ox) / m, (f64::from(y) + oy) / m)
    };
    let magnify = |s: &mut Session| s.md_graphics(s.focus).unwrap().0.magnify();
    // Pinched on the link, it stays under the fingers.
    let at = {
        let (p, page) = s.md_graphics(s.focus).unwrap();
        let r = page.find("jump")[0];
        (((r.x0 + r.x1) / 2.0) as i32, ((r.y0 + r.y1) / 2.0 - p.scroll()) as i32)
    };
    let before = on_page(&mut s, at);
    pinch(&mut s, 0, PinchPhase::Begin, at);
    for _ in 0..5 {
        pinch(&mut s, 1000, PinchPhase::Change, at);
    }
    assert!((magnify(&mut s) - 1.1f64.powi(5)).abs() < 1e-9);
    let after = on_page(&mut s, at);
    assert!((before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6, "{before:?} {after:?}");
    // Not laid out again; while pinching, the strips stretched, and
    // nothing painted at the magnification.
    // (Layers come clipped to the pane.)
    let scaled = |l: &Layer| f64::from(l.dst.w) / f64::from(l.src.w);
    let stretched = |f: &Frame| f.layers.iter().all(|l| (scaled(l) - 1.1f64.powi(5)).abs() < 0.01);
    let frame = s.frame();
    assert!(stretched(&frame), "{:?}", frame.layers);
    assert_eq!((s.md_docs[&md].pages.len(), s.md_docs[&md].pages[0].id), (1, page));
    assert!((s.md_docs[&md].pages[0].zoom - 1.0).abs() < 1e-6, "the zoom is another thing");
    assert!(s.md_docs[&md].strips.iter_used().all(|(k, _)| k.magnify == 0));
    assert!(s.is_idle(), "nothing painted while pinching");
    // When the fingers leave: rounded to hundredths, painted at it,
    // tiles alone (not stretched) covering the pane.
    pinch(&mut s, 0, PinchPhase::End, at);
    assert_eq!(magnify(&mut s), 1.61);
    let frame = painted(&mut s);
    let tiles: Vec<_> = frame.layers.iter().filter(|l| l.z == 0).collect();
    assert!(tiles.iter().all(|l| l.src.w <= 512 && (l.src.w, l.src.h) == (l.dst.w, l.dst.h)), "{tiles:?}");
    assert!(tiles.len() > 2, "{tiles:?}");
    let area: i64 =
        tiles.iter().filter_map(|l| l.dst.intersect(&Rect::new(0, 0, 900, 580))).map(|r| i64::from(r.w * r.h)).sum();
    assert_eq!(area, 900 * 580);
    assert!(s.md_docs[&md].strips.iter_used().any(|(k, _)| k.magnify == 161));
    // A click on the link (where it is magnified) follows it.
    for kind in [MouseEventKind::Press(MouseButton::Left), MouseEventKind::Release(MouseButton::Left)] {
        s.mouse(&ev(kind, at), Instant::now());
    }
    assert_eq!(s.md_top_line(s.focus), Some(2 + 4 * 12), "followed");
    assert_eq!(magnify(&mut s), 1.61, "still magnified");
    // Fingers move it sideways too.
    let x0 = s.md_graphics(s.focus).unwrap().0.viewport.offset().0;
    let kind = MouseEventKind::ScrollBy { dx: 5000, dy: 0, unit: ScrollUnit::Pixel, phase: ScrollPhase::Touch };
    s.mouse(&ev(kind, (300, 300)), Instant::now());
    assert_eq!(s.md_graphics(s.focus).unwrap().0.viewport.offset().0, x0 + 50.0);
    // The zoom lays the page out again; the magnification stays.
    let line = s.md_top_line(s.focus);
    run(&mut s, "markdown.zoom", &[str("in")]).unwrap();
    assert!((s.md_docs[&md].pages[0].zoom - 1.1).abs() < 1e-3);
    assert_eq!((magnify(&mut s), s.md_top_line(s.focus)), (1.61, line));
    // Pinched back (past 1, which is as small as it gets): the page as
    // it was shown before.
    pinch(&mut s, 0, PinchPhase::Begin, at);
    pinch(&mut s, -5000, PinchPhase::Change, at);
    pinch(&mut s, 0, PinchPhase::End, at);
    assert_eq!(magnify(&mut s), 1.0);
    let frame = painted(&mut s);
    assert!(frame.layers.iter().all(|l| (l.src.w, l.dst.w) == (900, 900)), "{:?}", frame.layers);
    assert_eq!(s.md_graphics(s.focus).unwrap().0.viewport.offset().0, 0.0);
    // Search marks are images as large as shown: a pinch makes new
    // sizes at each step, and they do not pile up.
    run(&mut s, "search-for", &[str("Section")]).unwrap();
    let at = {
        let (p, page) = s.md_graphics(s.focus).unwrap();
        let scroll = p.scroll();
        let r = *page.find("Section").iter().find(|r| r.y0 >= scroll).unwrap();
        (((r.x0 + r.x1) / 2.0) as i32, ((r.y0 + r.y1) / 2.0 - scroll) as i32)
    };
    let images = s.images().len();
    pinch(&mut s, 0, PinchPhase::Begin, at);
    for _ in 0..400 {
        pinch(&mut s, 54, PinchPhase::Change, at);
        let marks = s.frame().layers.iter().filter(|l| l.z == 1).count();
        assert!(marks > 0);
    }
    assert!(s.images().len() < images + 300, "{} images, {images} before", s.images().len());
    pinch(&mut s, 0, PinchPhase::End, at);
}

#[test]
#[cfg_attr(miri, ignore = "the file system, Blitz")]
fn a_zoom_of_several_steps_lays_the_markdown_page_out_once() {
    let words = "word ".repeat(30);
    let mut src = String::new();
    for i in 0..20 {
        src.push_str(&format!("## Section {i}\n\n{words}\n\n"));
    }
    let (mut s, _path) = markdown_session("z.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    run(&mut s, "markdown.goto", &[str("section-7")]).unwrap();
    run(&mut s, "markdown.set-mode", &[str("graphics")]).unwrap();
    let md = *s.md_docs.keys().next().unwrap();
    let pane = s.focus;
    let line = s.md_top_line(pane);
    // Three steps one by one, then back, then three at once.
    for _ in 0..3 {
        s.md_zoom(pane, 1, 1).unwrap();
    }
    let (zoom, top) = (s.md_docs[&md].pages[0].zoom, s.md_top_line(pane));
    assert_eq!(top, line, "zoomed in place");
    s.md_zoom(pane, 0, 1).unwrap();
    let before = s.md_docs[&md].layouts;
    // Ctrl and a high-resolution wheel three notches up at once.
    let kind = MouseEventKind::ScrollBy {
        dx: 0,
        dy: -300,
        unit: termpix_core::input::ScrollUnit::Line,
        phase: termpix_core::input::ScrollPhase::Wheel,
    };
    s.mouse(&MouseEvent { kind, x: 100, y: 100, unit: CoordUnit::Pixel, modifiers: Modifiers::CTRL }, Instant::now());
    assert_eq!(s.md_docs[&md].layouts - before, 1, "laid out once");
    assert_eq!((s.md_docs[&md].pages.len(), s.md_docs[&md].pages[0].zoom), (1, zoom));
    assert_eq!(s.md_top_line(pane), top);
}

#[test]
#[cfg_attr(miri, ignore = "the file system, Blitz")]
fn the_markdown_graphics_mode_scrolls_zooms_searches_and_switches_back_in_place() {
    let words = "word ".repeat(30);
    let mut src = String::from("[jump](#section-12)\n\n");
    for i in 0..20 {
        src.push_str(&format!("## Section {i}\n\n{words}\n\n"));
    }
    let (mut s, path) = markdown_session("g.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    run(&mut s, "markdown.goto", &[str("section-5")]).unwrap();
    s.options.scrollstep = 100.0;
    s.apply_options();
    // Strips come from worker threads.
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    s.events = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let painted = move |s: &mut Session| {
        // (As headless renders wait: until idle, the strips around too.)
        let mut frame = s.frame();
        while !s.is_complete() || !s.is_idle() {
            let ev = rx.recv_timeout(Duration::from_secs(20)).expect("a strip");
            s.handle(ev, Instant::now());
            frame = s.frame();
        }
        frame
    };
    run(&mut s, "markdown.set-mode", &[str("graphics")]).unwrap();
    assert_eq!(run(&mut s, "markdown.mode", &[]).unwrap().0, str("graphics"));
    assert_eq!(s.pane_mode(s.focus), "markdown");
    // The outline (`o`, `gO`) in this mode too.
    let ScriptValue::List(outline) = run(&mut s, "markdown.outline", &[]).unwrap().0 else { panic!("a list") };
    assert_eq!(outline.len(), 20);
    // Before they arrive, the page's background stands in; the strips
    // in sight are asked for first, then those a screen above and below.
    let first = s.frame();
    assert!(!s.is_complete() && !s.is_idle(), "strips are being painted");
    let one_image = |f: &Frame| f.layers.windows(2).all(|w| w[0].image == w[1].image);
    assert!(first.layers.len() > 1 && one_image(&first), "{:?}", first.layers);
    let md = *s.md_docs.keys().next().unwrap();
    let in_flight = |s: &Session, strip| {
        let page = s.md_docs[&md].pages[0].id;
        s.md_docs[&md].strips.is_in_flight(temzdf_mdview::StripKey::strip(page, strip))
    };
    let top_strip = s.md_graphics(s.focus).unwrap().0.scroll() as u32 / 256;
    assert!(in_flight(&s, top_strip) && in_flight(&s, top_strip + 3), "in sight and below");
    assert!(in_flight(&s, top_strip - 2), "above");
    assert!(!in_flight(&s, top_strip + 8), "not two screens away");
    // Strips cover the pane (90x29 cells of 10x20 pixels).
    let frame = painted(&mut s);
    let covered: u32 = frame.layers.iter().map(|l| l.dst.h).sum();
    assert!(covered >= 29 * 20 && frame.layers.iter().all(|l| l.dst.w == 900), "{:?}", frame.layers);
    assert!(!one_image(&frame), "painted: {:?}", frame.layers);
    let top = |s: &mut Session| s.md_top_line(s.focus).unwrap();
    let section = |n: usize| 2 + 4 * n;
    assert_eq!(top(&mut s), section(5), "at the same heading");
    // Scrolling moves the strips; the top and bottom are the page's ends.
    let ys = |s: &mut Session| painted(s).layers.iter().map(|l| (l.key, l.dst.y)).collect::<Vec<_>>();
    let settle = |s: &mut Session| {
        s.tick(Instant::now() + Duration::from_secs(5));
        assert!(!s.is_animating());
    };
    let before = ys(&mut s);
    run(&mut s, "markdown.scroll", &[ScriptValue::Num(2.0)]).unwrap();
    assert!(s.is_animating(), "it eases there");
    settle(&mut s);
    let after = ys(&mut s);
    // (The first strip is clipped at the pane's top: the last one.)
    let (key, y) = *before.last().unwrap();
    assert_eq!(after.iter().find(|l| l.0 == key).map(|l| l.1), Some(y - 40), "{before:?} {after:?}");
    run(&mut s, "markdown.top", &[]).unwrap();
    settle(&mut s);
    assert_eq!(top(&mut s), 0);
    // The wheel: steps of o.scrollstep (set before the pane was), eased;
    // Ctrl zooms in place.
    let wheel = |s: &mut Session, dir, modifiers| {
        let kind = MouseEventKind::Scroll(dir);
        s.mouse(&MouseEvent { kind, x: 300, y: 300, unit: CoordUnit::Pixel, modifiers }, Instant::now());
    };
    wheel(&mut s, termpix_core::input::ScrollDirection::Down, Modifiers::NONE);
    assert!(s.is_animating());
    settle(&mut s);
    assert_eq!(s.md_graphics(s.focus).unwrap().0.scroll(), 100.0);
    // A new height keeps the place (not the top of the block there).
    s.resize(Geometry { rows: GEOMETRY.rows - 3, ..GEOMETRY });
    let _ = painted(&mut s);
    assert_eq!(s.md_graphics(s.focus).unwrap().0.scroll(), 100.0);
    s.resize(GEOMETRY);
    let _ = painted(&mut s);
    assert_eq!(s.md_graphics(s.focus).unwrap().0.scroll(), 100.0);
    // And follow the option (back up: a step the same way this soon
    // would be a flick's, larger).
    s.options.scrollstep = 40.0;
    s.apply_options();
    wheel(&mut s, termpix_core::input::ScrollDirection::Up, Modifiers::NONE);
    settle(&mut s);
    assert_eq!(s.md_graphics(s.focus).unwrap().0.scroll(), 60.0);
    run(&mut s, "markdown.goto", &[str("section-5")]).unwrap();
    wheel(&mut s, termpix_core::input::ScrollDirection::Up, Modifiers::CTRL);
    assert!(!s.is_animating(), "a zoom does not scroll");
    assert!((s.md_docs[&md].pages[0].zoom - 1.1).abs() < 1e-3);
    assert_eq!(top(&mut s), section(5), "zoomed in place");
    run(&mut s, "markdown.zoom", &[str("reset")]).unwrap();
    run(&mut s, "markdown.top", &[]).unwrap();
    settle(&mut s);
    // A click on the link follows it.
    let (x, y) = {
        let (p, page) = s.md_graphics(s.focus).unwrap();
        let r = page.find("jump")[0];
        (((r.x0 + r.x1) / 2.0) as i32, ((r.y0 + r.y1) / 2.0 - p.scroll()) as i32)
    };
    for kind in [MouseEventKind::Press(MouseButton::Left), MouseEventKind::Release(MouseButton::Left)] {
        s.mouse(&MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE }, Instant::now());
    }
    assert_eq!(top(&mut s), section(12), "followed");
    // Zoom: laid out again at the same place, one page kept; the old
    // page's strips go, and one painted for it arriving late is dropped.
    painted(&mut s);
    let old = s.md_docs[&md].pages[0].id;
    run(&mut s, "markdown.zoom", &[str("in")]).unwrap();
    assert_eq!(top(&mut s), section(12));
    assert_eq!(s.md_docs[&md].pages.len(), 1);
    painted(&mut s);
    assert!(s.md_docs[&md].strips.iter_used().all(|(k, _)| k.page != old), "the old page's strips went");
    let n = s.md_docs[&md].strips.len();
    let key = temzdf_mdview::StripKey::strip(old, 0);
    let late = temzdf_mdview::Strip { key, tag: md, width: 900, rows: 1, pixels: vec![0; 900 * 4] };
    assert_eq!(s.handle(SessionEvent::MarkdownStrip(late), Instant::now()), Outcome::None);
    assert_eq!(s.md_docs[&md].strips.len(), n);
    assert!((s.md_docs[&md].pages[0].zoom - 1.1).abs() < 1e-3);
    // Search: the matches marked, n moves on.
    run(&mut s, "search-for", &[str("Section 1")]).unwrap();
    let marks = s.frame().layers.iter().filter(|l| l.z == 1).count();
    assert!(marks >= 1, "marked");
    assert!(top(&mut s) >= section(12), "from where it was");
    assert_eq!(run(&mut s, "markdown.search-next", &[]).unwrap().0, ScriptValue::Bool(true));
    // Laid out again (a zoom): the marks are on the new page's words.
    run(&mut s, "markdown.zoom", &[str("in")]).unwrap();
    let frame = s.frame();
    let scroll = s.md_graphics(s.focus).unwrap().0.scroll();
    let hits = s.md_docs[&md].pages[0].find("Section 1");
    let marks: Vec<_> = frame.layers.iter().filter(|l| l.z == 1).collect();
    assert!(!marks.is_empty());
    for l in marks {
        let on_a_hit = hits.iter().any(|r| (r.x0.floor() as i32, (r.y0 - scroll).floor() as i32) == (l.dst.x, l.dst.y));
        assert!(on_a_hit, "a mark at {:?}, off the words: {hits:?}", l.dst);
    }
    run(&mut s, "search-cancel", &[]).unwrap();
    assert_eq!(s.frame().layers.iter().filter(|l| l.z == 1).count(), 0);
    // Back to text, at the same place.
    let line = top(&mut s);
    run(&mut s, "markdown.set-mode", &[str("text")]).unwrap();
    assert_eq!(run(&mut s, "markdown.mode", &[]).unwrap().0, str("text"));
    assert_eq!(top(&mut s), line);
    assert!(s.md_docs[&md].pages.is_empty(), "the page went with the mode");
    assert!(run(&mut s, "markdown.set-mode", &[str("other")]).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system, Blitz")]
fn the_markdown_graphics_mode_selects_and_copies_with_the_mouse() {
    let mut src = String::from("First paragraph alpha.\n\nSecond paragraph beta.\n\n[jump](#section-3)\n\n");
    for i in 0..20 {
        src.push_str(&format!("## Section {i}\n\n{}\n\n", "word ".repeat(30)));
    }
    let (mut s, path) = markdown_session("sel.md", &src, &[]);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    run(&mut s, "markdown.set-mode", &[ScriptValue::Str("graphics".into())]).unwrap();
    let left = MouseButton::Left;
    let at = |kind, (x, y): (i32, i32)| MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
    // Where a word's box starts or ends, in the pane.
    let spot = |s: &mut Session, word: &str, end: bool| {
        let (p, page) = s.md_graphics(s.focus).unwrap();
        let r = page.find(word)[0];
        let x = if end { r.x1 - 1.0 } else { r.x0 + 1.0 };
        (x as i32, ((r.y0 + r.y1) / 2.0 - p.scroll()) as i32)
    };
    let boxes = |s: &mut Session| s.frame().layers.iter().filter(|l| l.z == 1).count();
    // A drag from "alpha" to the end of "beta": selected, and copied
    // when released, a line a block.
    let (a, b) = (spot(&mut s, "alpha", false), spot(&mut s, "beta", true));
    s.mouse(&at(MouseEventKind::Press(left), a), Instant::now());
    s.mouse(&at(MouseEventKind::Drag(left), b), Instant::now());
    assert!(boxes(&mut s) >= 2, "the selection is shown");
    assert!(copied(&mut s).is_empty(), "not copied before the release");
    s.mouse(&at(MouseEventKind::Release(left), b), Instant::now());
    assert_eq!(copied(&mut s), ["alpha.\nSecond paragraph beta"]);
    let newest = match run(&mut s, "history", &[]).unwrap().0 {
        ScriptValue::List(l) => l[0].clone(),
        v => panic!("{v:?}"),
    };
    assert_eq!(newest.get("source").and_then(|x| x.get("kind")), Some(&ScriptValue::Str("markdown".into())));
    assert_eq!(newest.get("kind"), Some(&ScriptValue::Str("char".into())));
    assert!(s.notices.iter().any(|n| n.text == "Copied 28 characters"), "said so");
    // `y` copies it again; `<Esc>` forgets it.
    assert_eq!(run(&mut s, "markdown.copy", &[]).unwrap().0, ScriptValue::Bool(true));
    assert_eq!(copied(&mut s), ["alpha.\nSecond paragraph beta"]);
    assert_eq!(run(&mut s, "markdown.clear-selection", &[]).unwrap().0, ScriptValue::Bool(true));
    assert_eq!(boxes(&mut s), 0);
    assert_eq!(run(&mut s, "markdown.copy", &[]).unwrap().0, ScriptValue::Bool(false));
    assert_eq!(run(&mut s, "markdown.clear-selection", &[]).unwrap().0, ScriptValue::Bool(false));
    // A click on text selects nothing, copies nothing.
    s.mouse(&at(MouseEventKind::Press(left), a), Instant::now());
    s.mouse(&at(MouseEventKind::Release(left), a), Instant::now());
    assert!(copied(&mut s).is_empty());
    assert_eq!(boxes(&mut s), 0);
    // A drag past the bottom edge scrolls on by itself, selecting on.
    let t0 = Instant::now();
    s.mouse(&at(MouseEventKind::Press(left), a), t0);
    let bottom = (300, s.md_graphics(s.focus).unwrap().0.height as i32 - 2);
    s.mouse(&at(MouseEventKind::Drag(left), bottom), t0);
    assert!(s.is_animating());
    let before = s.md_graphics(s.focus).unwrap().0.scroll();
    for i in 1..=30 {
        s.tick(t0 + Duration::from_millis(16 * i));
    }
    assert!(s.md_graphics(s.focus).unwrap().0.scroll() > before + 50.0, "scrolled");
    s.mouse(&at(MouseEventKind::Release(left), bottom), t0 + Duration::from_millis(500));
    assert!(!s.is_animating(), "it stops with the release");
    let long = copied(&mut s);
    assert!(long.len() == 1 && long[0].starts_with("alpha.\n") && long[0].contains("Section 0"), "{long:?}");
    // A click on a link still follows it, selecting nothing.
    {
        let (p, page) = s.md_graphics(s.focus).unwrap();
        p.scroll_to(page, 0.0);
    }
    let link = spot(&mut s, "jump", false);
    s.mouse(&at(MouseEventKind::Press(left), link), Instant::now());
    s.mouse(&at(MouseEventKind::Release(left), link), Instant::now());
    assert!(copied(&mut s).is_empty());
    assert!(s.md_graphics(s.focus).unwrap().0.scroll() > 0.0, "followed");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

fn text_of(s: &mut Session) -> String {
    s.run_command("text.get-text", &[], Instant::now()).unwrap().0.as_str().unwrap().to_owned()
}

fn cursor_of(s: &mut Session) -> (usize, usize) {
    let (c, _) = s.run_command("text.cursor", &[], Instant::now()).unwrap();
    let n = |k| c.get(k).unwrap().as_num().unwrap() as usize;
    (n("line"), n("character"))
}

fn pos(line: usize, character: usize) -> ScriptValue {
    ScriptValue::map([("line", ScriptValue::Num(line as f64)), ("character", ScriptValue::Num(character as f64))])
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_text_file_opens_takes_typing_and_is_written_back() {
    let (mut s, path) = text_session("notes.txt", "one\r\ntwo\r\n");
    assert_eq!(s.panes[&s.focus].mode_kind(), "edit");
    assert_eq!(s.history.current(), "edit", "the pane's mode");
    assert_eq!(text_of(&mut s), "one\ntwo\n");
    key(&mut s, "hi");
    assert_eq!(text_of(&mut s), "hione\ntwo\n");
    assert_eq!(cursor_of(&mut s), (0, 2));
    // Moves; typing goes on from there.
    s.run_command("text.move", &[ScriptValue::Str("line".into())], Instant::now()).unwrap();
    assert_eq!(cursor_of(&mut s), (1, 2));
    key(&mut s, "!");
    assert_eq!(text_of(&mut s), "hione\ntw!o\n");
    // Each typing run is one step; undo puts the cursor back.
    cmd(&mut s, "text.undo", &[]).unwrap();
    assert_eq!((text_of(&mut s).as_str(), cursor_of(&mut s)), ("hione\ntwo\n", (1, 2)));
    cmd(&mut s, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut s), "one\ntwo\n");
    cmd(&mut s, "text.redo", &[]).unwrap();
    cmd(&mut s, "write", &[]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hione\r\ntwo\r\n", "CRLF as it was read");
    let (info, _) = s.run_command("text.info", &[], Instant::now()).unwrap();
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn panes_on_one_text_buffer_follow_each_others_edits() {
    let (mut s, path) = text_session("shared.txt", "abc\n");
    let first = s.focus;
    s.run_command("text.set-cursor", &[pos(0, 3)], Instant::now()).unwrap();
    let (second, _) = s.run_command("split", &[], Instant::now()).unwrap();
    let second = second.as_num().unwrap() as PaneId;
    assert_eq!(s.focus, second);
    assert_eq!(cursor_of(&mut s), (0, 3), "the split starts where the other pane was");
    let at0 = ScriptValue::map([("pane", ScriptValue::Num(first as f64))]);
    s.run_command("text.insert", &[ScriptValue::Str(">> ".into()), pos(0, 0), at0], Instant::now()).unwrap();
    assert_eq!(cursor_of(&mut s), (0, 6), "moved along by the other pane's edit");
    // The selection is replaced by typing.
    s.run_command("text.set-selection", &[pos(0, 3), pos(0, 6)], Instant::now()).unwrap();
    key(&mut s, "X");
    assert_eq!(text_of(&mut s), ">> X\n");
    assert_eq!(cursor_of(&mut s), (0, 4), "after the replacement");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn text_edit_takes_lsp_text_edits() {
    let (mut s, path) = text_session("lsp.txt", "let a = 1;\n");
    let range = |a: ScriptValue, b: ScriptValue| ScriptValue::map([("start", a), ("end", b)]);
    let edit = |r: ScriptValue, t: &str| ScriptValue::map([("range", r), ("newText", ScriptValue::Str(t.into()))]);
    // As a formatter's: two insertions at one point, in the array's order.
    let edits = ScriptValue::List(vec![
        edit(range(pos(0, 4), pos(0, 5)), "b"),
        edit(range(pos(0, 0), pos(0, 0)), "// x\n"),
        edit(range(pos(0, 0), pos(0, 0)), "// y\n"),
    ]);
    s.run_command("text.edit", &[edits], Instant::now()).unwrap();
    assert_eq!(text_of(&mut s), "// x\n// y\nlet b = 1;\n");
    let old = ScriptValue::List(vec![ScriptValue::map([
        ("range", range(pos(0, 0), pos(0, 1))),
        ("text", ScriptValue::Str("z".into())),
    ])]);
    let err = s.run_command("text.edit", &[old], Instant::now()).unwrap_err();
    assert!(err.contains("newText"), "{err}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_text_pane_draws_its_text_cursor_and_status_line() {
    let (mut s, path) = text_session("draw.txt", "hello\nあい\n");
    s.run_command("text.set-cursor", &[pos(1, 1)], Instant::now()).unwrap();
    let frame = s.frame();
    let row = |y: usize| -> String {
        let g = frame.text.iter().find(|g| g.row as usize <= y && y < g.row as usize + g.rows.len()).unwrap();
        g.rows[y - g.row as usize]
            .iter()
            .map(|c| if c.text.is_empty() && c.width != 0 { " " } else { &c.text })
            .collect()
    };
    assert!(row(0).starts_with("hello "), "{:?}", row(0));
    assert!(row(1).starts_with("あい "), "{:?}", row(1));
    assert!(row(2).trim().is_empty(), "the empty line after the last line break");
    assert!(row(3).starts_with('~'), "past the text");
    assert_eq!(frame.cursor, Some((2, 1)), "after あ");
    assert_eq!(frame.title, "draw.txt");
    key(&mut s, "x");
    let bar = Session::text_status(
        s.texts.values().next(),
        match &s.panes[&s.focus].kind {
            Kind::Text { view, .. } => view,
            _ => unreachable!(),
        },
        "edit",
    );
    assert_eq!(bar.left, "draw.txt [+]");
    assert!(bar.right.contains("2:3"), "{}", bar.right);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn text_files_are_refused_when_binary_and_new_when_missing() {
    let (mut s, path) = text_session("a.txt", "x");
    let dir = path.parent().unwrap().to_owned();
    std::fs::write(dir.join("bin.dat"), b"\0\0").unwrap();
    let e = s.run_command("open-file", &[ScriptValue::Str(dir.join("bin.dat").display().to_string())], Instant::now());
    assert!(e.unwrap_err().contains("binary"));
    // A file that does not exist yet opens empty (new-pane FILE).
    s.new_pane(&PaneSpec::File(dir.join("new.txt")), None).unwrap();
    key(&mut s, "made");
    cmd(&mut s, "write", &[]).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "made");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn base_modes_come_from_the_buffer_then_the_kind() {
    let (mut s, path) = text_session("base.txt", "x");
    s.modes.define("kindmode");
    s.modes.define("bufmode");
    s.options.basemode.insert("edit".into(), "kindmode".into());
    let first = s.focus;
    // A buffer made with its own base mode, in a split.
    let o = ScriptValue::map([("basemode", ScriptValue::Str("bufmode".into()))]);
    let (buf, _) = s.run_command("new-buffer", &[o], Instant::now()).unwrap();
    let o = ScriptValue::map([("buffer", buf)]);
    s.run_command("split", &[o], Instant::now()).unwrap();
    assert_eq!(s.history.current(), "bufmode");
    s.set_focus(first);
    assert_eq!(s.history.current(), "kindmode");
    // A mode nobody defined: the kind's own.
    s.options.basemode.insert("edit".into(), "nowhere".into());
    let (second, _) = s.run_command("split", &[], Instant::now()).unwrap();
    assert_eq!(s.focus, second.as_num().unwrap() as PaneId);
    assert_eq!(s.history.current(), "edit");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_text_shown_in_a_pane_shows_the_cursor_it_takes_from_another() {
    let content: String = (0..200).map(|i| format!("line{i}\n")).collect();
    let (mut s, path) = text_session("far.txt", &content);
    let far = s.pane_buffer(s.focus).unwrap();
    s.run_command("text.set-cursor", &[pos(150, 0)], Instant::now()).unwrap();
    let other = path.with_file_name("other.txt");
    s.open_text(&other, s.next_spot(), true).unwrap();
    // The pane starts at the cursor of the other pane on the text,
    // revealed (as a split of it does).
    assert_eq!(s.show_in(s.focus, far), Ok(Outcome::Redraw));
    let Kind::Text { view, .. } = &s.panes[&s.focus].kind else { panic!("a text pane") };
    assert!(view.view.top > 0, "the view scrolled to line 150");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn text_panes_search_and_paint_patterns() {
    let (mut s, path) = text_session("search.txt", "one two\ntwo three two\n");
    let o = |pairs: Vec<(&str, ScriptValue)>| ScriptValue::map(pairs);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let start = |v: &ScriptValue| {
        let p = v.get("range").unwrap().get("start").unwrap();
        (p.get("line").unwrap().as_num().unwrap() as usize, p.get("character").unwrap().as_num().unwrap() as usize)
    };
    let two = ScriptValue::Str("two".into());
    assert_eq!(start(&run(&mut s, "text.search", std::slice::from_ref(&two))), (0, 4), "from the cursor");
    let back = o(vec![("from", pos(1, 0)), ("backward", ScriptValue::Bool(true))]);
    assert_eq!(start(&run(&mut s, "text.search", &[two.clone(), back])), (0, 4));
    let lines = o(vec![("lines", ScriptValue::List(vec![ScriptValue::Num(1.0), ScriptValue::Num(1.0)]))]);
    assert_eq!(start(&run(&mut s, "text.search", &[two.clone(), lines])), (1, 0));
    let ScriptValue::List(all) = run(&mut s, "text.search-all", std::slice::from_ref(&two)) else { panic!() };
    assert_eq!(all.len(), 3);
    assert_eq!(run(&mut s, "text.search", &[ScriptValue::Str("nothing".into())]), ScriptValue::Null);
    assert!(s.run_command("text.search", &[ScriptValue::Str("(".into())], Instant::now()).is_err());
    // hlsearch: the matches painted in Search's colours.
    let hl = temzdf_texteditor::Style { bg: Some(temzdf_scene::Color::Indexed(3)), ..Default::default() };
    s.highlights.set("Search", hl);
    run(&mut s, "highlight-pattern", &[ScriptValue::Str("hl".into()), two]);
    let frame = s.frame();
    let row = &frame.text[0].rows[0];
    assert_eq!(
        (row[3].bg, row[4].bg, row[6].bg, row[7].bg),
        (temzdf_scene::Color::Default, hl.bg.unwrap(), hl.bg.unwrap(), temzdf_scene::Color::Default)
    );
    run(&mut s, "highlight-pattern", &[ScriptValue::Str("hl".into()), ScriptValue::Null]);
    assert_eq!(s.frame().text[0].rows[0][4].bg, temzdf_scene::Color::Default, "removed");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn scripts_decorate_text_and_define_highlight_groups() {
    let (mut s, path) = text_session("deco.txt", "keep gone keep\n");
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let range = |a: usize, b: usize| ScriptValue::map([("start", pos(0, a)), ("end", pos(0, b))]);
    let o = |pairs: Vec<(&str, ScriptValue)>| ScriptValue::map(pairs);
    let red = o(vec![("bg", ScriptValue::Str("#ff0000".into())), ("bold", ScriptValue::Bool(true))]);
    run(&mut s, "highlight", &[ScriptValue::Str("Red".into()), red]);
    let style = o(vec![("style", ScriptValue::Str("Red".into())), ("namespace", ScriptValue::Str("ns".into()))]);
    let id = run(&mut s, "text.add-decoration", &[range(5, 9), style]);
    let cell = s.frame().text[0].rows[0][5].clone();
    assert_eq!((cell.bg, cell.attrs.bold), (temzdf_scene::Color::Rgb([255, 0, 0]), true));
    // Its text deleted: flagged, and undo restores it.
    run(&mut s, "text.delete", &[range(5, 10)]);
    let d = run(&mut s, "text.decoration", std::slice::from_ref(&id));
    assert_eq!(d.get("deleted"), Some(&ScriptValue::Bool(true)));
    run(&mut s, "text.undo", &[]);
    let d = run(&mut s, "text.decoration", std::slice::from_ref(&id));
    assert_eq!((d.get("deleted"), d.get("range")), (Some(&ScriptValue::Bool(false)), Some(&range(5, 9))));
    let ScriptValue::List(list) =
        run(&mut s, "text.decorations", &[o(vec![("namespace", ScriptValue::Str("ns".into()))])])
    else {
        panic!()
    };
    assert_eq!(list.len(), 1);
    run(&mut s, "text.clear-namespace", &[ScriptValue::Str("ns".into())]);
    assert_eq!(run(&mut s, "text.decoration", &[id]), ScriptValue::Null);
    // A flash: gone once its time is up.
    let flash = o(vec![("style", ScriptValue::Str("Red".into())), ("ttl", ScriptValue::Num(1.0))]);
    run(&mut s, "text.add-decoration", &[range(0, 4), flash]);
    let due = s.next_deadline().unwrap();
    assert_eq!(s.poll(due + Duration::from_millis(1)), Outcome::Redraw);
    assert_eq!(s.frame().text[0].rows[0][0].bg, temzdf_scene::Color::Default);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn swap_files_follow_the_buffer_and_a_gone_sessions_is_recovered() {
    let (s0, path) = text_session("swap.txt", "one\n");
    drop(s0);
    let dir = path.parent().unwrap().to_owned();
    let swaps = dir.join("state-swap");
    // A session `name` on `path`, swap files in `swaps`, SwapExists heard
    // (`listen`) or not.
    let session = |name: &str, listen: bool| {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new(name.into(), GEOMETRY, Hints::default(), settings, sink);
        s.swap_dir = Some(swaps.clone());
        if listen {
            s.listen(&["SwapExists"]);
        }
        s.start(PaneSpec::File(path.clone()), None).unwrap();
        s
    };
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let canonical = std::fs::canonicalize(&path).unwrap();
    let names = swap::candidates(&swaps, &canonical);
    let swapped = |p: &std::path::Path| swap::read(p).unwrap();
    // Opened: its swap file, written as edits settle (o.updatetime).
    let mut a = session("first", true);
    assert_eq!((swapped(&names[0]).text.as_str(), swapped(&names[0]).modified), ("one\n", false));
    run(&mut a, "text.insert", &[str("x"), pos(0, 0)]).unwrap();
    assert_eq!(swapped(&names[0]).text, "one\n", "not before o.updatetime");
    assert_eq!(a.swap_deadline(), a.next_deadline(), "its deadline is the session's");
    a.poll(Instant::now() + a.options.updatetime + Duration::from_millis(1));
    assert_eq!((swapped(&names[0]).text.as_str(), swapped(&names[0]).modified), ("xone\n", true));
    run(&mut a, "write", &[]).unwrap();
    assert!(!swapped(&names[0]).modified, "written with the file");
    // (In the same invocation as the write: still a change of its own.)
    run(&mut a, "text.insert", &[str("y"), pos(0, 0)]).unwrap();
    a.poll(Instant::now() + a.options.updatetime + Duration::from_millis(1));
    // `first` is gone (as after a crash: its swap file stays). Another
    // session opening the file hears of it and waits.
    // (Starting runs the events: SwapExists said what swapInfo says.)
    let mut b = session("second", true);
    let info = run(&mut b, "swap-info", &[]).unwrap().0;
    assert_eq!(info.get("session"), Some(&str("first")));
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(true)));
    assert!(!names[1].exists(), "no swap file of its own until answered");
    // Recover: the text that was not saved, as one change.
    run(&mut b, "text.recover", &[]).unwrap();
    assert_eq!(text_of(&mut b), "yxone\n");
    assert_eq!(run(&mut b, "text.info", &[]).unwrap().0.get("modified"), Some(&ScriptValue::Bool(true)));
    run(&mut b, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut b), "xone\n");
    // Its own swap file next to that one; the old one deleted.
    let own = run(&mut b, "use-swap", &[]).unwrap().0;
    assert_eq!(own, str(names[1].to_str().unwrap()));
    run(&mut b, "delete-swap", &[str(names[0].to_str().unwrap())]).unwrap();
    assert!(!names[0].exists());
    assert_eq!(run(&mut b, "swap-info", &[]).unwrap().0, ScriptValue::Null);
    assert!(run(&mut b, "delete-swap", &[str("/etc/passwd")]).is_err(), "only swap files");
    // A normal end: its swap files go.
    b.end_swaps();
    assert!(!names[1].exists());
    // Nobody hearing SwapExists: edited anyway, with its own swap file.
    swap::write(&names[0], &swapped_of("first", "lost\n")).unwrap();
    let mut c = session("third", false);
    assert!(names[1].exists());
    assert!(c.notices.iter().any(|n| n.text.contains("found the swap file")));
    c.end_swaps();
    // o.swapfile off: none.
    std::fs::remove_file(&names[0]).unwrap();
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut d = Session::new("fourth".into(), GEOMETRY, Hints::default(), settings, sink);
    d.swap_dir = Some(swaps.clone());
    d.options.swapfile = false;
    d.start(PaneSpec::File(path.clone()), None).unwrap();
    assert!(!names[0].exists());
    // Read-only: written with force only.
    let attrs = ScriptValue::List(vec![str("readonly")]);
    let buf = run(&mut d, "text.info", &[]).unwrap().0.get("buffer").cloned().unwrap();
    run(&mut d, "set-buffer-attrs", &[buf, attrs]).unwrap();
    assert!(run(&mut d, "write", &[]).unwrap_err().contains("read-only"));
    run(&mut d, "write", &[ScriptValue::map([("force", ScriptValue::Bool(true))])]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn options_choose_where_undo_and_swap_files_go() {
    let (s0, path) = text_session("dirs.txt", "one\n");
    drop(s0);
    let dir = path.parent().unwrap().to_owned();
    let str = |v: &str| ScriptValue::Str(v.into());
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("dirs".into(), GEOMETRY, Hints::default(), settings, sink);
    let (undo, swaps) = (dir.join("my-undo"), dir.join("my-swap"));
    s.set_option("undodir", &str(undo.to_str().unwrap())).unwrap();
    s.set_option("directory", &str(swaps.to_str().unwrap())).unwrap();
    s.set_option("undofile", &ScriptValue::Bool(true)).unwrap();
    assert_eq!(s.get_option("directory").unwrap(), str(swaps.to_str().unwrap()));
    s.start(PaneSpec::File(path.clone()), None).unwrap();
    let canonical = std::fs::canonicalize(&path).unwrap();
    assert!(swap::candidates(&swaps, &canonical)[0].exists(), "the swap file there");
    s.run_command("text.insert", &[str("x"), pos(0, 0)], Instant::now()).unwrap();
    s.run_command("write", &[], Instant::now()).unwrap();
    assert!(text::undo_file_of(&undo, &canonical).exists(), "the undo file there");
    s.end_swaps();
    // "~/" is the home directory; null or "" the default.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(text::dir_option(Some("~/u".into()), text::undo_dir).unwrap(), Some(home.join("u")));
    assert_eq!(text::dir_option(None, text::undo_dir).unwrap(), text::undo_dir());
    assert_eq!(text::dir_option(Some(String::new()), swap::swap_dir).unwrap(), swap::swap_dir());
    assert!(text::dir_option(Some("rel".into()), text::undo_dir).unwrap().unwrap().is_absolute());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A swap file of session `session` (gone) holding `text`.
fn swapped_of(session: &str, text: &str) -> swap::SwapFile {
    swap::SwapFile {
        version: 1,
        path: String::new(),
        session: session.into(),
        pid: 1,
        host: String::new(),
        time: 0,
        modified: true,
        encoding: "utf-8".into(),
        line_ending: "lf".into(),
        text: text.into(),
    }
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn undo_trees_are_kept_in_files_when_written_and_read_back() {
    let (mut s, path) = text_session("undo.txt", "one\n");
    let dir = path.parent().unwrap().to_owned();
    let undo_dir = dir.join("state-undo");
    // A session on `path` with undo files in `undo_dir` (o.undofile).
    let session = |undofile: bool| {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
        s.undo_dir = Some(undo_dir.clone());
        s.options.undofile = undofile;
        s.start(PaneSpec::File(path.clone()), None).unwrap();
        s
    };
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    // Off (the default): nothing written.
    run(&mut s, "text.insert", &[str("zero "), pos(0, 0)]).unwrap();
    run(&mut s, "write", &[]).unwrap();
    assert!(!undo_dir.exists());
    // On: written with the file, 0600 in a 0700 directory.
    s.undo_dir = Some(undo_dir.clone());
    s.options.undofile = true;
    // (Commands run here are one invocation: a boundary between the
    // two edits.)
    run(&mut s, "text.undo-boundary", &[]).unwrap();
    run(&mut s, "text.insert", &[str("two "), pos(0, 0)]).unwrap();
    run(&mut s, "write", &[]).unwrap();
    let canonical = std::fs::canonicalize(&path).unwrap();
    let file = text::undo_file_of(&undo_dir, &canonical);
    assert!(file.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&undo_dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    // Opened again: undo goes back through both edits.
    let mut again = session(true);
    assert_eq!(text_of(&mut again), "two zero one\n");
    let info = run(&mut again, "text.info", &[]).unwrap().0;
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
    run(&mut again, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut again), "zero one\n");
    run(&mut again, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut again), "one\n");
    // Without o.undofile it is not read.
    let mut off = session(false);
    run(&mut off, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut off), "two zero one\n");
    // Changed elsewhere: the old tree is not put on other text.
    std::fs::write(&path, "changed\n").unwrap();
    let mut changed = session(true);
    run(&mut changed, "text.undo", &[]).unwrap();
    assert_eq!(text_of(&mut changed), "changed\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn buffer_attributes_nofile_and_unlisted() {
    let (mut s, path) = text_session("attrs.txt", "x\n");
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let names = |s: &mut Session, all: bool| -> Vec<String> {
        let o = ScriptValue::map([("all", ScriptValue::Bool(all))]);
        match run(s, "buffers", &[o]).unwrap().0 {
            ScriptValue::List(l) => l.iter().map(|b| b.get("name").unwrap().as_str().unwrap().to_owned()).collect(),
            _ => Vec::new(),
        }
    };
    let o = ScriptValue::map([("attrs", ScriptValue::List(vec![str("nofile"), str("unlisted")]))]);
    let buf = run(&mut s, "new-buffer", &[o]).unwrap().0;
    assert_eq!(names(&mut s, false), ["attrs.txt"], "unlisted: left out");
    assert_eq!(names(&mut s, true).len(), 2, "all: listed");
    let count = s.buffer_count();
    // nofile: typed into, never modified; written only to a path given.
    let split = ScriptValue::map([("side", str("right")), ("buffer", buf.clone())]);
    run(&mut s, "split", &[split]).unwrap();
    run(&mut s, "text.type", &[str("typed")]).unwrap();
    let info = run(&mut s, "text.info", &[]).unwrap().0;
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
    assert_eq!(info.get("attrs"), Some(&ScriptValue::List(vec![str("nofile"), str("unlisted")])));
    assert!(run(&mut s, "write", &[]).is_err());
    // Changed: listed and modified like any.
    run(&mut s, "set-buffer-attrs", &[buf.clone(), ScriptValue::List(vec![])]).unwrap();
    assert_eq!(names(&mut s, false).len(), 2);
    assert_eq!(s.buffer_count(), count + 1);
    let info = run(&mut s, "text.info", &[]).unwrap().0;
    assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(true)));
    assert!(run(&mut s, "set-buffer-attrs", &[buf, ScriptValue::List(vec![str("bogus")])]).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Write a `w` x `h` PNG of one colour at `path`.
fn write_png(path: &std::path::Path, w: u32, h: u32) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().unwrap();
    writer.write_image_data(&vec![200; (w * h * 4) as usize]).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn inline_images_are_layers_over_holes_in_the_text() {
    let (mut s, path) = text_session("img.txt", "abc\n");
    let dir = path.parent().unwrap().to_owned();
    let png = dir.join("pic.png");
    // 10x20-pixel cells (GEOMETRY): 40x20 is four columns one row high.
    write_png(&png, 40, 20);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
    let at = ScriptValue::map([("start", pos(0, 1)), ("end", pos(0, 1))]);
    let o = ScriptValue::map([
        ("virtImage", ScriptValue::map([("image", id.clone()), ("alt", str("pic"))])),
        ("virtTextPos", str("inline")),
    ]);
    run(&mut s, "text.add-decoration", &[at, o]).unwrap();
    let frame = s.frame();
    assert_eq!(frame.layers.len(), 1, "{:?}", frame.layers);
    let l = frame.layers[0];
    assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (10, 0, 40, 20), "after the a, its pixels");
    assert_eq!(s.frame().cursor, Some((0, 0)));
    // A hole in the text there; the alt text for frontends without images.
    let row = screen_text(&frame, GEOMETRY)[0].clone();
    assert!(row.starts_with("a    bc"), "{row:?}");
    let fallback = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
    assert!(fallback[0].starts_with(" pic"), "{:?}", fallback[0]);
    // Read again at another size: the same id, redrawn.
    write_png(&png, 20, 20);
    assert_eq!(run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0, id);
    assert_eq!(s.frame().layers[0].dst.w, 20);
    // Unloaded: its alt text in the text, no layer.
    run(&mut s, "unload-image", &[id]).unwrap();
    let frame = s.frame();
    assert!(frame.layers.is_empty());
    assert!(screen_text(&frame, GEOMETRY)[0].starts_with("apicbc"));
    assert!(run(&mut s, "load-image", &[str(path.to_str().unwrap())]).unwrap_err().contains("not an image"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn concealed_ranges_show_text_or_an_image_off_the_cursor_line() {
    let (mut s, path) = text_session("conceal.txt", "a\\alpha b\n\\pic.\nx\n");
    let dir = path.parent().unwrap().to_owned();
    let png = dir.join("pic.png");
    write_png(&png, 20, 20);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let range = |l0, c0, l1, c1| ScriptValue::map([("start", pos(l0, c0)), ("end", pos(l1, c1))]);
    let rows = |s: &mut Session| screen_text(&s.frame(), GEOMETRY);
    let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
    let alpha = ScriptValue::map([("conceal", ScriptValue::map([("text", str("α"))]))]);
    run(&mut s, "text.add-decoration", &[range(0, 1, 0, 7), alpha]).unwrap();
    let image = ScriptValue::map([(
        "conceal",
        ScriptValue::map([
            ("image", ScriptValue::map([("image", id.clone()), ("alt", str("P"))])),
            ("cursorLine", ScriptValue::Bool(true)),
        ]),
    )]);
    run(&mut s, "text.add-decoration", &[range(1, 0, 1, 4), image]).unwrap();
    // The cursor's line as it is; the image hidden there too.
    let r = rows(&mut s);
    assert!(r[0].starts_with("a\\alpha b") && r[1].starts_with("  ."), "{r:?}");
    let l = s.frame().layers[0];
    assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (0, 20, 20, 20));
    // Elsewhere, concealed; the cursor in a range at its replacement.
    run(&mut s, "text.set-cursor", &[pos(2, 0)]).unwrap();
    assert!(rows(&mut s)[0].starts_with("aα b"), "{:?}", rows(&mut s));
    run(&mut s, "text.set-cursor", &[pos(1, 2)]).unwrap();
    assert_eq!(s.frame().cursor, Some((0, 1)));
    // Its image unloaded: the alt text.
    run(&mut s, "unload-image", &[id]).unwrap();
    assert!(s.frame().layers.is_empty());
    assert!(rows(&mut s)[1].starts_with("P."), "{:?}", rows(&mut s));
    // Within one line only.
    let bad = ScriptValue::map([("conceal", ScriptValue::map([("text", str("x"))]))]);
    assert!(run(&mut s, "text.add-decoration", &[range(0, 1, 1, 1), bad]).unwrap_err().contains("one line"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn virtual_lines_are_rows_of_text_and_images_cut_where_scrolled() {
    let (mut s, path) = text_session("vlines.txt", "ab\ncd\n");
    let dir = path.parent().unwrap().to_owned();
    let png = dir.join("pic.png");
    // 10x20-pixel cells: 30x60 pixels are three columns, three rows.
    write_png(&png, 30, 60);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let str = |v: &str| ScriptValue::Str(v.into());
    let at = |l| ScriptValue::map([("start", pos(l, 0)), ("end", pos(l, 0))]);
    let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
    let below = ScriptValue::map([(
        "virtLines",
        ScriptValue::map([
            ("text", ScriptValue::List(vec![str("note")])),
            ("image", ScriptValue::map([("image", id), ("alt", str("P"))])),
        ]),
    )]);
    run(&mut s, "text.add-decoration", &[at(0), below]).unwrap();
    let above = ScriptValue::map([(
        "virtLines",
        ScriptValue::map([
            ("text", ScriptValue::List(vec![ScriptValue::List(vec![str("u"), str("p")])])),
            ("above", ScriptValue::Bool(true)),
        ]),
    )]);
    run(&mut s, "text.add-decoration", &[at(1), above]).unwrap();
    let rows = screen_text(&s.frame(), GEOMETRY);
    assert_eq!(rows[..7].iter().map(|r| r.trim_end()).collect::<Vec<_>>(), ["ab", "note", "", "", "", "up", "cd"]);
    let l = s.frame().layers[0];
    assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (0, 40, 30, 60));
    assert_eq!((l.src.y, l.src.h), (0, 60));
    // Scrolled into the image: its lower two thirds at the top.
    run(&mut s, "text.scroll", &[ScriptValue::Num(3.0)]).unwrap();
    let l = s.frame().layers[0];
    assert_eq!((l.dst.y, l.dst.h, l.src.y, l.src.h), (0, 40, 20, 40));
    let bad = ScriptValue::map([("virtLines", ScriptValue::map([("text", str("x"))]))]);
    assert!(run(&mut s, "text.add-decoration", &[at(0), bad]).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn virtual_text_shows_in_the_pane_and_the_cursor_skips_it() {
    let (mut s, path) = text_session("virt.txt", "abc\n");
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now());
    let at = |a: usize| ScriptValue::map([("start", pos(0, a)), ("end", pos(0, a))]);
    let virt = |text: &str, where_: &str| {
        ScriptValue::map([
            ("virtText", ScriptValue::List(vec![ScriptValue::map([("text", ScriptValue::Str(text.into()))])])),
            ("virtTextPos", ScriptValue::Str(where_.into())),
        ])
    };
    let row = |s: &mut Session| -> String {
        s.frame().text[0].rows[0]
            .iter()
            .filter(|c| c.width > 0)
            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
            .collect::<String>()
    };
    let id = run(&mut s, "text.add-decoration", &[at(0), virt("> ", "inline")]).unwrap().0;
    run(&mut s, "text.add-decoration", &[at(3), virt("note", "eol")]).unwrap();
    run(&mut s, "text.add-decoration", &[at(1), virt("B", "overlay")]).unwrap();
    assert!(row(&mut s).starts_with("> aBc note"), "{:?}", row(&mut s));
    assert_eq!(s.frame().cursor, Some((2, 0)), "after the prompt");
    // Typed at its place: after it (it stays before what is inserted).
    run(&mut s, "text.insert", &[ScriptValue::Str("x".into()), pos(0, 0)]).unwrap();
    assert!(row(&mut s).starts_with("> xaBc note"), "{:?}", row(&mut s));
    let d = run(&mut s, "text.decoration", &[id]).unwrap().0;
    assert_eq!(d.get("virtTextPos"), Some(&ScriptValue::Str("inline".into())));
    let bad = ScriptValue::map([("virtTextPos", ScriptValue::Str("left".into()))]);
    assert!(run(&mut s, "text.add-decoration", &[at(0), bad]).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn encodings_pairs_word_characters_variables_and_the_clipboard() {
    let (mut s, path) = text_session("misc.txt", "f(a, (b))\nfoo-bar\n");
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let str = |v: &str| ScriptValue::Str(v.into());
    // A Shift_JIS file, named.
    let sjis = path.parent().unwrap().join("sjis.txt");
    std::fs::write(&sjis, b"\x82\xa0\n").unwrap();
    assert!(s.run_command("open-file", &[str(sjis.to_str().unwrap())], Instant::now()).is_err());
    let o = ScriptValue::map([("encoding", str("shift_jis"))]);
    run(&mut s, "open-file", &[str(sjis.to_str().unwrap()), o]);
    assert_eq!(text_of(&mut s), "あ\n");
    key(&mut s, "い");
    cmd(&mut s, "write", &[]).unwrap();
    assert_eq!(std::fs::read(&sjis).unwrap(), b"\x82\xa2\x82\xa0\n", "written back in Shift_JIS");
    cmd(&mut s, "close-pane", &[]).unwrap();
    // Brackets.
    let p = run(&mut s, "text.find-pair", &[pos(0, 1)]);
    assert_eq!(p, pos(0, 8));
    let inner = ScriptValue::map([("direction", str("backward"))]);
    assert_eq!(run(&mut s, "text.find-pair", &[pos(0, 7), inner]), pos(0, 5));
    // Word characters per buffer.
    run(&mut s, "text.set-cursor", &[pos(1, 0)]);
    run(&mut s, "text.set-wordchars", &[str(r"[\p{L}\p{N}_-]")]);
    run(&mut s, "text.move", &[str("wordEnd")]);
    assert_eq!(cursor_of(&mut s), (1, 7), "foo-bar is one word now");
    assert!(s.run_command("text.set-wordchars", &[str("[")], Instant::now()).is_err());
    // Variables, kept by the server, gone with their owner.
    run(&mut s, "set-var", &[str("buffer"), str("x"), ScriptValue::Num(3.0)]);
    run(&mut s, "set-var", &[str("pane"), str("y"), str("p")]);
    assert_eq!(run(&mut s, "get-var", &[str("buffer"), str("x")]), ScriptValue::Num(3.0));
    assert_eq!(run(&mut s, "get-var", &[str("pane"), str("y")]), str("p"));
    assert_eq!(run(&mut s, "get-var", &[str("tab"), str("y")]), ScriptValue::Null);
    let (pane, _) = s.run_command("split", &[], Instant::now()).unwrap();
    let pane = pane.as_num().unwrap();
    assert_eq!(run(&mut s, "get-var", &[str("pane"), str("y")]), ScriptValue::Null, "the new pane's own");
    cmd(&mut s, "close-pane", &[]).unwrap();
    assert!(s.run_command("get-var", &[str("pane"), str("y"), ScriptValue::Num(pane)], Instant::now()).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
#[cfg_attr(miri, ignore = "spawns programs (actions opening panes run `true`)")]
fn every_declared_action_has_a_handler() {
    // Each name of temzdf-actions.d.ts, without arguments, in a session
    // without panes: an action's own error is fine, the dispatch's "no
    // action" (nothing ran it) is not.
    let start = Instant::now();
    let mut unhandled = Vec::new();
    for name in commands::action_names() {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("actions".into(), GEOMETRY, Hints::default(), settings, sink);
        s.shell = Some(vec!["true".into()]);
        if let Err(e) = s.run_command(name, &[], Instant::now())
            && e.starts_with(commands::NO_ACTION)
        {
            unhandled.push(format!("{name}: {e}"));
        }
    }
    assert!(unhandled.is_empty(), "declared but not handled: {unhandled:#?}");
    eprintln!("{} actions in {:?}", commands::action_names().len(), start.elapsed());
}

#[test]
fn every_declared_option_reads_and_writes_back() {
    // Each name of `interface Options` (temzdf-core.d.ts): get_option reads
    // it, and set_option takes what it read and leaves it so.
    //
    // Not from its start, by design: `directory` starts undefined in this
    // crate's tests (no swap files), and undefined sets the state
    // directory's (so does it outside tests, where it also starts there).
    // It round-trips from a path, below.
    const NOT_FROM_THE_START: &[&str] = &["directory"];
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("options".into(), GEOMETRY, Hints::default(), settings, sink);
    let mut failed = Vec::new();
    for name in options::OPTIONS.iter().filter(|n| !NOT_FROM_THE_START.contains(n)) {
        let value = match s.get_option(name) {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("get {name}: {e}"));
                continue;
            }
        };
        if let Err(e) = s.set_option(name, &value) {
            failed.push(format!("set {name} to {value:?}: {e}"));
            continue;
        }
        let after = s.get_option(name);
        if after.as_ref() != Ok(&value) {
            failed.push(format!("{name}: {value:?} read back as {after:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
    let dir = ScriptValue::Str(std::env::temp_dir().display().to_string());
    for name in NOT_FROM_THE_START {
        s.set_option(name, &dir).unwrap();
        assert_eq!(s.get_option(name), Ok(dir.clone()), "{name}");
    }
    // Every value of the options naming one of a few, both ways.
    for (name, values) in [
        ("showtabline", &["never", "auto", "always"][..]),
        ("showstatusline", &["never", "always"]),
        ("reloadanchor", &["position", "text"]),
        ("number", &["off", "absolute", "relative", "both"]),
        ("clipboard", &["all", "copies", "none"]),
        ("markdownmode", &["text", "graphics"]),
    ] {
        for v in values {
            let v = ScriptValue::Str((*v).into());
            s.set_option(name, &v).unwrap_or_else(|e| panic!("{name} = {v:?}: {e}"));
            assert_eq!(s.get_option(name), Ok(v), "{name}");
        }
    }
    // A name the declarations do not have is refused both ways.
    assert!(!options::OPTIONS.contains(&"nosuchoption"));
    assert_eq!(s.get_option("nosuchoption"), Err("no option \"nosuchoption\"".into()));
    assert_eq!(s.set_option("nosuchoption", &ScriptValue::Bool(true)), Err("no option \"nosuchoption\"".into()));
}

#[test]
fn command_names_are_unique() {
    // A name twice: the first one found would hide the other (`copy`,
    // the PDF pane's, once was).
    let mut seen = std::collections::HashSet::new();
    for name in commands::action_names() {
        assert!(seen.insert(name), "{name} twice");
    }
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn copies_registers_and_the_clipboard() {
    let (mut s, path) = text_session("copies.txt", "one\ntwo\n");
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let str = |v: &str| ScriptValue::Str(v.into());
    let opts = |kv: &[(&str, ScriptValue)]| ScriptValue::map(kv.iter().cloned());
    let texts = |v: ScriptValue| match v {
        ScriptValue::List(l) => l.iter().map(|e| e.get("text").unwrap().as_str().unwrap().to_owned()).collect(),
        _ => Vec::<String>::new(),
    };
    // Copies and deletes go to the clipboard (o.clipboard "all").
    let e = run(&mut s, "copy-text", &[str("a")]);
    assert_eq!(e.get("kind"), Some(&str("char")));
    assert_eq!(e.get("source").and_then(|x| x.get("kind")), Some(&str("edit")));
    run(&mut s, "copy-text", &[str("gone"), opts(&[("how", str("delete"))])]);
    assert_eq!(copied(&mut s), ["a", "gone"]);
    // Lines end with a line break.
    run(&mut s, "copy-text", &[str("line"), opts(&[("kind", str("line")), ("clipboard", ScriptValue::Bool(false))])]);
    assert_eq!(copied(&mut s), Vec::<String>::new());
    assert_eq!(texts(run(&mut s, "history", &[])), ["line\n", "gone", "a"]);
    // "copies": not deletes.
    s.set_option("clipboard", &str("copies")).unwrap();
    run(&mut s, "copy-text", &[str("cut"), opts(&[("how", str("delete"))])]);
    assert_eq!(copied(&mut s), Vec::<String>::new());
    // Named registers: not to the clipboard; appending lines to text.
    run(&mut s, "copy-text", &[str("r1"), opts(&[("register", str("a"))])]);
    run(
        &mut s,
        "copy-text",
        &[str("r2"), opts(&[("register", str("a")), ("kind", str("line")), ("append", ScriptValue::Bool(true))])],
    );
    assert_eq!(copied(&mut s), Vec::<String>::new());
    let a = run(&mut s, "register", &[str("a")]);
    assert_eq!((a.get("text"), a.get("kind")), (Some(&str("r1\nr2\n")), Some(&str("line"))));
    assert_eq!(run(&mut s, "register", &[str("b")]), ScriptValue::Null);
    // The history keeps o.copyhistory entries.
    s.set_option("copyhistory", &ScriptValue::Num(2.0)).unwrap();
    run(&mut s, "copy-text", &[str("last")]);
    assert_eq!(copied(&mut s), ["last"]);
    assert_eq!(texts(run(&mut s, "history", &[])), ["last", "r1\nr2\n"]);
    assert_eq!(s.cli_get_copy(None, 1), Ok("r1\nr2\n".to_owned()));
    assert_eq!(s.cli_get_copy(Some("a"), 0), Ok("r1\nr2\n".to_owned()));
    assert!(s.cli_get_copy(None, 5).is_err());
    // Putting: characters at the cursor, lines before the cursor's line.
    run(&mut s, "text.set-cursor", &[pos(1, 1)]);
    run(&mut s, "put", &[]);
    assert_eq!(text_of(&mut s), "one\ntlastwo\n");
    run(&mut s, "put", &[opts(&[("register", str("a"))])]);
    assert_eq!(text_of(&mut s), "one\nr1\nr2\ntlastwo\n");
    assert!(s.run_command("put", &[opts(&[("register", str("z"))])], Instant::now()).is_err());
    // Forgetting.
    let ScriptValue::List(history) = run(&mut s, "history", &[]) else { panic!() };
    let id = history[0].get("id").cloned().unwrap();
    run(&mut s, "forget-copy", &[id]);
    assert_eq!(texts(run(&mut s, "history", &[])), ["r1\nr2\n"]);
    run(&mut s, "forget-copy", &[]);
    assert_eq!(texts(run(&mut s, "history", &[])), Vec::<String>::new());
    assert_eq!(run(&mut s, "register", &[str("a")]), ScriptValue::Null);
    // Reading the clipboard asks the client; no answer in time fails it.
    let p = run(&mut s, "read-clipboard", &[opts(&[("primary", ScriptValue::Bool(true))])]);
    let ticket = p.get("$promise").and_then(ScriptValue::as_num).unwrap() as u64;
    assert_eq!(s.take_effects(), [temzdf_scene::Effect::ReadClipboard { ticket, primary: true }]);
    let due = s.next_deadline().unwrap();
    assert!(due > Instant::now() + Duration::from_secs(5));
    s.poll(due + Duration::from_millis(1));
    assert!(s.copies_deadline().is_none(), "given up");
    // A late answer is dropped.
    assert_eq!(s.clipboard_answer(ticket, Ok("late".into())), Outcome::None);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
