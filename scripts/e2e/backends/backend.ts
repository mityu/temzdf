// What depends on the display server (scripts/e2e.sh's `be_*`): pointer
// and keys, screenshots, the clipboard, windows. Coordinates are the
// screen's; key names are XKB keysym names (xdotool's).

export interface WindowRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Backend {
  /** The pointer to (x, y). */
  move(x: number, y: number): Promise<void>;
  /** Presses or releases BUTTON. */
  button(state: "down" | "up", btn: number): Promise<void>;
  /** N presses of BUTTON, DELAY ms apart, in one go (a wheel flick). */
  clicks(btn: number, n: number, delayMs: number): Promise<void>;
  key(key: string): Promise<void>;
  keydown(key: string): Promise<void>;
  keyup(key: string): Promise<void>;
  /** Types literal text (DELAY ms between keys). */
  type(text: string, delayMs?: number): Promise<void>;
  /** A screenshot of the screen into FILE (PNG). */
  shot(file: string): Promise<void>;
  /** The clipboard's text, or undefined when it cannot be read. */
  clipboard(): Promise<string | undefined>;
  setClipboard(text: string): Promise<void>;
  /** The inside of PID's window, waited for up to 10 s. */
  windowRect(pid: number): Promise<WindowRect | undefined>;
  /** Gives PID's window the focus. */
  focus(pid: number): Promise<void>;
  /** Opens a plain window of another program; its PID. */
  otherWindow(): Promise<number>;
  /** Where the screenshots show the pointer, if they do (Windows). */
  pointer?(): { x: number; y: number } | undefined;
}
