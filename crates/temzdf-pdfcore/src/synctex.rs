//! SyncTeX: map source lines to PDF positions (forward search) and back
//! (inverse search), through the vendored parser library
//! (`vendor/synctex/`). See doc/synctex.md.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::path::{Path, PathBuf};

// The parser needs zlib; make sure it is linked.
use libz_sys as _;

use crate::pdf::PageRect;

#[allow(non_camel_case_types)]
type synctex_scanner_p = *mut c_void;
#[allow(non_camel_case_types)]
type synctex_node_p = *mut c_void;

unsafe extern "C" {
    fn synctex_scanner_new_with_output_file(
        output: *const c_char,
        build_directory: *const c_char,
        parse: c_int,
    ) -> synctex_scanner_p;
    fn synctex_scanner_free(scanner: synctex_scanner_p) -> c_int;
    fn synctex_display_query(
        scanner: synctex_scanner_p,
        name: *const c_char,
        line: c_int,
        column: c_int,
        page_hint: c_int,
    ) -> c_long;
    fn synctex_edit_query(scanner: synctex_scanner_p, page: c_int, h: f32, v: f32) -> c_long;
    fn synctex_scanner_next_result(scanner: synctex_scanner_p) -> synctex_node_p;
    fn synctex_scanner_get_name(scanner: synctex_scanner_p, tag: c_int) -> *const c_char;
    fn synctex_node_page(node: synctex_node_p) -> c_int;
    fn synctex_node_box_visible_h(node: synctex_node_p) -> f32;
    fn synctex_node_box_visible_v(node: synctex_node_p) -> f32;
    fn synctex_node_box_visible_width(node: synctex_node_p) -> f32;
    fn synctex_node_box_visible_height(node: synctex_node_p) -> f32;
    fn synctex_node_box_visible_depth(node: synctex_node_p) -> f32;
    fn synctex_node_tag(node: synctex_node_p) -> c_int;
    fn synctex_node_line(node: synctex_node_p) -> c_int;
    fn synctex_node_column(node: synctex_node_p) -> c_int;
}

/// A place in a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based; 0 when unknown (TeX rarely records columns).
    pub column: u32,
}

/// A region of the PDF produced by some source line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfBox {
    /// 0-based page index.
    pub page: usize,
    pub rect: PageRect,
}

/// Parsed SyncTeX data for one PDF. Queries mutate the parser's internal
/// iterator, hence `&mut self`.
pub struct Scanner {
    ptr: synctex_scanner_p,
    /// Directory of the PDF; relative source names resolve against it.
    base: PathBuf,
}

// SAFETY: the scanner is only ever used from one thread at a time (it is owned,
// and all methods take `&mut self`); the library keeps no thread-local state.
unsafe impl Send for Scanner {}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from synctex_scanner_new_with_output_file and is freed once.
        unsafe { synctex_scanner_free(self.ptr) };
    }
}

