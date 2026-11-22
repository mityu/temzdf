# Completion and fuzzy finding for scripts (draft)

A draft. It records what was decided with the user (2026-11-22) about a
framework, written as scripts, for completion in text panes and prompts
and for fuzzy finders, and the reasons, with the details the code settled
(`crates/temzdf-script/runtime/ff/`). What is still open, and what is
built, is in `todo.md`.

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
script cannot do (`PaneScrolled`, below; for the popup,
`text.screenPosition` and `screenSize`, floats with `border: false` and
`focusable: false`, and a pane's own highlight groups through
`text.setOptions({ highlights })`, added 2026-11-22); matching speed in
the script host is judged after using it (see "Speed").

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
  hands the matcher at most 1000 items at a time and waits for a timer
  (`setTimeout`) after each such chunk, and also within one when its
  matches took 100 ms (vim-fall's match processor counts 1000 matched
  items or 100 ms; counting the items handed in also breaks up a query
  that matches little). Collecting waits for a timer every 1000 items or
  100 ms the same way (a source yielding from an array would otherwise run
  in promise jobs alone), and what was collected until the host's next
  turn goes to the matcher as one batch. A matcher therefore never sees
  all items at once; one that keeps only the best N would do so per
  chunk.
  The script host runs pending promise jobs until none is left before it
  takes the session's next message (JavaScript's event loop: microtasks
  before the next task), so a chain of `yield`s alone would keep mapped
  functions waiting; a timer lets the host take messages in between
  (vim-fall does the same with `delay(0)`). Each job also stays far below
  the 10 s unattended limit.
- A new query also aborts the matcher's signal; the engine itself drops
  whatever a match of an older query still yields (it compares
  generations), so a matcher that ignores the signal only wastes time.
- What a UI reads is published: the matches so far, sorted (the sorter
  runs on all of them each time, on a copy, so the order is stable as
  more come), with the query they match. Publishing is throttled: the
  first change after a quiet spell is published at the host's next turn,
  later ones at most every 40 ms (an option), or every twice as long as
  the last sort took when that is longer (sorting 100k items again and
  again would otherwise take most of the time). The listeners
  (`onChange`) are called after each publication and read a snapshot:
  a range of the matched items, the counts (collected, matched), the
  state (collecting, matching, done; each source's own: collecting, done,
  failed with its error, stopped), the marked items' keys, the errors
  (of sources, the matcher, the sorter, listeners). Until the new query's
  first publication the old one's matches stay, so the list does not
  flash empty.
- An item's key in a session is its source's index and its id: marks are
  kept by key, so they survive new queries (the matcher returns new
  objects) and a curator collecting again with the same ids.
- A curator's collection is aborted and started again on every new
  query; a source's goes on. Stopping a session keeps what was collected
  (a new query still matches it); disposing it stops everything.
- Renderers run only on what is written to the list (below), not on
  every match.
- Combining several matchers, sorters, renderers, previewers, actions and
  sources follows vim-fall (`composeMatchers`, `composeSorters`,
  `refineSource`, `composeSources`, ...). A few matchers are shipped;
  which is the default is not important. As vim-fall: `composeMatchers`
  is a pipe (each narrows what the one before matched, decorations
  adding up), `composeRefiners` and `refineSource`/`refineCurator` pipe
  refiners lazily, `composeSorters` and `composeRenderers` run one after
  the other on the same array. Two differences: `composeSources` gives
  each item the id `JSON.stringify([index, id])` (vim-fall numbers the
  items again, which would lose the selection and marks when a source
  collects again), and `composeActions` returns `true` when any of its
  actions did (vim-fall drops what they return).
