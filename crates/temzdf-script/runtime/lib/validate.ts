// Checking values scripts pass (or stored values read back) with zod
// (zod/mini, English messages). A schema is checked against the type
// written by hand, which stays what scripts see (`satisfies
// z.ZodMiniType<T>`, or `Takes` for options it fills in): zod's types never
// reach temzdf-all.d.ts. Modules
// import zod themselves (`import * as z from "zod/mini"`): a namespace
// passed on from here would bundle all of it.
//
// Not z.file() nor the size checks (z.minSize, z.maxSize, z.size): they
// look at `File`, which QuickJS lacks.
import type { IsEqual } from "./types";
import { en } from "zod/locales";
import * as z from "zod/mini";

// English messages, set when a check first needs them: nothing runs on
// import, so a module using only isNullish bundles none of zod.
let english = false;
function messages(): void {
  if (english) return;
  z.config(en());
  english = true;
}

/** Throw a TypeError naming `what` (`command.add(w)`, say) for what
 * `schema` finds wrong in `value` (`name`: what it is, in the message), one
 * line a problem (`command.add(w): opts.bar: Invalid option: ...`). */
export function check<T>(what: string, schema: z.ZodMiniType<T>, value: unknown, name = ""): asserts value is T {
  messages();
  const r = z.safeParse(schema, value);
  if (!r.success) fail(what, name, r.error);
}

/** `value` as `schema` makes it (its defaults filled in, keys it does
 * not know dropped), or a TypeError as `check` throws. */
export function parse<S extends z.ZodMiniType>(what: string, schema: S, value: unknown, name = ""): z.output<S> {
  messages();
  const r = z.safeParse(schema, value);
  if (!r.success) fail(what, name, r.error);
  return r.data;
}

function fail(what: string, name: string, error: z.core.$ZodError): never {
  const lines = error.issues.map((i) => {
    const path = [name, ...i.path.map(String)].filter((p) => p !== "").join(".");
    return path === "" ? `${what}: ${i.message}` : `${what}: ${path}: ${i.message}`;
  });
  throw new TypeError(lines.join("\n"));
}

/** null or undefined: a value left out (the API's are undefined; either
 * is taken, so a check does not depend on which). */
export const isNullish = (v: unknown): v is null | undefined => v == null;

/** `schema` as a type predicate. */
export function guard<T>(schema: z.ZodMiniType<T>): (value: unknown) => value is T {
  return (value): value is T => z.safeParse(schema, value).success;
}

/** A function (of the type given: zod cannot look at its parameters). */
// biome-ignore lint/suspicious/noExplicitAny: any function, whatever its parameters.
export function func<F extends (...args: any[]) => unknown = (...args: any[]) => unknown>(): z.ZodMiniCustom<F, F> {
  return z.custom<F>((v) => typeof v === "function", { error: "expected a function" });
}

/** Options: `shape`, `{}` when left out (its defaults then). */
export function options<S extends z.core.$ZodLooseShape>(shape: S) {
  // `{}` is a valid input when every key has a default or is optional,
  // which zod's types cannot see for a generic shape.
  return z.prefault(z.object(shape), {} as never);
}

/** Schema `S` takes every value of `T` and no key `T` does not have:
 * options it fills in (`?? null` as defaults) checked against their type. */
export type Takes<S extends z.ZodMiniType, T> = [T] extends [z.input<S>]
  ? IsEqual<keyof NonNullable<z.input<S>>, keyof T>
  : false;
