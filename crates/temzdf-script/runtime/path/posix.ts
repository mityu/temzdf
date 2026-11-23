// `temzdf/path/posix`: JSR's `@std/path/posix` (POSIX paths on any OS),
// but `resolve`: scripts have no working directory (where one would be is
// not settled yet; `relative` and others reaching it throw a TypeError).
export {
  basename,
  common,
  DELIMITER,
  dirname,
  extname,
  format,
  fromFileUrl,
  globToRegExp,
  isAbsolute,
  isGlob,
  join,
  joinGlobs,
  normalize,
  normalizeGlob,
  parse,
  relative,
  SEPARATOR,
  SEPARATOR_PATTERN,
  toFileUrl,
  toNamespacedPath,
} from "@std/path/posix";
export type { GlobOptions, ParsedPath } from "@std/path/posix";
