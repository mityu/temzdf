// A configuration using the whole scripting API; crates/temzdf/tests/types.rs
// type checks it against `temzdf print-types`.
import { action, atomic, cmd, currentMode, defineMode, dispatch, feedkeys, map, o, on, unmap } from "temzdf";

o.timeoutlen = 500;
o.splitright = false;

defineMode("window", { inherit: "normal", unmapped: "drop", count: true });
map("normal", "w", action.switchMode("window"));
map("window", "v", [action.split("right"), action.exitMode()], { desc: "split right" });
map("window", "q", action.exitMode());
map(["pdf", "terminal"], "<C-b>%", (ctx) => {
  cmd.split(ctx.count === null ? "right" : "bottom");
});
unmap("normal", "x");

on("ModeChanged", (old, now) => console.log(`${old} -> ${now} (${currentMode()})`));
on("PaneOpened", (pane, kind) => {
  if (kind === "terminal") cmd.message(`terminal ${pane}`);
}, { nested: true });

dispatch(async (ctx) => {
  await new Promise<void>((resolve) => setTimeout(resolve, 10));
  ctx.atomic(() => ctx.cmd.message("later"));
});

map("normal", "gx", () => {
  atomic(() => feedkeys("<Esc>", { remap: false }));
  const panes: { id: number }[] = cmd.panes();
  cmd.message(`${panes.length} panes`);
});
