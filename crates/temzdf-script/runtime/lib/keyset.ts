// A set of mappings and hooks that is defined only when enabled (no
// effect on import): the shipped modules (temzdf/pane/pdf, temzdf/edit/vim,
// ...) are made with it. Its mappings are in one namespace, so disable()
// clears them with keymap.clear; its hooks are removed with what on()
// returned.
import { keymap, on } from "temzdf";
import type { Events, HookAnswer, MapOptions, Rhs } from "temzdf";

export interface Keyset {
  /** Define the mappings and hooks (again: nothing more). */
  enable(): void;
  /** Remove them. */
  disable(): void;
  readonly enabled: boolean;
}

export interface Define {
  /** Map `keys` in each of `modes` (in the set's namespace). */
  map(modes: string | readonly string[], keys: readonly string[], rhs: Rhs, options?: Omit<MapOptions, "namespace">): void;
  /** Hear `event` while enabled. */
  hook<E extends keyof Events>(event: E, fn: (...args: Events[E]) => void | HookAnswer): void;
}

/** A keyset in `namespace`: `setup` defines it, `teardown` (if any) undoes
 * what else it left (state, modes' options) after the mappings and hooks
 * went. */
export function keyset(namespace: string, setup: (d: Define) => void, teardown?: () => void): Keyset {
  let offs: (() => void)[] | null = null;
  return {
    enable() {
      if (offs !== null) return;
      const mine: (() => void)[] = [];
      offs = mine;
      setup({
        map(modes, keys, rhs, options) {
          for (const m of typeof modes === "string" ? [modes] : modes) {
            for (const k of keys) keymap.add(m, k, rhs, { ...options, namespace });
          }
        },
        hook(event, fn) {
          mine.push(on(event, fn));
        },
      });
    },
    disable() {
      if (offs === null) return;
      keymap.clear({ namespace });
      for (const off of offs) off();
      offs = null;
      teardown?.();
    },
    get enabled() {
      return offs !== null;
    },
  };
}
