//! The session's actions by name, from the declarations written by hand
//! (crates/temzdf-script/ts-types/temzdf-actions.d.ts): every method of
//! `interface Actions` in `declare module "temzdf"`, `moveFocus` as
//! `move-focus`, and the methods of a property's object type under its
//! name (`visual: { left(): void }` is `visual.left`). Written to
//! `OUT_DIR/actions.rs` as `&[&str]`.

use std::path::PathBuf;

use swc_common::sync::Lrc;
use swc_common::{FileName, SourceMap};
use swc_ecma_ast::{
    Decl, Module, ModuleItem, Stmt, TsModuleName, TsNamespaceBody, TsPropertySignature, TsType, TsTypeElement,
};
use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_module};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let path = manifest.join("../temzdf-script/ts-types/temzdf-actions.d.ts");
    println!("cargo::rerun-if-changed={}", path.display());
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let cm: Lrc<SourceMap> = Default::default();
    let file = cm.new_source_file(Lrc::new(FileName::Real(path.clone())), source);
    let syntax = Syntax::Typescript(TsSyntax { dts: true, ..Default::default() });
    let module = parse_file_as_module(&file, syntax, Default::default(), None, &mut Vec::new())
        .unwrap_or_else(|e| panic!("{}: cannot parse: {e:?}", path.display()));
    let names = actions(&module).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let list: String = names.iter().map(|n| format!("    {n:?},\n")).collect();
    std::fs::write(out.join("actions.rs"), format!("&[\n{list}]\n")).unwrap();
}

/// The actions `interface Actions` declares, in its order.
fn actions(module: &Module) -> Result<Vec<String>, String> {
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
                && i.id.sym == "Actions"
            {
                found = Some(&i.body.body);
            }
        }
    }
    let members = found.ok_or("no `interface Actions` in `declare module \"temzdf\"`")?;
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

fn key(k: &swc_ecma_ast::Expr) -> Result<String, String> {
    match k {
        swc_ecma_ast::Expr::Ident(i) => Ok(i.sym.to_string()),
        _ => Err("an action is named by an identifier".into()),
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