- The shipped matchers (`temzdf/ff/builtin`): `substring` (every word of
  the query, split at white space, somewhere in the value), `prefix` (the
  query as it is at the start; for command and option names, as Vim) and
  `fuzzy` (each word a subsequence, scored: matches at the start, after a
  separator or a lower-case letter, and in a row score more; the score in
  `detail.score`). All ignore case by default (or respect it, or
  `"smart"`), lower-casing only where that keeps a code unit one code unit
  so that positions stay positions in the value, and decorate matches with
  the style `FFMatch`. The sorter `byScore` orders by `detail.score`,
  stably, by counting when the scores are integers (a comparison sort of
  100k items took 0.2-0.3 s in a debug build, counting under 0.1 s).

## The list

- The list is a text buffer (no file), one candidate per line, in a pane
  without the focus (the prompt or the edited buffer has it). The
  selected candidate is the list pane's cursor line, shown by
  `cursorline`; moving, scrolling and keeping the cursor in view
  (`scrolloff`, `reveal`, `scroll`, the mouse wheel) are the text pane's
  own. Decorations become the buffer's decorations.
- `createList({ session, open, renderer, actions, defaultAction, ... })`
  (`temzdf/ff`, `runtime/ff/list.ts`) makes the buffer (`nofile`,
  `unlisted`, `nomodifiable`; writing lifts `nomodifiable` for the
  moment) and calls `open(buffer)`, which opens the pane (a float, a
  split) and returns it; or a pane is `attach`ed later (and `detach`ed:
  completion's popup hides when nothing matches). Text actions act
  through a pane, so writing waits for one. The pane goes without
  wrapping, line numbers and a status line, with `cursorline` and its own
  highlight groups `{ CursorLine: "FFSelection", CursorUnfocused: "None" }`
  (the colour, and no underline for the cursor of a pane without the
  focus), and the list's `highlights` over them.
- What is written is always the first lines of the published order: as
  many as are wanted, three screens of the pane at first (`initial`), a
  chunk more (`chunk`, as many) when the cursor (moved by the list's
  methods) or the pane's last line in sight (`PaneScrolled`, an event
  added in Rust for this, Vim's `WinScrolled`; also wanted for
  highlighting only the visible range later, tree-sitter, LSP semantic
  tokens) comes within a screen (`margin`) of the end. As results come,
  lines are added; when the order changes (sorting by score moves later
  items up), the lines from the first that differs are written again. A
  step writes at most a chunk more than was written, with a timer
  between steps. A new query writes from the top, puts the cursor there
  and wants the first screens again.
- The selection stays at the top until the user moves it (the list's
  methods, or the cursor found elsewhere than where the list put it);
  then the candidate on it is kept on the cursor line by its key when
  lines are written again (else the line, when it is gone).
- Writing waits for the renderer (which may be asynchronous), and the
  session's listeners run in timers, where only dispatched tasks act: the
  list writes in a task of its own, each step one `ctx.atomic`. Its
  methods run where the plain `action` works (mapped functions,
  scheduled ones). Renderers run on the lines written only; a label's
  line breaks become spaces.
- Decorations with a style (the matchers' `FFMatch`, the renderers' own)
  become buffer decorations in the list's namespace; a marked line is
  styled `FFMarked`. `temzdf/ff` defines `FFMatch`, `FFSelection` and
  `FFMarked` in a `SessionReady` hook registered when it is imported, so
  a configuration's own `highlight()` in a `SessionReady` hook of its
  own (registered after the import) wins; `defineHighlights()` defines
  them again.
- What the list hands its caller is small: `view()` (the query written,
  the counts collected, matched and written, the cursor's line, the
  marked count, the state, errors), `selected()`, `written()`, and
  `onChange`, called after lines were written or the selection moved. No
  query of its own (the prompt or the buffer has it) and no scroll offset
  (the pane has it). A fuzzy finder's screen and completion's popup are
  two UIs over this.
