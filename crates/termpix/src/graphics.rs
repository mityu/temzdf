//! Kitty graphics protocol encoding and image transport.
//!
//! See `doc/kitty-protocol.md`. This module only produces bytes; writing them to
//! the terminal (and wrapping frames in synchronized output) is the caller's job.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::geom::Rect;

/// Maximum base64 payload bytes per escape sequence for direct transmission.
const CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 3 bytes per pixel (`f=24`).
    Rgb,
    /// 4 bytes per pixel, straight alpha (`f=32`).
    Rgba,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    const fn code(self) -> u32 {
        match self {
            Self::Rgb => 24,
            Self::Rgba => 32,
        }
    }
}

/// How pixel data reaches the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Medium {
    /// Inline base64 in the escape sequences (`t=d`), optionally zlib-compressed.
    Direct { compress: bool },
    /// POSIX shared memory (`t=s`). Same host only.
    SharedMemory,
    /// A temporary file the terminal deletes after reading (`t=t`). Same host only.
    TempFile,
}

/// Borrowed raw pixels.
#[derive(Clone, Copy, Debug)]
pub struct Pixels<'a> {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: &'a [u8],
}

impl Pixels<'_> {
    fn check(&self) -> io::Result<()> {
        let expected = self.width as usize * self.height as usize * self.format.bytes_per_pixel();
        if self.data.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pixel buffer has {} bytes, expected {expected}", self.data.len()),
            ));
        }
        Ok(())
    }
}

/// Where and how to show an image (`a=p`, or the placement part of `a=T`).
/// The cursor must already be at the target cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Placement {
    pub placement_id: u32,
    /// Source rectangle inside the image (`x,y,w,h`). `None` shows the whole image.
    pub src: Option<Rect>,
    /// Target size in cells (`c,r`); the source is scaled to fill it. `None` = native size.
    pub cells: Option<(u32, u32)>,
    /// Pixel offset inside the first cell (`X,Y`); must be smaller than a cell.
    pub offset: (u32, u32),
    pub z: i32,
    /// A virtual placement (`U=1`), shown where Unicode placeholder text refers
    /// to it (see [`crate::placeholder`]); `cells` gives its size.
    pub virtual_placement: bool,
}

impl Placement {
    fn write_keys(&self, out: &mut Vec<u8>) {
        if self.placement_id != 0 {
            write!(out, ",p={}", self.placement_id).unwrap();
        }
        if let Some(r) = self.src {
            write!(out, ",x={},y={},w={},h={}", r.x.max(0), r.y.max(0), r.w, r.h).unwrap();
        }
        if let Some((c, r)) = self.cells {
            write!(out, ",c={c},r={r}").unwrap();
        }
        if self.offset.0 != 0 {
            write!(out, ",X={}", self.offset.0).unwrap();
        }
        if self.offset.1 != 0 {
            write!(out, ",Y={}", self.offset.1).unwrap();
        }
        if self.z != 0 {
            write!(out, ",z={}", self.z).unwrap();
        }
        if self.virtual_placement {
            out.extend_from_slice(b",U=1");
        }
        // Never move the cursor: the caller positions it explicitly for every placement.
        out.extend_from_slice(b",C=1");
    }
}

/// What to delete (`a=d`). `free` also releases the image data (uppercase `d`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delete {
    All { free: bool },
    Image { id: u32, free: bool },
    Placement { id: u32, placement_id: u32 },
    ZIndex { z: i32, free: bool },
}

/// Encoder for kitty graphics commands. Keeps track of shared-memory objects so
/// stale ones can be cleaned up.
#[derive(Debug)]
pub struct Encoder {
    medium: Medium,
    zlib_level: u32,
    shm: ShmTracker,
}

/// Default zlib level for `o=z` payloads.
///
/// Level 2, not 1: zlib-rs level 1 ("deflate_quick") emits fixed-Huffman blocks,
/// and Ghostty 1.3.1 crashes (SIGSEGV) inflating fixed-Huffman streams larger
/// than 64 KiB. See doc/terminal-compat.md. Level 2 is about as fast and
/// compresses better.
pub const DEFAULT_ZLIB_LEVEL: u32 = 2;

impl Encoder {
    pub fn new(medium: Medium) -> Self {
        Self { medium, zlib_level: DEFAULT_ZLIB_LEVEL, shm: ShmTracker::default() }
    }

