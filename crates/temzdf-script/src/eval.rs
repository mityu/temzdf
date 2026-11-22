//! `temzdf eval`'s code as a function the host can run: each step (a
//! `-c`, or the file) is stripped of its types and made the body of an
//! async function inside `with (scope)`, so that steps share their
//! top-level declarations through `scope` (one object per `temzdf eval`,
//! dropped with it: nothing reaches the globals) and may `await`. See
//! doc/design/script-host.md, "temzdf eval".
//!
//! Only top-level statements change, text spliced into the source (lines
//! kept): `const`/`let`/`var` become assignments to names put in `scope`
//! first, functions and classes are copied into `scope`, `import` becomes
//! `await import(...)` (`"temzdf"` is the eval's own view of the API), and
//! a last expression statement is returned (the value `-p` prints).

use swc_common::sync::Lrc;
use swc_common::{BytePos, FileName, SourceMap, Spanned};
use swc_ecma_ast::{Decl, ImportSpecifier, ModuleDecl, ModuleItem, ObjectPatProp, Pat, Stmt, VarDeclKind};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

/// The scope object's parameter, and the eval's view of `temzdf` (what
/// `import ... from "temzdf"` gives it): names the code could see, so
/// unlikely ones.
const SCOPE: &str = "__temzdfScope";
const VIEW: &str = "__temzdfView";

/// A step as JavaScript: an expression giving `(scope, view) => () =>
/// Promise`, and how to give its errors the step's own positions.
#[derive(Debug)]
pub struct Compiled {
    pub source: String,
    pub positions: Positions,
}

/// What moved columns of the source: the wrapper before the step's first
/// line, and text inserted (line, column where, characters), in order.
#[derive(Debug, Default)]
pub struct Positions {
    prefix: usize,
    inserted: Vec<(usize, usize, usize)>,
}