- Operations are the list's methods, called from key mappings: `move`
  (a count; past the end of what is written it stops at the last line
  and more is written; `wrap` goes round past the ends of everything),
  `next`, `prev`, `page`, `first`, `last` (everything is written, a chunk
  at a time, the cursor following the end), `toggleMark`, `mark`,
  `clearMarks` (the session keeps the marks), `invoke` (below) and
  `dispose` (its hooks, the pane unless `closePane: false`, the buffer;
  running actions are aborted; the session is its owner's to dispose).

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
- A list has named actions and a default one (`actions`,
  `defaultAction`); key mappings invoke one by name: `list.invoke(name?)`
  takes the cursor's item, the marked items (in the order they were
  marked) and every match at once, and runs the action in a dispatched
  task (`action` is the task's, `signal` aborts when the list goes).
  Unless it returns `true` the list is disposed after it; the action runs
  while the list is still open. Picking an action is a fuzzy finder over
  that table.
- Accepting a completion is an action too: the default one replaces the
  candidate's range (else the anchor to the cursor) through the adapter;
  LSP's additional edits and command are actions composed on it. The
  completion layer passes the source's `CompletionContext` along (its
  own shape, `CompletionAction`: "The completion layer" below).

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
The engine with the shipped `fuzzy` and `byScore` (a debug build, 100k
path-like strings, `runtime/ff/session.test.ts`): collecting them all
(matched against the empty query as they come) took about 3 s, of which
the source's generator alone took 0.5 s; one query then 0.4 s.

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

## The completion layer

Built as `temzdf/ff/completion` (`runtime/ff/completion/`). Decided with
the user (2026-11-22): the keys, inserting on select, the predicates for
`when`, starting by hand in the command line, the end conditions and the
item fields; the rest is how the code settled it.

- **Items**: `CompletionItem<T> = Item<T> & { insertText?, range?, kind?,
  description? }`. `value` is what matchers match and the text that goes
  in unless `insertText`; `range` replaces the anchor for that candidate
  (its end counts as the cursor when before it); `kind` and `description`
  are columns of the default renderer (`columns()`: label, kind,
  description, as wide as the widest of the lines written at once, counted
  in UTF-16 code units). Long documentation, resolving details later,
  snippets and edits elsewhere are left for later.
- **Registration**: `add(source, { when, priority, namespace, group,
  auto, minLength, matcher, sorter, action })`, `remove(source, {
  namespace })`, `clear({ namespace })`, `get()`, as key mappings: the same
  source in the same namespace again replaces it. Sources are mixed by
  group, then priority (higher first), then the order added (earlier
  first: unlike mappings, where the last added wins). `when` gets a
  mapping's context (the focused pane, its kind, the mode; `keys` empty).
- **Matching per source**: each source of a completion has a session of
  its own (`createSession`, one source in it): its context, its query and
  its matcher and sorter (by default `fuzzy()` and `byScore()`; Vim's own
  defaults, prefix for command and option names, are for the command
  line's sources to choose). The sessions are read as one by the popup's
  list (`Merged`, a `ListSession`: `createList` takes any engine shaped
  like a session): concatenated in the sources' order, each source's
  matches in its sorter's order (no sorting across sources), only the
  first group with a match left; items keyed by the source's registration
  and their id. The session's query is a key of the cursor and the text
  before it; the matcher narrows each candidate by the text from its
  range's start (else its source's anchor) to the cursor, read from the
  line as it was at the last check (matchers run in timers, where the
  buffer cannot be read).
- **Curators** are handed the query of the moment their context was
  found; like sources they collect again only for a new context (`prev`
  keeps what they collected, narrowed as the query changes), so a curator
  wanting every keystroke returns a new context each time.
