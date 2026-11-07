// What browser-minded polyfills look for first: `self` (the global object)
// and `DOMException` (AbortSignal.timeout's reason). The npm packages for
// DOMException (domexception, node-domexception) are deprecated; this is
// the part of it scripts can meet: a name, a message and the legacy code
// of the two names used here.
const g = globalThis as Record<string, unknown>;
g.self ??= globalThis;
if (g.DOMException === undefined) {
  const codes: Record<string, number> = { AbortError: 20, TimeoutError: 23 };
  g.DOMException = class DOMException extends Error {
    readonly code: number;
    constructor(message = "", name = "Error") {
      super(message);
      this.name = name;
      this.code = codes[name] ?? 0;
    }
  };
}
