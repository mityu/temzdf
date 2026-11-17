//! Repository tasks, run as `cargo xtask TASK` (the alias is in
//! `.cargo/config.toml`). Portable where shell scripts are not: they need
//! only Cargo.

mod fonts;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
usage: cargo xtask TASK [ARGS]

tasks:
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
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
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
        "fetch-fonts" => fonts::fetch(&manifest, &dir),
        "update-fonts" => fonts::update(&manifest, &dir, &tags, check),
        "" | "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        _ => bail!("unknown task {task:?}\n\n{USAGE}"),
    }
}

/// The workspace root: this crate's parent directory.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is inside the workspace").to_path_buf()
}

/// Cargo's target directory: `CARGO_TARGET_DIR`, else `target/` in the root.
fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) => std::env::current_dir().map(|c| c.join(&d)).unwrap_or_else(|_| PathBuf::from(d)),
        None => root().join("target"),
    }
}
