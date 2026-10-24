//! A mock terminal that understands the subset of escape sequences termpix emits
//! (cursor positioning and kitty graphics) and renders the resulting screen.
//!
//! It exists for tests and for inspecting frames without a real terminal: feed it
//! the bytes an application wrote, then call [`MockTerminal::render`] and look at
//! (or save) the image. It models kitty semantics as documented, not any
//! terminal's quirks. Text cells are ignored.

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::geom::{Geometry, Rect};
use crate::raster::{self, Blending, RgbaImage, RgbaRef};

#[derive(Clone, Debug)]
struct MockPlacement {
    image: u32,
    id: u32,
    col: u32,
    row: u32,
    src: Option<Rect>,
    cells: Option<(u32, u32)>,
    offset: (u32, u32),
    z: i32,
    /// Insertion order, to break z ties like a real terminal (later on top).
    seq: u64,
}

#[derive(Debug)]
pub struct MockTerminal {
    geometry: Geometry,
    background: [u8; 4],
    cursor: (u32, u32),
    images: HashMap<u32, RgbaImage>,
    placements: Vec<MockPlacement>,
    /// Keys and accumulated payload of a chunked transmission in progress.
    chunked: Option<(HashMap<char, String>, Vec<u8>)>,
    replies: Vec<u8>,
    seq: u64,
    /// Partial escape sequence carried over between `feed` calls.
    buf: Vec<u8>,
    /// Foreground colour as a 24-bit value (placeholder text encodes image ids).
    fg_id: u32,
    /// Screen cell -> (image id, image row, image column) from placeholder text.
    placeholder_cells: HashMap<(u32, u32), (u32, u32, u32)>,
    /// Virtual placements (`U=1`): image id -> size in cells.
    virtual_sizes: HashMap<u32, (u32, u32)>,
    blending: Blending,
}

fn utf8_len(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7f => Some(1),
        0xc0..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf7 => Some(4),
        _ => None,
    }
}

fn diacritic_index(c: char) -> Option<u32> {
    crate::placeholder::DIACRITICS.iter().position(|&d| d == c as u32).map(|n| n as u32)
}

impl MockTerminal {
    /// Blend translucent images like a particular terminal (kitty: linear).
    pub fn set_blending(&mut self, blending: Blending) {
        self.blending = blending;
    }

    pub fn new(geometry: Geometry, background: [u8; 4]) -> Self {
        Self {
            geometry,
            background,
            cursor: (0, 0),
            images: HashMap::new(),
            placements: Vec::new(),
            chunked: None,
            replies: Vec::new(),
            seq: 0,
            buf: Vec::new(),
            fg_id: 0,
            placeholder_cells: HashMap::new(),
            blending: Blending::Srgb,
            virtual_sizes: HashMap::new(),
        }
    }

