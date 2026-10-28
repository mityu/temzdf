# The key mapping system

Status: agreed with the user on 2026-10-28 and implemented the same day,
except decision 8 (deferred, see there). How to use it is in
`doc/config.md` ("Modes and keys"); the code is in
`crates/temzdf-server/src/keymap/` (modes, the matcher), `session.rs`
(`drive`, `layers`) and `session/script.rs` (conditions through the script
host). This document supersedes decision 8
(`inherit`) of `doc/scripting.md`; the rest of that document's decisions on
keys (notation, sequences, counts, `<C-c>`, feedkeys, escape hatches) still
hold and are summarised here.

## Ideas

Two kinds of state decide what a key does, and they are kept apart:

- **Modes**: states the user enters and leaves with keys. One at a time;
  a mode also decides what unmapped keys do and whether digits are counts.
- **Conditions** (`when`, as nvim-insx's `enabled`): states the world
  decides, such as the kind of the focused pane or whether a PDF pane's
  search prompt is open. They are independent of each other and combine
  freely. A mapping whose condition is false does not exist for that key.

On top of modes, **transient modes** (submodes) sit over the current mode
for a while, as Emacs' transient keymaps (`set-transient-map`) and
kana/vim-submode do.

Rule of thumb for configurations: something entered on purpose and exclusive
is a mode; something that happens (a popup opened, a program switched to the
alternate screen) and combines with other such things is a condition. A
completion popup, for instance, is a condition: modelling it as a mode needs
the mode entered and left on every open and close, misses ones that happen
between keys, and multiplies with every other such state.

The aim is that both modal (Vim-like) and mode-less (Emacs-like)
configurations are written with the same system.

## Modes

- **Built-in modes stay**: `normal` (operating: keys act on temzdf, unmapped
  keys are dropped, counts), `terminal` (pass-through: unmapped keys go to
  the pane, no counts) and `pdf` (PDF panes: unmapped keys dropped, counts).
  The pane kinds decide them, so switching panes changes the mode without
  anything to configure. Their mappings come from `defaults.ts`, as now.
- `defineMode(name, properties)` makes a mode or changes one:

  ```ts
  defineMode(name, {
    unmapped?: "drop" | "passthrough", // keys no mapping takes
    count?: boolean,                   // digits are counts
    transient?: boolean,               // a submode (below)
    keepalive?: "mapped" | "always" | ((ctx: KeyContext) => boolean),
    timeout?: number,                  // ms without keys before leaving
  });
  ```

- **The mode history**: `switchMode` pushes a mode, `exitMode` pops one, at
  most `modehistory` entries. The bottom is the focused pane's built-in mode.
- **Focus changes** (keys, the mouse, a pane closing or exiting, a new pane
  or tab page, the first pane, CLI actions) rebuild the history from the new
  pane's built-in mode, in the server, synchronously. This is why the
  built-in modes stay: done by a hook calling `switchMode`, it would need the
  script host, could run in the middle of a mapping (`[moveFocus(),
  exitMode()]` would pop what the hook pushed), and would have to fire once
  on every path that moves the focus.
- `inherit` is gone: sharing another mode's keys is what transient modes
  with `keepalive` do, with the parent being whatever mode they were entered
  from rather than one fixed in the definition. Whether a static parent is
  wanted after all is left until the system has been used (it does not
  conflict with conditions: Emacs has both keymap parents and conditional
  keymaps).

## Mappings

- `keymap.add(mode, lhs, rhs, { when, nowait, desc })`; `rhs` is an action, an
  array of actions run in order by the server, or a synchronous function, as
  now.
- **`when`**: the mapping exists only while it returns true. When it is
  false the key is looked up as if the mapping were not there: another
  mapping, else the mode's `unmapped`.
- Conditions are JavaScript functions (decision 1 below), synchronous and
  without side effects: they may run
  several times for one key (sequences, key readings). Commands that change
  state are refused inside them; queries work. One that throws counts as
  false, and the error is shown.
- **When conditions are evaluated**: with the context of the first key of a
  sequence (focused pane, its state), kept for the rest of the sequence, so
  a focus change by the mouse between `g` and `t` does not change what `gt`
  means. Only the mappings that are candidates for the keys typed are
  evaluated, in one round trip to the script host; keys with no candidate
  (most typing into a terminal) call nothing.
- **Sequences** wait only for mappings whose condition holds: if `gt` is
  disabled, `g` runs at once instead of waiting `timeoutlen`.
- Unchanged: key notation and readings (`<S-a>` falls back to `A`, ...),
  `timeoutlen`, `nowait`, counts, `feedkeys` and its flavours; `<C-c>`
  cannot be mapped (it interrupts a running function and cancels pending
  keys and the count).

## Transient modes (submodes)

- A mode with `transient: true`, entered with `switchMode`, sits over the
  mode it was entered from (the mode below), so one submode works from any
  mode.
- **`keepalive`** says how long it stays (Emacs' KEEP-PRED):
  - `"mapped"`: while keys are mapped in it. Another key leaves it and is
    handled again in the mode below (kana/vim-submode, Emacs' default).
  - `"always"`: it stays; keys it does not map are looked up in the mode
    below and, failing that, handled by that mode's `unmapped`.
  - a function: asked before each key; when false the submode is left and
    the key handled again below.
- `timeout`: leave after that many milliseconds without keys (each key
  restarts it).
- `unmapped` and `count` not given: the mode below's.
- Example: a resize submode entered from `normal`, where `+` and `-`
  repeat:

  ```ts
  defineMode("resize", { transient: true, keepalive: "mapped", timeout: 1500 });
  keymap.add("resize", "+", action.resizePane("taller"));
  keymap.add("resize", "-", action.resizePane("shorter"));
  keymap.add("normal", "s", action.switchMode("resize"));
  ```

## Unchanged

- The script host down: mappings are off and the fixed keymap works
  (`<C-b>d` detach, `<C-b>q` end the session, `<C-b>r` restart the host).
- Escape hatches of `doc/scripting.md` (decision 10).

## Decisions on the former open questions (2026-10-28)

1. **Conditions are JavaScript functions only**, at first. A declarative
   form the server evaluates itself (`when: { pane: "pdf" }`: no round trip,
   works while a script runs) can come later if the round trips show.
2. **Several mappings for one key whose conditions hold**: the one
   registered last is tried first (the user's configuration before the
   defaults); an optional `priority` orders them explicitly.
3. **Overriding and removing**: mappings take a `namespace` (named `tag`
   at first); mapping the same mode, keys and namespace again replaces the
   mapping, while mappings without one are always added (the user's
   decision: several conditional mappings of a key need no namespace, and
   reloading the configuration starts from the built-in modes, so nothing
   doubles). `unmap` removes a namespace's mappings of the keys, or all of
   them. The defaults are in the namespace `default`. (Later, 2026-10-28:
   `map` and `unmap` became `keymap.add` and `keymap.remove`, and
   `keymap.clear({ mode, namespace })` removes every mapping of a mode, a
   namespace or both, as Vim's `:mapclear`.)
4. **Listing**: `getMappings({ mode, namespace })`, for help and key hints. Used
   to copy one mode's mappings into another it gives a snapshot, not a live
   parent (mappings added later are missing); copying in the `SessionReady`
   hook avoids most of that.
5. **A focus change inside a submode** rebuilds the history, so the submode
   is left, as the `normal` mode is today.
6. **Submodes over submodes**: with `keepalive: "always"` lookups walk down
   the history until a mode that is not transient.
7. **Sequences across a submode and the mode below**: with `"always"` the
   two form one table (a submode's `g` and the mode below's `gt` wait as in
   Vim); with `"mapped"` only the submode's mappings count.
8. **Fixed keys of PDF panes** (the search prompt, the outline): deferred
   (the user, 2026-10-28). As conditional mappings, every default mapping
   of the `pdf` mode would need the condition "the prompt is closed" (else
   typing `n` or `j` into the prompt runs them), and with JavaScript
   conditions only, every key in a PDF pane would cost a round trip to the
   script host. Alternatives: built-in modes entered by `/` and `<Tab>`
   (unmapped keys going to the prompt), or a declarative condition form
   the server evaluates itself (decision 1). Until then the pane takes
   those keys before the mappings, as before.
9. **The mouse** keeps today's behaviour: in the `normal` mode the left
   button selects in terminal panes even where the program uses the mouse.
10. **`ModeChanged`** fires when a submode is entered or left too, so a
    status line can show it.

## Prior art

- **nvim-insx**: per key, recipes with `enabled(ctx)` and an action; the
  first enabled one runs, otherwise the key is typed as is.
- **kana/vim-submode**: enter with a key, stay while pressing the submode's
  keys, leave on any other key (optionally handled again) or a timeout.
- **Emacs**: no mode switching; active keymaps are layered (overriding and
  emulation maps, minor modes' maps active while their variable is true, the
  major mode's map, the global map) and a key not found in a layer is looked
  up below. Keymaps have parents (`set-keymap-parent`); single bindings can
  have conditions (`menu-item` with `:filter`); `set-transient-map` (with
  KEEP-PRED, an exit function and a timeout in recent versions),
  `repeat-mode` (Emacs 28) and packages such as hydra and transient build
  submodes. company-mode's `company-active-map` is a layer active while
  candidates show: the completion popup as a condition, not a mode.
- **QMK and ZMK keyboard firmware**: layers whose keys can be transparent,
  falling through to the layer below.
