# Terminal queries: what Vim asks and what temzdf could

Question (2026-11-17): Vim's `src/term.c` is known for finding out much
about the terminal it runs in (`'ambiwidth'`, for one). Which queries does
it send, what does it read from the answers, and which of them would help
temzdf?

Status: research only, no decision. Read from Vim's `src/term.c` (master on
that date) and temzdf's `crates/termpix/src/probe.rs`; nothing was tried in a
terminal. **[V]** verified in source, **[I]** inference.

## What Vim asks [V]

| Query | Sent | Read | Used for |
|---|---|---|---|
| Ambiguous width (`t_u7`) | U+25BD (an East Asian ambiguous width character) at the start of row 2, then `CSI 6n` | `CSI 2;2R`: width 1; `CSI 2;3R`: width 2 | `'ambiwidth'`, unless the user set it. Row 2 because `CSI 1;2R` is also Shift+F3. Sent right after entering termcap mode; the cells are cleared afterwards |
| xterm compatibility (xcc) | At row 3: an unknown DCS (`ESC P zz ESC \`) and a CSI with an intermediate byte (`CSI 0%m`), then `CSI 6n` | The cursor did not move: unknown strings are dropped; it moved: they were printed as garbage | Whether DCS queries (DECRQSS below) are safe to send |
| Terminal version (`t_RV`, DA2) | `CSI > c` | `CSI > kind ; version ; x c` | A table of terminals: mintty 77, screen 83, kitty `1;400x`, foot `1;12700;0`, Terminal.app `1;95;0`, iTerm2 `0;95;0`, libvterm and Konsole `0;100;0`/`0;115;0`, PuTTY `0;136;0`, versions over 2500 are not xterm. Decides SGR mouse, the underline colour (`t_8u`), and whether DECRQM is safe |
| Colours (`t_RB`, `t_RF`) | OSC 11 and OSC 10 with `?` | `rgb:RRRR/GGGG/BBBB` | `'background'` (light or dark); recent versions also flatten image alpha onto the real background |
| DEC modes (DECRQM) | `CSI ? 2026 $p`, `CSI ? 2048 $p` | `CSI ? mode ; state $y` | Synchronized output, in-band resize. Not sent to Terminal.app and screen (they print the trailing `p`), known from DA2 |
| Curly underline (DECRQSS) | `CSI 4:3 m`, then `ESC P $q m ESC \` | Whether the reported SGR keeps `4:3` | Undercurl; only when xcc found DCS strings dropped |
| Cursor shape and blink | DECRQSS ` q`, `CSI ? 12 $p` | The current shape and blink | Restoring them on exit; not sent where they garble the screen (GNOME Terminal, screen) |
| termcap codes (XTGETTCAP) | `ESC P + q NAME ESC \`, ten at a time | The key codes the terminal really sends | xterm 141 and later only |
| Window position | `CSI 13 t` | `CSI 3 ; x ; y t` | `getwinpos()` |

## What temzdf already asks [V]

`termpix`'s probe sends, in one write: XTVERSION (`CSI > 0 q`), kitty
graphics queries (direct, shm, file), DECRQM 1016 (SGR pixel mouse) and
2026 (synchronized output), the kitty keyboard flags (`CSI ? u`), the cell
size (`CSI 16 t`), and DA1 (`CSI c`, last, as the end of the answers).
In-band resize (2048) is turned on. The curly underline is written without
asking (`crates/temzdf-tty/src/text.rs`).

## What would help temzdf, most useful first

1. **Ambiguous width, as Vim's `t_u7`.** The TTY client writes terminal
   panes and text-mode screens as cells. libghostty-vt (terminal panes) and
   `unicode-width` (the text editor) give ambiguous width characters (○, ①,
   some box drawing) width 1; a terminal set up for CJK draws them 2 wide
   and the rest of the row shifts. One more query in the probe (U+25BD and
   `CSI 6n`) tells. Then either position the cursor after every ambiguous
   width character, which holds for both widths, or make the widths follow
   the terminal (whether libghostty-vt has such a setting is unchecked
   [I]). Emoji widths can be checked the same way; Vim does not. Some
   terminals (Ghostty, WezTerm, foot) also have a grapheme cluster mode
   (DECRQM 2027) [I].
2. **The terminal's colours (OSC 10, 11 and 4).** A program in a terminal
   pane asking OSC 11 gets the configured theme (`set_colors`,
   `crates/temzdf-term/src/lib.rs`) or libghostty's defaults, not the outer
   terminal's colours. Without colours in the configuration, the client
   could ask its terminal and tell the server, so that Neovim in a pane
   picks the right `'background'`; tmux does this in recent versions [I].
   `termpix` already parses OSC answers (`Reply::Osc`). The same answer
   could pick a light or dark UI.
3. **DA2 (`CSI > c`).** Names the terminals that do not answer XTVERSION
   (Terminal.app, VTE terminals such as GNOME Terminal, mintty, screen), at
   least for the log when kitty graphics is missing.
4. **Garbage detection, as Vim's xcc.** The probe sends kitty graphics APCs
   to every terminal; one that prints unknown strings (Terminal.app) leaves
   garbage. Writing the queries at a known place and checking with `CSI 6n`
   whether the cursor moved finds it, so the screen can be cleared.
5. **Curly underline (DECRQSS).** Fall back to a plain underline where
   `4:3` is not understood.
6. Restoring the cursor shape on exit: low priority.
