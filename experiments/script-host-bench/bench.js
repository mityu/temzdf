// The JavaScript side of forms B and C (doc/survey/script-host-transport.md),
// shaped like crates/temzdf-script/src/prelude.js: a command is a native
// `request(token, op, payload)` call with JSON strings both ways. `__host`
// (current, request) is installed by the embedding before this runs.
(() => {
  const host = globalThis.__host;
  const current = () => JSON.parse(host.current());
  function request(token, op, payload) {
    return JSON.parse(host.request(JSON.stringify(token), op, JSON.stringify(payload)));
  }
  const cmd = {
    currentMode: (...args) => request(current(), "command", { name: "current-mode", args }),
  };
  // The mapped function: one key issuing `k` commands.
  globalThis.mapped = (argsJson) => {
    const ctx = JSON.parse(argsJson)[0];
    for (let i = 0; i < ctx.k; i++) {
      if (cmd.currentMode() !== "normal") throw new Error("unexpected reply");
    }
  };
  // Plain JavaScript work, to compare the interpreter's speed.
  globalThis.compute = (n) => {
    let s = 0;
    let t = "";
    for (let i = 0; i < n; i++) {
      s = (s + i * 7) % 1000003;
      if (i % 1000 === 0) t += String(s).slice(0, 2);
    }
    return s + t.length;
  };
})();
