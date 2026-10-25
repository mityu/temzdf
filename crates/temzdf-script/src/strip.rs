//! TypeScript to JavaScript by type stripping (swc's fast strip): types are
//! replaced by whitespace, so lines and columns of the output match the
//! source and errors point at what the user wrote. Only erasable syntax is
//! accepted (as TypeScript's `erasableSyntaxOnly`); `enum`, `namespace` with
//! values and parameter properties are errors.

use std::sync::{Arc, Mutex};

use swc_common::errors::{DiagnosticBuilder, Emitter, Handler};
use swc_common::sync::Lrc;
use swc_common::{BytePos, SourceMap};
use swc_ts_fast_strip::{Mode, Options, operate};

/// Strip `source` (named `name` in messages). The error lists each problem as
/// `name:line:column: message`.
pub fn strip(name: &str, source: &str) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Default::default();
    let found: Found = Arc::default();
    let handler = Handler::with_emitter(true, false, Box::new(Collect(found.clone())));
    let options =
        Options { module: Some(true), filename: Some(name.to_owned()), mode: Mode::StripOnly, ..Default::default() };
    let result = swc_common::GLOBALS.set(&Default::default(), || operate(&cm, &handler, source.to_owned(), options));
    let problems: Vec<String> = found
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .map(|(message, pos)| match pos {
            Some(pos) => {
                let loc = cm.lookup_char_pos(*pos);
                format!("{name}:{}:{}: {message}", loc.line, loc.col_display + 1)
            }
            None => format!("{name}: {message}"),
        })
        .collect();
    match result {
        Ok(out) if problems.is_empty() => Ok(out.code),
        Ok(_) => Err(problems.join("\n")),
        Err(e) if problems.is_empty() => Err(format!("{name}: {e}")),
        Err(_) => Err(problems.join("\n")),
    }
}

/// Diagnostics as message and position.
type Found = Arc<Mutex<Vec<(String, Option<BytePos>)>>>;

/// Collects diagnostics instead of printing them.
struct Collect(Found);

impl Emitter for Collect {
    fn emit(&mut self, db: &mut DiagnosticBuilder<'_>) {
        let pos = db.span.primary_span().map(|s| s.lo);
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push((db.message(), pos));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_become_whitespace() {
        let src = "const n: number = 1;\nfunction f(a: string): void {}\ntype T = { x: number };\n";
        let out = strip("c.ts", src).unwrap();
        assert_eq!(out.lines().count(), src.lines().count(), "lines kept:\n{out}");
        assert_eq!(out.lines().next().unwrap().trim_end(), "const n         = 1;");
        assert!(!out.contains("number") && !out.contains("string"), "{out}");
    }

    #[test]
    fn non_erasable_syntax_is_an_error_with_a_position() {
        let e = strip("c.ts", "const a = 1;\nenum E { A }\n").unwrap_err();
        assert!(e.starts_with("c.ts:2:"), "{e}");
        let e = strip("c.ts", "let = = ;").unwrap_err();
        assert!(e.contains("c.ts:1:"), "{e}");
    }
}
