//! Code blocks highlighted with syntect, as highlight groups (the scripts'
//! colour scheme gives them colours): Vim's standard names.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Scope prefixes and the groups they show as, the most specific first.
const GROUPS: &[(&str, &str)] = &[
    ("comment", "Comment"),
    ("string", "String"),
    ("constant.numeric", "Number"),
    ("constant.character.escape", "Special"),
    ("constant", "Constant"),
    ("keyword.operator", "Operator"),
    ("keyword", "Keyword"),
    // Keywords some syntaxes scope as types (Rust's `fn`, `struct`).
    ("storage.type.function", "Keyword"),
    ("storage.type.struct", "Keyword"),
    ("storage.type.enum", "Keyword"),
    ("storage.type.impl", "Keyword"),
    ("storage.type.trait", "Keyword"),
    ("storage.type.module", "Keyword"),
    ("storage.type.type", "Keyword"),
    ("storage.type.class", "Keyword"),
    ("storage.type", "Type"),
    ("storage", "Keyword"),
    ("entity.name.function", "Function"),
    ("support.function", "Function"),
    ("entity.name.type", "Type"),
    ("entity.name.class", "Type"),
    ("entity.name.struct", "Type"),
    ("entity.name.enum", "Type"),
    ("support.type", "Type"),
    ("support.class", "Type"),
    ("entity.name.tag", "Keyword"),
    ("entity.other.attribute-name", "Type"),
    ("meta.preprocessor", "PreProc"),
    ("variable.language", "Constant"),
];

fn group(stack: &ScopeStack) -> Option<&'static str> {
    // The innermost scope that has a group.
    stack.as_slice().iter().rev().find_map(|s: &Scope| {
        let name = s.build_string();
        GROUPS.iter().find(|(prefix, _)| name == *prefix || name.starts_with(&format!("{prefix}."))).map(|(_, g)| *g)
    })
}

/// `code` in `lang` (a fence's info word: a name or an extension) as lines
/// of pieces and their groups; plain lines when the language is unknown.
pub fn highlight(lang: &str, code: &str) -> Vec<Vec<(String, Option<&'static str>)>> {
    let plain = || code.split('\n').map(|l| vec![(l.to_owned(), None)]).collect();
    if lang.is_empty() {
        return plain();
    }
    let set = syntaxes();
    let Some(syntax) = set.find_syntax_by_token(lang) else { return plain() };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    for line in code.split('\n') {
        let with_nl = format!("{line}\n");
        let Ok(ops) = state.parse_line(&with_nl, set) else { return plain() };
        let mut pieces: Vec<(String, Option<&'static str>)> = Vec::new();
        let mut at = 0;
        let mut push = |text: &str, g: Option<&'static str>| {
            if text.is_empty() {
                return;
            }
            match pieces.last_mut() {
                Some((t, last)) if *last == g => t.push_str(text),
                _ => pieces.push((text.to_owned(), g)),
            }
        };
        for (i, op) in ops {
            let i = i.min(line.len());
            push(&line[at..i], group(&stack));
            at = at.max(i);
            if stack.apply(&op).is_err() {
                return plain();
            }
        }
        push(&line[at..], group(&stack));
        out.push(pieces);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "slow under Miri (loading the syntaxes)")]
    fn rust_comes_out_as_groups() {
        let lines = highlight("rust", "fn main() { // hi\n    let s = \"x\";\n}");
        assert_eq!(lines.len(), 3);
        let groups: Vec<_> = lines[0].iter().map(|(t, g)| (t.as_str(), *g)).collect();
        assert!(groups.contains(&("fn", Some("Keyword"))), "{groups:?}");
        assert!(groups.contains(&("main", Some("Function"))), "{groups:?}");
        assert!(groups.iter().any(|(t, g)| t.contains("// hi") && *g == Some("Comment")), "{groups:?}");
        assert!(lines[1].iter().any(|(t, g)| t == "\"x\"" && *g == Some("String")), "{:?}", lines[1]);
        // The text is all there.
        let text: Vec<String> = lines.iter().map(|l| l.iter().map(|p| p.0.as_str()).collect()).collect();
        assert_eq!(text, ["fn main() { // hi", "    let s = \"x\";", "}"]);
    }

    #[test]
    fn unknown_languages_are_plain() {
        assert_eq!(highlight("no-such-language", "a\nb"), [vec![("a".to_owned(), None)], vec![("b".to_owned(), None)]]);
    }
}
