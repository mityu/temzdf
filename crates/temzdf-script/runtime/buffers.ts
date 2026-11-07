// What happens to buffers (namespace builtin-buffers; hooks only): unsaved
// changes are not lost by accident, and a file changed elsewhere is read
// again. Defined when enabled: `import buffers from "temzdf/buffers";
// buffers.enable();`.
import { cmd, schedule } from "temzdf";
import { keyset } from "./lib/keyset";

export default keyset("builtin-buffers", (d) => {
  // Unsaved changes are not lost by accident: deleting a text buffer, or
  // ending the session (kill-session, closing the last pane), is refused
  // while one has them (`kill-session --force` ends anyway). Closing a pane
  // is fine: its buffer stays, hidden.
  // A buffer with unsaved changes is not deleted, unless its variable
  // forceDelete says so (Vim's :bd!).
  d.hook("BufferDeletePre", (buf: number) => {
    const b = cmd.buffers().find((b) => b.id === buf);
    if (b?.modified && cmd.getVar("buffer", "forceDelete", buf) !== true) return { cancel: true, reason: `${b.name} has unsaved changes (write it first)` };
  });
  d.hook("SessionEndPre", () => {
    const names = cmd.buffers().filter((b) => b.modified).map((b) => b.name);
    if (names.length > 0) {
      return { cancel: true, reason: `unsaved changes in ${names.join(", ")} (write them, or kill-session --force)` };
    }
  });

  // A text buffer's file changed elsewhere: read again when it has no
  // unsaved changes (Vim's autoread); otherwise say so (writing then needs
  // write({ force: true }), or reload() to drop the changes).
  d.hook("FileChangedOnDisk", (buf: number) => {
    const b = cmd.buffers().find((b) => b.id === buf);
    if (b === undefined) return;
    if (b.modified) cmd.message(`${b.name} changed on disk (reload() drops your changes)`);
    else schedule(() => cmd.reload(buf));
  });
});
