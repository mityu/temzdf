// What `import ... from "temzdf"` gets (the module loader in
// crates/temzdf-script/src/host.rs): the API prelude.ts built in
// `globalThis.__temzdf`, export by export. Bundled into an ES module by
// crates/temzdf-script/build.rs.
import type * as Self from "./temzdf.ts";

const t = (globalThis as unknown as { __temzdf: typeof import("temzdf") }).__temzdf;

export const {
  action,
  on,
  keymap,
  defineMode,
  o,
  feedkeys,
  currentMode,
  dispatch,
  atomic,
  schedule,
  openPrompt,
  acceptPrompt,
  cancelPrompt,
  isPrompt,
} = t;

// Exactly the exports (and types) the declarations give `temzdf`: one
// missing here or one too many fails the type check.
type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
type Assert<T extends true> = T;
type _Exports = Assert<Same<typeof Self, typeof import("temzdf")>>;
