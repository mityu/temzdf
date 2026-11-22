# Ex commands: design

Commands typed on a `:` command line, as Vim's Ex commands, that scripts
(and the user's configuration) define. Agreed with the user on 2026-11-19.

Status: built, in TypeScript only: the module `temzdf/command`
(`crates/temzdf-script/runtime/command.ts`; its tests in
`crates/temzdf-script/runtime/command.test.ts`). The Vim mode defines its
commands with it (`crates/temzdf-script/runtime/vim/ex.ts`, in its
namespace; those on text with a `when` for text panes), and two command
lines run it: the Vim mode's `:` in text panes and `<C-b>:` (the normal
mode's `:`, `crates/temzdf-script/runtime/lib/cmdline.ts`) in any pane.

"Command" in this document means an Ex command. The session's registry
commands are **actions** (`action.*` in scripts, `temzdf action` on the
command line; `doc/design/scripting.md`), and an Ex command's callback
usually runs some of them.

## Registering

```ts
command.add(name: string, fn: (ctx: CmdCtx) => void, opts?: {
  shortName?: string;  // the shortest abbreviation accepted
  when?: (ctx) => boolean;
  prefix?: "vim-range"; // accepts a range; absent: none
  bar?: "auto" | "handle"; // how `|` after it is treated (default "auto")
  desc?: string;
  namespace?: string;
});
```

- **Names are letters only**, as Vim's. So `:b1` is `b` with the argument
  `1`, `:s#a#b#` is `s` with `#a#b#`, and `:w!` is `w` with `!`.
- **The empty name** is the command of lines with no name: a range alone
  (`:5`, which the Vim mode defines as going to line 5) or a line that
  starts with another character (`:!ls` gives it the argument `!ls`).
  Without one such lines are `E492`. It takes no `shortName` (none is a
  prefix of the empty name), so no name typed ever reaches it.
- `shortName` is a prefix of `name`. Without it, only the full name runs
  the command.
- `when` works as a mapping's `when` (`doc/design/keymaps.md`), with the
  same context (the pane's kind, whether it floats, ...). It is a plain
  function call: commands are matched in the script host, not in the
  server, so no round trip.
- `namespace` lets a keyset enable and disable a group of commands, as it
  does mappings (`d.command(...)` in `crates/temzdf-script/runtime/lib/keyset.ts`).
  Adding the same name in the same namespace again replaces the command.
- Wrong arguments (a name with other characters, a `shortName` that is
  not a prefix of the name, ...) are a `TypeError`.
- There is no `priority`; see "Matching".

## Matching

For the name typed (the letters after the range):

1. Commands whose `when` is false are left out.
2. A command whose `name` equals the input wins.
3. Otherwise a command matches when the input is a prefix of `name` and
   at least as long as its `shortName` (`shortName ≤ input ≤ name`).
4. When several commands remain, the one **defined last** wins: the same
   rule for the same name defined twice and for two names the input
   abbreviates. A keyset disabled and enabled again counts as defined
   again, so its commands move to the end; harmless, but stated here.

No match is an error (Vim's `E492: Not an editor command`).

## What the callback gets

```ts
type CmdCtx = {
  name: string;   // the full name, even when abbreviated
  args: string;   // everything after the name, untrimmed
  range: VimRangeSpec | undefined; // as typed; see "Ranges"
  mods: Mods;     // what the caller of `command.execute` passed, else {}
  line: string;   // this command's part: its range, name and arguments
};
```

- **`args` is not trimmed**: `:w!` (force) and `:w !sort` (to a program)
  differ only by the space.
- **No bang and no argument count are parsed centrally.** In Vim script a
  command was something like an alias that pasted `<bang>` and `<args>`
  into another command line, so splitting them out was useful; here the
  callback is a function and decides itself. Helpers do the parsing:
  - `parseBang(s): [boolean, string]`;
  - `splitArgs(s): string[]`: split at spaces, with backslash escapes;
  - `splitBar(s): [string, string | undefined]`: the text before the first
    unescaped `|`, and the rest after it;
  - `parsePrefix(s): [VimRangeSpec | undefined, string]`: the range as
    typed and what follows it;
  - `resolveRange(spec, env)`: its lines (see "Ranges").
- **No count**: `:3b` and `:b 3` are told apart by `range` (one address)
  when a command cares.
- `line` is this command's part of the line, not the whole: after an
  `"auto"` `|` the next command gets its own.
- Values that may be absent are `undefined`, not `null` (the direction
  for all of temzdf's script API).

## Ranges

Whether a command takes a range is declared, as in Vim (`-range`,
`-count` and `EX_RANGE`). `prefix: "vim-range"` accepts a Vim range; the
option is a string so other kinds can be added beside it.

- A range on a command without `prefix` (left `undefined`) is an error,
  raised before the callback runs, as Vim's `E481: No range allowed`. The
  callback only ever sees `range: undefined`. Checked in Vim 9.2 and
  Neovim on 2026-11-19: a user command without `-range` and a built-in
  without a range (`:2pwd`) both give `E481`, and the error stops the
  commands after a `|` too.
- **The grammar is built in and fixed**: Vim's addresses (a number, `.`,
  `$`, `'x`, `/pat/`, `?pat?`, `\/`, `\?`, `\&`, each followed by any
  `+N` and `-N`; an offset alone counts from `.`), joined by `,` or `;`,
  and `%` for `1,$`. The core reads only that much, to find where the name
  starts.
- **The range is passed as typed** (`VimRangeSpec`: a start, an end and
  the separator, each address a base and an offset), not as lines: what
  `.`, `'a` or `'<` mean belongs to the pane and to whoever keeps the marks
  (the Vim mode keeps its marks itself). A command turns it into lines
  with `resolveRange(spec, env)`, `env` giving the current line, the last
  one, marks and, optionally, searches; a text pane's `env` is the Vim
  mode's (its marks; a pattern searches as `/` does and becomes the last
  search). A pane that has no lines refuses the range there, with an
  error of its own (the Vim mode's commands taking ranges are for text
  panes only).
- `resolveRange` gives zero-based lines, first ≤ last (Vim asks before
  swapping a backward range; here it is swapped), and after `;` counts the
  second address from the first.
- Count ranges (Vim's `-count`) are left out; another value of `prefix`
  (`"count"`, say) can be added later.

A range alone (`:5`) runs the command with the empty name (see
"Registering"; decided 2026-11-20).

Undecided (2026-11-19):

- Whether to state as a rule that **prefixes never start with a letter**,
  so that a word before a command (`:vert edit`) is always a command of
  its own taking the rest. Vim's range grammar starts with no letter, so
  it already holds.

## Running a line, and `|`

`command.execute(line: string, opts?: { mods?: Mods })` runs a command
line. It is the one way in: the Vim mode's command line, `@:` and macros,
and later the CLI, call it.

It parses the range and the name, picks the command, and then:

- **`bar: "auto"`** (the default): the core splits the rest at the first
  unescaped `|`, calls the callback with the part before it (`\|` in it
  becomes `|`), and when the callback returns normally, runs the part
  after it with `command.execute`. `:w`, `:e`, `:b` work this way.
- **`bar: "handle"`**: the callback gets the whole rest, `|` included,
  untouched, and handles it itself. `command.execute` is in effect the
  continuation a callback may call:
  - `:s/a|b/c/ | next` finds the end of its pattern and replacement
    itself, then calls `command.execute` on what follows;
  - `:normal` and `:!` take the rest as their argument and never call it;
  - `:g/pat/cmd` calls it once per matching line;
  - a modifier such as `:vertical` runs the rest with changed `mods`:
    `command.execute(rest, { mods: { ...call.mods, vertical: true } })`;
  - `:silent!` wraps it in `try`.

  The core does not check how often a callback calls it; the cases above
  need none, one, or many calls.

`"auto"` is the same as a `"handle"` command wrapping the callback, a
built-in convenience; it runs the part after `|` in a loop rather than
nested, so long chains do not grow the stack.

Rules:

- **Errors** thrown by a callback reach the caller of `command.execute`.
  With `"auto"`, an error before `|` stops what follows it (as Vim).
- **`mods` are passed on only explicitly**: the part after an `"auto"`
  `|` starts with `{}`. In Vim `:vertical split | new` makes only `split`
  vertical.
- **Depth**: nested `command.execute` calls are counted and refused past
  100 (Vim's `E169: Command too recursive`, at 200 there), so a command
  that runs itself does not run away. 200 levels of a command running
  itself exhaust the script host's WebAssembly stack first (150 still
  fit, 2026-11-19), which ends the host.

## Prefixes

Vim's `:vertical`, `:aboveleft`, `:tab`, `:silent` and such are not
special: they are `bar: "handle"` commands that call `command.execute`
on the rest with different `mods` (above). Which modifiers exist and what
`Mods` holds is decided when the first of them is built; commands that
open panes read the ones they understand.

## Left for later

- Running JavaScript from the CLI (`temzdf action eval`, say), which then
  reaches commands through `command.execute` without an entry point of
  its own.
- Completion of command names and arguments.

## Alternatives considered

- **The callback gets the raw line** (the first proposal): every command
  would parse the range and prefixes again, and the name cannot be found
  without parsing the range first.
- **Bang, argument counts and ranges as declarations** (as Neovim's
  `nvim_create_user_command`): only the range stayed, because the core
  must reject it before anything runs; the others the callback checks.
- **Prefixes as values parsed by the core** (Neovim's `smods`), or as
  commands marked `{ modifier: true }` that return changed `mods`:
  replaced by commands calling `command.execute`, which needs no extra
  mechanism.
- **Prefix parsers chosen per command** (`prefixParser` in the
  registration): the core cannot pick a command's parser before it knows
  the name, and cannot find the name before it has read the prefix. Then
  **registered parsers, all tried**, each command naming (or holding a
  handle to) the one it takes: the value's type changes with the parser,
  and a word that is both a prefix and a command (`:vert edit`) is
  ambiguous. Rejected for a built-in grammar: variations belong in the
  arguments rather than in more kinds of prefix.
- **Ranges resolved by the core**, with a reader registered per pane kind
  or marks put in from outside: resolving needs the Vim mode's marks, so
  the range is passed as typed instead.
- **A range alone handled elsewhere** than by a command with the empty
  name: by the caller before `command.execute` (then not after a `|`, in
  `@:` or from the CLI); by a handler of its own (`command.onRangeOnly`,
  needing its own namespaces and order); by marking a named command as
  the one for no name (`{ bare: true }`, a name Vim does not have); or by
  a handler for every line no command matches (typing mistakes would go
  there too).
- **Three kinds of `bar`** (`"split"`, `"self"` returning the rest it did
  not use, `"none"`), as Vim's command flags: the continuation covers
  `"self"` and `"none"` with one kind and also expresses `:g` and the
  modifiers, which returning the rest cannot.