impl Scanner {
    /// Load the SyncTeX file belonging to `pdf` (`name.synctex.gz` or
    /// `name.synctex` next to it). `None` if there is none or it cannot be parsed.
    pub fn open(pdf: &Path) -> Option<Self> {
        let pdf = std::path::absolute(pdf).ok()?;
        let c = CString::new(pdf.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: valid NUL-terminated path; a null build directory is allowed.
        let ptr = unsafe { synctex_scanner_new_with_output_file(c.as_ptr(), std::ptr::null(), 1) };
        if ptr.is_null() {
            return None;
        }
        let base = pdf.parent().map(Path::to_path_buf).unwrap_or_default();
        Some(Self { ptr, base })
    }

    /// Forward search: the boxes typeset from `line` (1-based) of `source`. The
    /// first box is the best match. `page_hint` (0-based) breaks ties.
    pub fn forward(&mut self, source: &Path, line: u32, column: u32, page_hint: Option<usize>) -> Vec<PdfBox> {
        let mut out = Vec::new();
        // Query with the name exactly as SyncTeX recorded it (see resolve_input),
        // falling back to the name as given.
        let mut names = Vec::new();
        names.extend(self.resolve_input(source));
        names.push(source.as_os_str().to_owned());
        for name in names {
            let Ok(c) = CString::new(name.as_encoded_bytes()) else { continue };
            let hint = page_hint.map_or(-1, |p| p as c_int + 1);
            // SAFETY: valid scanner and NUL-terminated name.
            let n = unsafe { synctex_display_query(self.ptr, c.as_ptr(), line as c_int, column as c_int, hint) };
            if n <= 0 {
                continue;
            }
            loop {
                // SAFETY: valid scanner; nodes stay valid until the next query.
                let node = unsafe { synctex_scanner_next_result(self.ptr) };
                if node.is_null() {
                    break;
                }
                // SAFETY: node is a live result node.
                let (page, h, v, w, ht, d) = unsafe {
                    (
                        synctex_node_page(node),
                        synctex_node_box_visible_h(node),
                        synctex_node_box_visible_v(node),
                        synctex_node_box_visible_width(node),
                        synctex_node_box_visible_height(node),
                        synctex_node_box_visible_depth(node),
                    )
                };
                if page < 1 {
                    continue;
                }
                // h/v: left edge and baseline, in points from the page's top-left.
                let rect = PageRect { x0: h, y0: v - ht, x1: h + w.abs(), y1: v + d };
                out.push(PdfBox { page: page as usize - 1, rect });
            }
            break;
        }
        out
    }

    /// Input file names recorded in the SyncTeX file (tags start at 1).
    fn inputs(&self) -> Vec<String> {
        (1..)
            .map_while(|tag| {
                // SAFETY: valid scanner; names are owned by it and NUL-terminated.
                let p = unsafe { synctex_scanner_get_name(self.ptr, tag) };
                (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// The recorded input name that `source` refers to. Editors pass absolute
    /// paths, users relative ones, and SyncTeX records names the way TeX opened
    /// them (`./ch1.tex`, `/abs/dir/./main.tex`, ...). Prefer the same absolute
    /// path, then a recorded name ending with the given path's components, then
    /// the same file name.
    fn resolve_input(&self, source: &Path) -> Option<std::ffi::OsString> {
        let inputs = self.inputs();
        let abs = |p: &Path| normalize(&if p.is_absolute() { p.to_owned() } else { self.base.join(p) });
        let wanted = normalize(&std::path::absolute(source).unwrap_or_else(|_| source.to_owned()));
        let given = normalize(source);
        let pick = inputs
            .iter()
            .find(|n| abs(Path::new(n)) == wanted)
            .or_else(|| inputs.iter().find(|n| abs(Path::new(n)).ends_with(&given)))
            .or_else(|| inputs.iter().find(|n| Path::new(n).file_name() == source.file_name()));
        pick.map(Into::into)
    }

    /// Inverse search: the source location that produced the point (`x`, `y`,
    /// in points from the top-left) on `page` (0-based).
    pub fn inverse(&mut self, page: usize, x: f32, y: f32) -> Option<SourceLocation> {
        // SAFETY: valid scanner.
        let n = unsafe { synctex_edit_query(self.ptr, page as c_int + 1, x, y) };
        if n <= 0 {
            return None;
        }
        // SAFETY: valid scanner; the node is a live result.
        let node = unsafe { synctex_scanner_next_result(self.ptr) };
        if node.is_null() {
            return None;
        }
        // SAFETY: as above; the returned name is owned by the scanner.
        let (tag, line, column) =
            unsafe { (synctex_node_tag(node), synctex_node_line(node), synctex_node_column(node)) };
        let name = unsafe { synctex_scanner_get_name(self.ptr, tag) };
        if name.is_null() || line < 1 {
            return None;
        }
        // SAFETY: non-null, NUL-terminated, owned by the scanner.
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        let path = Path::new(&name);
        let file = if path.is_absolute() { path.to_owned() } else { self.base.join(path) };
        Some(SourceLocation { file: normalize(&file), line: line as u32, column: column.max(0) as u32 })
    }
}

/// Remove `.` components (SyncTeX records names like `/dir/./main.tex`).
fn normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;

    fn line_of(text: &str, needle: &str) -> u32 {
        text.lines().position(|l| l.contains(needle)).expect("needle in fixture") as u32 + 1
    }

    #[test]
    fn forward_and_inverse_on_the_fixture() {
        let pdf_path = pdf::fixture("sample.pdf");
        let tex = std::fs::read_to_string(pdf::fixture("sample.tex")).unwrap();
        let mut s = Scanner::open(&pdf_path).expect("fixture has a synctex file");

        // "Back to Section" is in the second filler subsection, on page 2.
        let line = line_of(&tex, "Back to Section");
        let boxes = s.forward(Path::new("sample.tex"), line, 0, None);
        assert!(!boxes.is_empty());
        let b = boxes[0];
        assert_eq!(b.page, 1, "{boxes:?}");
        assert!(b.rect.y0 > 0.0 && b.rect.y1 < 842.0 && b.rect.x1 > b.rect.x0, "{b:?}");

        // The same line found through other spellings of the source path.
        for name in [pdf::fixture("sample.tex"), "tests/fixtures/sample.tex".into(), "fixtures/sample.tex".into()] {
            assert_eq!(s.forward(&name, line, 0, None).first().map(|b| b.page), Some(1), "{name:?}");
        }

        // Inverse from the middle of that box leads back to the same line.
        let loc = s.inverse(b.page, (b.rect.x0 + b.rect.x1) / 2.0, (b.rect.y0 + b.rect.y1) / 2.0).unwrap();
        assert_eq!(loc.line, line);
        assert_eq!(loc.file, pdf::fixture("sample.tex").components().collect::<PathBuf>());
    }

    #[test]
    fn inverse_on_the_title_points_at_maketitle() {
        let tex = std::fs::read_to_string(pdf::fixture("sample.tex")).unwrap();
        let mut s = Scanner::open(&pdf::fixture("sample.pdf")).unwrap();
        // The title is centred near the top of page 1 (about 127 pt down).
        let loc = s.inverse(0, 297.0, 127.0).expect("a source location for the title");
        // SyncTeX attributes the title block to \maketitle or the line after it.
        let line = line_of(&tex, "\\maketitle");
        assert!((line..=line + 1).contains(&loc.line), "{loc:?}");
    }

    #[test]
    fn missing_synctex_is_none() {
        let dir = std::env::temp_dir().join(format!("temzdf-nosynctex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.pdf");
        std::fs::copy(pdf::fixture("sample.pdf"), &p).unwrap();
        assert!(Scanner::open(&p).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
