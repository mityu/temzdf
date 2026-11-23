//! Bundles the shipped scripts (runtime/) with esbuild into `$OUT_DIR`, for
//! the host to embed (src/host.rs): `prelude.js` (the API's JavaScript
//! half, from src/prelude.ts), `polyfills.js` (globals scripts expect,
//! from npm packages), `defaults.js` (the default configuration) and the
//! modules scripts import as `temzdf/NAME` ([`MODULES`]), with
//! `bundled.rs` listing them for the host. `"temzdf"` and `"temzdf/NAME"`
//! stay imports (the host provides them: one instance of each module, so
//! defaults.ts and a configuration enable and disable the same).
//!
//! npm packages come from runtime/node_modules, which `npm ci` installs
//! from runtime/package-lock.json (refused when it no longer matches the
//! lock file), else from `$TEMZDF_NODE_MODULES`: the dev shells and `nix
//! build` set it to node_modules Nix built from the same lock file, so `nix
//! build` needs no network. esbuild comes from `PATH` (`$ESBUILD`
//! overrides). Only with the feature `host` (the type declarations alone
//! need none of this).
//!
//! Cargo runs this again when a file a bundle was made from changes (as
//! esbuild's metafile lists them), not for any file under runtime/: the
//! tests beside the scripts (`*.test.ts`) are not bundled, and editing
//! them rebuilds nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Modules scripts import as `temzdf/NAME`, and their entries in runtime/.
const MODULES: &[(&str, &str)] = &[
    ("edit/vim", "edit/vim.ts"),
    ("edit/notepad", "edit/notepad.ts"),
    ("pane/pdf", "pane/pdf.ts"),
    ("pane/terminal", "pane/terminal.ts"),
    ("pane/markdown", "pane/markdown.ts"),
    ("mode/normal", "mode/normal.ts"),
    ("mode/input", "mode/input.ts"),
    ("buffers", "buffers.ts"),
    ("command", "command.ts"),
    ("keyset", "lib/keyset.ts"),
    ("test", "test/index.ts"),
    ("ff", "ff/index.ts"),
    ("ff/builtin", "ff/builtin/index.ts"),
    ("ff/completion", "ff/completion/index.ts"),
    ("path", "path.ts"),
    ("path/posix", "path/posix.ts"),
    ("path/windows", "path/windows.ts"),
];

