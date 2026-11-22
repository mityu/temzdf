//! The pre-commit gate (`cargo xtask check`) and the tests alone
//! (`cargo xtask test`): rustfmt, Biome, the generated declarations
//! (`node scripts/ts-types.ts`), the Node scripts' types (`tsc -p
//! scripts`), shellcheck, references to documents (`node
//! scripts/check-docs.ts`), clippy and the workspace's tests, then the unit
//! tests under Miri (`scripts/miri.sh`), with terse output: a failed
//! step's main lines, its whole output in `target/check/`.
//!
//! No Nix assumed: the tools are found on PATH (Biome and shellcheck also
//! in the npm packages' `.bin`). shellcheck and Miri are Unix-only steps
//! and say `SKIP` where they cannot run. `scripts/check.sh` enters the Nix
//! dev shell first and runs this.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::{root, target_dir};

/// The shipped scripts' npm packages (`npm ci` there).
const NODE_MODULES: &str = "crates/temzdf-script/runtime/node_modules";
/// The generated declarations, committed.
const DECLARATIONS: &str = "crates/temzdf-script/ts-types/temzdf-all.d.ts";

/// `cargo xtask check`'s options.
#[derive(Default)]
pub struct Options {
    /// Apply rustfmt's formatting, Biome's safe fixes and the generated
    /// declarations instead of only checking them.
    pub fix: bool,
    pub miri: bool,
    /// Run the tests even when only Markdown under doc/ changed.
    pub all: bool,
    /// Print a failed step's whole output, not its main lines.
    pub verbose: bool,
    /// Print each step's output as it comes, under a header.
    pub stream: bool,
    /// Passed to `cargo test`.
    pub test_args: Vec<OsString>,
}

impl Options {
    /// Reads leading options until the first other argument; that one and
    /// the rest are cargo test's. `test` takes only `--stream` and
    /// `--verbose`.
    pub fn parse(args: impl Iterator<Item = OsString>, test: bool) -> Self {
        let mut o = Options { miri: true, ..Options::default() };
        let mut args = args.peekable();
        while let Some(a) = args.peek() {
            match a.to_str() {
                Some("--fix") if !test => o.fix = true,
                Some("--no-miri") if !test => o.miri = false,
                Some("--all") if !test => o.all = true,
                Some("--verbose") => o.verbose = true,
                Some("--stream") => o.stream = true,
                _ => break,
            }
            args.next();
        }
        o.test_args = args.collect();
        o
    }
}