- **When findContext runs**: hooks only `schedule()` a check (TextChanged
  of the completed buffer, InvokePost in its pane, ModeChanged,
  PaneFocused, PaneClosed); nothing changed since the last check (the
  buffer's version and the cursor) is nothing to do. Without a completion
  only text changes (typing or scripts, not undo) are looked at, and only
  while an `auto` source is registered.
- **Starting**: by hand (`start()`, `<C-n>`), every source whose `when`
  holds and whose `findContext(undefined)` answers takes part; by typing,
  one `auto` source must answer with a query of at least `minLength`
  (1), and then every applicable source takes part (`auto` says who may
  start, not who may join). A context whose anchor is after the cursor or
  on another line counts as not applying.
- **Ending**: accept, cancel, leaving the modes where text is typed
  (`configure({ modes })`: `vim/insert`, `vim/cmdline`, `input`, `edit`),
  the focus leaving the pane or the pane closing, the cursor before an
  anchor (or on another line) or moved past where the last change left it
  without typing, no source applying, and in a completion started by hand
  a source answering with another anchor. In an automatic one a new anchor
  only makes that source collect again. Nothing matching hides the popup
  and keeps the completion.
- **Selecting puts the candidate in** (Vim's way): `next()` / `prev()`
  move the list's cursor and replace the candidate's range with its text,
  the text it replaced remembered; the first `next()` takes the first
  candidate, `prev()` the last, and moving past either end puts the
  original text back (nothing selected: the popup's cursor line is not
  drawn). Moves asked for before anything was written (`<C-n><C-n>` typed
  at once) are made once lines are; a text change drops them. The
  popup's list never takes the focus: the edited pane keeps it and the
  keys.
- **Accepting** keeps the text in and ends, then runs the source's action
  (`CompletionAction`: `{ item, context, range, pane }`, in a dispatched
  task with the task's actions); the default (`replaceAction()`) puts the
  item's text in its range unless it is there already, which it is after
  selecting. Typing on after a candidate went in accepts it (and may start
  a new completion); moving the cursor ends without the action.
  `accept()` with nothing selected just ends. `cancel()` puts the original
  text back and ends.
- **Undo**: the first candidate put in opens an undo group
  (`text.undoBegin`) that closes when the completion ends, so the
  candidates put in and taken out are one step (inside the Vim insert
  mode's own group, nested). A cancelled completion leaves a step that
  changes nothing.
- **The popup** is a float without a border or the focus (`border:
  false`, `focusable: false`) over the list's buffer, at the earliest
  anchor's (the selected candidate's range's start) `text.screenPosition`:
  below it when the rows fit within `screenSize()`, else above, else on
  the side with more room, cut to it; as wide as its longest line (at
  most `maxWidth`, 60, and the screen), as high as the matches (at most
  `maxHeight`, 10), moved left to stay on the screen. It closes while
  nothing matches (the list detached) and opens again when something
  does. `visible()` tells whether it shows, `active()` whether a
  completion runs (for mappings' `when`, as Vim's `pumvisible()`).
- **Keys** (`keys`, a keyset in namespace `builtin-ff-completion`,
  enabled by the defaults): `<C-n>` / `<C-p>` select (starting a
  completion when none runs), `<C-y>` accepts and `<C-e>` cancels while
  one runs (`when: active`, priority 1 over the modes' own `<C-y>` and
  `<C-e>`). `<Tab>` / `<S-Tab>` start and select only in the command
  line: they come with the command line's marker (stage 4 in `todo.md`);
  in other text panes `<Tab>` stays a tab.
- **The adapter** (`CompletionAdapter`, `textPaneAdapter(pane, act?)`):
  what completion does to the pane (cursor, lines, replacing a range, the
  buffer's version), with the plain actions or a task's.
- **Helpers** for `findContext` act on the pane being completed in (the
  focused one otherwise): `cursor()`, `lineBeforeCursor()`,
  `wordStart(chars?)` (a run of the buffer's word characters, a regex
  class, ending at the cursor), `textFrom(anchor)`, `bufferInfo()` (the
  path, the name, and the extension as a file type until temzdf has file
  types). `firstOf(a, b, ...)` asks its sources in turn and keeps the one
  that answered while it answers with the same context.

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