/// Compile step `name`: strip its types, rewrite its top level. A problem
/// is `name:line:column: message`.
pub fn compile(name: &str, code: &str) -> Result<Compiled, String> {
    let js = crate::strip::strip(name, code)?;
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(Lrc::new(FileName::Custom(name.to_owned())), js.clone());
    let at = |pos: BytePos| {
        let loc = cm.lookup_char_pos(pos);
        format!("{name}:{}:{}", loc.line, loc.col_display + 1)
    };
    let mut parser = Parser::new(Syntax::Es(EsSyntax::default()), StringInput::from(&*fm), None);
    let parsed = parser.parse_module();
    let mut errors = parser.take_errors();
    let module = match parsed {
        Ok(m) if errors.is_empty() => m,
        Ok(_) => return Err(format!("{}: {}", at(errors[0].span().lo), errors.remove(0).kind().msg())),
        Err(e) => return Err(format!("{}: {}", at(e.span().lo), e.kind().msg())),
    };
    let start = fm.start_pos;
    let off = |pos: BytePos| (pos - start).0 as usize;
    let text = |lo: BytePos, hi: BytePos| &js[off(lo)..off(hi)];
    // Replacements of byte ranges of `js` (none overlap), and the names
    // the step declares.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut functions: Vec<String> = Vec::new();
    let last = module.body.len().checked_sub(1);
    for (i, item) in module.body.iter().enumerate() {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(v))) => {
                for d in &v.decls {
                    bound_names(&d.name, &mut names);
                }
                // `const a = 1, {b} = o;` as `;(  a = 1, {b} = o);`, the
                // keyword's columns kept.
                let kw = match v.kind {
                    VarDeclKind::Const => 5,
                    VarDeclKind::Let | VarDeclKind::Var => 3,
                };
                let lo = off(v.span.lo);
                edits.push((lo, lo + kw, format!(";({}", " ".repeat(kw - 2))));
                if let Some(d) = v.decls.last() {
                    let hi = off(d.span.hi);
                    edits.push((hi, hi, ")".into()));
                }
            }
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(f))) => functions.push(f.ident.sym.to_string()),
            ModuleItem::Stmt(Stmt::Decl(Decl::Class(c))) => {
                let n = c.ident.sym.to_string();
                let hi = off(c.span().hi);
                edits.push((hi, hi, format!(";{SCOPE}.{n} = {n};")));
            }
            ModuleItem::Stmt(Stmt::Expr(e)) if Some(i) == last => {
                let span = e.expr.span();
                edits.push((off(span.lo), off(span.lo), "return (".into()));
                edits.push((off(span.hi), off(span.hi), ")".into()));
            }
            ModuleItem::Stmt(_) => {}
            ModuleItem::ModuleDecl(ModuleDecl::Import(d)) => {
                if d.type_only {
                    continue;
                }
                let src = text(d.src.span.lo, d.src.span.hi);
                let from = if &*d.src.value == "temzdf" { VIEW.to_owned() } else { format!("await import({src})") };
                let (mut default, mut namespace, mut named) = (None, None, Vec::new());
                for s in &d.specifiers {
                    let local = s.local().sym.to_string();
                    names.push(local.clone());
                    match s {
                        ImportSpecifier::Default(_) => default = Some(local),
                        ImportSpecifier::Namespace(_) => namespace = Some(local),
                        ImportSpecifier::Named(n) if n.is_type_only => {
                            names.pop();
                        }
                        ImportSpecifier::Named(n) => {
                            let imported = n.imported.as_ref().map(|m| text(m.span().lo, m.span().hi).to_owned());
                            named.push(match imported {
                                Some(key) => format!("{key}: {local}"),
                                None => local,
                            });
                        }
                    }
                }
                let assign = match (namespace, default) {
                    (Some(ns), Some(def)) => format!("{ns} = {from}, {def} = {ns}.default"),
                    (Some(ns), None) => format!("{ns} = {from}"),
                    (None, def) => {
                        named.extend(def.map(|d| format!("default: {d}")));
                        if named.is_empty() { from } else { format!("{{{}}} = {from}", named.join(", ")) }
                    }
                };
                // As many lines as the import had.
                let lines = text(d.span.lo, d.span.hi).matches('\n').count();
                edits.push((off(d.span.lo), off(d.span.hi), format!(";({assign});{}", "\n".repeat(lines))));
            }
            ModuleItem::ModuleDecl(d) => {
                return Err(format!("{}: temzdf eval's code exports nothing", at(d.span().lo)));
            }
        }
    }
    edits.sort_by_key(|e| (e.0, e.1));
    let mut inserted = Vec::new();
    for (lo, hi, with) in &edits {
        let (old, new) = (js[*lo..*hi].chars().count(), with.chars().count());
        if new > old && !with.contains('\n') && !js[*lo..*hi].contains('\n') {
            let line_start = js[..*lo].rfind('\n').map_or(0, |i| i + 1);
            let line = js[..*lo].matches('\n').count() + 1;
            inserted.push((line, js[line_start..*lo].chars().count() + 1 + old, new - old));
        }
    }
    let mut body = String::with_capacity(js.len() + 64);
    let mut pos = 0;
    for (lo, hi, with) in edits {
        body.push_str(&js[pos..lo]);
        body.push_str(&with);
        pos = hi;
    }
    body.push_str(&js[pos..]);
    names.extend(functions.iter().cloned());
    let declare = serde_json::to_string(&names).unwrap_or_else(|_| "[]".into());
    let exports: String = functions.iter().map(|f| format!("{SCOPE}.{f} = {f}; ")).collect();
    // One line before the step's first: positions in it stay its own
    // (strict mode inside; `with` is only allowed around it).
    let prefix = format!(
        "(function ({SCOPE}, {VIEW}) {{ for (const n of {declare}) if (!(n in {SCOPE})) {SCOPE}[n] = undefined; \
         with ({SCOPE}) {{ return async function () {{ \"use strict\"; {exports}"
    );
    let source = format!("{prefix}{body}\n}}; }} }})");
    Ok(Compiled { source, positions: Positions { prefix: prefix.chars().count(), inserted } })
}

/// The names a declaration's pattern binds.
fn bound_names(p: &Pat, out: &mut Vec<String>) {
    match p {
        Pat::Ident(i) => out.push(i.id.sym.to_string()),
        Pat::Array(a) => a.elems.iter().flatten().for_each(|e| bound_names(e, out)),
        Pat::Rest(r) => bound_names(&r.arg, out),
        Pat::Assign(a) => bound_names(&a.left, out),
        Pat::Object(o) => {
            for prop in &o.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => bound_names(&kv.value, out),
                    ObjectPatProp::Assign(a) => out.push(a.key.id.sym.to_string()),
                    ObjectPatProp::Rest(r) => bound_names(&r.arg, out),
                }
            }
        }
        Pat::Invalid(_) | Pat::Expr(_) => {}
    }
}

