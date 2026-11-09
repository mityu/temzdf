//! `cargo run -p temzdf-markdown --example draw -- FILE.md [WIDTH]`: the
//! text mode's lines, to look at (math shown as its source).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = std::fs::read_to_string(&args[1]).expect("a Markdown file");
    let width = args.get(2).and_then(|w| w.parse().ok()).unwrap_or(60);
    let r = temzdf_markdown::render(&temzdf_markdown::Document::parse(&src), width, &|_| None);
    println!("{}", r.text);
}