    pub fn medium(&self) -> Medium {
        self.medium
    }

    pub fn set_medium(&mut self, medium: Medium) {
        self.medium = medium;
    }

    pub fn zlib_level(&self) -> u32 {
        self.zlib_level
    }

    /// zlib level (0-9) for compressed direct transmissions. Level 1 crashes
    /// Ghostty 1.3.1; see [`DEFAULT_ZLIB_LEVEL`].
    pub fn set_zlib_level(&mut self, level: u32) {
        self.zlib_level = level.min(9);
    }

    /// Transmit an image (`a=t`), or transmit and place it (`a=T`) when `place` is set.
    /// Replies: OK replies are suppressed, errors are reported (`q=1`).
    pub fn transmit(
        &mut self,
        out: &mut Vec<u8>,
        id: u32,
        px: Pixels<'_>,
        place: Option<&Placement>,
    ) -> io::Result<()> {
        px.check()?;
        let mut keys = Vec::new();
        let action = if place.is_some() { 'T' } else { 't' };
        write!(keys, "a={action},i={id},f={},s={},v={},q=1", px.format.code(), px.width, px.height).unwrap();
        if let Some(p) = place {
            p.write_keys(&mut keys);
        }
        match self.medium {
            Medium::Direct { compress } => {
                let payload = if compress {
                    keys.extend_from_slice(b",o=z");
                    zlib(px.data, self.zlib_level)
                } else {
                    px.data.to_vec()
                };
                write_chunked(out, &keys, &payload);
            }
            Medium::SharedMemory => {
                let name = self.shm.create(px.data)?;
                write!(keys, ",t=s,S={}", px.data.len()).unwrap();
                write_command(out, &keys, B64.encode(name.as_bytes()).as_bytes());
            }
            Medium::TempFile => {
                let path = write_temp_file(px.data)?;
                self.shm.files.push(path.clone());
                write!(keys, ",t=t,S={}", px.data.len()).unwrap();
                write_command(out, &keys, B64.encode(path.as_os_str().as_encoded_bytes()).as_bytes());
            }
        }
        Ok(())
    }

    /// Place an already transmitted image (`a=p`). Errors are reported (e.g. `ENOENT`
    /// when the terminal evicted the image), OK replies are suppressed.
    pub fn place(&self, out: &mut Vec<u8>, id: u32, place: &Placement) {
        let mut keys = Vec::new();
        write!(keys, "a=p,i={id},q=1").unwrap();
        place.write_keys(&mut keys);
        write_command(out, &keys, b"");
    }

    pub fn delete(&self, out: &mut Vec<u8>, what: Delete) {
        let mut keys = Vec::new();
        let d = |c: char, free: bool| if free { c.to_ascii_uppercase() } else { c };
        match what {
            Delete::All { free } => write!(keys, "a=d,d={}", d('a', free)),
            Delete::Image { id, free } => write!(keys, "a=d,d={},i={id}", d('i', free)),
            Delete::Placement { id, placement_id } => write!(keys, "a=d,d=i,i={id},p={placement_id}"),
            Delete::ZIndex { z, free } => write!(keys, "a=d,d={},z={z}", d('z', free)),
        }
        .unwrap();
        keys.extend_from_slice(b",q=2");
        write_command(out, &keys, b"");
    }

    /// Remove shared-memory objects the terminal did not consume. Call this
    /// periodically with the number of recent objects to keep (they may still be
    /// in flight), and with 0 on shutdown.
    pub fn cleanup_shm(&mut self, keep: usize) {
        self.shm.cleanup(keep);
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.shm.cleanup(0);
    }
}

