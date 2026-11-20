// Type-level helpers (type-fest's, and ours), for src/ too (runtime/
// node_modules is not on the way up from there). Types only: nothing of
// this is bundled.
export type { IsEqual, Writable } from "type-fest";

/** Holds at type level (`type _X = Assert<...>`). */
export type Assert<T extends true> = T;
