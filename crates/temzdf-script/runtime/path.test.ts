// temzdf/path (runtime/path.ts, runtime/path/): JSR's @std/path in the
// session's style, but `resolve`. The Windows style as the default is
// checked by the host's tests (crates/temzdf-script/tests/host/cases.rs).
import * as path from "temzdf/path";
import * as posix from "temzdf/path/posix";
import * as windows from "temzdf/path/windows";
import { test } from "temzdf/test";

test("temzdf/path is the session's style (POSIX here)", (assert) => {
  assert.equal(path.SEPARATOR, "/");
  assert.equal(path.DELIMITER, ":");
  assert.equal(path.join("/a/b", "../c", "d.ts"), "/a/c/d.ts");
  assert.equal(path.dirname("/home/u/.config/temzdf/config.ts"), "/home/u/.config/temzdf");
  assert.true(path.isAbsolute("/a"));
  assert.false(path.isAbsolute("C:\\a"));
  assert.deepEqual(path.parse("/a/b.ts"), { root: "/", dir: "/a", base: "b.ts", ext: ".ts", name: "b" });
  assert.true(path.SEPARATOR_PATTERN.test("/"));
});

test("either style is there on any OS", (assert) => {
  assert.equal(windows.join("C:\\Users\\x", "..\\y", "z.ts"), "C:\\Users\\y\\z.ts");
  assert.equal(windows.dirname("\\\\server\\share\\x\\a.ts"), "\\\\server\\share\\x");
  assert.true(windows.isAbsolute("C:/a"));
  assert.equal(windows.SEPARATOR, "\\");
  assert.equal(windows.fromFileUrl("file:///C:/a/b"), "C:\\a\\b");
  assert.equal(posix.relative("/a/b", "/a/c/d"), "../c/d");
  assert.equal(posix.toFileUrl("/a b").href, "file:///a%20b");
  assert.true(posix.globToRegExp("**/*.ts").test("a/b/c.ts"));
});

test("there is no resolve: scripts have no working directory", (assert) => {
  for (const m of [path, posix, windows]) {
    assert.false("resolve" in m);
  }
  // What reaches it inside @std/path says why.
  assert.throws(() => posix.relative("a", "b"), /working directory/);
});
