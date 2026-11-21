//! Lists the session takes from the declarations written by hand, so the
//! two cannot drift apart:
//!
//! - its actions (crates/temzdf-script/ts-types/temzdf-actions.d.ts): every
//!   method of `interface Actions` in `declare module "temzdf"`, `moveFocus`
//!   as `move-focus`, and the methods of a property's object type under its
//!   name (`visual: { left(): void }` is `visual.left`), written to
//!   `OUT_DIR/actions.rs`;
//! - the events scripts hook and the options they set
//!   (crates/temzdf-script/ts-types/temzdf-core.d.ts): the property names of
//!   `interface Events` and `interface Options` in the same module, written
//!   to `OUT_DIR/events.rs` and `OUT_DIR/options.rs`.
//!
//! Each file is a `&[&str]` expression, in the declaration's order.

use std::path::{Path, PathBuf};

use swc_common::sync::Lrc;
use swc_common::{FileName, SourceMap};
use swc_ecma_ast::{
    Decl, Module, ModuleItem, Stmt, TsModuleName, TsNamespaceBody, TsPropertySignature, TsType, TsTypeElement,
};
use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_module};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let types = manifest.join("../temzdf-script/ts-types");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let path = types.join("temzdf-actions.d.ts");
    let module = parse(&path);
    let names = interface(&module, "Actions").and_then(actions).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    write(&out.join("actions.rs"), &names);

    let path = types.join("temzdf-core.d.ts");
    let module = parse(&path);
    for (name, file) in [("Events", "events.rs"), ("Options", "options.rs")] {
        let names = interface(&module, name).and_then(properties).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        write(&out.join(file), &names);
    }
}

fn parse(path: &Path) -> Module {
    println!("cargo::rerun-if-changed={}", path.display());
    let source = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let cm: Lrc<SourceMap> = Default::default();
    let file = cm.new_source_file(Lrc::new(FileName::Real(path.to_owned())), source);
    let syntax = Syntax::Typescript(TsSyntax { dts: true, ..Default::default() });
    parse_file_as_module(&file, syntax, Default::default(), None, &mut Vec::new())
        .unwrap_or_else(|e| panic!("{}: cannot parse: {e:?}", path.display()))
}

fn write(path: &Path, names: &[String]) {
    let list: String = names.iter().map(|n| format!("    {n:?},\n")).collect();
    std::fs::write(path, format!("&[\n{list}]\n")).unwrap();
}

/// The members of `interface NAME` in `declare module "temzdf"`.
fn interface<'a>(module: &'a Module, wanted: &str) -> Result<&'a [TsTypeElement], String> {
    let mut found = None;
    for item in &module.body {
        let ModuleItem::Stmt(Stmt::Decl(Decl::TsModule(m))) = item else { continue };
        let TsModuleName::Str(name) = &m.id else { continue };
        if name.value.as_str() != Some("temzdf") {
            continue;
        }
        let Some(TsNamespaceBody::TsModuleBlock(block)) = &m.body else { continue };
        for item in &block.body {
            let decl = match item {
                ModuleItem::ModuleDecl(swc_ecma_ast::ModuleDecl::ExportDecl(e)) => &e.decl,
                ModuleItem::Stmt(Stmt::Decl(d)) => d,
                _ => continue,
            };
            if let Decl::TsInterface(i) = decl
                && i.id.sym == *wanted
            {
                found = Some(&i.body.body[..]);
            }
        }
    }
    found.ok_or_else(|| format!("no `interface {wanted}` in `declare module \"temzdf\"`"))
}

/// The actions `interface Actions` declares, in its order.
fn actions(members: &[TsTypeElement]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for m in members {
        match m {
            TsTypeElement::TsMethodSignature(s) => names.push(kebab(&key(&s.key)?)),
            TsTypeElement::TsPropertySignature(TsPropertySignature { key: k, type_ann: Some(t), .. }) => {
                let TsType::TsTypeLit(lit) = &*t.type_ann else {
                    return Err(format!("{}: a property of Actions is an object of methods", key(k)?));
                };
                let ns = kebab(&key(k)?);
                for m in &lit.members {
                    let TsTypeElement::TsMethodSignature(s) = m else {
                        return Err(format!("{ns}: only methods in its object"));
                    };
                    names.push(format!("{ns}.{}", kebab(&key(&s.key)?)));
                }
            }
            _ => return Err("Actions has methods and objects of methods only".into()),
        }
    }
    Ok(names)
}

/// The property names of an interface of properties only (Events, Options).
fn properties(members: &[TsTypeElement]) -> Result<Vec<String>, String> {
    members
        .iter()
        .map(|m| match m {
            TsTypeElement::TsPropertySignature(s) => key(&s.key),
            _ => Err("an interface of properties only".into()),
        })
        .collect()
}

fn key(k: &swc_ecma_ast::Expr) -> Result<String, String> {
    match k {
        swc_ecma_ast::Expr::Ident(i) => Ok(i.sym.to_string()),
        _ => Err("a member is named by an identifier".into()),
    }
}

/// `moveFocus` -> `move-focus`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
