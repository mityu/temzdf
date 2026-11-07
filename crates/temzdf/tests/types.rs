//! The shipped JavaScript and a sample configuration type check against the
//! declarations `temzdf print-types` generates, and mistakes do not.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

fn have_tsc() -> bool {
    Command::new("tsc").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Type check `files` (path, text) with the generated declarations; the
/// compiler's output on failure.
fn check(files: &[(&str, &str)]) -> Result<(), String> {
    let dir = tuitest::TempDir::new("types");
    let d = Command::new(BIN).arg("print-types").output().unwrap();
    assert!(d.status.success());
    std::fs::write(dir.path().join("temzdf.d.ts"), &d.stdout).unwrap();
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
    let out = Command::new("tsc").args(["-p", "."]).current_dir(dir.path()).output().unwrap();
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

#[test]
fn the_sample_configuration_type_checks() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    let sample =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/configs/sample.ts")).unwrap();
    check(&[("config.ts", &sample)]).unwrap();
}

#[test]
fn the_shipped_scripts_type_check() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    // runtime/ (as modules of their own: the generated declarations stand
    // for "temzdf" and "temzdf/NAME"; not the polyfills, from npm), and a
    // configuration using them.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../temzdf-script/runtime");
    let mut files: Vec<(String, String)> = Vec::new();
    for dir in ["", "lib", "edit", "pane", "mode", "vim"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if p.extension().is_some_and(|e| e == "ts") && name != "polyfills.ts" {
                let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
                files.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    files.sort();
    let config = "import vim from \"temzdf/edit/vim\";\nimport notepad from \"temzdf/edit/notepad\";\n\
        vim.options.shiftwidth = 2;\nif (vim.enabled) vim.disable();\nnotepad.enable();\n";
    files.push(("config.ts".into(), config.into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    check(&refs).unwrap();
}

#[test]
fn mistakes_do_not_type_check() {
    if !have_tsc() {
        eprintln!("SKIPPED: tsc not found");
        return;
    }
    for bad in [
        "import { cmd } from \"temzdf\";\ncmd.split(\"sideways\");\n",
        "import { on } from \"temzdf\";\non(\"NoSuchEvent\", () => {});\n",
        "import { keymap } from \"temzdf\";\nkeymap.add(\"normal\", \"x\", 42);\n",
        "import { o } from \"temzdf\";\no.timeoutlen = \"slow\";\n",
        "import { defineMode } from \"temzdf\";\ndefineMode(\"m\", { inherit: \"normal\" });\n",
        "import { keymap, action } from \"temzdf\";\nkeymap.add(\"normal\", \"x\", action.quit(), { when: () => 1 });\n",
    ] {
        assert!(check(&[("config.ts", bad)]).is_err(), "should not type check:\n{bad}");
    }
}
