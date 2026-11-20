// A configuration using the whole scripting API; crates/temzdf/tests/types.rs
// type checks it against `temzdf print-types`.
import { action, atomic, currentMode, defineMode, dispatch, feedkeys, keymap, o, on } from "temzdf";

o.timeoutlen = 500;
o.splitright = false;

defineMode("window", { fallback: "drop", count: true, transient: true, keepalive: "always", timeout: 2000 });
defineMode("resize", {
  transient: true,
  keepalive: (ctx) => ctx.paneKind !== undefined && ctx.paneKind !== "markdown",
});
keymap.add("normal", "w", () => action.switchMode("window"));
keymap.add(
  "window",
  "v",
  () => {
    action.split("right");
    action.exitMode();
  },
  { desc: "split right", namespace: "mine" },
);
keymap.add("window", "q", () => action.exitMode(), { priority: 1 });
keymap.add("window", "gt", (ctx) => action.nextTab(ctx.count));
keymap.add(
  ["pdf", "terminal"],
  "<C-b>%",
  (ctx) => {
    action.split(ctx.count === undefined ? "right" : "bottom");
  },
  { when: (ctx) => !ctx.floating && ctx.keys === "" },
);
keymap.remove("normal", "x");
keymap.remove("normal", "q", { namespace: "builtin-mode-normal" });
keymap.clear({ mode: "window", namespace: "mine" });
keymap.clear({ namespace: "nobody" });
for (const m of keymap.get({ mode: "normal", namespace: "builtin-mode-normal" })) {
  keymap.add("resize", m.lhs, m.rhs, { when: m.when, nowait: m.nowait, desc: m.desc });
}

on("ModeChanged", (old, now) => console.log(`${old} -> ${now} (${currentMode()})`));
on(
  "PaneOpened",
  (pane, kind) => {
    if (kind === "terminal") action.message(`terminal ${pane}`);
  },
  { nested: true },
);

dispatch(async (ctx) => {
  await new Promise<void>((resolve) => setTimeout(resolve, 10));
  ctx.atomic(() => ctx.action.message("later"));
});

keymap.add("normal", "gx", () => {
  atomic(() => feedkeys("<Esc>", { remap: false }));
  const panes: { id: number }[] = action.panes();
  action.message(`${panes.length} panes`);
});

o.remainonexit = true;
o.reloadanchor = "text";
o.linkopener = ["firefox"];
o.showstatusline = "never";
keymap.add("normal", "n", () => {
  const hidden = action.buffers().filter((b) => b.panes.length === 0 && !b.exited);
  if (hidden.length > 0) action.showBuffer(hidden[0].id);
});
keymap.add("normal", "e", () => {
  action.edit("paper.pdf");
  action.deleteBuffer();
});
on("BufferCreated", (buffer, kind) => console.log(`${kind} ${buffer}`));
on("BufferDeleted", (buffer) => console.log(`gone ${buffer}`));
