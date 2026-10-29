#!/usr/bin/env bash
# Run the judged e2e scenarios (scripts/e2e.sh with each action file's
# "Used with:" command) in one or more terminals, and sum up.
#
# Usage: scripts/e2e-all.sh [-t TERM]... [-o DIR] [SCENARIO...]
#   -t TERM   kitty, ghostty, wezterm or gui (repeatable; default: every one
#             that can run here: in a macOS VM not kitty, which needs OpenGL)
#   -o DIR    where the runs go, as DIR/TERM/SCENARIO (default target/e2e-all)
#   SCENARIO  action file names without .actions (default: every judged one
#             for the terminal; gui is the GUI's only)
#
# Prints PASS/FAIL/SKIP counts per terminal and scenario and their sum;
# exits 1 if anything failed or crashed. Builds nothing: build temzdf and
# temzdf-gui first (scripts/macos-vm.sh e2e-all does, in the VM).
set -uo pipefail
cd "$(dirname "$0")/.."

terms=() outdir=target/e2e-all
while getopts "t:o:h" opt; do
  case $opt in
    t) terms+=("$OPTARG") ;;
    o) outdir=$OPTARG ;;
    *) sed -n '2,/^set -uo/{/^set -uo/!p}' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
in_vm=
[[ $(uname) == Darwin && $(sysctl -n kern.hv_vmm_present 2>/dev/null) == 1 ]] && in_vm=1
if ((${#terms[@]} == 0)); then
  terms=(ghostty wezterm gui)
  [[ -z $in_vm ]] && terms=(kitty "${terms[@]}")
fi

# The terminal scenarios, in a stable order.
judged=(session terminal terminal-extras tabs floats resize float-wide expect links preview search select viewer synctex reload)

# A session's command: a shell that starts session NAME and stays after.
session() { cmd=(sh -c "SHELL=/bin/sh PS1=\"\$ \" target/debug/temzdf --clean -s $1; exec sh"); }
pdf() { cmd=(target/debug/temzdf standalone pdf --clean "${@:-target/fixtures/sample.pdf}"); }

# Sets cmd (the COMMAND), opts (more e2e.sh options) and prepares files, for
# scenario $1 (as its "Used with:" says).
scenario() {
  opts=()
  case $1 in
    session)
      mkdir -p target/e2e-session
      cmd=(sh -c 'target/debug/temzdf --clean -s e2e target/fixtures/sample.pdf; exec sh')
      ;;
    terminal) mkdir -p target/e2e-terminal; session e2e-term ;;
    terminal-extras) mkdir -p target/e2e-term-extras; session e2e-x ;;
    tabs) mkdir -p target/e2e-tabs; session e2e-t ;;
    floats) mkdir -p target/e2e-floats; session e2e-f ;;
    resize) mkdir -p target/e2e-resize; session e2e-r ;;
    float-wide) opts=(-s 800x600); session e2e-w ;;
    synctex)
      mkdir -p target/e2e-sync
      cp target/fixtures/sample.pdf target/fixtures/sample.synctex.gz target/e2e-sync/
      rm -f target/e2e-sync/inverse.txt
      pdf --editor 'echo %{input}:%{line} > target/e2e-sync/inverse.txt' target/e2e-sync/sample.pdf
      ;;
    reload)
      mkdir -p target/e2e-reload
      cp target/fixtures/sample.pdf target/e2e-reload/doc.pdf
      pdf target/e2e-reload/doc.pdf
      ;;
    gui) cmd=(target/debug/temzdf-gui --clean target/fixtures/sample.pdf) ;;
    expect | links | preview | search | select | viewer) pdf ;;
    *)
      echo "e2e-all.sh: no command for scenario $1" >&2
      return 1
      ;;
  esac
}

failed=0 total_pass=0 total_fail=0 total_skip=0
start=$SECONDS
for t in "${terms[@]}"; do
  if [[ $t == kitty && -n $in_vm ]]; then
    echo "$t: not in a VM (no OpenGL there)"
    continue
  fi
  if (($# > 0)); then
    list=("$@")
  elif [[ $t == gui ]]; then
    list=(gui)
  else
    list=("${judged[@]}")
  fi
  for s in "${list[@]}"; do
    # The GUI runs its own scenario only, terminals not that one.
    if [[ $t == gui && $s != gui ]] || [[ $t != gui && $s == gui ]]; then continue; fi
    out=$outdir/$t/$s
    rm -rf "$out"
    mkdir -p "$out"
    scenario "$s" || { failed=1; continue; }
    t0=$SECONDS
    scripts/e2e.sh -t "$t" "${opts[@]}" -o "$out" "tests/e2e/$s.actions" -- "${cmd[@]}" >"$out.log" 2>&1
    rc=$?
    p=0 f=0 k=0
    if [[ -f $out/expect.txt ]]; then
      p=$(grep -c '^PASS' "$out/expect.txt") f=$(grep -c '^FAIL' "$out/expect.txt") k=$(grep -c '^SKIP' "$out/expect.txt")
    fi
    ((total_pass += p, total_fail += f, total_skip += k))
    # 4: failed expectations (counted above); anything else is the run's.
    if ((rc != 0)); then failed=1; fi
    printf '%-8s %-16s %3ss PASS %2s FAIL %s SKIP %s%s %s\n' "$t" "$s" $((SECONDS - t0)) "$p" "$f" "$k" \
      "$( ((rc != 0 && rc != 4)) && echo " (exit $rc, see $out.log)")" \
      "$(grep '^FAIL' "$out/expect.txt" 2>/dev/null | cut -d: -f1 | cut -d' ' -f2 | tr '\n' ' ')"
  done
done
echo "total: PASS $total_pass FAIL $total_fail SKIP $total_skip in $((SECONDS - start))s ($outdir)"
exit "$failed"
