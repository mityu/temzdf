//! The text forms of commands that more than the command registry reads:
//! what `new-pane` opens (also `temzdf server`'s pane and floats' words)
//! and the side of a split. Their names follow zellij's actions.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::layout::Split;

/// What a new pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneSpec {
    /// The user's shell.
    Shell,
    /// A program and its arguments.
    Command(Vec<String>),
    /// A file: a PDF by its extension, otherwise text.
    File(PathBuf),
}

/// A new pane beside the focused one. Terminals start in `cwd` (the
/// server's directory when `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPane {
    pub spec: PaneSpec,
    pub cwd: Option<PathBuf>,
}

/// Parse the words after `new-pane`: `[--cwd DIR] [FILE | -- COMMAND...]`.
pub fn parse_new_pane(words: &[String]) -> Result<NewPane> {
    let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    let (cwd, rest) = match w.as_slice() {
        ["--cwd", dir, rest @ ..] => (Some(PathBuf::from(dir)), rest),
        rest => (None, rest),
    };
    let spec = match rest {
        [] => PaneSpec::Shell,
        ["--", command @ ..] if !command.is_empty() => {
            PaneSpec::Command(command.iter().map(|s| s.to_string()).collect())
        }
        [file] if !file.starts_with('-') => PaneSpec::File(PathBuf::from(file)),
        _ => bail!("usage: new-pane [--cwd DIR] [FILE | -- COMMAND...]"),
    };
    Ok(NewPane { spec, cwd })
}

/// A split towards `side` (`right`, `left`, `bottom` or `down`, `top` or
/// `up`): its direction, and whether the new pane goes before (left of or
/// above) the one split.
pub fn split_side(side: &str) -> Result<(Split, bool)> {
    Ok(match side {
        "right" => (Split::Horizontal, false),
        "left" => (Split::Horizontal, true),
        "bottom" | "down" => (Split::Vertical, false),
        "top" | "up" => (Split::Vertical, true),
        other => bail!("split: {other:?} is not right, left, bottom or top"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<NewPane> {
        parse_new_pane(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    fn new_pane(spec: PaneSpec, cwd: Option<&str>) -> NewPane {
        NewPane { spec, cwd: cwd.map(PathBuf::from) }
    }

    #[test]
    fn parses_the_text_form() {
        assert!(parse("a b.pdf").is_err(), "one file");
        assert_eq!(parse("doc.pdf").unwrap(), new_pane(PaneSpec::File("doc.pdf".into()), None));
        assert_eq!(parse("").unwrap(), new_pane(PaneSpec::Shell, None));
        assert_eq!(
            parse("--cwd /w -- vim -u NONE").unwrap(),
            new_pane(PaneSpec::Command(vec!["vim".into(), "-u".into(), "NONE".into()]), Some("/w"))
        );
        assert!(parse("--").is_err() && parse("--bogus").is_err());
        assert_eq!(split_side("down").unwrap(), (Split::Vertical, false));
        assert_eq!(split_side("left").unwrap(), (Split::Horizontal, true));
        assert!(split_side("sideways").unwrap_err().to_string().contains("sideways"));
    }
}