    pub fn placement_count(&self) -> usize {
        self.placements.len()
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Drop an image as if the terminal evicted it to stay under its quota.
    pub fn evict(&mut self, id: u32) {
        self.images.remove(&id);
        self.placements.retain(|p| p.image != id);
    }

    /// Bytes the terminal would have sent back (graphics replies).
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.buf);
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                // Text: only Unicode placeholders matter (each takes one cell).
                let Some(len) = utf8_len(buf[i]) else {
                    i += 1;
                    continue;
                };
                if i + len > buf.len() {
                    break;
                }
                let c = std::str::from_utf8(&buf[i..i + len]).ok().and_then(|s| s.chars().next());
                i += len;
                if c == Some(crate::placeholder::PLACEHOLDER) {
                    // The row and column diacritics follow.
                    let mut marks = Vec::new();
                    while marks.len() < 2 {
                        let Some(l) = buf.get(i).and_then(|&b| utf8_len(b)) else { break };
                        let next =
                            buf.get(i..i + l).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next());
                        let Some(n) = next.and_then(diacritic_index) else { break };
                        marks.push(n);
                        i += l;
                    }
                    if let [row, col] = marks[..] {
                        let cell = (self.cursor.0, self.cursor.1);
                        self.placeholder_cells.insert(cell, (self.fg_id, row, col));
                    }
                    self.cursor.0 += 1;
                } else if c.is_some_and(|c| !c.is_control() && diacritic_index(c).is_none()) {
                    self.cursor.0 += 1;
                }
                continue;
            }
            let Some(&kind) = buf.get(i + 1) else { break };
            match kind {
                b'[' => {
                    let Some(len) = buf[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b)) else { break };
                    let end = i + 2 + len;
                    match buf[end] {
                        b'H' => self.cursor_position(&buf[i + 2..end]),
                        b'm' => self.sgr(&buf[i + 2..end]),
                        _ => {}
                    }
                    i = end + 1;
                }
                // tmux passthrough: `ESC P tmux; <inner with ESC doubled> ESC \`.
                b'P' if buf[i..].starts_with(b"\x1bPtmux;") => {
                    let mut j = i + 7;
                    let mut inner = Vec::new();
                    let mut done = false;
                    while j + 1 < buf.len() {
                        match (buf[j], buf[j + 1]) {
                            (0x1b, 0x1b) => {
                                inner.push(0x1b);
                                j += 2;
                            }
                            (0x1b, b'\\') => {
                                done = true;
                                j += 2;
                                break;
                            }
                            (c, _) => {
                                inner.push(c);
                                j += 1;
                            }
                        }
                    }
                    if !done {
                        break;
                    }
                    self.feed(&inner);
                    i = j;
                }
                b'_' => {
                    let Some(len) = buf[i + 2..].windows(2).position(|w| w == b"\x1b\\") else { break };
                    let body = buf[i + 2..i + 2 + len].to_vec();
                    if let Some(cmd) = body.strip_prefix(b"G") {
                        self.graphics(cmd);
                    }
                    i += 2 + len + 2;
                }
                _ => i += 2,
            }
        }
        self.buf = buf[i..].to_vec();
    }

    /// SGR: only true-colour foreground (`38;2;r;g;b`) and its reset matter.
    fn sgr(&mut self, params: &[u8]) {
        let s = String::from_utf8_lossy(params);
        let n: Vec<u32> = s.split(';').map(|v| v.parse().unwrap_or(0)).collect();
        match n.as_slice() {
            [38, 2, r, g, b] => self.fg_id = (r << 16) | (g << 8) | b,
            [39] | [0] | [] => self.fg_id = 0,
            _ => {}
        }
    }

    fn cursor_position(&mut self, params: &[u8]) {
        let s = String::from_utf8_lossy(params);
        let mut it = s.split(';').map(|v| v.parse::<u32>().unwrap_or(1).max(1));
        let row = it.next().unwrap_or(1);
        let col = it.next().unwrap_or(1);
        self.cursor = (col - 1, row - 1);
    }

    fn graphics(&mut self, cmd: &[u8]) {
        let text = String::from_utf8_lossy(cmd);
        let (keys_str, payload) = text.split_once(';').unwrap_or((&text, ""));
        let mut keys: HashMap<char, String> = HashMap::new();
        for kv in keys_str.split(',') {
            if let Some((k, v)) = kv.split_once('=')
                && let Some(c) = k.chars().next()
            {
                keys.insert(c, v.to_owned());
            }
        }
        let more = keys.get(&'m').map(String::as_str) == Some("1");
        let payload = payload.as_bytes();

        if let Some((first_keys, mut acc)) = self.chunked.take() {
            acc.extend_from_slice(payload);
            if more {
                self.chunked = Some((first_keys, acc));
            } else {
                self.execute(&first_keys, &acc);
            }
            return;
        }
        if more {
            self.chunked = Some((keys, payload.to_vec()));
            return;
        }
        self.execute(&keys, payload);
    }

    fn execute(&mut self, keys: &HashMap<char, String>, payload: &[u8]) {
        let num = |k: char| keys.get(&k).and_then(|v| v.parse::<i64>().ok());
        let action = keys.get(&'a').and_then(|v| v.chars().next()).unwrap_or('t');
        let id = num('i').unwrap_or(0) as u32;
        let quiet = num('q').unwrap_or(0);
        match action {
            't' | 'T' => match self.decode_image(keys, payload) {
                Ok(img) => {
                    self.placements.retain(|p| p.image != id);
                    self.images.insert(id, img);
                    if action == 'T' {
                        self.place(keys, id);
                    }
                }
                Err(e) => self.reply(id, &format!("EINVAL:{e}"), quiet, true),
            },
            'p' => {
                if self.images.contains_key(&id) {
                    self.place(keys, id);
                } else {
                    self.reply(id, "ENOENT:image not found", quiet, true);
                }
            }
            'd' => self.delete(keys),
            'q' => match self.decode_image(keys, payload) {
                Ok(_) => self.reply(id, "OK", quiet, false),
                Err(e) => self.reply(id, &format!("EINVAL:{e}"), quiet, true),
            },
            _ => {}
        }
    }

    fn reply(&mut self, id: u32, message: &str, quiet: i64, is_error: bool) {
        if quiet >= 2 || (quiet == 1 && !is_error) {
            return;
        }
        self.replies.extend_from_slice(format!("\x1b_Gi={id};{message}\x1b\\").as_bytes());
    }

    fn decode_image(&self, keys: &HashMap<char, String>, payload: &[u8]) -> Result<RgbaImage, String> {
        let num = |k: char| keys.get(&k).and_then(|v| v.parse::<u32>().ok());
        let raw = B64.decode(payload).map_err(|e| e.to_string())?;
        let medium = keys.get(&'t').and_then(|v| v.chars().next()).unwrap_or('d');
        let mut data = match medium {
            'd' => raw,
            's' => {
                let name = String::from_utf8(raw).map_err(|e| e.to_string())?;
                let path = std::path::Path::new("/dev/shm").join(name.trim_start_matches('/'));
                let data = std::fs::read(&path).map_err(|e| e.to_string())?;
                let _ = rustix::shm::unlink(name.as_str());
                data
            }
            't' | 'f' => {
                let path = String::from_utf8(raw).map_err(|e| e.to_string())?;
                let data = std::fs::read(&path).map_err(|e| e.to_string())?;
                if medium == 't' {
                    let _ = std::fs::remove_file(&path);
                }
                data
            }
            other => return Err(format!("unsupported medium {other}")),
        };
        if keys.get(&'o').map(String::as_str) == Some("z") {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(&data[..]).read_to_end(&mut out).map_err(|e| e.to_string())?;
            data = out;
        }
        let (w, h) = (num('s').ok_or("missing s")?, num('v').ok_or("missing v")?);
        let rgba = match num('f').unwrap_or(32) {
            32 => data,
            24 => data.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
            f => return Err(format!("unsupported format {f}")),
        };
        RgbaImage::from_raw(w, h, rgba).ok_or_else(|| "size mismatch".to_owned())
    }

    fn place(&mut self, keys: &HashMap<char, String>, image: u32) {
        let num = |k: char| keys.get(&k).and_then(|v| v.parse::<i64>().ok());
        if num('U') == Some(1) {
            let (c, r) = (num('c').unwrap_or(1) as u32, num('r').unwrap_or(1) as u32);
            self.virtual_sizes.insert(image, (c.max(1), r.max(1)));
            return;
        }
        let id = num('p').unwrap_or(0) as u32;
        let src = match (num('x'), num('y'), num('w'), num('h')) {
            (None, None, None, None) => None,
            (x, y, w, h) => {
                let img = &self.images[&image];
                Some(Rect::new(
                    x.unwrap_or(0) as i32,
                    y.unwrap_or(0) as i32,
                    w.map_or(img.width, |v| v as u32),
                    h.map_or(img.height, |v| v as u32),
                ))
            }
        };
        let cells = match (num('c'), num('r')) {
            (Some(c), Some(r)) => Some((c as u32, r as u32)),
            _ => None,
        };
        self.seq += 1;
        let placement = MockPlacement {
            image,
            id,
            col: self.cursor.0,
            row: self.cursor.1,
            src,
            cells,
            offset: (num('X').unwrap_or(0) as u32, num('Y').unwrap_or(0) as u32),
            z: num('z').unwrap_or(0) as i32,
            seq: self.seq,
        };
        // Same image and non-zero placement id: replace in place.
        if id != 0
            && let Some(existing) = self.placements.iter_mut().find(|p| p.image == image && p.id == id)
        {
            *existing = placement;
            return;
        }
        self.placements.push(placement);
    }

    fn delete(&mut self, keys: &HashMap<char, String>) {
        let num = |k: char| keys.get(&k).and_then(|v| v.parse::<i64>().ok());
        let what = keys.get(&'d').and_then(|v| v.chars().next()).unwrap_or('a');
        let free = what.is_ascii_uppercase();
        match what.to_ascii_lowercase() {
            'a' => {
                self.placements.clear();
                if free {
                    self.images.clear();
                }
            }
            'i' => {
                let id = num('i').unwrap_or(0) as u32;
                match num('p') {
                    Some(p) => self.placements.retain(|pl| !(pl.image == id && pl.id == p as u32)),
                    None => self.placements.retain(|pl| pl.image != id),
                }
                if free {
                    self.images.remove(&id);
                }
            }
            'z' => {
                let z = num('z').unwrap_or(0) as i32;
                self.placements.retain(|pl| pl.z != z);
            }
            _ => {}
        }
    }

    /// Draw the screen: background, then placements from lowest to highest z.
    pub fn render(&self) -> RgbaImage {
        let mut screen = RgbaImage::new(self.geometry.width_px(), self.geometry.height_px(), self.background);
        let mut order: Vec<&MockPlacement> = self.placements.iter().collect();
        order.sort_by_key(|p| (p.z, p.seq));
        let cell = self.geometry.cell;
        for p in order {
            let Some(img) = self.images.get(&p.image) else { continue };
            let src = p.src.unwrap_or(Rect::new(0, 0, img.width, img.height));
            let x = (p.col * cell.width + p.offset.0) as i32;
            let y = (p.row * cell.height + p.offset.1) as i32;
            let (w, h) = match p.cells {
                Some((c, r)) => (c * cell.width, r * cell.height),
                None => (src.w, src.h),
            };
            raster::blit_blended(
                &mut screen,
                Rect::new(x, y, w, h),
                RgbaRef { width: img.width, height: img.height, data: &img.data },
                src,
                self.blending,
            );
        }
        // Placeholder cells show one cell of their image's virtual placement.
        for (&(col, row), &(id, img_row, img_col)) in &self.placeholder_cells {
            let (Some(img), Some(&(c, r))) = (self.images.get(&id), self.virtual_sizes.get(&id)) else { continue };
            let (cw, ch) = (img.width / c, img.height / r);
            let src = Rect::new((img_col * cw) as i32, (img_row * ch) as i32, cw, ch);
            let dst = Rect::new((col * cell.width) as i32, (row * cell.height) as i32, cell.width, cell.height);
            let img = RgbaRef { width: img.width, height: img.height, data: &img.data };
            raster::blit_blended(&mut screen, dst, img, src, self.blending);
        }
        screen
    }
}
