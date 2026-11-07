# Dot-repeat: design draft

Vim's `.` for the future text editor (`doc/survey/text-editor.md`), with
users able to define what `.` repeats. Discussed with the user on
2026-11-06 and 2026-11-07. **Nothing is built yet**; this records the
direction agreed and the choice made for nesting. Items marked
**(decided)** were put to the user; the others are proposals accepted as
the direction.

**Superseded in part (2026-11-07)**: the text editor became a framework
whose Vim-like behaviour is a script (`doc/design/text-editor.md`). The
server no longer manages repeat units or `repeat.set`; it provides events
(`InvokePre`/`InvokePost`, `TextChanged`) and the Vim script builds `.`
from them, with declarations attached to regions of execution
(`doc/design/text-editor.md`, "Guidance for scripts"). What follows is kept
for its analysis of recording and nesting and is rewritten when the Vim
script is.

## What a repeat is recorded as

Vim records the keys of the last change (after mappings are expanded).
temzdf drives everything through actions (mappings, scripts and
`temzdf action` all end in commands), so keys are the wrong layer:

| Recorded as | For | Against |
|---|---|---|
| Keys (Vim) | records anything | replay reinterprets mappings and modes; only the last change inside a mapping is kept (why vim-repeat exists); scripts and `temzdf action` have no keys |
| Actions with arguments | the unit temzdf already uses; replay skips mappings; scripts build the same values | opaque input (completion, IME, text from elsewhere) is not an action |
| Resulting edits | reproduces anything opaque | "delete the next word" becomes "delete these five characters" |

**Direction**: actions, with inserted text recorded as text (Vim does the
same for Insert mode in effect).

- A record is a list of actions, a count, and the text inserted, if any.
  `cw foo<Esc>` is `[operator("change", motion("word-forward")),
  insertion("foo")]`, count 1.
- Arguments stay unevaluated: a motion is kept as the motion and
  evaluated again from the new cursor. Values the user gave (the inserted
  text, the character for `r`) are kept as they are.
- The count is kept apart; `3.` replaces it, as in Vim.
- Replay runs the actions directly, without mappings, as one undo step.

## Which commands are recorded

Each command declares it in the registry
(`crates/temzdf-server/src/session/commands.rs`), as Evil's `:repeat`
property and Blender's `bl_options` do:

| `repeat` | Meaning | Examples |
|---|---|---|
| `change` | replaces the record | delete, put, join, replace |
| `operator` | takes the following motion or range, unevaluated | `d`, `c` (not `y`: it changes nothing) |
| `motion` | recorded only as part of an operator | word motions, text objects |
| `insert` | starts an insertion; the text typed until it ends is recorded | `i`, `a`, `o` |
| `ignore` (default) | leaves the record alone | scrolling, search, pane moves, anything that does not edit |

`ignore` is the default so that commands that do not edit never clear `.`
by accident.

Only operations started by the user's input (keys, mouse) change the
record. Hooks (formatting on save, `TextChanged`), `temzdf action` and
other clients leave it alone unless they call `repeat.set` explicitly;
otherwise background edits would replace what `.` repeats. Recording keys
avoids this by construction; recording actions has to rule it out.

## Units

A **unit** is what one `.` replays:

- It starts when the user's input invokes a top-level mapping or command
  in the base mode (`normal`).
- It holds everything that invocation consumes: the motion after an
  operator, the character a command waits for, text typed in Insert mode,
  an answer typed into a prompt.
- It ends when the base mode is back and nothing is pending (`cw
  foo<Esc>` is one unit, up to `<Esc>`).
- It commits at its end. A unit cancelled on the way (`<Esc>` while an
  operator waits) leaves the record as it was and drops any `repeat.set`
  made inside it.
- The count and register (`3`, `"a`) belong to the unit and are not
  replaced by `repeat.set` unless it names them.

Without `repeat.set`, a unit that changed something is recorded as the
top-level invocation itself plus the input consumed after it; replay
invokes it again.

## `repeat.set`

