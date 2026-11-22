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

Everything here is TypeScript in the shipped scripts: temzdf's core is
Rust and everything else extends it in scripts. Rust gains only what a
script cannot do (`PaneScrolled`, below); matching speed in the script
host is judged after using it (see "Speed").

## Items

```ts
interface Item<T extends Detail = {}> {
  readonly id: string | number;          // unique within its source
  readonly value: string;                // what matchers match
  readonly detail: T;                    // the source's own data, typed
  label?: string;                        // what is shown (default: value); set by renderers only
  decorations?: readonly Decoration[];   // matchers add matched parts, renderers styling
}
interface Decoration { start: number; length: number; style?: string }
```

- As vim-fall's `IdItem`, with three changes: positions count UTF-16 code
  units (JavaScript strings and temzdf's text positions both do; vim-fall
  counts bytes), `id` is a string or number (the engine compares and keys
  by it) and unique within its source (the engine tells sources apart
  when it mixes them).
- `detail` carries what each part adds: refiners turn `Item<V>` into
  `Item<V & U>` (vim-fall's way), and a matcher's score goes there too.
  Scripts get freedom and the responsibility that comes with it.
- Matchers match `value` and add the matched parts to `decorations`, at
  a point where nothing has set `label` (so positions on `value` are
  positions on what is shown). Only renderers set `label`, after
  matching; a renderer that changes it moves the decorations along
  (vim-fall's rule: respect and extend existing decorations). A source or
  refiner that wants another display puts what it needs in `detail` for a
  renderer.
- Completion's own fields (a range, the text to insert, a kind) are not
  here: the completion layer adds them (`Item<T> & { range?, ... }`);
  LSP's own data (`data`, `command`) goes in `detail`.
- Once script-made TUI panes exist, a renderer may return an element tree
  (JSX) instead of a label and decorations, with a helper that splits a
  value into matched and unmatched spans; plain label and decorations
  stay the simple path.

## Collecting, matching, sorting

- A session (a fuzzy finder's, or one completion) keeps, per source, the
  running collection and its abort signal, the items collected and its
  state (collecting, done, failed; for completion also `prev`); and over
  all: the matched items (sorted), the marked items' ids, errors.
- Items arrive in batches from a source's async generator, through its
  refiners. While the query stays, only new batches are matched; a new
  query aborts the running match (an `AbortController` per generation)
  and matches everything collected again. Collecting goes on.
- Matchers and sorters are written plainly (`for await`, `yield`), given
  arrays of items (`{ query, items }`). The engine breaks the work up: it
  takes the matched items in chunks (1000 items or 100 ms, as vim-fall's
  match processor) and waits for a timer (`setTimeout`) between chunks.
  The script host runs pending promise jobs until none is left before it
  takes the session's next message (JavaScript's event loop: microtasks
  before the next task), so a chain of `yield`s alone would keep mapped
  functions waiting; a timer lets the host take messages in between
  (vim-fall does the same with `delay(0)`). Each job also stays far below
  the 10 s unattended limit.
- Renderers run only on what is written to the list (below), not on
  every match.
- Combining several matchers, sorters, renderers, previewers, actions and
  sources follows vim-fall (`composeMatchers`, `composeSorters`,
  `refineSource`, `composeSources`, ...). A few matchers are shipped;
  which is the default is not important.

## The list

- The list is a text buffer (no file), one candidate per line, in a pane
  without the focus (the prompt or the edited buffer has it). The
  selected candidate is the list pane's cursor line, shown by
  `cursorline`; moving, scrolling and keeping the cursor in view
  (`scrolloff`, `reveal`, `scroll`, the mouse wheel) are the text pane's
  own. Decorations become the buffer's decorations.
- The engine does not write every match: it writes the first part and
  adds more in chunks as results come, rewriting the first screens while
  the order changes (sorting by score moves later items up) and keeping
  the selected candidate (by id) on the cursor line. It writes more when
  the cursor nears the end (its own keys move the cursor) or when the
  list scrolls near the end: `PaneScrolled`, an event added in Rust for
  this (Vim's `WinScrolled`), also wanted for highlighting only the
  visible range later (tree-sitter, LSP semantic tokens).
- What the engine hands the UI is small: the lines (labels and
  decorations), the counts (collected, matched), the state (collecting,
  matching, done), the marked ids, errors. No query (the prompt or the
  buffer has it) and no scroll offset (the pane has it). The engine reads
  the selected candidate from the list pane's cursor line. A fuzzy
  finder's screen and completion's popup are two UIs over this.
- Operations are the session's methods (move the cursor, mark, invoke an
  action, cancel), called from key mappings.

## Actions

```ts
interface Action<T = {}> {
  invoke(
    p: { item: Item<T> | undefined; marked: readonly Item<T>[]; matched: readonly Item<T>[] },
    o: { signal: AbortSignal; action: Actions },
  ): void | true | Promise<void | true>;   // true keeps the session open
}
```

- As vim-fall's, with the actions to use passed in (`ctx.action`: an
  action runs asynchronously, where plain `action.*` has no context).
  Both the cursor's item and the marked ones are passed; a helper gives
  "the marked ones, else the cursor's".
- A session has named actions and a default one; key mappings invoke one
  by name. Picking an action is a fuzzy finder over that table.
- Accepting a completion is an action too: the default one replaces the
  candidate's range (else the anchor to the cursor) through the adapter;
  LSP's additional edits and command are actions composed on it. The
  completion layer passes the source's `CompletionContext` along; the
  details are decided with the completion layer.

## Fuzzy finder sources

```ts
interface Source<T>  { collect(p: { args: readonly string[] }, o: { signal: AbortSignal }): AsyncIterable<Item<T>> }
interface Curator<T> { curate(p: { args: readonly string[]; query: string }, o: { signal: AbortSignal }): AsyncIterable<Item<T>> }
interface Refiner<T, U> { refine<V extends T>(p: { items: AsyncIterable<Item<V>> }, o: { signal: AbortSignal }): AsyncIterable<Item<V & U>> }
```

vim-fall's; `args` are the finder's arguments as invoked. What a source
needs beyond them (the working directory, the current buffer) it reads
with helpers. Completion's sources (below) take a context they find
instead of `args`; refining, matching and sorting are shared.

## Modules and definitions

- `temzdf/ff`: the engine (sessions, the pipeline, the list), the types,
  the combinators. `temzdf/ff/completion`: the completion layer (finding
  contexts, registration, groups, the adapter), on `temzdf/ff`. Shipped
  matchers, sorters, renderers, actions and common sources go in a module
  of their own (`temzdf/ff/builtin`, name not final).
- A fuzzy finder is defined by name, as vim-fall's pickers:
  `ff.define("file", { source, matchers, sorters, renderers, actions,
  defaultAction })`, and started by an Ex command (`:FF NAME ARGS`) or a
  function (`ff.start(NAME, args)`).

## Speed

Measured in the script host (2026-11-22, a debug build, 100k path-like
strings averaging 40 characters): one query took 2-7 s with npm `fzf`
(v2, its default), 1-2 s with v1, 0.3-0.5 s with a plain subsequence
check and 0.1-0.3 s with `fuzzysort` (top 50); building `fzf`'s index
took 0.8 s; Node is 20-40 times faster. Completion's lists (hundreds to
thousands) are fine; a fuzzy finder over 100k files needs narrowing only
the previous matches when the query extends (`todo.md`) and a light
default matcher. Rust is considered only if that is still unusable.

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