/// A graphics query (`a=q`) with a 1x1 RGB image sent over `medium`. The terminal
/// replies with `OK` or an error for image `id` without storing anything.
/// Returns the shared memory object or file the image went in: a terminal
/// that does not take that medium leaves it behind, so [`Leftover::remove`]
/// it once the reply came (or did not).
pub fn query(out: &mut Vec<u8>, id: u32, medium: Medium) -> io::Result<Option<Leftover>> {
    let data = [0u8; 3];
    let mut keys = Vec::new();
    write!(keys, "a=q,i={id},s=1,v=1,f=24").unwrap();
    Ok(match medium {
        Medium::Direct { .. } => {
            write_command(out, &keys, B64.encode(data).as_bytes());
            None
        }
        Medium::SharedMemory => {
            let name = shm_create(&data)?;
            write!(keys, ",t=s,S=3").unwrap();
            write_command(out, &keys, B64.encode(name.as_bytes()).as_bytes());
            Some(Leftover::Shm(name))
        }
        Medium::TempFile => {
            let path = write_temp_file(&data)?;
            write!(keys, ",t=t,S=3").unwrap();
            write_command(out, &keys, B64.encode(path.as_os_str().as_encoded_bytes()).as_bytes());
            Some(Leftover::File(path))
        }
    })
}

/// Where a query's image went (see [`query`]).
#[derive(Debug)]
pub enum Leftover {
    Shm(String),
    File(PathBuf),
}

impl Leftover {
    /// Remove it, if the terminal did not (it removes what it consumed).
    pub fn remove(self) {
        match self {
            Self::Shm(name) => drop(rustix::shm::unlink(name.as_str())),
            Self::File(path) => drop(std::fs::remove_file(path)),
        }
    }
}

fn write_command(out: &mut Vec<u8>, keys: &[u8], payload: &[u8]) {
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(keys);
    if !payload.is_empty() {
        out.push(b';');
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(b"\x1b\\");
}

/// Direct transmission: base64 in chunks of at most 4096 bytes. All keys go on the
/// first chunk; every chunk but the last carries `m=1`.
fn write_chunked(out: &mut Vec<u8>, keys: &[u8], data: &[u8]) {
    let encoded = B64.encode(data);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(CHUNK).collect();
    if chunks.len() <= 1 {
        write_command(out, keys, encoded.as_bytes());
        return;
    }
    for (i, chunk) in chunks.iter().enumerate() {
        let more = if i + 1 < chunks.len() { 1 } else { 0 };
        let mut k = Vec::new();
        if i == 0 {
            k.extend_from_slice(keys);
            k.push(b',');
        }
        write!(k, "m={more}").unwrap();
        write_command(out, &k, chunk);
    }
}

fn zlib(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    enc.write_all(data).expect("writing to a Vec cannot fail");
    enc.finish().expect("writing to a Vec cannot fail")
}

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    format!("{}-{}", std::process::id(), UNIQUE.fetch_add(1, Ordering::Relaxed))
}

