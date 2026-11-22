//! Builds the script host's guest (crates/temzdf-script-guest) for
//! wasm32-wasip1 with a nested cargo, then precompiles it with Cranelift for
//! the target, into `$OUT_DIR/guest.cwasm` (embedded by src/lib.rs). The
//! nested build has a target directory of its own (`target/script-guest`), so
//! it does not wait on the outer build's lock, and keeps its build between
//! runs. QuickJS's C code needs `CC_wasm32_wasip1` (the dev shell sets it).
//! A guest built already comes in `TEMZDF_SCRIPT_GUEST_WASM` (a `.wasm`
//! file; `nix build` builds it apart, so it is cached on its own): then it
//! is only precompiled.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/engine_config.rs"]
mod engine_config;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn main() {
    let dest = PathBuf::from(env("OUT_DIR")).join("guest.cwasm");
    if std::env::var_os("CARGO_CFG_MIRI").is_some() {
        // Miri runs no WebAssembly: nothing to embed (the tests that would
        // load it are ignored there).
        std::fs::write(&dest, []).unwrap();
        return;
    }
    println!("cargo::rerun-if-env-changed=TEMZDF_SCRIPT_GUEST_WASM");
    let wasm = match std::env::var_os("TEMZDF_SCRIPT_GUEST_WASM").map(PathBuf::from) {
        Some(prebuilt) => {
            println!("cargo::rerun-if-changed={}", prebuilt.display());
            prebuilt
        }
        None => build_guest(),
    };
    let wasm = std::fs::read(&wasm).unwrap_or_else(|e| panic!("{}: {e}", wasm.display()));
    let mut config = wasmtime::Config::new();
    // The target's baseline, not this machine's CPU: the binary may run
    // elsewhere.
    config.target(&env("TARGET")).expect("a target wasmtime can compile for");
    engine_config::configure(&mut config);
    let engine = wasmtime::Engine::new(&config).expect("wasmtime's engine");
    let compiled = engine.precompile_module(&wasm).expect("precompiling the script host's guest");
    std::fs::write(&dest, compiled).unwrap();
}

/// Build the guest with a nested cargo; its `.wasm` file.
fn build_guest() -> PathBuf {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().and_then(Path::parent).expect("crates/temzdf-script-runner");
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "crates/temzdf-script-guest",
        "crates/temzdf-script/Cargo.toml",
        "crates/temzdf-script/build.rs",
        "crates/temzdf-script/src",
        "crates/temzdf-script/runtime/package-lock.json",
        "crates/temzdf-script/runtime/node_modules/.package-lock.json",
        "crates/temzdf-protocol/Cargo.toml",
        "crates/temzdf-protocol/src",
    ] {
        println!("cargo::rerun-if-changed={}", root.join(path).display());
    }
    // The shipped scripts, but their tests (editing one rebuilds nothing)
    // and npm's packages (the lock files above stand for them).
    for file in scripts(&root.join("crates/temzdf-script/runtime")) {
        println!("cargo::rerun-if-changed={}", file.display());
    }
    // The guest bundles the shipped scripts (crates/temzdf-script/build.rs)
    // with these too.
    for var in [
        "CC_wasm32_wasip1",
        "AR_wasm32_wasip1",
        "RQUICKJS_SYS_NO_WASI_SDK",
        "CARGO_TARGET_DIR",
        "TEMZDF_NODE_MODULES",
        "ESBUILD",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let target_dir = match std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => root.join("target"),
    }
    .join("script-guest");

    let mut cargo = Command::new(env("CARGO"));
    cargo
        .current_dir(root)
        .args(["build", "--package", "temzdf-script-guest", "--target", "wasm32-wasip1", "--profile"])
        .arg(PROFILE)
        .arg("--target-dir")
        .arg(&target_dir);
    // What cargo told this build script about the outer build would
    // mislead the nested one (its flags, its target, cfgs of this target).
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("CARGO_CFG_")
            || key.starts_with("CARGO_FEATURE_")
            || key.starts_with("CARGO_PKG_")
            || key.starts_with("CARGO_MANIFEST_")
            || [
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_TARGET",
                "CARGO_PRIMARY_PACKAGE",
                "RUSTFLAGS",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "RUSTC_LINKER",
                "TARGET",
                "HOST",
                "OUT_DIR",
                "OPT_LEVEL",
                "PROFILE",
                "DEBUG",
                "NUM_JOBS",
            ]
            .contains(&key.as_ref())
        {
            cargo.env_remove(key.as_ref());
        }
    }
    let status = cargo.status().expect("cannot run cargo for the script host's guest");
    assert!(status.success(), "building the script host's guest failed (CC_wasm32_wasip1 must name a WASI C compiler)");

    target_dir.join("wasm32-wasip1").join(PROFILE).join("temzdf_script_guest.wasm")
}

/// The files under `dir` but `node_modules` and tests (`*.test.ts`).
fn scripts(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            if name != "node_modules" {
                files.extend(scripts(&path));
            }
        } else if !name.ends_with(".test.ts") {
            files.push(path);
        }
    }
    files
}

/// The cargo profile the guest is built with (Cargo.toml).
const PROFILE: &str = "script-guest";
