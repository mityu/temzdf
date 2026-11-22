// temzdf's scripting API that Rust provides (the script host and the
// session), written by hand; the actions (action.*) are in
// temzdf-actions.d.ts. scripts/ts-types.ts puts both, with the shipped
// modules' declarations, into temzdf-all.d.ts (what `temzdf print-types`
// prints).

// What the script host provides besides the module (no DOM, no Node).
declare var console: {
  log(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
  debug(...args: unknown[]): void;
};
// biome-ignore lint/suspicious/noExplicitAny: the callback's own parameters, whatever they are.
declare function setTimeout(fn: (...args: any[]) => void, ms?: number, ...args: any[]): number;
declare function clearTimeout(id: number): void;

// AbortController and AbortSignal (runtime/polyfills.ts).
declare class AbortSignal {
  readonly aborted: boolean;
  readonly reason: unknown;
  onabort: (() => void) | null;
  throwIfAborted(): void;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
  static timeout(ms: number): AbortSignal;
  static any(signals: AbortSignal[]): AbortSignal;
}
declare class AbortController {
  readonly signal: AbortSignal;
  abort(reason?: unknown): void;
}

// The WHATWG URL Standard's URL and URLSearchParams (runtime/polyfills.ts).
declare class URL {
  constructor(url: string | URL, base?: string | URL);
  static canParse(url: string | URL, base?: string | URL): boolean;
  static parse(url: string | URL, base?: string | URL): URL | null;
  hash: string;
  host: string;
  hostname: string;
  href: string;
  readonly origin: string;
  password: string;
  pathname: string;
  port: string;
  protocol: string;
  search: string;
  readonly searchParams: URLSearchParams;
  username: string;
  toJSON(): string;
  toString(): string;
}
declare class URLSearchParams {
  constructor(init?: string[][] | Record<string, string> | string | URLSearchParams);
  readonly size: number;
  append(name: string, value: string): void;
  delete(name: string, value?: string): void;
  get(name: string): string | null;
  getAll(name: string): string[];
  has(name: string, value?: string): boolean;
  set(name: string, value: string): void;
  sort(): void;
  toString(): string;
  forEach(fn: (value: string, key: string, parent: URLSearchParams) => void): void;
  entries(): IterableIterator<[string, string]>;
  keys(): IterableIterator<string>;
  values(): IterableIterator<string>;
  [Symbol.iterator](): IterableIterator<[string, string]>;
}

declare module "temzdf" {
  /** The session's actions (synchronous; inside a mapped or hook function); see temzdf-actions.d.ts. */
  export const action: Actions;

  /** What conditions (`when`, `keepalive`) get: the context of the first key of the sequence. */
  export interface KeyContext {
    /** The focused pane. */
    pane: number;
    /** Its kind; undefined without a pane. */
    paneKind: PaneKind | undefined;
    /** Whether it is a floating pane. */
    floating: boolean;
    /** The current mode. */
    mode: string;
    /** Conditions: the keys of the sequence typed before the key asked about; mapped and fallback functions: the keys that ran them. */
    keys: string;
  }
  /** What a mapped function gets. */
  export interface MapContext extends KeyContext {
    /** The count typed before the keys (`3j`), if any. */
    count: number | undefined;
  }
  /** A mapping runs a synchronous function (counts go to commands as arguments: `(ctx) => action.scrollDown(ctx.count)`). */
  export type Rhs = (ctx: MapContext) => void;
  /** A condition: synchronous, without side effects (commands may only read). */
  export type Condition = (ctx: KeyContext) => boolean;
  export interface MapOptions {
    /** Run the mapping at once even if a longer one starts with its keys. */
    nowait?: boolean;
    desc?: string;
    /** The mapping exists only while this holds (else the key is looked up as if it were not there). */
    when?: Condition;
    /** Mapping the same keys with the same namespace again replaces it; without one, mappings are added. */
    namespace?: string;
    /** Several mappings of one key are tried by priority (higher first), then the last registered first. */
    priority?: number;
  }
  export const keymap: {
    add(mode: string | readonly string[], lhs: string, rhs: Rhs, options?: MapOptions): void;
    /** Remove lhs's mappings: those of a namespace, or all. */
    remove(mode: string | readonly string[], lhs: string, options?: { namespace?: string }): void;
    /** Remove every mapping of the mode and the namespace (each one left out: all of them). */
    clear(filter?: { mode?: string; namespace?: string }): void;
    /** The mappings of the mode, the namespace and the keys (each one left out: all of them): a snapshot. */
    get(filter?: { mode?: string; namespace?: string; lhs?: string }): MappingInfo[];
  };
  /** A key a mode's fallback function gets: its notation, and the text it types (if any). */
  export interface KeyInfo {
    keys: string;
    text: string | undefined;
  }
  export interface ModeProperties {
    /** What keys no mapping takes do (default: the mode below's, else drop): drop them, pass them to the pane (typed into a terminal, an input or a text pane), or call a function with the key (reading one key; leaving the mode: `() => action.exitMode()`). */
    fallback?: "drop" | "passthrough" | ((ctx: KeyContext, key: KeyInfo) => void);
    /** Whether digits are counts (default: the mode below's, else no). */
    count?: boolean;
    /** A submode: entered with switchMode, it sits over the mode it was entered from. */
    transient?: boolean;
    /** How long a submode stays: "mapped" while its keys are typed (another key leaves and is handled below), "always" (keys it does not map go to the mode below), or while the condition holds. */
    keepalive?: "mapped" | "always" | Condition;
    /** A submode is left after this many milliseconds without keys. */
    timeout?: number;
  }
  export function defineMode(name: string, properties?: ModeProperties): void;
  /** A mapping as keymap.get gives it (keymap.add's arguments). */
  export interface MappingInfo {
    mode: string;
    lhs: string;
    rhs: Rhs;
    when?: Condition;
    nowait: boolean;
    desc?: string;
    namespace?: string;
    priority: number;
  }
  export function feedkeys(keys: string, options?: { remap?: boolean }): void;
  export function currentMode(): string;

  /** A font of o.guifont: a family name, and its size in points (unset: the first font's, else 14). */
  export interface FontEntry {
    font: string;
    size?: number;
  }

  /** A position in a text buffer: a zero-based line and a column in UTF-16 code units (JavaScript string indices, LSP's). */
  export interface Position {
    line: number;
    character: number;
  }
  /** A half-open range of positions. */
  export interface Range {
    start: Position;
    end: Position;
  }
  /** A decoration or mark of a text buffer (`deleted`: its text went; it collapsed there). */
  export interface Decoration {
    id: number;
    range: Range;
    deleted: boolean;
    namespace: string;
    style: string | undefined;
    virtText: VirtChunk[];
    virtTextPos: "inline" | "eol" | "overlay";
  }
  /** An image a decoration shows (an id from loadImage), the text shown where images cannot be, and at most how many columns (and rows, in virtual lines) it takes. */
  export interface VirtImage {
    image: number;
    alt?: string;
    maxCols?: number;
    maxRows?: number;
  }
  /** A range (within one line) shown as text, an image or nothing instead of its text; on the cursor's line shown as it is unless `cursorLine`. */
  export interface Conceal {
    text?: (VirtChunk | string)[] | string;
    image?: VirtImage;
    cursorLine?: boolean;
  }
  /** Lines shown below (or `above`) a decoration's first line, not in the buffer: each of `text` one line, then `image` as many rows high as its pixels fill. */
  export interface VirtLines {
    text?: ((VirtChunk | string)[] | string)[];
    image?: VirtImage;
    above?: boolean;
  }
  /** A piece of virtual text and its style (a highlight group). */
  export interface VirtChunk {
    text: string;
    style?: string;
  }
  /** What copied text is, as Vim's register types: characters, whole lines (ending with a line break), or a block (one line of it per line). */
  export type CopyKind = "char" | "line" | "block";
  /** An entry of the copy history or a named register (doc/design/registers.md). */
  export interface Copied {
    id: number;
    text: string;
    kind: CopyKind;
    how: "copy" | "delete";
    /** The named register it was copied to. */
    register: string | undefined;
    /** Where it came from: the buffer's kind and name, or "cli" (temzdf copy). */
    source: { kind: PaneKind | "cli" | "script"; name: string; buffer: number | undefined };
    /** Milliseconds since the Unix epoch. */
    time: number;
  }
  /** What `text.move` and `text.positionOf` move by. */
  export type TextUnit =
    | "grapheme"
    | "wordStart"
    | "wordEnd"
    | "bigWordStart"
    | "bigWordEnd"
    | "lineStart"
    | "firstNonBlank"
    | "lineEnd"
    | "documentStart"
    | "documentEnd"
    | "line"
    | "displayLine"
    | "page";

  export interface Options {
    /** Milliseconds a key that starts a longer mapping waits. */
    timeoutlen: number;
    /** `split()` without a side goes right (else left). */
    splitright: boolean;
    /** Splits towards the bottom go below (else above). */
    splitbelow: boolean;
    /** How many modes the mode history keeps. */
    modehistory: number;
    /** How many entries the copy history keeps. */
    copyhistory: number;
    /** What goes to the client's clipboard as well: copies and deletes ("all", Vim's clipboard=unnamedplus), "copies", or "none"; a copy to a named register never does. */
    clipboard: "all" | "copies" | "none";
    /** Pixels per wheel step in PDF panes. */
    scrollstep: number;
    /** Milliseconds of the scrolling animation's easing. */
    scrollsmoothness: number;
    /** The command opening external links (the URI appended); undefined: the system's. */
    linkopener: string[] | undefined;
    /** Preview internal links when the pointer rests on one. */
    linkpreview: boolean;
    /** Milliseconds the pointer rests before a preview. */
    linkpreviewdelay: number;
    /** How long a forward search's highlight stays: milliseconds (0: none), or "input": until the next key or mouse input. */
    forwardflash: number | "input";
    /** Milliseconds to wait after the PDF changed before reloading. */
    reloaddebounce: number;
    /** What a reload keeps in place: the position or the line being read. */
    reloadanchor: "position" | "text";
    /** The inverse-search command (`%{input}`, `%{line}`, `%{column}`). */
    synctexeditor: string | undefined;
    /** A shown terminal whose program ended keeps its panes. */
    remainonexit: boolean;
    /** When the tab line shows: "auto" with two tab pages or more. */
    showtabline: "never" | "auto" | "always";
    /** Whether the window's status line shows (its bottom row: the mode, the session, the tab page). */
    showstatusline: "never" | "always";
    /** Default text colour (#rrggbb) for GUI clients and terminal panes; undefined: the client's. */
    foreground: string | undefined;
    /** Default background colour (#rrggbb); undefined: the client's. */
    background: string | undefined;
    /** The first palette colours (#rrggbb), index 0 first; the rest stay the default. */
    palette: string[];
    /** The GUI fonts: the first installed one sets the cell size, the rest fill in what it lacks, then JetBrains Mono, Noto Color Emoji, Symbols Nerd Font Mono and the system's fonts. */
    guifont: FontEntry[];
    /** Fonts tried first for wide (CJK) characters; []: none. */
    guifontwide: FontEntry[];
    /** The base mode (the bottom of the mode history) in panes of each kind, e.g. { edit: "vim/normal" }; set any time: the focused pane takes its new base at once, the others when focused. */
    basemode: { [kind: string]: string };
    /** Text panes wrap long lines (else they scroll sideways). */
    wrap: boolean;
    /** Wrap after a blank rather than anywhere. */
    linebreak: boolean;
    /** Continuation rows start at the line's indent. */
    breakindent: boolean;
    /** Shown at the start of continuation rows. */
    showbreak: string;
    /** Display columns between tab stops. */
    tabstop: number;
    /** Line numbers in text panes. */
    number: "off" | "absolute" | "relative" | "both";
    /** Highlight the cursor's line (the CursorLine group). */
    cursorline: boolean;
    /** Rows kept between the cursor and the top or bottom of text panes. */
    scrolloff: number;
    /** Keep text buffers' undo trees in files (written with the file, read back when it is opened). */
    undofile: boolean;
    /** Keep text buffers' text in swap files while they are edited (recovering after a crash). */
    swapfile: boolean;
    /** The mode Markdown viewers open in (markdown.open): as text, or in pixels like a PDF page. */
    markdownmode: "text" | "graphics";
    /** Milliseconds after the last change before the swap file is written. */
    updatetime: number;
    /** Where undo files go (undefined or "": $XDG_STATE_HOME/temzdf/undo; "~/" is the home directory). */
    undodir: string | undefined;
    /** Where swap files go (undefined or "": $XDG_STATE_HOME/temzdf/swap; "~/" is the home directory). */
    directory: string | undefined;
  }
  export const o: Options;

  export interface Events {
    SessionReady: [];
    SessionEndPre: [];
    PaneOpened: [pane: number, kind: PaneKind];
    PaneClosePre: [pane: number];
    PaneClosed: [pane: number];
    PaneFocused: [pane: number];
    TabEntered: [tab: number];
    TabClosed: [tab: number];
    DocumentReloaded: [path: string];
    ModeChanged: [old: string, new: string];
    BufferCreated: [buffer: number, kind: PaneKind];
    BufferDeletePre: [buffer: number];
    BufferDeleted: [buffer: number];
    BufferHidden: [buffer: number, kind: PaneKind];
    InvokePre: [info: Invocation];
    InvokePost: [info: Invocation & { cursor: Position | undefined; selection: Range | undefined }];
    KeyTyped: [key: { keys: string; text: string | undefined; source: "typed" | "feedkeys" }];
    SwapExists: [buffer: number, info: SwapInfo];
    TextChanged: [buffer: number, change: TextChange];
    PaneScrolled: [pane: number, lines: ScrollInfo];
    FileChangedOnDisk: [buffer: number];
    Copied: [entry: Copied];
  }
  /** What a "before" hook (PaneClosePre, BufferDeletePre, SessionEndPre) may answer: cancel the operation, saying why (to whoever asked). */
  export interface HookAnswer {
    cancel?: boolean;
    reason?: string;
  }
  /** What an invocation ran: a `temzdf action` command ([{ name, args }]), a function's id (a mapping, a fallback function), or typed text. */
  export type InvokeTarget = [{ name: string; args: unknown[] }] | number | { textInput: string };
  /** One invocation (a key sequence that ran a mapping, typed text, a fallback function, a CLI action): InvokePre and InvokePost get it. */
  export interface Invocation {
    seq: number;
    target: InvokeTarget;
    keys: string;
    count: number | undefined;
    mode: string;
    pane: number;
    source: "key" | "cli";
  }
  /** What TextChanged gets: the changes in positions before them (rangeLength in UTF-16 units), the buffer's version after, why, and the invocation it happened in (undefined from hooks, timers, tasks). */
  export interface TextChange {
    changes: { range: Range; rangeLength: number; text: string }[];
    version: number;
    cause: "typing" | "script" | "undo" | "redo" | "reload";
    invocation: number | undefined;
  }
  /** What PaneScrolled gets: the buffer lines a text pane shows now (0-based, inclusive: the top and bottom rows' lines, a wrapped line's rows and virtual lines counting as their line's), and those it showed at the last event (undefined when the pane is new or showed another buffer). */
  export interface ScrollInfo {
    buffer: number;
    first: number;
    last: number;
    previous: { first: number; last: number } | undefined;
  }
  /** Run `fn` at the safe point: after the current invocation and its hooks, before the next key, in the order scheduled. Hooks change things this way (only InvokePost and SessionReady hooks may act themselves). */
  export function schedule(fn: () => void): void;
  /** The kinds of panes and buffers (a Markdown viewer's pane and its document are `markdown` in either mode; `markdown.mode()` tells which). */
  export type PaneKind = "pdf" | "terminal" | "edit" | "markdown";
  /** A text buffer's attributes (Vim's buftype, several at once): `nofile` never modified, written only to a path given; `unlisted` left out of buffers(). */
  export type BufferAttr = "nofile" | "unlisted" | "readonly" | "nomodifiable";
  /** A swap file found when a file was opened, of a session that is gone (SwapExists, swapInfo). `time` is milliseconds since the epoch; `newer`: the file is newer than the swap file. */
  export interface SwapInfo {
    swap: string;
    session: string;
    pid: number;
    host: string;
    time: number;
    modified: boolean;
    newer: boolean;
  }
  /** Run `fn` when `event` happens (synchronously, as Vim's autocmds). */
  export function on<E extends keyof Events>(
    event: E,
    // biome-ignore lint/suspicious/noConfusingVoidType: a hook may return nothing.
    fn: (...args: Events[E]) => void | HookAnswer,
    options?: { nested?: boolean },
  ): () => void;

  /** What a dispatched task gets: commands under its own context. */
  export interface TaskContext {
    action: Actions;
    /** Run `fn` with everything else held; `fn` must be synchronous. */
    atomic<T>(fn: () => T): T;
  }
  /** What openPrompt is given. */
  export interface PromptOptions {
    /** Shown before the text (inline virtual text, style `style`, default `Prompt`). */
    prompt?: string;
    text?: string;
    side?: "bottom" | "top" | "left" | "right";
    height?: number;
    /** The base mode of its pane (default `input`). */
    mode?: string;
    style?: string;
    /** <CR> (acceptPrompt): the cursor's line. */
    onLine?: (line: string) => void;
    /** <Esc> (cancelPrompt), or the prompt closed otherwise. */
    onCancel?: () => void;
    /** The cursor's line as it changes (a preview). */
    onChange?: (line: string) => void;
  }
  /** A prompt (Vim's command-line window): a text buffer (nofile, unlisted) in a pane, its base mode `input`, the prompt inline virtual text at its start. Returns its buffer and pane, or undefined. */
  export function openPrompt(opts?: PromptOptions): { buffer: number; pane: number } | undefined;
  /** Run the prompt (the focused pane's, or `buffer`) with the cursor's line, closing it. */
  export function acceptPrompt(buffer?: number): void;
  /** Close the prompt (the focused pane's, or `buffer`) without running it. */
  export function cancelPrompt(buffer?: number): void;
  /** Whether `buffer` (the focused pane's) is an open prompt. */
  export function isPrompt(buffer?: number): boolean;
  /** The pane that was focused when the prompt (the focused pane's, or `buffer`) opened, where the focus goes back when it closes; undefined when there was none or `buffer` is not a prompt. */
  export function promptOrigin(buffer?: number): number | undefined;
  /** Start an asynchronous task without waiting for it. */
  export function dispatch(fn: (ctx: TaskContext) => Promise<void> | void): void;
  export function atomic<T>(fn: () => T): T;
}
