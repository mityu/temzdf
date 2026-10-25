//! Session actions: what prefix keys, `temzdf action ...` and (later) a
//! command line ask the session to do. Their text form follows zellij's action
//! names where one exists.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::layout::{Direction, Split};

/// What a new pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneSpec {
    /// The user's shell.
    Shell,
    /// A program and its arguments.
    Command(Vec<String>),
    Pdf(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Another pane like the focused one, beside or below it: the same
    /// document again, or a new shell.
    Split(Split),
    /// A new pane beside the focused one. Terminals start in `cwd` (the
    /// server's directory when `None`).
    NewPane {
        spec: PaneSpec,
        cwd: Option<PathBuf>,
    },
    MoveFocus(Direction),
    FocusNextPane,
    ClosePane,
    Detach,
    /// Restart the script host and load the configuration again.
    ReloadConfig,
}

impl Action {
    /// Parse `new-pane [--cwd DIR] [FILE | -- COMMAND...]`, `split right|down`,
    /// `move-focus left|right|up|down`, `focus-next-pane`, `close-pane` or
    /// `detach`.
    pub fn parse(words: &[String]) -> Result<Self> {
        let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
        Ok(match w.as_slice() {
            ["new-pane", rest @ ..] => {
                let (cwd, rest) = match rest {
                    ["--cwd", dir, rest @ ..] => (Some(PathBuf::from(dir)), rest),
                    rest => (None, rest),
                };
                let spec = match rest {
                    [] => PaneSpec::Shell,
                    ["--", command @ ..] if !command.is_empty() => {
                        PaneSpec::Command(command.iter().map(|s| s.to_string()).collect())
                    }
                    [file] if !file.starts_with('-') => PaneSpec::Pdf(PathBuf::from(file)),
                    _ => bail!("usage: new-pane [--cwd DIR] [FILE | -- COMMAND...]"),
                };
                Action::NewPane { spec, cwd }
            }
            ["split", "right"] => Action::Split(Split::Horizontal),
            ["split", "down"] => Action::Split(Split::Vertical),
            ["move-focus", d] => Action::MoveFocus(match *d {
                "left" => Direction::Left,
                "right" => Direction::Right,
                "up" => Direction::Up,
                "down" => Direction::Down,
                other => bail!("unknown direction {other:?} (left, right, up, down)"),
            }),
            ["focus-next-pane"] => Action::FocusNextPane,
            ["close-pane"] => Action::ClosePane,
            ["detach"] => Action::Detach,
            ["reload-config"] => Action::ReloadConfig,
            _ => bail!(
                "unknown action {:?}; known: new-pane [--cwd DIR] [FILE | -- COMMAND...], split right|down, \
                 move-focus DIRECTION, focus-next-pane, close-pane, detach, reload-config",
                words.join(" ")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Action> {
        Action::parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    fn new_pane(spec: PaneSpec, cwd: Option<&str>) -> Action {
        Action::NewPane { spec, cwd: cwd.map(PathBuf::from) }
    }

    #[test]
    fn parses_the_text_form() {
        assert!(parse("new-pane a b.pdf").is_err(), "one file");
        assert_eq!(parse("new-pane doc.pdf").unwrap(), new_pane(PaneSpec::Pdf("doc.pdf".into()), None));
        assert_eq!(parse("new-pane").unwrap(), new_pane(PaneSpec::Shell, None));
        assert_eq!(
            parse("new-pane --cwd /w -- vim -u NONE").unwrap(),
            new_pane(PaneSpec::Command(vec!["vim".into(), "-u".into(), "NONE".into()]), Some("/w"))
        );
        assert!(parse("new-pane --").is_err() && parse("new-pane --bogus").is_err());
        assert_eq!(parse("split down").unwrap(), Action::Split(Split::Vertical));
        assert_eq!(parse("move-focus up").unwrap(), Action::MoveFocus(Direction::Up));
        assert!(parse("move-focus sideways").unwrap_err().to_string().contains("sideways"));
        assert_eq!(parse("detach").unwrap(), Action::Detach);
        assert!(parse("explode").is_err());
    }
}