impl Positions {
    /// Positions in `text` (an error's message and stack) of step `name`,
    /// `NAME:LINE:COLUMN`, as the step has them.
    pub fn fix(&self, text: &str, name: &str) -> String {
        let needle = format!("{name}:");
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(&needle) {
            out.push_str(&rest[..i + needle.len()]);
            rest = &rest[i + needle.len()..];
            let number = |s: &str| {
                let n = s.bytes().take_while(u8::is_ascii_digit).count();
                s[..n].parse::<usize>().ok().map(|v| (v, n))
            };
            let Some((line, n)) = number(rest) else { continue };
            let Some((col, m)) = rest[n..].strip_prefix(':').and_then(number) else { continue };
            out.push_str(&format!("{line}:{}", self.column(line, col)));
            rest = &rest[n + 1 + m..];
        }
        out.push_str(rest);
        out
    }

    /// Column `col` of the compiled source's line `line` in the step.
    fn column(&self, line: usize, col: usize) -> usize {
        let mut col = if line == 1 { col.saturating_sub(self.prefix).max(1) } else { col };
        // Each insertion before it moved it right.
        let mut moved = 0;
        for &(_, at, len) in self.inserted.iter().filter(|i| i.0 == line) {
            if col >= at + moved + len {
                moved += len;
            } else {
                break;
            }
        }
        col -= moved;
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(code: &str) -> String {
        let c = compile("-c 1", code).unwrap();
        let s = &c.source[c.positions.prefix..];
        s[..s.rfind("\n};").unwrap()].to_owned()
    }

    #[test]
    fn top_level_declarations_become_assignments_to_the_scope() {
        assert_eq!(body("const a = 1, b = 2"), ";(    a = 1, b = 2)");
        assert_eq!(body("let {x, y: [z]} = o;"), ";(  {x, y: [z]} = o);");
        let c = compile("-c 1", "var q; function f() {}").unwrap();
        assert!(c.source.contains(r#"["q","f"]"#), "{}", c.source);
        assert!(c.source.contains("__temzdfScope.f = f;"), "{}", c.source);
        assert_eq!(body("class C {}"), "class C {};__temzdfScope.C = C;");
        // Not at the top level: as written.
        assert_eq!(body("{ const a = 1 }"), "{ const a = 1 }");
    }

    #[test]
    fn the_last_expression_is_returned() {
        assert_eq!(body("const a = 1; a + 1"), ";(    a = 1); return (a + 1)");
        assert_eq!(body("1;\nfoo();"), "1;\nreturn (foo());");
        assert_eq!(body("if (x) y()"), "if (x) y()");
    }

    #[test]
    fn imports_become_dynamic_imports_and_keep_their_lines() {
        assert_eq!(body(r#"import ff from "temzdf/ff""#), r#";({default: ff} = await import("temzdf/ff"));"#);
        assert_eq!(body(r#"import { action as a, o } from "temzdf""#), ";({action: a, o} = __temzdfView);");
        assert_eq!(body(r#"import * as t from "temzdf""#), ";(t = __temzdfView);");
        assert_eq!(body("import \"./x.js\""), r#";(await import("./x.js"));"#);
        assert_eq!(body("import {\n  a,\n} from './m'\nb"), ";({a} = await import('./m'));\n\n\nreturn (b)");
    }

    #[test]
    fn types_go_and_problems_say_where() {
        assert_eq!(body("const n: number = 1"), ";(    n         = 1)");
        let e = compile("-c 2", "let = ;").unwrap_err();
        assert!(e.starts_with("-c 2:1:"), "{e}");
        let e = compile("f.ts", "1;\nexport const a = 1;").unwrap_err();
        assert_eq!(e, "f.ts:2:1: temzdf eval's code exports nothing");
    }

    #[test]
    fn errors_get_the_steps_own_columns() {
        let c = compile("-c 1", "a;\n  x + nope()").unwrap();
        // `return (` went before `x`.
        let t = "Error: x\n    at <anonymous> (-c 1:2:15)\n    at f (-c 2:2:3)";
        assert_eq!(c.positions.fix(t, "-c 1"), "Error: x\n    at <anonymous> (-c 1:2:7)\n    at f (-c 2:2:3)");
        let p = c.positions.prefix;
        assert_eq!(c.positions.fix(&format!("(-c 1:1:{})", p + 2), "-c 1"), "(-c 1:1:2)");
    }
}
