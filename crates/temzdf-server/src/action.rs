//! Session actions: what prefix keys, `temzdf action ...` and (later) a
//! command line ask the session to do. Their text form follows zellij's action
//! names where one exists.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::layout::{Direction, Split};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Show the focused pane's document again, beside or below it.
    Split(Split),
    /// Open a PDF in a new pane, beside the focused one.
    NewPane(PathBuf),
    MoveFocus(Direction),
    FocusNextPane,
    ClosePane,
    Detach,
}

impl Action {
    /// Parse `new-pane FILE`, `split right|down`, `move-focus left|right|up|down`,
    /// `focus-next-pane`, `close-pane` or `detach`.
    pub fn parse(words: &[String]) -> Result<Self> {
        let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
        Ok(match w.as_slice() {
            ["new-pane", file] => Action::NewPane(PathBuf::from(file)),
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
            _ => bail!(
                "unknown action {:?}; known: new-pane FILE, split right|down, move-focus DIRECTION, \
                 focus-next-pane, close-pane, detach",
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

    #[test]
    fn parses_the_text_form() {
        assert!(parse("new-pane a b.pdf").is_err(), "one file");
        assert_eq!(parse("new-pane doc.pdf").unwrap(), Action::NewPane("doc.pdf".into()));
        assert_eq!(parse("split down").unwrap(), Action::Split(Split::Vertical));
        assert_eq!(parse("move-focus up").unwrap(), Action::MoveFocus(Direction::Up));
        assert!(parse("move-focus sideways").unwrap_err().to_string().contains("sideways"));
        assert_eq!(parse("detach").unwrap(), Action::Detach);
        assert!(parse("explode").is_err());
    }
}
