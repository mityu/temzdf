// The options of an e2e run (scripts/e2e.sh's command line), as main.ts
// parses them and hands them to the scenario's process (E2E_OPTS, JSON).

export type Term = "kitty" | "ghostty" | "wezterm" | "gui";
export type Display = "x11" | "wayland" | "macos" | "windows";
export type Mux = "zellij" | "tmux";

export interface Options {
  /** The scenario's name (its module's file name), for the test's. */
  name?: string;
  term: Term;
  display: Display;
  mux?: Mux;
  /** -I: Japanese input (fcitx5 with Mozc). */
  ime?: boolean;
  /** -P: how clients take frames' pixels (TEMZDF_PIXELS). */
  pixels?: "shared" | "socket";
  /** -s: the window's (or screen's) size in pixels. */
  width: number;
  height: number;
  /** -o: where screenshots and logs go (absolute). */
  outdir: string;
  /** COMMAND given on the command line instead of the scenario's. */
  command?: string[];
}

export const optionsVar = "E2E_OPTS";

/** The options handed to this process, if it runs a scenario. */
export function received(): Options[] | undefined {
  const v = process.env[optionsVar];
  return v ? (JSON.parse(v) as Options[]) : undefined;
}

export function defaultDisplay(): Display {
  return process.platform === "darwin" ? "macos" : "x11";
}
