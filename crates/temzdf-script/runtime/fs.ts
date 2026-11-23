// `temzdf/fs`: reading files and directories, asynchronously, as Deno's
// file system API and JSR's `@std/fs` do (`readTextFile`, `readDir`,
// `exists`, ...). The server reads them natively on threads of its own
// (crates/temzdf-script/src/native_fs.rs): absolute paths only, regular
// files of at most 64 MiB, directories of at most 100k entries. Read-only.

/** What a path is (as Deno's `FileInfo`). */
export interface FileInfo {
  isFile: boolean;
  isDirectory: boolean;
  /** Only from `lstat`: `stat` follows links. */
  isSymlink: boolean;
  size: number;
  /** Null where the OS does not tell. */
  mtime: Date | null;
  atime: Date | null;
  birthtime: Date | null;
}

/** One entry of a directory (as Deno's `DirEntry`; a link not followed). */
export interface DirEntry {
  /** The name alone; one that is not UTF-8 has replacement characters. */
  name: string;
  isFile: boolean;
  isDirectory: boolean;
  isSymlink: boolean;
}

/** Why an operation failed, by class (as Deno's `Deno.errors`). Relative
 * paths throw a `TypeError` instead. */
export class FsError extends Error {}
class NotFound extends FsError {
  override name = "NotFound";
}
class PermissionDenied extends FsError {
  override name = "PermissionDenied";
}
class IsADirectory extends FsError {
  override name = "IsADirectory";
}
class NotADirectory extends FsError {
  override name = "NotADirectory";
}
/** Neither a regular file nor a directory where one is read (a FIFO, a device). */
class NotAFile extends FsError {
  override name = "NotAFile";
}
/** A file larger than 64 MiB. */
class TooLarge extends FsError {
  override name = "TooLarge";
}
/** A directory of more than 100k entries. */
class TooMany extends FsError {
  override name = "TooMany";
}
class Other extends FsError {
  override name = "Other";
}

/** The error classes: `e instanceof errors.NotFound`. */
export const errors = {
  NotFound,
  PermissionDenied,
  IsADirectory,
  NotADirectory,
  NotAFile,
  TooLarge,
  TooMany,
  Other,
} as const;

type Op = "readFile" | "readTextFile" | "stat" | "lstat" | "readDir" | "realPath";

const start = (globalThis as unknown as { __temzdfFs: (op: Op, path: string) => Promise<string | ArrayBuffer> })
  .__temzdfFs;

async function call(name: string, op: Op, path: unknown): Promise<string | ArrayBuffer> {
  if (typeof path !== "string") throw new TypeError(`${name}: path: expected a string, got ${typeof path}`);
  try {
    return await start(op, path);
  } catch (e) {
    if (typeof e !== "string") throw e;
    const { kind, message } = JSON.parse(e) as { kind: string; message: string };
    if (kind === "NotAbsolute") throw new TypeError(`${name}: ${message}`);
    const C: typeof FsError = (errors as Record<string, typeof FsError>)[kind] ?? Other;
    throw new C(`${name}: ${message}`);
  }
}

async function json(name: string, op: Op, path: unknown): Promise<unknown> {
  return JSON.parse((await call(name, op, path)) as string);
}

const date = (ms: number | null): Date | null => (ms == null ? null : new Date(ms));

function info(raw: unknown): FileInfo {
  const i = raw as Omit<FileInfo, "mtime" | "atime" | "birthtime"> & {
    mtime: number | null;
    atime: number | null;
    birthtime: number | null;
  };
  return { ...i, mtime: date(i.mtime), atime: date(i.atime), birthtime: date(i.birthtime) };
}

/** A file's bytes (links followed). */
export async function readFile(path: string): Promise<Uint8Array> {
  return new Uint8Array((await call("readFile", "readFile", path)) as ArrayBuffer);
}

/** A file's text, as UTF-8 (malformed bytes replaced). */
export async function readTextFile(path: string): Promise<string> {
  return (await call("readTextFile", "readTextFile", path)) as string;
}

/** What `path` is, links followed. */
export async function stat(path: string): Promise<FileInfo> {
  return info(await json("stat", "stat", path));
}

/** What `path` is, a link itself. */
export async function lstat(path: string): Promise<FileInfo> {
  return info(await json("lstat", "lstat", path));
}

/** A directory's entries, in the order the OS gives them (read at once,
 * then given one by one). */
export async function* readDir(path: string): AsyncIterable<DirEntry> {
  yield* (await json("readDir", "readDir", path)) as DirEntry[];
}

/** `path` with every link resolved. */
export async function realPath(path: string): Promise<string> {
  return (await json("realPath", "realPath", path)) as string;
}

/** Whether `path` exists (links followed), and, when asked, is a file or a
 * directory (as `@std/fs`'s `exists`). Errors other than not finding it
 * are thrown. */
export async function exists(path: string, options?: { isFile?: boolean; isDirectory?: boolean }): Promise<boolean> {
  if (options?.isFile && options?.isDirectory) {
    throw new TypeError("exists: options: isFile and isDirectory cannot both be true");
  }
  try {
    const i = await stat(path);
    if (options?.isFile) return i.isFile;
    if (options?.isDirectory) return i.isDirectory;
    return true;
  } catch (e) {
    if (e instanceof errors.NotFound) return false;
    throw e;
  }
}
