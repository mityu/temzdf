//! Repository tasks, run as `cargo xtask TASK` (the alias is in
//! `.cargo/config.toml`). Portable where shell scripts are not: they need
//! only Cargo (and the tools a task runs, never Nix).

mod check;
mod fonts;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
usage: cargo xtask TASK [ARGS]

tasks:
  check [--fix] [--no-miri] [--all] [--verbose] [--stream] [CARGO TEST ARGS...]
      The pre-commit gate: rustfmt check, Biome (the scripts' formatting
      and lints), the generated declarations (node scripts/ts-types.ts),
      the Node scripts' types (tsc -p scripts), shellcheck, references to
      documents (node scripts/check-docs.ts), clippy (warnings are errors),
      the workspace's tests and the scripts' (temzdf test on
      crates/temzdf-script/runtime/**/*.test.ts), then the unit tests
      under Miri (scripts/miri.sh) when `cargo miri` is there, with terse
      output: a failed step's main lines, its whole output in
      target/check/.
      shellcheck and Miri are Unix only (SKIP elsewhere).
        --fix      apply rustfmt, Biome's safe fixes and temzdf-all.d.ts
                   instead of only checking
        --no-miri  skip Miri (about a minute and a half)
        --all      run the tests even when only Markdown under doc/
                   changed (then they and Miri are skipped; a clean tree
                   or cargo test arguments run them; the scripts' tests
                   are skipped with cargo test arguments)
        --verbose  print a failed step's whole output
        --stream   print each step's output as it comes, under a
                   `==> STEP` header (GitHub Actions: a log group)
      The tests see only the fonts in TEMZDF_TEST_FONTCONFIG when it is
      set (the dev shell's, as in CI). scripts/check.sh runs this inside
      the Nix dev shell.
  test [--stream] [--verbose] [CARGO TEST ARGS...]
      The workspace's tests and the scripts' alone, as check runs them.
  fetch-fonts [--manifest PATH]
      Download the fonts temzdf-gui embeds (crates/temzdf-gui/fonts.toml)
      into target/fonts/, checking their sha256; files already there and
      right are kept. For builds without Nix (Nix fetches them itself).
  update-fonts [--check] [--manifest PATH] [REPO=TAG ...]
      Move the fonts to the latest releases of their GitHub repositories
      (or to TAG for REPO), writing the new tags and sha256 into the list.
      --check only shows what would change.
";

fn main() {
    match run() {
        Ok(status) => std::process::exit(status),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

/// The exit status.
fn run() -> Result<i32> {
    let mut args = std::env::args_os().skip(1);
    let task = args.next().map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
    match task.as_str() {
        "check" => return check::check(&check::Options::parse(args, false)),
        "test" => return check::test(&check::Options::parse(args, true)),
        _ => {}
    }
    let mut args = args.map(|a| a.to_string_lossy().into_owned());
    let mut manifest = root().join("crates/temzdf-gui/fonts.toml");
    let mut check = false;
    let mut tags = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--manifest" => manifest = PathBuf::from(args.next().context("--manifest needs a path")?),
            "--check" if task == "update-fonts" => check = true,
            _ if task == "update-fonts" && a.contains('=') => {
                let (repo, tag) = a.split_once('=').unwrap();
                tags.push((repo.to_owned(), tag.to_owned()));
            }
            _ => bail!("unexpected argument {a:?}\n\n{USAGE}"),
        }
    }
    let dir = target_dir().join("fonts");
    match task.as_str() {
        "fetch-fonts" => fonts::fetch(&manifest, &dir)?,
        "update-fonts" => fonts::update(&manifest, &dir, &tags, check)?,
        "" | "help" | "-h" | "--help" => print!("{USAGE}"),
        _ => bail!("unknown task {task:?}\n\n{USAGE}"),
    }
    Ok(0)
}

/// The workspace root: this crate's parent directory.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is inside the workspace").to_path_buf()
}

/// Cargo's target directory: `CARGO_TARGET_DIR` (a relative one from the
/// root, as crates/temzdf-gui/build.rs reads it: a build script cannot know
/// where cargo was run), else `target/` in the root.
fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) => root().join(d),
        None => root().join("target"),
    }
}