/// Create a POSIX shared memory object holding `data`; returns its name.
fn shm_create(data: &[u8]) -> io::Result<String> {
    use rustix::fs::Mode;
    use rustix::shm;

    let name = format!("/termpix-{}", unique_suffix());
    let fd =
        shm::open(name.as_str(), shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR, Mode::RUSR | Mode::WUSR)?;
    let result = (|| -> io::Result<()> {
        let len = data.len() as u64;
        // fallocate makes a full /dev/shm fail here with ENOSPC instead of SIGBUS on write.
        #[cfg(target_os = "linux")]
        rustix::fs::fallocate(&fd, rustix::fs::FallocateFlags::empty(), 0, len)?;
        #[cfg(not(target_os = "linux"))]
        rustix::fs::ftruncate(&fd, len)?;
        let file = File::from(fd);
        // SAFETY: the object was just created exclusively by us, so nobody else
        // resizes it while mapped.
        let mut map = unsafe { memmap2::MmapMut::map_mut(&file)? };
        // macOS rounds the object up to whole pages; `S=` tells the terminal the
        // real length.
        map[..data.len()].copy_from_slice(data);
        map.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = shm::unlink(name.as_str());
        return Err(e);
    }
    Ok(name)
}

/// Shared-memory objects and temporary files we created, oldest first.
#[derive(Debug, Default)]
struct ShmTracker {
    names: Vec<String>,
    files: Vec<PathBuf>,
}

impl ShmTracker {
    fn create(&mut self, data: &[u8]) -> io::Result<String> {
        let name = shm_create(data)?;
        self.names.push(name.clone());
        Ok(name)
    }

    fn cleanup(&mut self, keep: usize) {
        let excess = self.names.len().saturating_sub(keep);
        for name in self.names.drain(..excess) {
            // The terminal unlinks objects it consumed; ENOENT is the normal case.
            let _ = rustix::shm::unlink(name.as_str());
        }
        let excess = self.files.len().saturating_sub(keep);
        for path in self.files.drain(..excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// kitty only deletes `t=t` files whose path contains this marker. The name
/// is predictable, so the file is created new (never through a symlink or
/// over a file someone else put there) and readable by us alone.
fn write_temp_file(data: &[u8]) -> io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("tty-graphics-protocol-termpix-{}", unique_suffix()));
    create_private(&path)?.write_all(data)?;
    Ok(path)
}

/// A new file only we can read; fails if anything (a symlink too) is there.
fn create_private(path: &std::path::Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &[u8]) -> String {
        String::from_utf8(b.to_vec()).unwrap()
    }

    #[test]
    fn what_queries_and_file_transfers_leave_behind_is_removed() {
        let mut out = Vec::new();
        let Some(Leftover::File(path)) = query(&mut out, 1, Medium::TempFile).unwrap() else { panic!() };
        assert!(path.exists());
        Leftover::File(path.clone()).remove();
        assert!(!path.exists());
        let Some(Leftover::Shm(name)) = query(&mut out, 2, Medium::SharedMemory).unwrap() else { panic!() };
        Leftover::Shm(name.clone()).remove();
        assert!(rustix::shm::open(name.as_str(), rustix::shm::OFlags::RDONLY, rustix::fs::Mode::empty()).is_err());
        assert!(query(&mut out, 3, Medium::Direct { compress: false }).unwrap().is_none());
        // Files a terminal did not take are cleaned up like shm objects.
        let mut enc = Encoder::new(Medium::TempFile);
        let px = Pixels { width: 1, height: 1, format: PixelFormat::Rgba, data: &[0, 0, 0, 255] };
        enc.transmit(&mut out, 9, px, None).unwrap();
        let sent = enc.shm.files[0].clone();
        assert!(sent.exists());
        enc.cleanup_shm(0);
        assert!(!sent.exists());
    }

    #[test]
    fn temp_files_are_new_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = write_temp_file(b"x").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_file(&path).unwrap();
        // A symlink planted under the name is not followed.
        let planted = std::env::temp_dir().join(format!("termpix-planted-{}", std::process::id()));
        let target = std::env::temp_dir().join(format!("termpix-target-{}", std::process::id()));
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &planted).unwrap();
        assert!(create_private(&planted).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        std::fs::remove_file(&planted).unwrap();
        std::fs::remove_file(&target).unwrap();
    }

    #[test]
    fn place_command() {
        let enc = Encoder::new(Medium::Direct { compress: false });
        let mut out = Vec::new();
        let p = Placement {
            placement_id: 3,
            src: Some(Rect::new(10, 20, 100, 50)),
            cells: None,
            offset: (4, 7),
            z: -1073741825,
            virtual_placement: false,
        };
        enc.place(&mut out, 9, &p);
        assert_eq!(s(&out), "\x1b_Ga=p,i=9,q=1,p=3,x=10,y=20,w=100,h=50,X=4,Y=7,z=-1073741825,C=1\x1b\\");
    }

    #[test]
    fn small_direct_transmit_is_one_command() {
        let mut enc = Encoder::new(Medium::Direct { compress: false });
        let mut out = Vec::new();
        let data = [255u8, 0, 0, 255];
        enc.transmit(&mut out, 1, Pixels { width: 1, height: 1, format: PixelFormat::Rgba, data: &data }, None)
            .unwrap();
        assert_eq!(s(&out), "\x1b_Ga=t,i=1,f=32,s=1,v=1,q=1;/wAA/w==\x1b\\");
    }

    #[test]
    fn large_direct_transmit_is_chunked() {
        let mut enc = Encoder::new(Medium::Direct { compress: false });
        let mut out = Vec::new();
        let data = vec![7u8; 64 * 64 * 4]; // 16 KiB -> ~21.8 KiB base64 -> 6 chunks
        enc.transmit(&mut out, 2, Pixels { width: 64, height: 64, format: PixelFormat::Rgba, data: &data }, None)
            .unwrap();
        let text = s(&out);
        let cmds: Vec<&str> = text.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert_eq!(cmds.len(), 6);
        assert!(cmds[0].starts_with("\x1b_Ga=t,i=2,f=32,s=64,v=64,q=1,m=1;"));
        assert!(cmds[1].starts_with("\x1b_Gm=1;"));
        assert!(cmds[5].starts_with("\x1b_Gm=0;"));
        for c in &cmds[..5] {
            assert_eq!(c.split_once(';').unwrap().1.len(), CHUNK);
        }
        // Reassembled payload decodes back to the pixels.
        let joined: String = cmds.iter().map(|c| c.split_once(';').unwrap().1).collect();
        assert_eq!(B64.decode(joined).unwrap(), data);
    }

    #[test]
    fn compressed_transmit_round_trips() {
        let mut enc = Encoder::new(Medium::Direct { compress: true });
        let mut out = Vec::new();
        let data = vec![1u8; 10 * 10 * 3];
        let place = Placement { placement_id: 1, ..Default::default() };
        enc.transmit(
            &mut out,
            5,
            Pixels { width: 10, height: 10, format: PixelFormat::Rgb, data: &data },
            Some(&place),
        )
        .unwrap();
        let text = s(&out);
        assert!(text.starts_with("\x1b_Ga=T,i=5,f=24,s=10,v=10,q=1,p=1,C=1,o=z;"));
        let payload = text.trim_end_matches("\x1b\\").split_once(';').unwrap().1;
        let compressed = B64.decode(payload).unwrap();
        let mut dec = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut back = Vec::new();
        io::Read::read_to_end(&mut dec, &mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn zlib_level_is_configurable() {
        let data = vec![255u8; 129 * 128 * 4];
        let px = Pixels { width: 129, height: 128, format: PixelFormat::Rgba, data: &data };
        let mut payloads = Vec::new();
        for level in [1, DEFAULT_ZLIB_LEVEL, 6] {
            let mut enc = Encoder::new(Medium::Direct { compress: true });
            enc.set_zlib_level(level);
            assert_eq!(enc.zlib_level(), level);
            let mut out = Vec::new();
            enc.transmit(&mut out, 1, px, None).unwrap();
            let text = s(&out);
            let payload: String = text.split("\x1b\\").filter_map(|c| c.split_once(';')).map(|(_, p)| p).collect();
            let compressed = B64.decode(payload).unwrap();
            let mut back = Vec::new();
            io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&compressed[..]), &mut back).unwrap();
            assert_eq!(back, data);
            payloads.push(compressed);
        }
        // Level 1 is a different encoding (fixed-Huffman blocks: BTYPE=01 in the
        // first deflate byte after the 2-byte zlib header).
        assert_eq!((payloads[0][2] >> 1) & 3, 1);
        assert_ne!((payloads[1][2] >> 1) & 3, 1);
    }

    #[test]
    fn rejects_wrong_buffer_size() {
        let mut enc = Encoder::new(Medium::Direct { compress: false });
        let err = enc
            .transmit(
                &mut Vec::new(),
                1,
                Pixels { width: 2, height: 2, format: PixelFormat::Rgba, data: &[0; 3] },
                None,
            )
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn delete_commands() {
        let enc = Encoder::new(Medium::Direct { compress: false });
        let mut out = Vec::new();
        enc.delete(&mut out, Delete::Image { id: 4, free: true });
        enc.delete(&mut out, Delete::Placement { id: 4, placement_id: 2 });
        assert_eq!(s(&out), "\x1b_Ga=d,d=I,i=4,q=2\x1b\\\x1b_Ga=d,d=i,i=4,p=2,q=2\x1b\\");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_memory_transmit_and_cleanup() {
        let mut enc = Encoder::new(Medium::SharedMemory);
        let mut out = Vec::new();
        let data = [9u8; 2 * 2 * 4];
        enc.transmit(&mut out, 1, Pixels { width: 2, height: 2, format: PixelFormat::Rgba, data: &data }, None)
            .unwrap();
        let text = s(&out);
        assert!(text.contains(",t=s,S=16;"));
        let name_b64 = text.trim_end_matches("\x1b\\").split_once(';').unwrap().1;
        let name = String::from_utf8(B64.decode(name_b64).unwrap()).unwrap();
        let on_disk = std::path::Path::new("/dev/shm").join(name.trim_start_matches('/'));
        assert_eq!(std::fs::read(&on_disk).unwrap(), data);
        enc.cleanup_shm(0);
        assert!(!on_disk.exists());
    }
}
