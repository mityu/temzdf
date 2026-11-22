# Completion and fuzzy finding for scripts (draft)

A draft: nothing is built yet. It records what was decided with the user
(2026-11-22) about a framework, written as scripts, for completion in text
panes and prompts and for fuzzy finders, and the reasons. What is still
open is in `todo.md`.

## A skeleton shared with fuzzy finders

The parts follow vim-fall (a Vim/Neovim fuzzy finder): candidates are
produced by async generators that take an `AbortSignal`.

- **Source**: collects candidates once; a **Matcher** narrows them to the
  query, a **Sorter** orders them.
- **Curator**: collects again for each query (a live grep; for completion,
  LSP and prefix lookups).
- **Action**: what accepting a candidate does.

Source, Matcher, Sorter, Action and Curator are general enough for both
completion and fuzzy finders. **Renderer** and **Previewer** are to be
rethought for temzdf (they may build on script-made TUI panes,
`doc/design/tui-panes.md`, once those exist).

A fuzzy finder is started by the user, who names its sources; it lives
until closed. Completion is started by events (typing, a trigger
character) or by hand, from sources registered for the context, and is
short-lived: the engine starts and drops completions as the user types.
Emacs works the same way: completion tables and completion styles are
shared between the minibuffer (`completing-read`) and in-buffer
completion (`completion-at-point-functions`); only the entry points and
the UI differ.

## Completion sources

```ts
type CompletionContext<T> = { anchor: Position; detail: T };

type CompletionSource<T> = {
  findContext: (prev: CompletionContext<T> | undefined) => CompletionContext<T> | undefined;
  source: (p: { detail: T }, o: { signal: AbortSignal }) => AsyncIterable<Item>;
};
type CompletionCurator<T> = {
  findContext: (prev: CompletionContext<T> | undefined) => CompletionContext<T> | undefined;
  curator: (p: { query: string; detail: T }, o: { signal: AbortSignal }) => AsyncIterable<Item>;
};
```

- **`findContext`** is the source's own decision, called by the engine on
  every keystroke while completing (synchronously, in a hook's context, so
  plain `action.*` and the helpers read the state as it is at that moment).
  It returns:
  - a new context: collect (again) with it;
  - `prev` itself (the same object): keep the earlier results, and a
    collection still running goes on;
  - `undefined`: the source does not apply here.

  `prev` is what it returned last time in this completion (`undefined` at
  the start). So whether to collect again is the source's call: LSP looks
  at the anchor and whether its last answer was incomplete, a prefix
  lookup of words at whether the new query extends the old one, a live
  grep always collects again.
- **`anchor`** is the completion's base point: candidates without a range
  of their own replace the text from it to the cursor, and a new anchor is
  a new completion. The query is the text from the anchor to the cursor;
  the engine computes it and hands it to curators (a source that needs
  another form, an unescaped path, say, converts it itself).
- **`detail`** is the source's own context, typed: what the collection
  needs, fixed when `findContext` ran (the collection runs later, in a
  dispatched task, while the state may change). A collection may write
  into it for the next `findContext` (LSP marks an incomplete answer
  there).
- A candidate may carry its own range (LSP's `textEdit`), which wins over
  the anchor for that candidate: replacing, and narrowing by the text from
  its range's start to the cursor.
- **Source or curator**: the only difference is whether the collection
  takes the query. Either way the engine narrows what it holds with the
  Matcher as the query changes, since kept results must be narrowed again.
- There is no context type handed in by the engine: helper functions read
  what a source needs (the text before the anchor, the cursor, the
  buffer's path and file type, the syntax at the cursor). Trigger
  characters stay inside a source (`findContext` looks at the character
  before the cursor), and whether the completion was started by hand is
  the engine's business (a source's options say whether it starts by
  itself and from how many characters); LSP's `triggerKind` does not tell
  typing from a manual call either, and its other kinds (a trigger
  character, asking again after an incomplete answer) a source can tell
  by itself.
- Asynchronous collection runs in dispatched tasks, which use `ctx.action`
  (plain `action.*` needs a mapped or hook function's context); helpers
  that change the UI take the actions to use as an argument, so both work.

## Registering, and how sources mix

Registration follows key mappings (`doc/design/keymaps.md`): `when` (a
condition on the context, as for mappings: the user's narrowing on top of
the source's own `findContext`), `priority`, `namespace`, plus whether the
source starts by itself and from how many characters.

- The engine uses **every** applicable source, each with its own context,
  collecting in parallel, and mixes the candidates. Using only the first
  applicable source is a wrapper on top (`firstOf(a, b, c)`: its
  `findContext` asks each in turn and records which one applied). The
  other way round, a wrapper that merges sources (Emacs's Cape
  `cape-capf-super`) would have to merge their anchors and keep each
  one's `prev`: the engine's own work done again.
- **Groups** (nvim-cmp's `group_index`): sources of a group mix; group N
  is shown only when no earlier group has a candidate left after
  narrowing to the current query. All groups collect at once and the
  engine hides later groups when showing, so nothing waits. Chosen over
  blink.cmp's per-source `fallbacks`: in the common case ("LSP and
  snippets, else words") a fallback cannot say "when both are empty", and
  groups cover the cases configurations use. Neither includes the other
  (a fallback can say "words when LSP is empty, snippets regardless");
  the engine keeps the mix as a tree (merge, else) so expressions can be
  added later without contradiction.
- Emptiness is judged after narrowing, and narrowing happens in the
  engine, so mixing cannot be a transformer of sources (vim-fall's
  refiners); transformers are for per-candidate work (labels, details,
  dropping, removing duplicates).

## LSP, for reference

`textDocument/completion` sends the document, the cursor position (UTF-16
by default, as temzdf's text positions) and a trigger kind (invoked,
trigger character, asking again after an incomplete answer); the server
answers with items (label, kind, detail, documentation, `filterText`,
`sortText`, `insertText` or `textEdit` with one or two ranges, additional
edits, a command, commit characters, `data` for `completionItem/resolve`)
and `isIncomplete`. The client narrows each item by the text from its
range's start to the cursor, and asks again on typing only after an
incomplete answer. An LSP source maps onto the above: `findContext` keeps
`prev` while the anchor stays and the last answer was complete; the
curator sends the request with the position fixed in `detail`.

## Open

What is still to decide is listed in `todo.md`.
