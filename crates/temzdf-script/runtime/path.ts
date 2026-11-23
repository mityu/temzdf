// `temzdf/path`: JSR's `@std/path` as the session's OS writes paths (the
// guest is one build for every OS: the host names the style, in
// `globalThis.__temzdfPathStyle`, before any module loads), but `resolve`:
// scripts have no working directory (where one would be is not settled
// yet; `relative` and others reaching it throw a TypeError).
// `temzdf/path/posix` and `temzdf/path/windows` are either style anywhere.
// Export by export, each typed as the POSIX one (the two take and give the
// same), so that the declarations name no module object.
import {
  basename as basenameP,
  common as commonP,
  dirname as dirnameP,
  extname as extnameP,
  format as formatP,
  fromFileUrl as fromFileUrlP,
  globToRegExp as globToRegExpP,
  isAbsolute as isAbsoluteP,
  isGlob as isGlobP,
  join as joinP,
  joinGlobs as joinGlobsP,
  normalize as normalizeP,
  normalizeGlob as normalizeGlobP,
  parse as parseP,
  relative as relativeP,
  toFileUrl as toFileUrlP,
  toNamespacedPath as toNamespacedPathP,
  SEPARATOR_PATTERN as SEPARATOR_PATTERNP,
} from "@std/path/posix";
import * as windows from "@std/path/windows";

const win = (globalThis as { __temzdfPathStyle?: string }).__temzdfPathStyle === "windows";

export const basename: typeof basenameP = win ? windows.basename : basenameP;
export const common: typeof commonP = win ? windows.common : commonP;
export const dirname: typeof dirnameP = win ? windows.dirname : dirnameP;
export const extname: typeof extnameP = win ? windows.extname : extnameP;
export const format: typeof formatP = win ? windows.format : formatP;
export const fromFileUrl: typeof fromFileUrlP = win ? windows.fromFileUrl : fromFileUrlP;
export const globToRegExp: typeof globToRegExpP = win ? windows.globToRegExp : globToRegExpP;
export const isAbsolute: typeof isAbsoluteP = win ? windows.isAbsolute : isAbsoluteP;
export const isGlob: typeof isGlobP = win ? windows.isGlob : isGlobP;
export const join: typeof joinP = win ? windows.join : joinP;
export const joinGlobs: typeof joinGlobsP = win ? windows.joinGlobs : joinGlobsP;
export const normalize: typeof normalizeP = win ? windows.normalize : normalizeP;
export const normalizeGlob: typeof normalizeGlobP = win ? windows.normalizeGlob : normalizeGlobP;
export const parse: typeof parseP = win ? windows.parse : parseP;
export const relative: typeof relativeP = win ? windows.relative : relativeP;
export const toFileUrl: typeof toFileUrlP = win ? windows.toFileUrl : toFileUrlP;
export const toNamespacedPath: typeof toNamespacedPathP = win ? windows.toNamespacedPath : toNamespacedPathP;
export const SEPARATOR_PATTERN: RegExp = win ? windows.SEPARATOR_PATTERN : SEPARATOR_PATTERNP;
/** The separator: `/`, or `\\` on Windows. */
export const SEPARATOR: "/" | "\\" = win ? windows.SEPARATOR : "/";
/** What separates paths in a list (`PATH`): `:`, or `;` on Windows. */
export const DELIMITER: ":" | ";" = win ? windows.DELIMITER : ":";
export type { GlobOptions, ParsedPath } from "@std/path/posix";