/// `cargo xtask check`: the exit status.
pub fn check(o: &Options) -> Result<i32> {
    let root = root();
    std::env::set_current_dir(&root).context("cannot enter the workspace root")?;
    let mut miri = o.miri;

    // Tests are skipped when every path changed since the last commit is
    // Markdown under doc/ (and there is at least one).
    let mut tests = true;
    if !o.all && o.test_args.is_empty() {
        let mut changed = output(command("git").args(["diff", "--name-only", "--no-renames", "HEAD"]))?;
        changed += &output(command("git").args(["ls-files", "--others", "--exclude-standard"]))?;
        let mut paths = changed.lines().filter(|l| !l.is_empty()).peekable();
        if paths.peek().is_some()
            && paths.all(|p| {
                p.strip_prefix("doc/").and_then(|p| p.strip_suffix(".md")).is_some_and(|stem| !stem.is_empty())
            })
        {
            tests = false;
            miri = false;
        }
    }

    // The shipped scripts' npm packages, for tsc (the scripts' types and
    // crates/temzdf/tests/types.rs) and esbuild; build.rs refuses them when
    // they no longer match the lock.
    if !Path::new(NODE_MODULES).is_dir() {
        println!("FAIL npm (no {NODE_MODULES}: run `npm ci` there)");
        return Ok(1);
    }
    let tools = Tools::find()?;
    let mut gate = Gate::new(o)?;
    let mut ran = Vec::new();

    gate.header("fmt");
    if o.fix {
        let st = tools.cargo().args(["fmt", "--all"]).status().context("cannot run cargo fmt")?;
        if !st.success() {
            println!("FAIL fmt (cargo fmt --all failed)");
            gate.status = 1;
        }
    } else {
        let st = tools
            .cargo()
            .args(["fmt", "--all", "--check"])
            .stdout(Stdio::null())
            .status()
            .context("cannot run cargo fmt")?;
        if !st.success() {
            gate.end();
            println!("FAIL fmt (run cargo xtask check --fix)");
            gate.status = 1;
        }
    }
    gate.end();
    ran.push("fmt");

    // Biome's safe fixes with --fix; what is left (lints to fix by hand) fails.
    gate.header("biome");
    if o.fix {
        gate.run(command(&tools.biome).args(["check", "--write", "--colors=off", "."]), "biome")?;
    }
    if !gate.run(command(&tools.biome).args(["check", "--colors=off", "."]), "biome")? {
        gate.failed(
            "biome (cargo xtask check --fix formats)",
            "biome",
            |l| l.contains("━━") || l.trim_start_matches(' ').starts_with('×'),
            20,
        );
    }
    gate.end();
    ran.push("biome");

    // temzdf-all.d.ts as scripts/ts-types.ts writes it from the declarations
    // by hand and the shipped modules.
    gate.header("ts-types");
    if o.fix {
        gate.run(tools.node().arg("scripts/ts-types.ts"), "ts-types")?;
    }
    let written = gate.dir.join("temzdf-all.d.ts");
    if !gate.run(tools.node().arg("scripts/ts-types.ts").arg(&written), "ts-types")? {
        gate.failed("ts-types", "ts-types", |_| true, 20);
    } else if fs::read(&written).ok() != fs::read(DECLARATIONS).ok() {
        gate.end();
        println!("FAIL ts-types ({DECLARATIONS} is not what scripts/ts-types.ts writes; run cargo xtask check --fix)");
        gate.status = 1;
    }
    gate.end();
    ran.push("ts-types");

    // The Node scripts' types (scripts/tsconfig.json): Node only strips
    // them. The TypeScript of the npm packages (the lock's version).
    gate.header("scripts");
    let tsc = format!("{NODE_MODULES}/typescript/bin/tsc");
    if !gate.run(tools.node().args([tsc.as_str(), "-p", "scripts", "--pretty", "false"]), "scripts")? {
        gate.failed("scripts", "scripts", |l| l.contains("error"), 40);
    }
    gate.end();
    ran.push("scripts");

    match &tools.shellcheck {
        Some(shellcheck) => {
            gate.header("shellcheck");
            let mut files = Vec::new();
            for (dir, depth) in [("scripts", 0), ("scripts", 1), ("tests", 1)] {
                files.extend(shell_scripts(Path::new(dir), depth));
            }
            let mut cmd = command(shellcheck);
            cmd.args(["-x", "--color=never", "--format=gcc"]).args(&files);
            if !gate.run(&mut cmd, "shellcheck")? {
                gate.failed("shellcheck", "shellcheck", |_| true, 100);
            }
            gate.end();
            ran.push("shellcheck");
        }
        None if cfg!(unix) => println!("SKIP shellcheck (not found on PATH)"),
        None => println!("SKIP shellcheck (Unix only)"),
    }

    gate.header("docs");
    if !gate.run(tools.node().arg("scripts/check-docs.ts"), "docs")? {
        gate.failed("docs", "docs", |_| true, 40);
    }
    gate.end();
    ran.push("docs");

    gate.header("clippy");
    let mut clippy = tools.cargo();
    clippy.args(["clippy", "--workspace", "--all-targets", "--color", "never", "--", "-D", "warnings"]);
    if !gate.run(&mut clippy, "clippy")? {
        gate.failed(
            "clippy",
            "clippy",
            |l| l.starts_with("error") || l.starts_with("warning") || l.contains("-->"),
            40,
        );
    }
    gate.end();
    ran.push("clippy");

    let summary = if tests {
        format!("tests ({} passed)", gate.tests(&tools, &o.test_args)?)
    } else {
        "tests skipped (only Markdown under doc/ changed; --all runs them)".to_owned()
    };

    if gate.status == 0 {
        println!("OK {}, {summary}", ran.join(", "));
    }

    if miri {
        gate.miri()?;
    }
    Ok(gate.status)
}

