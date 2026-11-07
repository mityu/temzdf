# Registers and the copy history

What panes copy goes to the session, not only to the client's clipboard:
text copied in a terminal pane's visual mode is put into a text pane with
Vim's `p`, a PDF's selection goes into a LaTeX source, and a shell in a
terminal pane pipes into it (`temzdf copy`). Decided with the user on
2026-11-07; **(user)** marks the user's decisions.

## The model

- The session keeps a **history of copies**, newest first, at most
  `o.copyhistory` entries (30 by default **(user)**), in the server's
  memory only: nothing is written to disk (terminal output copied often
  holds secrets), and it ends with the session.
- An entry: `{ id, text, kind, how, register, source, time }`.
  - `kind`: `"char"`, `"line"` or `"block"`, as Vim's register types. It is
    kept whichever pane copied: a terminal's visual `V` copies lines, its
    `<C-v>` a block, a PDF's selection characters. A text pane puts lines
    as lines; a terminal is sent the text (a bracketed paste).
  - `how`: `"copy"` (a yank) or `"delete"` (taken out of the text).
  - `register`: the name it was copied to, or null.
  - `source`: `{ kind, name }`, the pane kind (`terminal`, `pdf`, `edit`)
    or `cli`, and the buffer's name.
- **Named registers** map a name (any string) to an entry. The server
  attaches no meaning to names; Vim's rules are the Vim mode's.
- Vim's numbered registers are queries of the history, not stores of their
  own, so that what any pane copies is in them (a terminal's copy is the
  newest yank, `"0`):

  | Vim | the history |
  |---|---|
  | `"` | the newest entry |
  | `0` | the newest copy without a register |
  | `1`-`9` | the n-th newest delete of a line or more, without a register |
  | `-` | the newest delete within a line, without a register |
  | `a`-`z` (`A`-`Z` append) | the named register |
  | `+` `*` | the system clipboard (read from the client) |
  | `_` | nothing is stored |

## The system clipboard

- Every entry goes to the client's clipboard as well (OSC 52 in a terminal,
  the GUI's clipboard), deletes included, as Vim with
  `clipboard=unnamedplus` **(user)**, unless `o.clipboard` says otherwise
  (`"all"`, `"copies"`: not deletes, `"none"`) or the copy says
  `clipboard: false`. Copies to a named register do not go (Vim's `"ayy`).
  To keep a key from touching the clipboard, map it so (`"_x`, or a
  function copying with `clipboard: false`) **(user)**.
- Reading it is asynchronous: the session asks the client that typed last
  (`Effect::ReadClipboard`), which answers `ClientMsg::Clipboard`. A
  terminal client asks the terminal with an OSC 52 query (kitty and
  Ghostty may ask the user first; WezTerm does not answer); the GUI reads
  its clipboard. No answer within 10 s is an error, and so is no client
  attached. Inside tmux the query goes to tmux, not through passthrough
  (tmux answers it as its `set-clipboard` says). Not yet tried in real
  terminals (kitty, Ghostty, WezTerm, tmux): see todo.md. Scripts get a Promise
  (`await ctx.cmd.readClipboard()` in a dispatched task); it is built
  **(user)**, so the Vim mode's `"+p` reads the real clipboard.
- What other applications copy is not seen until read; it does not enter
  the history by itself.

## Scripts

- `copyText(text, { kind?, how?, register?, append?, clipboard? })` records an
  entry (not `copy`: that is the PDF pane's command, `y`);
  `history({ limit? })`, `register(name)`, `registers()`,
  `setRegister(name, entry | null)`, `forgetCopy(id?)` (one entry, or all)
  read and change them; `put({ register?, index?, pane? })` puts an entry
  into a pane (a terminal is pasted to, without the last line break of
  lines; a text pane gets it at the cursor, lines before the cursor's
  line); `readClipboard({ primary? })` is a Promise. The shipped mappings
  put the newest entry with `<C-b>]` (tmux's paste-buffer key) or `<C-b>p`.
- The hook `Copied(entry)` hears every new entry (highlight on yank, or
  dropping secrets with a scheduled `forgetCopy`).
- A command may answer with a Promise: the server replies
  `{ "$promise": n }` and later sends `ToHost::Resolve`; the prelude turns
  it into a Promise. Its continuation has no context of its own, so it is
  awaited in a dispatched task; inside `ctx.atomic(fn)` the plain `cmd`
  works too (it issues under the task's token), which lets shared code
  (the Vim mode's put) run there unchanged.

## The command line

- `cmd | temzdf copy [-r NAME] [--line]` records standard input (in the
  session the shell runs in); `temzdf paste [-r NAME] [-n N]` prints an
  entry. They work over SSH where OSC 52 does not reach.

## Not yet

- Picking from the history in a list, and cycling through it after a put
  (yankring, Emacs's `M-y`): later, on a general completion and fuzzy
  finder framework for scripts **(user)**.
- Sharing between sessions other than through the system clipboard and
  the command line.