fn main() {
    if std::env::var_os("CARGO_FEATURE_HOST").is_none() {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let runtime = manifest.join("runtime");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    // What esbuild reads besides the sources (the sources: below).
    for file in ["package.json", "tsconfig.json"] {
        println!("cargo::rerun-if-changed={}", runtime.join(file).display());
    }
    let prelude = manifest.join("src/prelude.ts");
    println!("cargo::rerun-if-changed={}", prelude.display());
    let module = manifest.join("src/temzdf.ts");
    println!("cargo::rerun-if-changed={}", module.display());
    for var in ["TEMZDF_NODE_MODULES", "ESBUILD"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    // runtime/node_modules (npm's), else Nix's. NODE_PATH for the entries
    // outside runtime/ (src/), which esbuild would not find it from.
    let installed = runtime.join("node_modules");
    let modules = if installed.is_dir() {
        check_installed(&runtime);
        Some(installed)
    } else {
        std::env::var_os("TEMZDF_NODE_MODULES").map(PathBuf::from)
    };
    let esbuild = std::env::var_os("ESBUILD").unwrap_or_else(|| "esbuild".into());
    // The files the bundles were made from.
    let mut inputs = BTreeSet::new();
    let mut bundle = |entry: &Path, out_file: &str, format: &str| {
        let meta = out.join(format!("{out_file}.meta.json"));
        let mut cmd = Command::new(&esbuild);
        cmd.arg(entry)
            .arg("--bundle")
            .arg(format!("--format={format}"))
            .arg("--platform=neutral")
            .arg("--target=es2022")
            // A package without `exports` (loupe) by its `module` or `main`
            // field (the neutral platform reads none).
            .arg("--main-fields=module,main")
            .arg("--external:temzdf")
            .arg("--external:temzdf/*")
            .arg("--log-level=warning")
            .arg(format!("--outfile={}", out.join(out_file).display()))
            .arg(format!("--metafile={}", meta.display()))
            // node_modules from the lock file, wherever it lies.
            .env_remove("NODE_PATH");
        if let Some(m) = &modules {
            cmd.env("NODE_PATH", m);
        }
        let status = cmd.status().unwrap_or_else(|e| {
            panic!("cannot run esbuild ({e}): the dev shell provides it (crates/temzdf-script/build.rs)")
        });
        assert!(
            status.success(),
            "esbuild failed on {} (npm packages come from runtime/node_modules (`npm ci` there) or $TEMZDF_NODE_MODULES, which the dev shell sets)",
            entry.display()
        );
        inputs.extend(sources(&manifest, &meta));
    };
    // The host's prelude: a script (a function, called at once), as the
    // polyfills.
    bundle(&prelude, "prelude.js", "iife");
    // What `import ... from "temzdf"` gets.
    bundle(&module, "temzdf.js", "esm");
    bundle(&runtime.join("polyfills.ts"), "polyfills.js", "iife");
    bundle(&runtime.join("defaults.ts"), "defaults.js", "esm");
    let mut list = String::from("&[\n");
    for (name, entry) in MODULES {
        let file = format!("{}.js", name.replace('/', "-"));
        bundle(&runtime.join(entry), &file, "esm");
        list.push_str(&format!("    (\"temzdf/{name}\", include_str!(concat!(env!(\"OUT_DIR\"), \"/{file}\"))),\n"));
    }
    list.push(']');
    std::fs::write(out.join("bundled.rs"), list).unwrap();
    for file in inputs {
        println!("cargo::rerun-if-changed={}", file.display());
    }
}

/// The source files esbuild's metafile `meta` lists as a bundle's inputs
/// (paths relative to the working directory, the manifest's), npm's
/// packages left out (the lock file and npm's record of them, or
/// `$TEMZDF_NODE_MODULES`, stand for them).
fn sources(manifest: &Path, meta: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(meta).unwrap_or_else(|e| panic!("{}: {e}", meta.display()));
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", meta.display()));
    let Some(inputs) = json["inputs"].as_object() else { panic!("{}: no inputs", meta.display()) };
    inputs.keys().filter(|p| !p.contains("node_modules/")).map(|p| manifest.join(p)).collect()
}

/// Refuse runtime/node_modules when it is not what runtime/package-lock.json
/// says: npm's record of what it installed (`node_modules/.package-lock.json`)
/// has every package of the lock file at its version and integrity, the
/// devDependencies excepted when left out (`npm ci --omit=dev`), and no other.
fn check_installed(runtime: &Path) {
    let read = |path: &Path| -> serde_json::Value {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e} (run `npm ci` in crates/temzdf-script/runtime)", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let lock = runtime.join("package-lock.json");
    let record = runtime.join("node_modules/.package-lock.json");
    println!("cargo::rerun-if-changed={}", lock.display());
    println!("cargo::rerun-if-changed={}", record.display());
    let (lock, record) = (read(&lock), read(&record));
    let empty = serde_json::Map::new();
    let packages = |v: &serde_json::Value| v["packages"].as_object().unwrap_or(&empty).clone();
    let (wanted, installed) = (packages(&lock), packages(&record));
    let same =
        |a: &serde_json::Value, b: &serde_json::Value| a["version"] == b["version"] && a["integrity"] == b["integrity"];
    let stale = wanted.iter().filter(|(name, _)| !name.is_empty()).find(|(name, want)| match installed.get(*name) {
        Some(have) => !same(want, have),
        None => want["dev"] != serde_json::Value::Bool(true),
    });
    let extra = installed.keys().find(|name| !name.is_empty() && !wanted.contains_key(*name));
    if let Some(name) = stale.map(|(name, _)| name).or(extra) {
        panic!(
            "crates/temzdf-script/runtime/node_modules is not what package-lock.json says ({name}): \
             run `npm ci` in crates/temzdf-script/runtime"
        );
    }
}