1. **`repeat.set(actions)` replaces the unit so far**: everything recorded
   since the unit began (the invocation and the input consumed until
   now) becomes `actions`. Input consumed afterwards (typed text) is
   appended after it.
2. **A later call replaces an earlier one**, since each one stands for
   the whole unit so far. To build on the current record, read it with
   `repeat.get()` and set the combination.
3. Called **outside a unit** (after asynchronous work), it replaces the
   whole record, as vim-repeat's `repeat#set()`.
4. Calls **during replay** are ignored; a record that changes each time
   says so with an optional `next` function (Vim's `"1p` then `.` putting
   `"2p` is one such case).
5. A mapped or command function may instead **return `{ repeat: [...] }`**:
   the same as `repeat.set` when it returns. `repeat.set` is needed only
   to mark a point in the middle (the snippet example below).

Examples:

- Surround (`ys{motion}{char}`): the function reads the motion and the
  character, then sets `[surround.add({ motion, char })]`. The whole unit
  is replaced, so `.` does not ask for the character again.
- A snippet that expands and enters Insert mode: it sets
  `[snippet.expand("for")]` right after expanding; the text typed in the
  placeholder is appended, so `.` expands and types it again.
- A function that only wraps a plugin command (`map("ys", () =>
  cmd.surround.add())`): the inner `repeat.set` replaces the unit,
  including the wrapper's invocation, so `.` repeats correctly.

### Nesting (decided 2026-11-07: A)

A function that makes change X, calls a command that uses `repeat.set`,
then makes change Y. Three options were weighed:

- **A. A `repeat.set` at any depth replaces the unit so far.** Wrappers
  work. X and Y are lost silently: `.` repeats only the inner command, so
  the author of a function doing several things sets the record last
  (with `repeat.get()`).
- B. Only the top-level `repeat.set` counts. X and Y are kept, but a
  wrapper's inner declaration is ignored and `.` runs the wrapper again,
  asking for the character again.
- C. Record every action run inside the unit, folding inner `repeat.set`
  calls into their values. Both cases work, but replay retraces the steps
  instead of calling the function, so its branches are not re-evaluated;
  that is what macro recording is for.

**A** was chosen: wrappers are what users write most, and breaking them
silently is worse; an author of a composite mapping can set the record
themselves. A command that shows what `.` would repeat helps notice the
other case.

## Prior art

- **Vim**: keys after mapping expansion; vim-repeat's `repeat#set()`; the
  `operatorfunc` and `g@` trick to make any function repeatable.
- **Evil**: the `:repeat` property per command (`t`, `nil`, `motion`,
  `change`, `abort`, ...).
- **VSCodeVim, Zed's vim mode**: record action objects; Zed keeps
  actions and inserted text apart.
- **Helix, Kakoune**: `.` repeats the last insertion; Helix repeats the
  last motion with `A-.`.
- **Emacs** `repeat` (`C-x z`): the last command and its prefix argument.
- Outside editors:
  - Blender's Repeat Last, Repeat History and Adjust Last Operation:
    operators with stored properties, declared in `bl_options`.
  - Illustrator's Transform Again repeats a relative transform.
  - Office's Repeat (`F4`).
  - Photoshop's "last filter", with and without its dialog.
  - Excel's macro recorder with relative references.
  - PowerPoint's and Figma's duplicate that learns the offset of the
    user's next move.

## Possible later

Recording actions rather than keys makes these possible:

- **Adjusting the last repeat** (Blender's Adjust Last Operation): edit
  the record's arguments and run it again, e.g. fix the replacement
  character.
- **A repeat history** (Blender's Repeat History): keep several records
  and pick one in a picker.

## Open

- Whether macros (`q`) record keys (users edit them as register text)
  or the same actions as `.`.
- Visual-mode repeats: the region as a relative extent (lines, columns on
  the last line) taken from the cursor, as Vim does; to confirm when the
  visual mode of the editor is designed.
- Where insertions break: moving the cursor in Insert mode ends one, as
  in Vim.
