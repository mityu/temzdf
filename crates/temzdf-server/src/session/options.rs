//! Session options: the types scripts set with `o.<name> = ...` and the
//! session's `set_option` and `get_option` that convert them.

use std::time::Duration;

use temzdf_pdfview::host::HostOptions;
use temzdf_pdfview::pane::FlashLength;
use temzdf_protocol::script::ScriptValue;
use termpix_core::scroll::ScrollConfig;

use super::*;

/// The options `interface Options` declares in
/// crates/temzdf-script/ts-types/temzdf-core.d.ts (build.rs reads them from
/// there), for the tests that hold `set_option` and `get_option` to them.
#[cfg(test)]
pub(super) const OPTIONS: &[&str] = include!(concat!(env!("OUT_DIR"), "/options.rs"));

/// When the tab line shows (the option `showtabline`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowTabline {
    Never,
    /// With two tab pages or more.
    Auto,
    Always,
}

/// The values of the options that name one of a few: one table each, used
/// both by `set_option` and by `get_option`.
const SHOWTABLINE: &[(&str, ShowTabline)] =
    &[("never", ShowTabline::Never), ("auto", ShowTabline::Auto), ("always", ShowTabline::Always)];
/// `reloadanchor`: whether reloads keep the line being read.
const RELOADANCHOR: &[(&str, bool)] = &[("position", false), ("text", true)];
/// `fileIgnoreCase`'s names (it takes true and false as well).
const FILEIGNORECASE: &[(&str, temzdf_paths::IgnoreCase)] = &[("auto", temzdf_paths::IgnoreCase::Auto)];
/// `showstatusline`: whether the window's status line shows.
const SHOWSTATUSLINE: &[(&str, bool)] = &[("never", false), ("always", true)];

