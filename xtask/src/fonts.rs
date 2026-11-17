//! The fonts temzdf-gui embeds: the list (`crates/temzdf-gui/fonts.toml`),
//! fetching them, and moving them to newer releases.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The largest font we accept (Noto Color Emoji is about 11 MB).
const MAX_BYTES: u64 = 64 << 20;

#[derive(Debug, Deserialize)]
struct Manifest {
    font: Vec<Font>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Font {
    pub file: String,
    pub repo: String,
    pub tag: String,
    url: String,
    pub sha256: String,
}

impl Font {
    /// The URL of the font at release `tag`.
    fn url_at(&self, tag: &str) -> String {
        self.url.replace("{tag}", tag)
    }
}

fn read(manifest: &Path) -> Result<(String, Vec<Font>)> {
    let text = fs::read_to_string(manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let fonts = parse(&text).with_context(|| format!("parsing {}", manifest.display()))?;
    Ok((text, fonts))
}

fn parse(text: &str) -> Result<Vec<Font>> {
    Ok(toml::from_str::<Manifest>(text)?.font)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn get(url: &str, accept: Option<&str>) -> Result<Vec<u8>> {
    let mut req = ureq::get(url);
    if let Some(a) = accept {
        req = req.header("Accept", a);
    }
    if url.starts_with("https://api.github.com/")
        && let Ok(token) = std::env::var("GITHUB_TOKEN")
    {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let mut resp = req.call().with_context(|| format!("fetching {url}"))?;
    resp.body_mut().with_config().limit(MAX_BYTES).read_to_vec().with_context(|| format!("reading {url}"))
}

/// Write `bytes` to `dir/file` through a temporary file, so a reader never
/// sees half a font.
fn put(dir: &Path, file: &str, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".{file}.part"));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, dir.join(file)).with_context(|| format!("writing {}", dir.join(file).display()))
}

/// `cargo xtask fetch-fonts`.
pub fn fetch(manifest: &Path, dir: &Path) -> Result<()> {
    let (_, fonts) = read(manifest)?;
    for f in &fonts {
        let path = dir.join(&f.file);
        if fs::read(&path).is_ok_and(|b| sha256_hex(&b) == f.sha256) {
            println!("kept    {}", path.display());
            continue;
        }
        let url = f.url_at(&f.tag);
        let bytes = get(&url, None)?;
        let got = sha256_hex(&bytes);
        if got != f.sha256 {
            bail!("{}: sha256 {got} from {url}, expected {} ({})", f.file, f.sha256, manifest.display());
        }
        put(dir, &f.file, &bytes)?;
        println!("fetched {}", path.display());
    }
    println!("cargo build -p temzdf-gui embeds them (without TEMZDF_BUNDLED_FONTS set)");
    Ok(())
}

/// `cargo xtask update-fonts`.
pub fn update(manifest: &Path, dir: &Path, tags: &[(String, String)], check: bool) -> Result<()> {
    let (text, fonts) = read(manifest)?;
    let mut latest: BTreeMap<&str, String> = BTreeMap::new();
    for f in &fonts {
        if latest.contains_key(f.repo.as_str()) {
            continue;
        }
        let tag = match tags.iter().find(|(r, _)| *r == f.repo) {
            Some((_, t)) => t.clone(),
            None => latest_tag(&f.repo)?,
        };
        latest.insert(&f.repo, tag);
    }
    if let Some((r, _)) = tags.iter().find(|(r, _)| !latest.contains_key(r.as_str())) {
        bail!("{r} is not a repository in {}", manifest.display());
    }
    let mut shown = Vec::new();
    for f in &fonts {
        let new = &latest[f.repo.as_str()];
        if !shown.contains(&(&f.repo, &f.tag)) {
            shown.push((&f.repo, &f.tag));
            let note = if *new == f.tag { "up to date" } else { "update" };
            println!("{}: {} -> {new} ({note})", f.repo, f.tag);
        }
    }
    if check {
        return Ok(());
    }
    // Download everything first: the list changes only if all of it worked.
    let mut changes = Vec::new();
    for (i, f) in fonts.iter().enumerate() {
        let new = &latest[f.repo.as_str()];
        if *new == f.tag {
            continue;
        }
        let bytes = get(&f.url_at(new), None).with_context(|| format!("{} at {new}", f.file))?;
        changes.push((i, new.clone(), sha256_hex(&bytes), bytes));
    }
    if changes.is_empty() {
        println!("nothing to change");
        return Ok(());
    }
    let edits: Vec<_> = changes.iter().map(|(i, t, h, _)| (*i, t.as_str(), h.as_str())).collect();
    let out = set_versions(&text, &edits)?;
    fs::write(manifest, out).with_context(|| format!("writing {}", manifest.display()))?;
    for (i, _, _, bytes) in &changes {
        put(dir, &fonts[*i].file, bytes)?;
    }
    for (i, tag, _, _) in &changes {
        println!("updated {} to {tag}", fonts[*i].file);
    }
    println!("check `git diff {}`, run the tests, then commit", manifest.display());
    Ok(())
}

fn latest_tag(repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = get(&url, Some("application/vnd.github+json"))?;
    let v: serde_json::Value = serde_json::from_slice(&body).with_context(|| format!("parsing {url}"))?;
    v["tag_name"].as_str().map(str::to_owned).with_context(|| format!("no tag_name in {url}"))
}

/// The list with font `i`'s `tag` and `sha256` replaced, for each `(i, tag,
/// sha256)`; comments and everything else stay as they are.
fn set_versions(text: &str, edits: &[(usize, &str, &str)]) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let fonts = doc["font"].as_array_of_tables_mut().context("no [[font]] tables")?;
    for &(i, tag, sha256) in edits {
        let t = fonts.get_mut(i).with_context(|| format!("no font {i}"))?;
        t["tag"] = toml_edit::value(tag);
        t["sha256"] = toml_edit::value(sha256);
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = include_str!("../../crates/temzdf-gui/fonts.toml");

    #[test]
    fn the_list_is_well_formed() {
        let fonts = parse(LIST).unwrap();
        assert_eq!(fonts.len(), 6);
        for (i, f) in fonts.iter().enumerate() {
            assert!(fonts[..i].iter().all(|g| g.file != f.file), "{} twice", f.file);
            assert!(f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{f:?}");
            assert!(f.url.contains("{tag}") && f.url.ends_with(&f.file), "{f:?}");
        }
    }

    #[test]
    fn a_url_takes_the_tag() {
        let f = &parse(LIST).unwrap()[0];
        assert_eq!(
            f.url_at("v9"),
            "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/v9/fonts/ttf/JetBrainsMono-Regular.ttf"
        );
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn updating_changes_tag_and_sha256_only() {
        let hash = "0".repeat(64);
        let out = set_versions(LIST, &[(4, "v3.0", &hash)]).unwrap();
        let (old, new) = (parse(LIST).unwrap(), parse(&out).unwrap());
        assert_eq!((new[4].tag.as_str(), new[4].sha256.as_str()), ("v3.0", hash.as_str()));
        assert_eq!(new[4].url, old[4].url);
        for i in [0, 1, 2, 3, 5] {
            assert_eq!((&new[i].tag, &new[i].sha256), (&old[i].tag, &old[i].sha256));
        }
        assert!(out.starts_with("# The fonts temzdf-gui embeds"), "comments stay");
        assert_eq!(out.lines().count(), LIST.lines().count());
    }
}
