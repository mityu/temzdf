// Checking values scripts pass (or stored values read back) with zod
// (zod/mini, English messages). A schema is checked against the type
// written by hand, which stays what scripts see (`satisfies
// z.ZodMiniType<T>`): zod's types never reach temzdf-all.d.ts. Modules
// import zod themselves (`import * as z from "zod/mini"`): a namespace
// passed on from here would bundle all of it.
//
// Not z.file() nor the size checks (z.minSize, z.maxSize, z.size): they
// look at `File`, which QuickJS lacks.
import { en } from "zod/locales";
import * as z from "zod/mini";

z.config(en());

/** Throw a TypeError naming `what` (`command.add(w)`, say) for what
 * `schema` finds wrong in `value`, one line a problem
 * (`command.add(w): opts.bar: Invalid option: ...`). */
export function check<T>(what: string, schema: z.ZodMiniType<T>, value: unknown, name = ""): asserts value is T {
  const r = z.safeParse(schema, value);
  if (r.success) return;
  const lines = r.error.issues.map((i) => {
    const path = [name, ...i.path.map(String)].filter((p) => p !== "").join(".");
    return path === "" ? `${what}: ${i.message}` : `${what}: ${path}: ${i.message}`;
  });
  throw new TypeError(lines.join("\n"));
}

/** `schema` as a type predicate. */
export function guard<T>(schema: z.ZodMiniType<T>): (value: unknown) => value is T {
  return (value): value is T => z.safeParse(schema, value).success;
}

/** A function (of the type given: zod cannot look at its parameters). */
// biome-ignore lint/suspicious/noExplicitAny: any function, whatever its parameters.
export function func<F extends (...args: any[]) => unknown = (...args: any[]) => unknown>(): z.ZodMiniCustom<F, F> {
  return z.custom<F>((v) => typeof v === "function", { error: "expected a function" });
}
