//! Floating messages at the top right of the screen (as WezTerm's), drawn as
//! text cells over the panes: errors stay until the next key, information
//! fades after a few seconds, the script-host notice stays until the host is
//! back.

use std::time::{Duration, Instant};

use temzdf_scene::{Color, TextGrid};

use super::Session;

/// How long information stays.
const INFO_FOR: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Stays until the next key.
    Error,
    /// Fades after [`INFO_FOR`].
    Info,
    /// Stays until taken back (the script host is down).
    Host,
}

pub struct Notice {
    pub kind: Kind,
    pub text: String,
    pub until: Option<Instant>,
}

pub(super) use temzdf_scene::notice::width;

impl Session {
    /// Show a floating message.
    pub(super) fn notify(&mut self, kind: Kind, text: &str) {
        eprintln!("temzdf: {text}");
        if kind == Kind::Host {
            self.notices.retain(|n| n.kind != Kind::Host);
        }
        let until = (kind == Kind::Info).then(|| Instant::now() + INFO_FOR);
        self.notices.push(Notice { kind, text: text.to_owned(), until });
        // Keep a handful: the newest.
        while self.notices.len() > 5 {
            self.notices.remove(0);
        }
    }

    /// A key was pressed: errors go.
    pub(super) fn dismiss_errors(&mut self) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| n.kind != Kind::Error);
        before != self.notices.len()
    }

    pub(super) fn clear_host_notice(&mut self) {
        self.notices.retain(|n| n.kind != Kind::Host);
    }

    /// Faded information goes. Returns whether something went.
    pub(super) fn expire_notices(&mut self, now: Instant) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| n.until.is_none_or(|t| now < t));
        before != self.notices.len()
    }

    pub(super) fn notice_deadline(&self) -> Option<Instant> {
        self.notices.iter().filter_map(|n| n.until).min()
    }

    /// The notices as boxes of text cells, stacked down from the top right.
    /// Where the floating messages are, in cells.
    pub(super) fn notice_rects(&self) -> Vec<temzdf_scene::Rect> {
        let rect =
            |g: &TextGrid| temzdf_scene::Rect::new(g.col as i32, g.row as i32, g.width() as u32, g.rows.len() as u32);
        self.notice_grids().iter().map(rect).collect()
    }

    pub(super) fn notice_grids(&self) -> Vec<TextGrid> {
        let border = |k: Kind| match k {
            Kind::Error | Kind::Host => Color::Indexed(1),
            Kind::Info => Color::Indexed(4),
        };
        let messages: Vec<(&str, Color)> =
            self.notices.iter().rev().map(|n| (n.text.as_str(), border(n.kind))).collect();
        // Below the tab line.
        let g = self.geometry;
        temzdf_scene::notice::boxes(&messages, g.cols as usize, g.rows as usize, self.area().y as usize)
    }
}