/// `cargo xtask test`: the exit status.
pub fn test(o: &Options) -> Result<i32> {
    std::env::set_current_dir(root()).context("cannot enter the workspace root")?;
    let tools = Tools::find_cargo();
    let mut gate = Gate::new(o)?;
    let passed = gate.tests(&tools, &o.test_args)?;
    if gate.status == 0 {
        println!("OK tests ({passed} passed)");
    }
    Ok(gate.status)
}

/// The tools the gate runs.
struct Tools {
    cargo: OsString,
    node: PathBuf,
    biome: PathBuf,
    shellcheck: Option<PathBuf>,
}

impl Tools {
    /// Fails, saying what to install, when a tool the gate needs is missing.
    fn find() -> Result<Self> {
        let Some(node) = find_tool("node", false) else {
            bail!("no node on PATH: install Node.js 22.18 or later (https://nodejs.org; the Nix dev shell has it)");
        };
        let Some(biome) = find_tool("biome", true) else {
            bail!("no biome on PATH: install Biome (https://biomejs.dev; the Nix dev shell has it)");
        };
        let shellcheck = if cfg!(unix) { find_tool("shellcheck", true) } else { None };
        Ok(Tools { cargo: cargo(), node, biome, shellcheck })
    }

    /// Only cargo, for the tests alone.
    fn find_cargo() -> Self {
        Tools { cargo: cargo(), node: PathBuf::new(), biome: PathBuf::new(), shellcheck: None }
    }

    fn cargo(&self) -> Command {
        command(&self.cargo)
    }

    fn node(&self) -> Command {
        command(&self.node)
    }
}

