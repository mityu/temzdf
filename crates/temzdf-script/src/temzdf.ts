// What `import ... from "temzdf"` gets (the module loader in
// crates/temzdf-script/src/host.rs): the API prelude.ts built in
// `globalThis.__temzdf`, export by export. Bundled into an ES module by
// crates/temzdf-script/build.rs.
import type { Assert, IsEqual, Writable } from "../runtime/lib/types";
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
  waitResolved,
  atomic,
  schedule,
  openPrompt,
  acceptPrompt,
  cancelPrompt,
  isPrompt,
  promptOrigin,
} = t;

// Exactly the exports (and types) the declarations give `temzdf`: one
// missing here or one too many fails the type check (readonly aside: the
// declarations' functions are not, these consts are).
type _Exports = Assert<IsEqual<Writable<typeof Self>, Writable<typeof import("temzdf")>>>;
