// Globals scripts (and npm packages) expect that QuickJS does not have,
// from npm packages where a maintained one exists; bundled by
// crates/temzdf-script/build.rs and evaluated before any configuration.
import "./polyfills/self";
// AbortController and AbortSignal (abort, reason, throwIfAborted,
// addEventListener, AbortSignal.timeout and any).
import "abortcontroller-polyfill/dist/abortcontroller-polyfill-only";
