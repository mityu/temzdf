// The session's actions (action.* in scripts, `temzdf action` on the
// command line), written by hand: crates/temzdf-server/build.rs reads the
// names from here (moveFocus is the action move-focus, visual.left is
// visual.left), and a test checks that the server runs each of them.

declare module "temzdf" {
  export interface Actions {
    /** Another pane like the focused one (the same document at the same place, or a new shell), or showing `buffer`; `screen` puts it at that edge of the whole screen; `pane` splits that pane; `width`/`height` (cells or "N%" of the screen) make it that size, fixed (as Vim's winfixwidth/winfixheight); returns the new pane (undefined for a document still opening) */
    split(
      side?:
        | "right"
        | "left"
        | "bottom"
        | "top"
        | {
            side?: "right" | "left" | "bottom" | "top";
            scope?: "pane" | "screen";
            buffer?: number;
            width?: number | string;
            height?: number | string;
            pane?: number;
          },
      scope?: "pane" | "screen",
    ): number | undefined;
    /** As `temzdf action new-pane`: a file, or "--" and a command */
    newPane(...words: string[]): void;
    /** Focus the pane in that direction */
    moveFocus(dir: "left" | "right" | "up" | "down"): void;
    /** Focus the next pane */
    focusNextPane(): void;
    /** Focus pane `pane`, going to its tab page (as Vim's win_gotoid); not a hidden float */
    focusPane(pane: number): void;
    /** Move the focused pane's right (bottom) border, else its left (top) one, `cells` cells that way (1 by default); or make the pane wider, narrower, taller, shorter by `cells` (Vim's <C-w>> < + -); or give it a size */
    resizePane(
      how:
        | "left"
        | "right"
        | "up"
        | "down"
        | "wider"
        | "narrower"
        | "taller"
        | "shorter"
        | { width?: number; height?: number },
      cells?: number,
    ): void;
    /** Make the tiled panes the same size (only their widths, or heights), as Vim's <C-w>= */
    equalizePanes(only?: "width" | "height"): void;
    /** Make the focused pane as wide (high) as the others' minimum sizes allow, as Vim's <C-w>| and <C-w>_; `cells` makes it that many cells instead */
    maximizePane(axis: "width" | "height", cells?: number): void;
    /** Move the focused pane to that edge of the screen, the full height or width there (Vim's <C-w>H J K L) */
    movePane(side: "left" | "right" | "top" | "bottom"): void;
    /** Rotate the panes in the focused pane's row or column, each to the next place (Vim's <C-w>r; reverse: <C-w>R); `count` repeats it */
    rotatePanes(reverse?: boolean, count?: number): void;
    /** Close a pane, the focused one by default (its buffer stays, hidden) */
    closePane(pane?: number): void;
    /** Open a floating pane (a new shell by default); sizes in cells (the content's) or "N%" of the screen, centred unless col/row (counted as screenSize and text.screenPosition do); tabpage as Vim's popups (0 the current tab page, -1 all, N tab page N); `border: false` leaves out the frame and the title (the content at col/row, one cell at least); `focusable: false` never takes the focus: clicks and the wheel go through it, focusNextPane and moveFocus skip it, toggleFloats neither hides nor counts it, focusPane refuses it; returns the pane (undefined for a file still opening) */
    openFloat(
      what?: { buffer: number } | { file: string } | { command: string[] },
      opts?: {
        width?: number | string;
        height?: number | string;
        col?: number | string;
        row?: number | string;
        title?: string;
        focus?: boolean;
        tabpage?: number;
        border?: boolean;
        focusable?: boolean;
      },
    ): number | undefined;
    /** Move or resize a floating pane (the focused one for undefined), change its title, its frame (border), whether it takes the focus (focusable: false moves the focus off it), or the tab pages it shows on (tabpage) */
    floatConfig(
      pane: number | undefined,
      opts: {
        width?: number | string;
        height?: number | string;
        col?: number | string;
        row?: number | string;
        title?: string;
        focus?: boolean;
        tabpage?: number;
        border?: boolean;
        focusable?: boolean;
      },
    ): void;
    /** Show or hide every floating pane; with none, open a floating shell */
    toggleFloats(): void;
    /** Turn a pane (the focused one) floating, or tiled again */
    toggleFloating(pane?: number): void;
    /** The buffers (documents, terminals, text), shown or hidden, but `unlisted` ones (all: those too); `modified`: a text buffer with unsaved changes; `attrs` a text buffer's attributes */
    buffers(opts?: { all?: boolean }): {
      id: number;
      kind: PaneKind;
      name: string;
      panes: number[];
      exited: boolean;
      modified: boolean;
      attrs: BufferAttr[];
    }[];
    /** Show a buffer in a pane (the focused one), as Vim's :buffer */
    showBuffer(buffer: number, pane?: number): void;
    /** Open a file in the focused pane, as Vim's :edit: a PDF by its extension, anything else as text */
    edit(path: string): void;
    /** Delete a buffer (the focused pane's): end its program or close its document, and close its panes; a BufferDeletePre hook may cancel it */
    deleteBuffer(buffer?: number): void;
    /** End the session; a SessionEndPre hook may cancel it (unsaved changes), unless `force` */
    killSession(opts?: { force?: boolean }): void;
    /** The operating client detaches */
    detach(): void;
    /** Restart the script host and load the configuration again */
    reloadConfig(): void;
    /** Show a message: it fades after a few seconds; with error, it stays until the next key (an error, a question asked) */
    message(text: string, opts?: { error?: boolean }): void;
    /** Show or hide a pane's status line (the focused pane's; no `show`: the other way round); its content gets or gives back the row */
    statusLine(show?: boolean, pane?: number): void;
    /** Page view: search the PDF pane (the focused one) for `query`, the first hit shown once known; after a searchPreview of it, from where that began (the hit shown stays); an empty query clears the search */
    searchFor(query: string, pane?: number): void;
    /** Page view: show the first hit of `query` after where the preview began, as typed (Vim's incsearch); nothing found or empty: back there. searchFor ends it, searchCancel drops it */
    searchPreview(query: string, pane?: number): void;
    /** Page view: drop the search of the PDF pane (the focused one); a preview's view goes back where it began */
    searchCancel(pane?: number): void;
    /** Type text into the focused terminal pane (Vim's chansend) */
    sendText(text: string): void;
    /** Paste the client's clipboard (or the primary selection) into the focused pane; terminals paste with their own keys */
    paste(from?: "clipboard" | "primary"): void;
    /** GUI clients: the first guifont's size in points (the other sizes scale with it), "+N"/"-N" points more or less for every size, none: back to the configured sizes */
    fontSize(size?: number | string): void;
    /** Keys as if typed, after the running mapping; remap false skips mappings */
    feedkeys(keys: string, remap?: boolean): void;
    /** Fail quietly, as Vim's beep: the keys feedkeys queued are dropped (a macro stops); an error does the same */
    beep(): void;
    /** Run an invocation's target again (as InvokePre and InvokePost give it), as part of what runs now; `count` goes to a function's ctx; a command's error is thrown */
    invoke(target: InvokeTarget, opts?: { count?: number }): void;
    /** Enter a mode (pushed on the mode history) */
    switchMode(mode: string): void;
    /** Back to the previous mode */
    exitMode(): void;
    /** The current mode's name */
    currentMode(): string;
    /** The panes of every tab (tab: its index from 1, undefined for a float on all tabs; focusable: false for a float that never takes the focus; rect: the pane in cells from the screen's top left, its status line included, a float's frame not) */
    panes(): {
      id: number;
      kind: PaneKind;
      buffer: number;
      focused: boolean;
      floating: boolean;
      focusable: boolean;
      tab: number | undefined;
      rect: { col: number; row: number; width: number; height: number };
    }[];
    /** The size in cells of the room panes and floats take (the screen without the tab line and the window's status line): what openFloat's col, row and "N%" count in, as text.screenPosition does */
    screenSize(): { width: number; height: number };
    /** The focused pane's id */
    focusedPane(): number | undefined;
    /** Open a tab page after the current one, its pane showing `what` (a new shell by default), as Vim's :tabnew; returns the pane (undefined for a file still opening) */
    newTab(what?: { buffer: number } | { file: string } | { command: string[] }): number | undefined;
    /** Close a tab page (the current one; counted from 1) and its panes; their buffers stay, hidden. The last tab page stays (kill-session ends the session) */
    closeTab(tab?: number): void;
    /** Go to the next tab page, after the last to the first; with `count` N, to tab page N (Vim's gt) */
    nextTab(count?: number): void;
    /** Go to the previous tab page, before the first to the last; `count` goes that many back (Vim's gT) */
    prevTab(count?: number): void;
    /** Go to tab page `tab` (counted from 1) */
    gotoTab(tab: number): void;
    /** Move the current tab page after tab page `to` (0: first; none: last), or "+N"/"-N" places, as Vim's :tabmove */
    moveTab(to?: number | string): void;
    /** Name the current tab page in the tab line (no name: the focused pane's buffer again) */
    renameTab(name?: string): void;
    /** Move a pane (the focused one) to a new tab page, as Vim's <C-w>T */
    breakPane(pane?: number): void;
    /** The tab pages in order (index from 1; panes: tiled, and floats on that tab only) */
    tabs(): { index: number; name: string | undefined; label: string; current: boolean; panes: number[] }[];
    /** The current tab page's index (from 1, as Vim's tabpagenr()) */
    currentTab(): number;
    /** The session's name */
    sessionName(): string;
    /** Open a file as text in a new pane (its buffer if it is open); returns the pane. `basemode`: the base mode of panes on a buffer made now; `encoding`: an Encoding Standard label (shift_jis, euc-jp, ...) for a file that is not UTF-8 (nothing is guessed), written back in it; `pane`: show it in that pane instead (Vim's :e) */
    openFile(path: string, opts?: { basemode?: string; encoding?: string; pane?: number }): number;
    /** A new text buffer (no pane); with only `path`, a new file. `basemode`: the base mode of panes on it (never changed after); `attrs` its attributes (Vim's buftype, several at once): `nofile` never modified and written only to a path given, `unlisted` left out of buffers(). Returns the buffer */
    newBuffer(opts?: { text?: string; path?: string; basemode?: string; attrs?: BufferAttr[] }): number;
    /** Set a text buffer's attributes (see newBuffer) */
    setBufferAttrs(buffer: number, attrs: BufferAttr[]): void;
    /** Write a text buffer (the focused pane's) to its file, or to `path`; a file changed on disk since it was read needs `force` */
    write(opts?: { path?: string; force?: boolean; buffer?: number }): void;
    /** Read a text buffer (the focused pane's) from its file again: the difference is one undoable edit, and the result the saved state */
    reload(buffer?: number): void;
    /** A buffer's, pane's or tab page's variable (Vim's b:, w:, t:; the focused one's; tabs by number from 1); undefined when unset */
    getVar(scope: "buffer" | "pane" | "tab", name: string, id?: number): unknown;
    /** Set one (JSON values; undefined unsets it). Held by the server: it outlives script host restarts and goes with its owner */
    setVar(scope: "buffer" | "pane" | "tab", name: string, value: unknown, id?: number): void;
    /** Define a highlight group for text panes: colours "#rrggbb", a palette index or "default" (left out: unchanged under it) */
    highlight(
      name: string,
      style: {
        fg?: string | number;
        bg?: string | number;
        bold?: boolean;
        italic?: boolean;
        underline?: boolean;
        undercurl?: boolean;
        inverse?: boolean;
        strikethrough?: boolean;
        faint?: boolean;
        overline?: boolean;
      },
    ): void;
    /** Paint a pattern's matches in every text pane with a highlight group (Search by default), one pattern per namespace; undefined removes it (hlsearch) */
    highlightPattern(namespace: string, pattern: string | undefined, style?: string): void;
    /** Record `text` in the copy history (doc/design/registers.md): as characters, lines or a block, copied or deleted, to a named `register` (`append` adds to its text); it goes to the client's clipboard as o.clipboard says (`clipboard` overrides; never for a named register unless true); returns the entry */
    copyText(
      text: string,
      opts?: { kind?: CopyKind; how?: "copy" | "delete"; register?: string; append?: boolean; clipboard?: boolean },
    ): Copied;
    /** The copy history, newest first (at most o.copyhistory entries) */
    history(opts?: { limit?: number }): Copied[];
    /** The entry of a named register */
    register(name: string): Copied | undefined;
    /** Set a named register to an entry (of the history, or text) without recording a copy; undefined empties it */
    setRegister(name: string, entry: Copied | { text: string; kind?: CopyKind } | undefined): void;
    /** The named registers */
    registers(): Record<string, Copied>;
    /** Drop an entry from the history (and registers); no id: everything */
    forgetCopy(id?: number): void;
    /** Put an entry (a named register's, else the history's `index`, 0 the newest) into a pane (the focused one): pasted into a terminal, typed into a text or input pane (lines go in before the cursor's line) */
    put(opts?: { register?: string; index?: number; pane?: number }): void;
    /** Read the client's clipboard (the primary selection): a terminal client asks the terminal (OSC 52), which may refuse or not answer (an error after 10 s); await it in a dispatched task */
    readClipboard(opts?: { primary?: boolean }): Promise<string>;
    /** The swap file found when the buffer's file was opened, of a session that is gone (SwapExists): undefined when none is waiting */
    swapInfo(buffer?: number): SwapInfo | undefined;
    /** Start the buffer's own swap file (its first free name; after SwapExists was answered); returns its path */
    useSwap(buffer?: number): string;
    /** Delete a swap file (one in the swap directory) */
    deleteSwap(path: string): void;
    /** Read an image (PNG, JPEG, GIF, WebP or SVG) for decorations to show (virtImage); the same file again replaces it under the same id. Returns its id */
    loadImage(path: string): number;
    /** Drop an image: decorations showing it show their alt text instead */
    unloadImage(id: number): void;
    /** Page view: quit in the focused PDF pane (`count` times, where that means something) */
    quit(count?: number): void;
    /** Page view: cancel in the focused PDF pane (`count` times, where that means something) */
    cancel(count?: number): void;
    /** Page view: scroll-down in the focused PDF pane (`count` times, where that means something) */
    scrollDown(count?: number): void;
    /** Page view: scroll-up in the focused PDF pane (`count` times, where that means something) */
    scrollUp(count?: number): void;
    /** Page view: scroll-left in the focused PDF pane (`count` times, where that means something) */
    scrollLeft(count?: number): void;
    /** Page view: scroll-right in the focused PDF pane (`count` times, where that means something) */
    scrollRight(count?: number): void;
    /** Page view: page-down in the focused PDF pane (`count` times, where that means something) */
    pageDown(count?: number): void;
    /** Page view: page-up in the focused PDF pane (`count` times, where that means something) */
    pageUp(count?: number): void;
    /** Page view: half-page-down in the focused PDF pane (`count` times, where that means something) */
    halfPageDown(count?: number): void;
    /** Page view: half-page-up in the focused PDF pane (`count` times, where that means something) */
    halfPageUp(count?: number): void;
    /** Page view: first-page in the focused PDF pane (page `count` instead, given one) */
    firstPage(count?: number): void;
    /** Page view: last-page in the focused PDF pane (page `count` instead, given one) */
    lastPage(count?: number): void;
    /** Page view: back in the focused PDF pane (`count` times, where that means something) */
    back(count?: number): void;
    /** Page view: forward in the focused PDF pane (`count` times, where that means something) */
    forward(count?: number): void;
    /** Page view: zoom-in in the focused PDF pane (`count` times, where that means something) */
    zoomIn(count?: number): void;
    /** Page view: zoom-out in the focused PDF pane (`count` times, where that means something) */
    zoomOut(count?: number): void;
    /** Page view: fit-width in the focused PDF pane (`count` times, where that means something) */
    fitWidth(count?: number): void;
    /** Page view: next-match in the focused PDF pane (`count` times, where that means something) */
    nextMatch(count?: number): void;
    /** Page view: prev-match in the focused PDF pane (`count` times, where that means something) */
    prevMatch(count?: number): void;
    /** Page view: copy in the focused PDF pane (`count` times, where that means something) */
    copy(count?: number): void;
    /** Page view: toggle-sidebar in the focused PDF pane (`count` times, where that means something) */
    toggleSidebar(count?: number): void;
    visual: {
      /** Visual mode: to the next match of `query` (plain text, case-insensitive) in the terminal's screen and scrollback (the focused pane's), after the cursor or where a preview began, backwards with `backward`; n and N go on */
      searchFor(query: string, pane?: number, backward?: boolean): void;
      /** Visual mode: show the first match of `query` after where the preview began, as typed; nothing found: back there. visual.searchFor ends it, visual.searchCancel drops it */
      searchPreview(query: string, pane?: number, backward?: boolean): void;
      /** Visual mode: drop the search being typed in the terminal (the focused pane's): back where its preview began */
      searchCancel(pane?: number): void;
      /** Visual mode: One character left (Vim's h) (the focused terminal; `count` times, where that means something) */
      left(count?: number): void;
      /** Visual mode: One character right (l) (the focused terminal; `count` times, where that means something) */
      right(count?: number): void;
      /** Visual mode: One row up, keeping the column (k) (the focused terminal; `count` times, where that means something) */
      up(count?: number): void;
      /** Visual mode: One row down, keeping the column (j) (the focused terminal; `count` times, where that means something) */
      down(count?: number): void;
      /** Visual mode: To the first column (0) (the focused terminal; `count` times, where that means something) */
      lineStart(count?: number): void;
      /** Visual mode: To the first character that is not blank (^) (the focused terminal; `count` times, where that means something) */
      firstNonBlank(count?: number): void;
      /** Visual mode: To the last character of the row ($) (the focused terminal; `count` times, where that means something) */
      lineEnd(count?: number): void;
      /** Visual mode: To the start of the next word (w) (the focused terminal; `count` times, where that means something) */
      wordForward(count?: number): void;
      /** Visual mode: To the start of the word (b) (the focused terminal; `count` times, where that means something) */
      wordBackward(count?: number): void;
      /** Visual mode: To the end of the word (e) (the focused terminal; `count` times, where that means something) */
      wordEnd(count?: number): void;
      /** Visual mode: To the start of the next blank-separated word (W) (the focused terminal; `count` times, where that means something) */
      bigWordForward(count?: number): void;
      /** Visual mode: To the start of the blank-separated word (B) (the focused terminal; `count` times, where that means something) */
      bigWordBackward(count?: number): void;
      /** Visual mode: To the end of the blank-separated word (E) (the focused terminal; `count` times, where that means something) */
      bigWordEnd(count?: number): void;
      /** Visual mode: To the first row of the scrollback, or row {count} (gg) (the focused terminal; `count` times, where that means something) */
      top(count?: number): void;
      /** Visual mode: To the last row with text, or row {count} (G) (the focused terminal; `count` times, where that means something) */
      bottom(count?: number): void;
      /** Visual mode: To the top row shown (H) (the focused terminal; `count` times, where that means something) */
      screenTop(count?: number): void;
      /** Visual mode: To the middle row shown (M) (the focused terminal; `count` times, where that means something) */
      screenMiddle(count?: number): void;
      /** Visual mode: To the bottom row shown (L) (the focused terminal; `count` times, where that means something) */
      screenBottom(count?: number): void;
      /** Visual mode: Half a screen up, the view with it (<C-u>) (the focused terminal; `count` times, where that means something) */
      halfPageUp(count?: number): void;
      /** Visual mode: Half a screen down, the view with it (<C-d>) (the focused terminal; `count` times, where that means something) */
      halfPageDown(count?: number): void;
      /** Visual mode: A screen up, the view with it (<C-b>) (the focused terminal; `count` times, where that means something) */
      pageUp(count?: number): void;
      /** Visual mode: A screen down, the view with it (<C-f>) (the focused terminal; `count` times, where that means something) */
      pageDown(count?: number): void;
      /** Visual mode: The view a row up, the cursor kept in it (<C-y>) (the focused terminal; `count` times, where that means something) */
      scrollUp(count?: number): void;
      /** Visual mode: The view a row down, the cursor kept in it (<C-e>) (the focused terminal; `count` times, where that means something) */
      scrollDown(count?: number): void;
      /** Visual mode: To the shell prompt above (OSC 133 marks) (the focused terminal; `count` times, where that means something) */
      prevPrompt(count?: number): void;
      /** Visual mode: To the shell prompt below (OSC 133 marks) (the focused terminal; `count` times, where that means something) */
      nextPrompt(count?: number): void;
      /** Visual mode: Select characters from the cursor; again: stop selecting (v) (the focused terminal; `count` times, where that means something) */
      charwise(count?: number): void;
      /** Visual mode: Select rows from the cursor; again: stop selecting (V) (the focused terminal; `count` times, where that means something) */
      linewise(count?: number): void;
      /** Visual mode: Select a block from the cursor; again: stop selecting (<C-v>) (the focused terminal; `count` times, where that means something) */
      blockwise(count?: number): void;
      /** Visual mode: The cursor to the other end of the selection (o) (the focused terminal; `count` times, where that means something) */
      swapEnds(count?: number): void;
      /** Visual mode: Copy the selection and leave the visual mode (y) (the focused terminal; `count` times, where that means something) */
      yank(count?: number): void;
      /** Visual mode: Copy the selected rows (else the cursor's, {count} of them) and leave (Y) (the focused terminal; `count` times, where that means something) */
      yankLines(count?: number): void;
      /** Visual mode: Drop the selection; without one, leave the visual mode (<Esc>) (the focused terminal; `count` times, where that means something) */
      escape(count?: number): void;
      /** Visual mode: To the next match of the search (n) (the focused terminal; `count` times, where that means something) */
      nextMatch(count?: number): void;
      /** Visual mode: To the previous match of the search (N) (the focused terminal; `count` times, where that means something) */
      prevMatch(count?: number): void;
    };
    text: {
      /** Lines of the text pane's buffer */
      lineCount(opts?: { pane?: number }): number;
      /** A line, without its line break */
      line(line: number, opts?: { pane?: number }): string;
      /** The text of a range (all of it by default) */
      getText(range?: Range, opts?: { pane?: number }): string;
      /** Where the cursor is */
      cursor(opts?: { pane?: number }): Position;
      /** The selection, if any (its active end is the cursor) */
      selection(opts?: {
        pane?: number;
      }): { anchor: Position; active: Position; kind: "char" | "line" | "block" } | undefined;
      /** A position's UTF-16 offset from the start */
      offsetAt(pos: Position, opts?: { pane?: number }): number;
      /** The position at a UTF-16 offset */
      positionAt(offset: number, opts?: { pane?: number }): Position;
      /** About the buffer */
      info(opts?: { pane?: number }): {
        buffer: number;
        path: string | undefined;
        name: string;
        modified: boolean;
        encoding: string;
        lineEnding: "lf" | "crlf";
        version: number;
        attrs: BufferAttr[];
      };
      /** Where `count` units from `from` is, without moving (negative counts go backwards) */
      positionOf(from: Position, unit: TextUnit, count?: number, opts?: { pane?: number }): Position;
      /** The text the pane shows */
      visibleRange(opts?: { pane?: number }): Range;
      /** The cell where a position (the cursor) shows, as the cursor there would be drawn, counted as openFloat's col and row are (from the top left of screenSize's room); undefined when the pane does not show it (scrolled away, past the edge, a hidden pane) */
      screenPosition(at?: Position, opts?: { pane?: number }): { col: number; row: number } | undefined;
      /** Search with a Rust regex (^ and $ at lines): forward the first match starting at or after `from` (the cursor), backward the last one starting before it; `wrap` goes on from the other end; `lines` (zero based, inclusive) narrows it; `group` is the capture that counts as the match; `word` wants word boundaries of the buffer's word characters */
      search(
        pattern: string,
        opts?: {
          from?: Position;
          backward?: boolean;
          wrap?: boolean;
          lines?: [number, number];
          group?: number;
          word?: boolean;
          pane?: number;
        },
      ): { range: Range; groups: (Range | undefined)[] } | undefined;
      /** Every match in a range (the whole text) */
      searchAll(
        pattern: string,
        opts?: { range?: Range; group?: number; word?: boolean; pane?: number },
      ): { range: Range; groups: (Range | undefined)[] }[];
      /** The matching bracket (Vim's %; ( and ) by default): from one on `open` forward, on `close` backward; with `direction`, the enclosing one that way. Nesting counts; strings and comments do not */
      findPair(
        at?: Position,
        opts?: { open?: string; close?: string; direction?: "forward" | "backward"; pane?: number },
      ): Position | undefined;
      /** The buffer's word characters, a regex character class */
      wordchars(opts?: { pane?: number }): string;
      /** Set the buffer's word characters (Vim's iskeyword): one regex character class, e.g. [\p{L}\p{N}_] */
      setWordchars(chars: string, opts?: { pane?: number }): void;
      /** Decorate a range (an empty one without a style is a mark): it follows edits, collapses and is flagged deleted when its text goes (undo restores it). `style` a highlight group, `showIn` a pane only, gravity where each end goes on an insertion exactly there, `ttl` milliseconds until it goes. `virtText` is text shown that is not in the buffer, at the range's start: `inline` between the characters (it takes columns; the cursor never goes in it), `eol` after the line's end, `overlay` over the text. `virtImage` (inline) is an image read with loadImage, one row high and as wide as its aspect asks (at most `maxCols`), `alt` shown where images cannot be. `conceal` shows the range (within one line) as other text, an image or nothing, except on the cursor's line unless `cursorLine`. `virtLines` are lines below the range's first line (above with `above`), not in the buffer: each of `text` a line (cut at the pane's edge), then `image` as many rows high as its pixels fill (at most `maxRows`, `maxCols`); the cursor skips them. Returns its id */
      addDecoration(
        range: Range,
        opts?: {
          style?: string;
          namespace?: string;
          priority?: number;
          showIn?: number;
          startGravity?: "before" | "after";
          endGravity?: "before" | "after";
          ttl?: number;
          virtText?: (VirtChunk | string)[];
          virtTextPos?: "inline" | "eol" | "overlay";
          virtImage?: VirtImage;
          conceal?: Conceal;
          virtLines?: VirtLines;
          pane?: number;
        },
      ): number;
      /** A decoration or mark by id */
      decoration(id: number, opts?: { pane?: number }): Decoration | undefined;
      /** Options of the pane alone, over the session's (Vim's window-local options: a prompt goes without wrapping and numbers) */
      setOptions(
        options: {
          wrap?: boolean;
          number?: "off" | "absolute" | "relative" | "both";
          cursorline?: boolean;
          scrolloff?: number;
          /** Groups drawn as others in the pane (Neovim's winhighlight), e.g. { CursorLine: "PickedLine", CursorUnfocused: "None" }; "None" (or a group never defined) draws nothing; replaces the pane's last ones, {} none */
          highlights?: Record<string, string>;
        },
        opts?: { pane?: number },
      ): void;
      /** The decorations of a namespace and range (all of them), in order */
      decorations(opts?: { namespace?: string; range?: Range; pane?: number }): Decoration[];
      /** Move a decoration (no longer deleted) */
      moveDecoration(id: number, range: Range, opts?: { pane?: number }): void;
      /** Delete a decoration */
      deleteDecoration(id: number, opts?: { pane?: number }): void;
      /** Delete a namespace's decorations */
      clearNamespace(namespace: string, opts?: { pane?: number }): void;
      /** Replace ranges at once, as LSP's TextEdit[]: ranges in the text before the edit, not overlapping; insertions at one point go in the array's order */
      edit(edits: { range: Range; newText: string }[], opts?: { pane?: number }): void;
      /** Insert text at a position (the cursor) */
      insert(text: string, at?: Position, opts?: { pane?: number }): void;
      /** Delete a range */
      delete(range: Range, opts?: { pane?: number }): void;
      /** Replace a range */
      replace(range: Range, text: string, opts?: { pane?: number }): void;
      /** Insert text as typed: it replaces the selection, and consecutive typing is one undo step */
      type(text: string, opts?: { pane?: number }): void;
      /** Delete the selection, else from the cursor `count` units (negative: backwards) */
      erase(unit?: TextUnit, count?: number, opts?: { pane?: number }): void;
      /** Move the cursor `count` units; `select` extends the selection */
      move(unit: TextUnit, count?: number, opts?: { select?: boolean; pane?: number }): void;
      /** Put the cursor at a position; `keepColumn` keeps the column vertical moves aim for (Vim's cursor kept off a line's end) */
      setCursor(pos: Position, opts?: { select?: boolean; keepColumn?: boolean; pane?: number }): void;
      /** Select from `anchor` to `active` (the cursor) */
      setSelection(
        anchor: Position,
        active: Position,
        kind?: "char" | "line" | "block",
        opts?: { pane?: number },
      ): void;
      /** Drop the selection */
      clearSelection(opts?: { pane?: number }): void;
      /** The cursor's shape in this pane */
      setCursorStyle(
        style: "block" | "bar" | "underline" | "default" | "block-blink" | "bar-blink" | "underline-blink",
        opts?: { pane?: number },
      ): void;
      /** Scroll by display rows (negative: up) */
      scroll(rows: number, opts?: { pane?: number }): void;
      /** Scroll so a position (the cursor) shows */
      reveal(pos?: Position, how?: "minimal" | "center" | "top", opts?: { pane?: number }): void;
      /** Undo a step; returns the ranges it changed */
      undo(opts?: { pane?: number }): Range[];
      /** Redo a step along the current branch */
      redo(opts?: { pane?: number }): Range[];
      /** Group what follows into one undo step (nestable) */
      undoBegin(opts?: { pane?: number }): void;
      /** End a group begun with undoBegin */
      undoEnd(opts?: { pane?: number }): void;
      /** End the undo step here, even inside a group */
      undoBoundary(opts?: { pane?: number }): void;
      /** Replace the text with a swap file's (the one found, by default), as one change that undo takes back */
      recover(opts?: { swap?: string; pane?: number }): void;
    };
    markdown: {
      /** Open a Markdown file in the viewer (its text mode), in a new pane or in `pane`; a file already open is shared. Returns the pane */
      open(path: string, opts?: { pane?: number }): number;
      /** Follow the link at the cursor of a Markdown viewer (the focused pane's): a URL to the link opener, `#anchor` to its heading, a .md or .pdf file in the same pane. Whether there was one */
      follow(pane?: number): boolean;
      /** Go back in a pane to the Markdown file it showed before following a link to another file. Whether there was one */
      back(pane?: number): boolean;
      /** Show the heading `anchor` names (as `#anchor` links do) at the top of a Markdown viewer */
      goto(anchor: string, opts?: { pane?: number }): void;
      /** Show a Markdown viewer (the focused pane) as text (a text pane: Vim's keys, selecting and copying) or in pixels like a PDF page, at the same place */
      setMode(mode: "text" | "graphics", opts?: { pane?: number }): void;
      /** The mode a pane shows its Markdown file in (undefined: not a Markdown viewer) */
      mode(pane?: number): "text" | "graphics" | undefined;
      /** Graphics mode: scroll by rows of text (negative: up) */
      scroll(rows: number, opts?: { pane?: number }): void;
      /** Graphics mode: scroll by screenfuls (negative: up) */
      page(pages: number, opts?: { pane?: number }): void;
      /** Graphics mode: to the top of the page */
      top(pane?: number): void;
      /** Graphics mode: to the bottom of the page */
      bottom(pane?: number): void;
      /** Graphics mode: zoom a step in or out, or back to 100%, at the same place */
      zoom(how: "in" | "out" | "reset", opts?: { pane?: number }): void;
      /** Graphics mode: the next match of the search (dir < 0: the one before), around the ends. Whether there was one */
      searchNext(dir?: number, opts?: { pane?: number }): boolean;
      /** Graphics mode: copy the selected text again (into the copy history and, as `o.clipboard` says, the clipboard; releasing a drag copies it). Whether some was selected */
      copy(pane?: number): boolean;
      /** Graphics mode: forget the selected text. Whether some was selected */
      clearSelection(pane?: number): boolean;
      /** Fetch the http(s) images of the Markdown file a viewer shows (only local images are read otherwise), in the background; each shows as it arrives. Returns how many it fetches */
      loadRemoteImages(pane?: number): number;
      /** The headings of the Markdown file a pane shows: level, text, anchor and source line (0-based) */
      outline(pane?: number): { level: number; text: string; anchor: string; line: number }[];
    };
    /** For scripts' tests (temzdf/test drives the session with these; a script's function or dispatched task only, not `temzdf action`) */
    test: {
      /** Type `keys` (key notation) one at a time, each once the session settled after the one before (mapped functions done, no script running or waiting to run), then wait until it settled again; `flush` (default true) runs a prefix still waiting for a longer mapping as if its time ran out, `remap` false feeds the keys without mappings. Resolves with the error messages and the other messages shown meanwhile, the keys still pending and the mode */
      keys(keys: string, opts?: { flush?: boolean; remap?: boolean }): Promise<TestSettled>;
      /** Wait until the session settled (see keys) */
      settle(opts?: { flush?: boolean }): Promise<TestSettled>;
      /** Abandon the waits (their Promises are rejected) and their keys, drop pending keys, and go back to the focused pane's base mode */
      reset(): void;
      /** A line of TAP: to `temzdf test`, else the log */
      report(line?: string): void;
      /** The run ended with `failed` of `total` tests failing: to `temzdf test`, else the log */
      end(summary?: { failed: number; total: number }): void;
      /** Whether `temzdf test` collects the reports (otherwise they go to the log) */
      reporting(): boolean;
      /** How many hooks scripts have on `event` (on()), to see that a module took its own away */
      hooks(event?: keyof Events): number;
    };
  }

  /** What test.keys and test.settle resolve with. */
  export interface TestSettled {
    /** The error messages shown while it waited. */
    errors: string[];
    /** The other messages (information) shown while it waited. */
    messages: string[];
    /** Keys waiting for a longer mapping (key notation; empty after a flush). */
    pending: string;
    /** The current mode. */
    mode: string;
  }
}
