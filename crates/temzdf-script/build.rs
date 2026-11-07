//! Bundles the shipped scripts (runtime/) with esbuild into `$OUT_DIR`, for
//! the host to embed (src/host.rs): `polyfills.js` (globals scripts expect,
//! from npm packages), `defaults.js` (the default configuration) and the
//! modules scripts import as `temzdf/NAME` (`vim.js` from `runtime/vim/`).
//! `"temzdf"` stays an import (the host provides it).
//!
//! npm packages come from `$TEMZDF_NODE_MODULES`: the dev shells and `nix
//! build` set it to node_modules built from runtime/package-lock.json, so
//! no build needs the network. esbuild comes from `PATH` (`$ESBUILD`
//! overrides). Only with the feature `host` (the type declarations alone
//! need none of this).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Modules scripts import as `temzdf/NAME`, from runtime/NAME/index.ts.
const MODULES: &[&str] = &["vim"];

fn main() {
    if std::env::var_os("CARGO_FEATURE_HOST").is_none() {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let runtime = manifest.join("runtime");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    println!("cargo::rerun-if-changed={}", runtime.display());
    for var in ["TEMZDF_NODE_MODULES", "ESBUILD"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let modules = std::env::var_os("TEMZDF_NODE_MODULES").map(PathBuf::from);
    let esbuild = std::env::var_os("ESBUILD").unwrap_or_else(|| "esbuild".into());
    let bundle = |entry: &Path, out_file: &str, format: &str| {
        let mut cmd = Command::new(&esbuild);
        cmd.arg(entry)
            .arg("--bundle")
            .arg(format!("--format={format}"))
            .arg("--platform=neutral")
            .arg("--target=es2022")
            .arg("--external:temzdf")
            .arg("--log-level=warning")
            .arg(format!("--outfile={}", out.join(out_file).display()))
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
            "esbuild failed on {} (npm packages come from $TEMZDF_NODE_MODULES, which the dev shell sets)",
            entry.display()
        );
    };
    bundle(&runtime.join("polyfills.ts"), "polyfills.js", "iife");
    bundle(&runtime.join("defaults.ts"), "defaults.js", "esm");
    for name in MODULES {
        let entry = runtime.join(name).join("index.ts");
        if entry.is_file() {
            bundle(&entry, &format!("{name}.js"), "esm");
        } else {
            std::fs::write(out.join(format!("{name}.js")), "export {};\n").unwrap();
        }
    }
}
