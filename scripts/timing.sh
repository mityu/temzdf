#!/usr/bin/env bash
# What frames cost between a session server and its client: joins the
# `timing:` lines (written with TEMZDF_TIMING set) of the server's log and the
# client's by frame, and prints the heaviest frames and a summary.
# doc/performance.md, "Session socket", has the measurement this is for.
#
# Usage: scripts/timing.sh OUTDIR
#   OUTDIR: an e2e run of tests/e2e/perf.actions (or any) with TEMZDF_TIMING=1.
#   The server's log is OUTDIR/state/temzdf/NAME.log; the client's is
#   OUTDIR/terminal.log for the GUI (its stderr), else
#   OUTDIR/state/temzdf/temzdf.log.
#
# Columns, in ms: send (the server's writer: encode and write until the socket
# took it), recv (the client: read and decode, from the message's first
# bytes), upload (GUI: images to textures, queued to the GPU), present (TTY:
# frame handed to the terminal writer), ack (the server: queued until the
# client acknowledged it, i.e. shown). It does not see the terminal's side.
set -euo pipefail
[[ $# -eq 1 && -d $1 ]] || { sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"; exit 2; }
out=$(realpath "$1")
self=$(realpath "$0")
if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  NIX_CONFIG="warn-dirty = false${NIX_CONFIG:+$'\n'$NIX_CONFIG}" exec nix develop --quiet -c "$self" "$out"
fi
server=$(find "$out/state/temzdf" -maxdepth 1 -name '*.log' ! -name temzdf.log | head -1)
client=$out/state/temzdf/temzdf.log
grep -q 'timing: received' "$client" 2>/dev/null || client=$out/terminal.log
[[ -n $server ]] || { echo "timing.sh: no session log in $out/state/temzdf" >&2; exit 1; }

python3 - "$server" "$client" <<'EOF'
import re, statistics, sys

server, client = sys.argv[1], sys.argv[2]
frames = {}


def row(seq):
    return frames.setdefault(int(seq), {})


for line in open(server, errors="replace"):
    if m := re.search(r"timing: sent frame (\d+) \((\d+) images, (\d+) bytes of pixels\) in ([\d.]+) ms", line):
        r = row(m[1])
        r["images"], r["bytes"], r["send"] = int(m[2]), int(m[3]), float(m[4])
    elif m := re.search(r"timing: client \d+ acknowledged frame (\d+) ([\d.]+) ms", line):
        row(m[1])["ack"] = float(m[2])
for line in open(client, errors="replace"):
    if m := re.search(r"timing: received frame (\d+) .* in ([\d.]+) ms", line):
        row(m[1])["recv"] = float(m[2])
    elif m := re.search(r"timing: uploaded the images of frame (\d+) in ([\d.]+) ms", line):
        row(m[1])["upload"] = float(m[2])
    elif m := re.search(r"timing: presented frame (\d+) \(\w+\) in ([\d.]+) ms", line):
        r = row(m[1])
        r["present"] = r.get("present", 0.0) + float(m[2])

heavy = sorted((s for s in frames if frames[s].get("bytes", 0) > 0), key=lambda s: -frames[s]["bytes"])
light = [s for s in frames if frames[s].get("bytes", 0) == 0]
print(f"{len(frames)} frames, {len(heavy)} with pixels ({sum(frames[s]['bytes'] for s in heavy) / 1e6:.1f} MB)")
print(f"{'frame':>5} {'images':>6} {'MB':>6} {'send':>7} {'recv':>7} {'upload':>7} {'present':>7} {'ack':>7}")
for s in heavy[:12]:
    r = frames[s]
    cell = lambda k: f"{r[k]:7.2f}" if k in r else "      -"
    print(f"{s:5} {r['images']:6} {r['bytes'] / 1e6:6.2f} {cell('send')} {cell('recv')} {cell('upload')} {cell('present')} {cell('ack')}")
for name, group in [("with pixels", heavy), ("without", light)]:
    for k in ["send", "recv", "upload", "present", "ack"]:
        v = [frames[s][k] for s in group if k in frames[s]]
        if v and (group is heavy or k == "ack"):
            print(f"{k:>7} {name:>11}: median {statistics.median(v):6.2f}  max {max(v):6.2f}")
EOF