/// The value a table gives a name.
pub(super) fn by_name<T: Copy>(table: &[(&str, T)], name: Option<&str>) -> Option<T> {
    let name = name?;
    table.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// A value's name in its table, as scripts read it.
fn name_in<T: PartialEq>(table: &[(&'static str, T)], value: &T) -> ScriptValue {
    let name = table.iter().find(|(_, v)| v == value).map(|(n, _)| *n).expect("every value is in its table");
    ScriptValue::Str(name.into())
}

/// Options scripts set with `o.<name> = ...` (global for now).
#[derive(Clone, Debug)]
pub struct Options {
    /// How long a key that starts a longer mapping waits (Vim's `timeoutlen`).
    pub timeoutlen: Duration,
    /// Where `split()` without a side puts the new pane: right (else left).
    pub splitright: bool,
    /// Below (else above), for splits towards the bottom.
    pub splitbelow: bool,
    /// How many modes the history keeps.
    pub modehistory: usize,
    /// Keep text buffers' undo trees in files (Vim's undofile): written
    /// with the file, read back when it is opened.
    pub undofile: bool,
    /// Keep text buffers' text in swap files while they are edited.
    pub swapfile: bool,
    /// Whether names of files that differ only in case name the same file,
    /// for files not there to compare (`temzdf_paths`).
    pub fileignorecase: temzdf_paths::IgnoreCase,
    /// The mode Markdown viewers open in: "text" or "graphics".
    pub markdownmode: String,
    /// How long after the last change a swap file is written.
    pub updatetime: Duration,
    /// Pixels per wheel step, and the easing's time constant.
    pub scrollstep: f64,
    pub scrollsmoothness: Duration,
    /// The command opening external links (the URI appended); the system
    /// handler when `None`.
    pub linkopener: Option<Vec<String>>,
    /// Preview internal links after the pointer rests on one this long.
    pub linkpreview: bool,
    pub linkpreviewdelay: Duration,
    /// How long a forward search's highlight stays.
    pub forwardflash: FlashLength,
    /// Wait this long after the PDF changed before reloading.
    pub reloaddebounce: Duration,
    /// Keep the line being read across reloads (else the position).
    pub reloadanchor_text: bool,
    /// The inverse-search command (`%{input}`, `%{line}`, `%{column}`).
    pub synctexeditor: Option<String>,
    /// A shown terminal whose program ended keeps its panes (tmux's
    /// `remain-on-exit`).
    pub remainonexit: bool,
    /// When the tab line shows (Vim's `showtabline`).
    pub showtabline: ShowTabline,
    /// The window's status line shows (its bottom row; Vim's `laststatus`
    /// 3, Neovim's global status line).
    pub showstatusline: bool,
    /// Colours for clients and terminal panes (`None`: the client's own),
    /// and the first palette entries.
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub palette: Vec<[u8; 3]>,
    /// The GUI fonts, first choice first, and those tried first for wide
    /// characters.
    pub guifont: Vec<temzdf_scene::FontEntry>,
    pub guifontwide: Vec<temzdf_scene::FontEntry>,
    /// Text panes: wrapping, tabs, line numbers, the cursor line,
    /// `scrolloff`.
    pub text: TextOptions,
    /// The mode at the bottom of the history in a pane of each kind (by
    /// the kind's name; its own mode when left out). Set only while the
    /// configuration loads.
    pub basemode: std::collections::BTreeMap<String, String>,
    /// How many entries the copy history keeps.
    pub copyhistory: usize,
    /// What goes to the client's clipboard (doc/design/registers.md).
    pub clipboard: registers::ClipboardSync,
}

#[derive(Clone, Debug, Default)]
pub struct TextOptions {
    pub layout: temzdf_texteditor::LayoutOptions,
    pub number: temzdf_texteditor::NumberMode,
    pub cursorline: bool,
    pub scrolloff: usize,
}

impl Options {
    /// The colours terminal panes report (OSC 10, 11, 4): the options', else
    /// what GUI clients draw (`temzdf_scene::theme`), so the two agree.
    pub(super) fn foreground_or_default(&self) -> Option<[u8; 3]> {
        Some(self.foreground.unwrap_or(temzdf_scene::theme::FOREGROUND))
    }

    pub(super) fn background_or_default(&self) -> Option<[u8; 3]> {
        Some(self.background.unwrap_or(temzdf_scene::theme::BACKGROUND))
    }

    pub(super) fn full_palette(&self) -> Vec<[u8; 3]> {
        temzdf_scene::theme::palette(&self.palette)
    }
}

impl Default for Options {
    fn default() -> Self {
        let scroll = ScrollConfig::default();
        Self {
            timeoutlen: Duration::from_millis(1000),
            splitright: true,
            swapfile: true,
            fileignorecase: temzdf_paths::IgnoreCase::Auto,
            markdownmode: "text".into(),
            updatetime: Duration::from_millis(4000),
            undofile: false,
            splitbelow: true,
            modehistory: 100,
            scrollstep: scroll.step,
            scrollsmoothness: scroll.tau,
            linkopener: None,
            linkpreview: true,
            linkpreviewdelay: temzdf_pdfview::pane::DEFAULT_PREVIEW_DELAY,
            forwardflash: temzdf_pdfview::pane::DEFAULT_FLASH,
            reloaddebounce: Duration::from_millis(200),
            reloadanchor_text: false,
            synctexeditor: None,
            remainonexit: false,
            showtabline: ShowTabline::Auto,
            // Off in this crate's tests, which count panes' rows; the window
            // status line's own tests turn it on.
            showstatusline: !cfg!(test),
            foreground: None,
            background: None,
            palette: Vec::new(),
            guifont: vec![temzdf_scene::FontEntry { family: "JetBrains Mono".into(), size: Some(14.0) }],
            guifontwide: Vec::new(),
            text: TextOptions::default(),
            basemode: std::collections::BTreeMap::new(),
            copyhistory: 30,
            clipboard: registers::ClipboardSync::All,
        }
    }
}

impl Options {
    pub(super) fn scroll(&self) -> ScrollConfig {
        ScrollConfig {
            step: self.scrollstep.max(1.0),
            tau: self.scrollsmoothness.max(Duration::from_millis(1)),
            ..Default::default()
        }
    }

    pub(super) fn preview_delay(&self) -> Option<Duration> {
        self.linkpreview.then_some(self.linkpreviewdelay)
    }

    pub(super) fn host(&self) -> HostOptions {
        HostOptions {
            debounce: self.reloaddebounce,
            editor: self.synctexeditor.clone(),
            ignore_case: self.fileignorecase,
        }
    }
}

impl Session {
    pub(super) fn set_option(&mut self, name: &str, value: &ScriptValue) -> Result<(), String> {
        let num = || value.as_num().filter(|n| *n >= 0.0).ok_or(format!("o.{name}: expected a number"));
        let ms = || num().map(|n| Duration::from_millis(n as u64));
        let flag = || match value {
            ScriptValue::Bool(b) => Ok(*b),
            _ => Err(format!("o.{name}: expected true or false")),
        };
        let text = || match value {
            ScriptValue::Null => Ok(None),
            ScriptValue::Str(s) => Ok(Some(s.clone())),
            _ => Err(format!("o.{name}: expected a string or undefined")),
        };
        let o = &mut self.options;
        match name {
            "timeoutlen" => o.timeoutlen = ms()?,
            "splitright" => o.splitright = flag()?,
            "splitbelow" => o.splitbelow = flag()?,
            "modehistory" => o.modehistory = (num()? as usize).max(1),
            "copyhistory" => o.copyhistory = (num()? as usize).max(1),
            "clipboard" => {
                o.clipboard = value
                    .as_str()
                    .and_then(super::registers::ClipboardSync::parse)
                    .ok_or("o.clipboard: expected \"all\", \"copies\" or \"none\"")?
            }
            "scrollstep" => o.scrollstep = num()?,
            "scrollsmoothness" => o.scrollsmoothness = ms()?,
            "linkopener" => {
                o.linkopener = match value {
                    ScriptValue::Null => None,
                    ScriptValue::List(l) => {
                        let words: Option<Vec<String>> = l.iter().map(|w| w.as_str().map(str::to_owned)).collect();
                        Some(words.filter(|w| !w.is_empty()).ok_or("o.linkopener: expected a list of strings")?)
                    }
                    _ => return Err("o.linkopener: expected a list of strings or undefined".into()),
                }
            }
            "linkpreview" => o.linkpreview = flag()?,
            "linkpreviewdelay" => o.linkpreviewdelay = ms()?,
            "forwardflash" => {
                o.forwardflash = match value {
                    ScriptValue::Str(s) if s == "input" => FlashLength::UntilInput,
                    ScriptValue::Num(_) => FlashLength::For(ms()?),
                    _ => return Err("o.forwardflash: expected milliseconds or \"input\"".into()),
                }
            }
            "reloaddebounce" => o.reloaddebounce = ms()?,
            "reloadanchor" => {
                o.reloadanchor_text =
                    by_name(RELOADANCHOR, value.as_str()).ok_or("o.reloadanchor: expected \"position\" or \"text\"")?
            }
            "synctexeditor" => o.synctexeditor = text()?,
            "remainonexit" => o.remainonexit = flag()?,
            "foreground" => o.foreground = color(value).map_err(|e| format!("o.foreground: {e}"))?,
            "background" => o.background = color(value).map_err(|e| format!("o.background: {e}"))?,
            "palette" => {
                let ScriptValue::List(l) = value else {
                    return Err("o.palette: expected a list of \"#rrggbb\"".into());
                };
                if l.len() > 256 {
                    return Err("o.palette: at most 256 colours".into());
                }
                o.palette = l
                    .iter()
                    .map(|c| color(c)?.ok_or_else(|| "null in a palette".to_owned()))
                    .collect::<Result<_, String>>()
                    .map_err(|e| format!("o.palette: {e}"))?;
            }
            "guifont" => {
                o.guifont = fonts(value).map_err(|e| format!("o.guifont: {e}"))?;
                if o.guifont.is_empty() {
                    return Err("o.guifont: at least one font".into());
                }
                self.configured_fonts = None;
            }
            "guifontwide" => {
                o.guifontwide = fonts(value).map_err(|e| format!("o.guifontwide: {e}"))?;
                self.configured_fonts = None;
            }
            "showtabline" => {
                o.showtabline = by_name(SHOWTABLINE, value.as_str())
                    .ok_or("o.showtabline: expected \"never\", \"auto\" or \"always\"")?
            }
            "basemode" => {
                let ScriptValue::Map(pairs) = value else {
                    return Err("o.basemode: expected { kind: mode, ... }".into());
                };
                let mut map = std::collections::BTreeMap::new();
                for (kind, mode) in pairs {
                    let mode = mode.as_str().ok_or(format!("o.basemode.{kind}: expected a mode's name"))?;
                    map.insert(kind.clone(), mode.to_owned());
                }
                self.options.basemode = map;
                // The focused pane's base changed (an editor enabled or
                // disabled): its history starts again from it, as when
                // focused; other panes take theirs when focused.
                let mode = self.pane_mode(self.focus);
                if self.history.modes().last() != Some(mode.as_str()) {
                    self.reset_mode(self.focus);
                }
            }
            "wrap" => o.text.layout.wrap = flag()?,
            "linebreak" => o.text.layout.linebreak = flag()?,
            "breakindent" => o.text.layout.breakindent = flag()?,
            "showbreak" => o.text.layout.showbreak = text()?.unwrap_or_default(),
            "tabstop" => o.text.layout.tabstop = (num()? as usize).clamp(1, 64),
            "cursorline" => o.text.cursorline = flag()?,
            "scrolloff" => o.text.scrolloff = num()? as usize,
            "undofile" => o.undofile = flag()?,
            "swapfile" => o.swapfile = flag()?,
            "fileIgnoreCase" => {
                o.fileignorecase = match value {
                    ScriptValue::Bool(true) => temzdf_paths::IgnoreCase::Yes,
                    ScriptValue::Bool(false) => temzdf_paths::IgnoreCase::No,
                    v => by_name(FILEIGNORECASE, v.as_str())
                        .ok_or("o.fileIgnoreCase: expected \"auto\", true or false")?,
                }
            }
            "markdownmode" => {
                o.markdownmode = match text()?.as_deref() {
                    Some(m @ ("text" | "graphics")) => m.to_owned(),
                    other => return Err(format!("markdownmode: text or graphics, not {other:?}")),
                }
            }
            "updatetime" => o.updatetime = Duration::from_millis(num()?.max(0.0) as u64),
            // Where undo and swap files go (Vim's undodir and directory);
            // null or "" the state directory's.
            "undodir" => self.undo_dir = super::text::dir_option(text()?, super::text::undo_dir)?,
            "directory" => self.swap_dir = super::text::dir_option(text()?, super::swap::swap_dir)?,
            "number" => {
                o.text.number = super::text::number_mode(value.as_str())
                    .ok_or("o.number: expected \"off\", \"absolute\", \"relative\" or \"both\"")?;
            }
            "showstatusline" => {
                o.showstatusline = by_name(SHOWSTATUSLINE, value.as_str())
                    .ok_or("o.showstatusline: expected \"never\" or \"always\"")?
            }
            other => return Err(format!("no option {other:?}")),
        }
        self.apply_options();
        if name == "showtabline" || name == "showstatusline" {
            self.relayout();
        }
        Ok(())
    }

    pub(super) fn get_option(&self, name: &str) -> Result<ScriptValue, String> {
        let o = &self.options;
        let ms = |d: Duration| ScriptValue::Num(d.as_millis() as f64);
        Ok(match name {
            "timeoutlen" => ms(o.timeoutlen),
            "splitright" => ScriptValue::Bool(o.splitright),
            "splitbelow" => ScriptValue::Bool(o.splitbelow),
            "modehistory" => ScriptValue::Num(o.modehistory as f64),
            "copyhistory" => ScriptValue::Num(o.copyhistory as f64),
            "clipboard" => ScriptValue::Str(o.clipboard.name().into()),
            "scrollstep" => ScriptValue::Num(o.scrollstep),
            "scrollsmoothness" => ms(o.scrollsmoothness),
            "linkopener" => o.linkopener.as_ref().map_or(ScriptValue::Null, |l| {
                ScriptValue::List(l.iter().map(|w| ScriptValue::Str(w.clone())).collect())
            }),
            "linkpreview" => ScriptValue::Bool(o.linkpreview),
            "linkpreviewdelay" => ms(o.linkpreviewdelay),
            "forwardflash" => match o.forwardflash {
                FlashLength::For(d) => ms(d),
                FlashLength::UntilInput => ScriptValue::Str("input".into()),
            },
            "reloaddebounce" => ms(o.reloaddebounce),
            "reloadanchor" => name_in(RELOADANCHOR, &o.reloadanchor_text),
            "synctexeditor" => o.synctexeditor.clone().map_or(ScriptValue::Null, ScriptValue::Str),
            "remainonexit" => ScriptValue::Bool(o.remainonexit),
            "showtabline" => name_in(SHOWTABLINE, &o.showtabline),
            "showstatusline" => name_in(SHOWSTATUSLINE, &o.showstatusline),
            "basemode" => {
                ScriptValue::Map(o.basemode.iter().map(|(k, v)| (k.clone(), ScriptValue::Str(v.clone()))).collect())
            }
            "wrap" => ScriptValue::Bool(o.text.layout.wrap),
            "linebreak" => ScriptValue::Bool(o.text.layout.linebreak),
            "breakindent" => ScriptValue::Bool(o.text.layout.breakindent),
            "showbreak" => ScriptValue::Str(o.text.layout.showbreak.clone()),
            "tabstop" => ScriptValue::Num(o.text.layout.tabstop as f64),
            "cursorline" => ScriptValue::Bool(o.text.cursorline),
            "scrolloff" => ScriptValue::Num(o.text.scrolloff as f64),
            "undofile" => ScriptValue::Bool(o.undofile),
            "swapfile" => ScriptValue::Bool(o.swapfile),
            "fileIgnoreCase" => match o.fileignorecase {
                temzdf_paths::IgnoreCase::Yes => ScriptValue::Bool(true),
                temzdf_paths::IgnoreCase::No => ScriptValue::Bool(false),
                auto => name_in(FILEIGNORECASE, &auto),
            },
            "markdownmode" => ScriptValue::Str(o.markdownmode.clone()),
            "updatetime" => ScriptValue::Num(o.updatetime.as_millis() as f64),
            "undodir" => {
                self.undo_dir.as_ref().map_or(ScriptValue::Null, |d| ScriptValue::Str(d.display().to_string()))
            }
            "directory" => {
                self.swap_dir.as_ref().map_or(ScriptValue::Null, |d| ScriptValue::Str(d.display().to_string()))
            }
            "number" => name_in(super::text::NUMBER_MODES, &o.text.number),
            "foreground" => o.foreground.map_or(ScriptValue::Null, |c| ScriptValue::Str(hex(c))),
            "background" => o.background.map_or(ScriptValue::Null, |c| ScriptValue::Str(hex(c))),
            "palette" => ScriptValue::List(o.palette.iter().map(|c| ScriptValue::Str(hex(*c))).collect()),
            "guifont" => font_list(&o.guifont),
            "guifontwide" => font_list(&o.guifontwide),
            other => return Err(format!("no option {other:?}")),
        })
    }
}

/// A font list option: `{ font: string, size?: number }[]`.
fn fonts(v: &ScriptValue) -> Result<Vec<temzdf_scene::FontEntry>, String> {
    const EXPECTED: &str = "expected a list of { font: string, size?: number }";
    let ScriptValue::List(l) = v else { return Err(EXPECTED.into()) };
    l.iter()
        .map(|entry| {
            let ScriptValue::Map(fields) = entry else { return Err(EXPECTED.to_owned()) };
            let field = |k: &str| fields.iter().find(|(name, _)| name == k).map(|(_, v)| v);
            let family = match field("font") {
                Some(ScriptValue::Str(s)) if !s.trim().is_empty() => s.trim().to_owned(),
                _ => return Err(format!("{EXPECTED} (font: a family name)")),
            };
            let size = match field("size") {
                None | Some(ScriptValue::Null) => None,
                Some(ScriptValue::Num(n)) if n.is_finite() && *n > 0.0 => {
                    Some(n.clamp(super::commands::MIN_FONT_POINTS, super::commands::MAX_FONT_POINTS) as f32)
                }
                Some(_) => return Err(format!("size of {family:?}: expected a number of points")),
            };
            if let Some((k, _)) = fields.iter().find(|(k, _)| k != "font" && k != "size") {
                return Err(format!("{family:?}: no field {k:?} (font, size)"));
            }
            Ok(temzdf_scene::FontEntry { family, size })
        })
        .collect()
}

/// A font list as scripts see it.
fn font_list(fonts: &[temzdf_scene::FontEntry]) -> ScriptValue {
    ScriptValue::List(
        fonts
            .iter()
            .map(|f| {
                let mut m = vec![("font".to_owned(), ScriptValue::Str(f.family.clone()))];
                if let Some(size) = f.size {
                    m.push(("size".to_owned(), ScriptValue::Num(size as f64)));
                }
                ScriptValue::Map(m)
            })
            .collect(),
    )
}

/// A colour option: "#rrggbb", or null for none.
fn color(v: &ScriptValue) -> Result<Option<[u8; 3]>, String> {
    match v {
        ScriptValue::Null => Ok(None),
        ScriptValue::Str(s) => {
            let h = s.strip_prefix('#').filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()));
            let h = h.ok_or_else(|| format!("{s:?} is not \"#rrggbb\""))?;
            let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).expect("checked");
            Ok(Some([byte(0), byte(2), byte(4)]))
        }
        other => Err(format!("{other:?} is not \"#rrggbb\"")),
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_sizes_stay_within_reason() {
        let list = |size: f64| {
            let entry = ScriptValue::map([("font", ScriptValue::Str("x".into())), ("size", ScriptValue::Num(size))]);
            fonts(&ScriptValue::List(vec![entry])).unwrap()[0].size
        };
        assert_eq!(list(12.5), Some(12.5));
        assert_eq!(list(100000.0), Some(200.0), "glyph atlases past the GPU's limits draw nothing");
        assert_eq!(list(0.5), Some(4.0));
        let inf = ScriptValue::map([("font", ScriptValue::Str("x".into())), ("size", ScriptValue::Num(f64::INFINITY))]);
        assert!(fonts(&ScriptValue::List(vec![inf])).is_err());
    }
}