/// A command for PROGRAM without the variables `cargo run` set for us
/// (`CARGO_PKG_NAME`, `CARGO_MANIFEST_DIR`, ...): the cargo it runs would
/// see them as changed since a build from a shell (ring's build script
/// reruns on `CARGO_PKG_*`, `env!("CARGO_MANIFEST_DIR")` is tracked), and
/// rebuild crates each time one or the other ran.
fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("CARGO_PKG_")
            || name.starts_with("CARGO_MANIFEST_")
            || matches!(name, "CARGO_CRATE_NAME" | "CARGO_BIN_NAME" | "CARGO_PRIMARY_PACKAGE")
        {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// The cargo running us (`cargo run` sets CARGO), else the one on PATH.
fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// NAME in the npm packages' `.bin` (with NPM_BIN), in Nix's
/// (`$TEMZDF_NODE_MODULES`), else on PATH; with the extensions Windows
/// runs.
fn find_tool(name: &str, npm_bin: bool) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if npm_bin {
        dirs.push(root().join(NODE_MODULES).join(".bin"));
        if let Some(nix) = std::env::var_os("TEMZDF_NODE_MODULES") {
            dirs.push(PathBuf::from(nix).join(".bin"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let names: Vec<String> = if cfg!(windows) {
        [".exe", ".cmd", ".bat"].iter().map(|ext| format!("{name}{ext}")).collect()
    } else {
        vec![name.to_owned()]
    };
    dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// The `*.sh` files in DIR (DEPTH 0) or in its subdirectories (DEPTH 1), as
/// `./DIR/*.sh` and `./DIR/*/*.sh` expand, sorted.
fn shell_scripts(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut dirs = vec![Path::new(".").join(dir)];
    for _ in 0..depth {
        dirs = dirs.iter().flat_map(|d| sorted_entries(d).into_iter().filter(|p| p.is_dir())).collect();
    }
    dirs.iter()
        .flat_map(|d| sorted_entries(d).into_iter().filter(|p| p.is_file() && p.extension() == Some(OsStr::new("sh"))))
        .collect()
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

/// A command's stdout; it must succeed.
fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.stderr(Stdio::inherit()).output().with_context(|| format!("cannot run {cmd:?}"))?;
    if !out.status.success() {
        bail!("{cmd:?} failed ({})", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The steps' shared state: options, the log directory, the exit status.
struct Gate<'a> {
    o: &'a Options,
    /// `target/check/`.
    dir: PathBuf,
    status: i32,
    /// GitHub Actions' log groups instead of `==>` headers.
    groups: bool,
    /// A header was printed and its group is not closed yet.
    open: bool,
}

impl<'a> Gate<'a> {
    fn new(o: &'a Options) -> Result<Self> {
        let dir = target_dir().join("check");
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let groups = std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true");
        Ok(Gate { o, dir, status: 0, groups, open: false })
    }

    /// With --stream, the header of step NAME.
    fn header(&mut self, name: &str) {
        if !self.o.stream {
            return;
        }
        if self.groups {
            println!("::group::{name}");
        } else {
            println!("==> {name}");
        }
        self.open = true;
    }

    /// Closes the step's group (before its FAIL line, outside the group).
    fn end(&mut self) {
        if self.open && self.groups {
            println!("::endgroup::");
        }
        self.open = false;
    }

    /// `target/check/NAME.log`.
    fn log(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.log"))
    }

    /// The log's path as written in messages: from the root when inside it.
    fn shown(&self, name: &str) -> String {
        let log = self.log(name);
        log.strip_prefix(root()).unwrap_or(&log).display().to_string()
    }

    /// Runs CMD with its stdout and stderr in `target/check/LOG.log` (and,
    /// with --stream, on our stdout too); whether it succeeded.
    fn run(&mut self, cmd: &mut Command, log: &str) -> Result<bool> {
        let path = self.log(log);
        let mut file = File::create(&path).with_context(|| format!("cannot create {}", path.display()))?;
        cmd.stdin(Stdio::null());
        let status = if self.o.stream {
            // One pipe for both, as `>LOG 2>&1` keeps their order.
            let (mut reader, writer) = io::pipe()?;
            cmd.stdout(writer.try_clone()?).stderr(writer);
            let mut child = cmd.spawn().with_context(|| format!("cannot run {cmd:?}"))?;
            // Our copies of the pipe's write end must go for EOF to come.
            drop(std::mem::replace(cmd, Command::new("")));
            let mut buf = [0; 8192];
            let mut out = io::stdout();
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                file.write_all(&buf[..n])?;
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
            child.wait()?
        } else {
            cmd.stdout(file.try_clone()?).stderr(file);
            cmd.status().with_context(|| format!("cannot run {cmd:?}"))?
        };
        Ok(status.success())
    }

    /// A failed step: its main lines (those MAIN keeps, at most N), or all
    /// of it with --verbose (not again with --stream); the whole output is
    /// in the log either way.
    fn failed(&mut self, what: &str, log: &str, main: impl Fn(&str) -> bool, n: usize) {
        self.end();
        self.status = 1;
        println!("FAIL {what} (all of it: {})", self.shown(log));
        let text = fs::read(self.log(log)).unwrap_or_default();
        let text = String::from_utf8_lossy(&text);
        if self.o.verbose && !self.o.stream {
            print!("{text}");
        } else {
            for line in text.lines().filter(|l| main(l)).take(n) {
                println!("{line}");
            }
        }
    }

    /// The workspace's tests: how many passed.
    fn tests(&mut self, tools: &Tools, args: &[OsString]) -> Result<u64> {
        self.header("tests");
        let mut cmd = tools.cargo();
        cmd.args(["test", "--workspace", "--color", "never"]).args(args);
        // Text is drawn with the fonts the dev shell fixes for tests
        // (flake.nix, testFontConfig), not the host's, so that it is drawn
        // as in CI.
        if let Some(fc) = std::env::var_os("TEMZDF_TEST_FONTCONFIG").filter(|v| !v.is_empty()) {
            cmd.env("FONTCONFIG_FILE", fc);
        }
        if !self.run(&mut cmd, "test")? {
            self.failed(
                "tests",
                "test",
                |l| {
                    l.starts_with("error")
                        || ["-->", "FAILED", "panicked", "left:", "right:"].iter().any(|p| l.contains(p))
                },
                60,
            );
        }
        self.end();
        let log = fs::read(self.log("test")).unwrap_or_default();
        Ok(count_passed(&String::from_utf8_lossy(&log)))
    }

    /// The unit tests under Miri (`scripts/miri.sh`, Unix only); its output
    /// goes on as it comes, and into `target/check/miri.log`.
    fn miri(&mut self) -> Result<()> {
        if !cfg!(unix) {
            println!("SKIP miri (Unix only)");
            return Ok(());
        }
        let path = self.log("miri");
        let mut file = File::create(&path).with_context(|| format!("cannot create {}", path.display()))?;
        let mut child =
            command("scripts/miri.sh").stdout(Stdio::piped()).spawn().context("cannot run scripts/miri.sh")?;
        let mut reader = child.stdout.take().expect("piped");
        let mut buf = [0; 8192];
        let mut out = io::stdout();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            file.write_all(&buf[..n])?;
            out.write_all(&buf[..n])?;
            out.flush()?;
        }
        let st = child.wait()?;
        // Each failed test's own log (named on its FAIL line).
        if self.o.verbose {
            let text = fs::read_to_string(&path).unwrap_or_default();
            for log in miri_logs(&text) {
                println!("--- {log}");
                print!("{}", fs::read_to_string(&log).unwrap_or_default());
            }
        }
        // 2: no Miri here (e.g. the CI shell); it said so.
        if !matches!(st.code(), Some(0 | 2)) {
            self.status = 1;
        }
        Ok(())
    }
}

/// The sum of every `N passed` in cargo test's output.
fn count_passed(log: &str) -> u64 {
    let mut sum = 0;
    for (i, _) in log.match_indices(" passed") {
        let digits = log[..i].bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            sum += log[i - digits..i].parse::<u64>().unwrap_or(0);
        }
    }
    sum
}

/// The `target/miri-logs/....log` paths in miri.sh's output.
fn miri_logs(text: &str) -> Vec<String> {
    let mut logs = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("target/miri-logs/") {
        let tail = &rest[i..];
        let end = tail.find([' ', ')', '\n']).unwrap_or(tail.len());
        // The longest prefix ending in `.log`, as `[^ )]+\.log` matches.
        let candidate = &tail[..end];
        if let Some(j) = candidate.rfind(".log")
            && j > "target/miri-logs/".len()
        {
            logs.push(candidate[..j + ".log".len()].to_owned());
        }
        rest = &tail[end.max(1)..];
    }
    logs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_every_passed() {
        let log = "test result: ok. 12 passed; 0 failed\nrunning\ntest result: ok. 3 passed; 1 failed\n0 passed\n";
        assert_eq!(count_passed(log), 15);
        assert_eq!(count_passed("no tests passed"), 0);
    }

    #[test]
    fn finds_miri_logs() {
        let text = "FAIL foo (target/miri-logs/a-b.log)\nFAIL bar target/miri-logs/c.log more\n";
        assert_eq!(miri_logs(text), ["target/miri-logs/a-b.log", "target/miri-logs/c.log"]);
    }

    #[test]
    fn parses_leading_options() {
        let args = ["--fix", "--no-miri", "--stream", "-p", "xtask", "--all"].map(OsString::from);
        let o = Options::parse(args.into_iter(), false);
        assert!(o.fix && !o.miri && o.stream && !o.all && !o.verbose);
        assert_eq!(o.test_args, ["-p", "xtask", "--all"]);
        let o = Options::parse(["--stream", "--fix"].map(OsString::from).into_iter(), true);
        assert!(o.stream && !o.fix);
        assert_eq!(o.test_args, ["--fix"]);
    }
}
