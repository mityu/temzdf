//! The shipped JavaScript and a sample configuration type check against the
//! declarations `temzdf print-types` generates, and mistakes do not.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

fn have_tsc() -> bool {
    Command::new("tsc").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// The repository's root.
fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// What `temzdf print-types` prints, committed (scripts/ts-types.ts).
fn declarations() -> PathBuf {
    root().join("crates/temzdf-script/ts-types/temzdf-all.d.ts")
}

/// Type check the project of `dir`'s tsconfig.json; the compiler's output on
/// failure.
fn check_project(dir: &Path) -> Result<(), String> {
    let out = Command::new("tsc").args(["-p", "."]).current_dir(dir).output().unwrap();
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

/// Type check `files` (path, text) as a configuration of a user's, against
/// what `temzdf print-types` prints.
fn check(files: &[(&str, &str)]) -> Result<(), String> {
    let dir = tuitest::TempDir::new("types");
    std::fs::copy(declarations(), dir.path().join("temzdf.d.ts")).unwrap();
    let mut names = vec!["\"temzdf.d.ts\"".to_owned()];
    for (name, text) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        names.push(format!("{name:?}"));
    }
    let tsconfig = format!(
        r#"{{
  "compilerOptions": {{
    "strict": true, "noEmit": true, "target": "es2022", "module": "esnext",
    "moduleResolution": "bundler", "allowImportingTsExtensions": true,
    "erasableSyntaxOnly": true, "types": [], "lib": ["es2022"]
  }},
  "files": [{}]
}}"#,
        names.join(", ")
    );
    std::fs::write(dir.path().join("tsconfig.json"), tsconfig).unwrap();
    check_project(dir.path())
}

#[test]
fn print_types_prints_the_committed_declarations() {
    let d = Command::new(BIN).arg("print-types").output().unwrap();
    assert!(d.status.success());
    assert!(d.stdout == std::fs::read(declarations()).unwrap(), "print-types differs from temzdf-all.d.ts");
}

#[test]
fn the_sample_configuration_type_checks() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    check_project(&root().join("crates/temzdf/tests/configs")).unwrap();
}

#[test]
fn the_end_to_end_configurations_type_check() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    // tests/e2e/configs/: the scenarios' configurations, which no other
    // test loads.
    check_project(&root().join("tests/e2e/configs")).unwrap();
}

#[test]
fn the_shipped_scripts_type_check() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    // runtime/ with its tsconfig.json (editors use it too), and a
    // configuration using the shipped modules as a user's would.
    check_project(&root().join("crates/temzdf-script/runtime")).unwrap();
    // The host's prelude builds what the declarations say `temzdf` is, and
    // its `temzdf` module exports exactly that.
    check_project(&root().join("crates/temzdf-script/src")).unwrap();
    let config = "import vim from \"temzdf/edit/vim\";\nimport notepad from \"temzdf/edit/notepad\";\n\
        import { keyset } from \"temzdf/keyset\";\nimport type { Keyset } from \"temzdf/keyset\";\n\
        vim.options.shiftwidth = 2;\nif (vim.enabled) vim.disable();\nnotepad.enable();\n\
        const mine: Keyset = keyset(\"mine\", (d) => d.map(\"normal\", [\"x\"], () => {}));\nmine.enable();\n";
    check(&[("config.ts", config)]).unwrap();
}

#[test]
fn mistakes_do_not_type_check() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    for bad in [
        "import { action } from \"temzdf\";\naction.split(\"sideways\");\n",
        "import { on } from \"temzdf\";\non(\"NoSuchEvent\", () => {});\n",
        "import { keymap } from \"temzdf\";\nkeymap.add(\"normal\", \"x\", 42);\n",
        "import { o } from \"temzdf\";\no.timeoutlen = \"slow\";\n",
        "import { defineMode } from \"temzdf\";\ndefineMode(\"m\", { inherit: \"normal\" });\n",
        "import { keymap, action } from \"temzdf\";\nkeymap.add(\"normal\", \"x\", () => action.closePane(), { when: () => 1 });\n",
        "import { keymap, action } from \"temzdf\";\nkeymap.add(\"normal\", \"x\", action.closePane());\n",
        // The shipped modules show what they export, nothing of their insides.
        "import vim from \"temzdf/edit/vim\";\nvim.state;\n",
        "import type { Operator } from \"temzdf/edit/vim\";\nlet o: Operator | undefined;\n",
    ] {
        assert!(check(&[("config.ts", bad)]).is_err(), "should not type check:\n{bad}");
    }
}
