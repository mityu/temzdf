#!/usr/bin/env bash
# Run the judged e2e scenarios (scripts/e2e.sh with each action file's
# "Used with:" command) in one or more terminals, and sum up.
#
# Usage: scripts/e2e-all.sh [-t TERM]... [-d DISP] [-P PATH] [-o DIR] [SCENARIO...]
#   -t TERM   kitty, ghostty, wezterm or gui (repeatable; default: every one
#             that can run here: in a macOS VM not kitty, which needs OpenGL)
#   -d DISP   display server, passed on to e2e.sh (x11, wayland; macos)
#   -P PATH   how clients take pixels, passed on to e2e.sh (shared, socket)
#   -o DIR    where the runs go, as DIR/TERM/SCENARIO (default target/e2e-all,
#             target/e2e-all-DISP with -d)
#   -j N      run N scenarios at once (default 1). A scenario runs in its
#             terminals one after another, never two of it at once: some use
#             fixed paths (target/e2e-sync, target/e2e-reload, ...). Each run
#             has its own Xvfb or sway (not on macOS: there -j is 1). Those
#             whose timing suffers when the CPU is shared run alone afterwards
#             (`alone` below).
#   -R        draw `expect`'s comparisons with the release builds
#             (target/release/temzdf, temzdf-gui; E2E_RENDERER and
#             E2E_GUI_RENDERER of e2e.sh): much quicker. Builds them first
#             (a second when they are up to date).
#   SCENARIO  action file names without .actions (default: every judged one
#             for the terminal; gui is the GUI's only)
#
# Prints PASS/FAIL/SKIP counts per terminal and scenario and their sum;
# exits 1 if anything failed or crashed. Builds nothing but -R's renderers:
# build temzdf and temzdf-gui first (scripts/macos-vm.sh e2e-all does, in the
# VM).
set -uo pipefail

# shellcheck disable=SC2164
# This cd never fails.
cd "$(dirname "$(dirname "${BASH_SOURCE[0]}")")"

terms=() outdir= pass=() display= jobs=1
while getopts "t:d:P:o:j:Rh" opt; do
  case $opt in
    t) terms+=("$OPTARG") ;;
    d) pass+=(-d "$OPTARG") display=$OPTARG ;;
    P) pass+=(-P "$OPTARG") ;;
    o) outdir=$OPTARG ;;
    j) jobs=$OPTARG ;;
    R)
      # Up to date for sure (cargo knows; a second when they are).
      cargo build -q --release -p temzdf -p temzdf-gui || exit 2
      export E2E_RENDERER=$PWD/target/release/temzdf E2E_GUI_RENDERER=$PWD/target/release/temzdf-gui
      ;;
    *) sed -n '2,/^set -uo/{/^set -uo/!p}' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
outdir=${outdir:-target/e2e-all${display:+-$display}}
in_vm=
[[ $(uname) == Darwin && $(sysctl -n kern.hv_vmm_present 2>/dev/null) == 1 ]] && in_vm=1
[[ $(uname) == Darwin ]] && jobs=1
if ((${#terms[@]} == 0)); then
  terms=(ghostty wezterm gui)
  [[ -z $in_vm ]] && terms=(kitty "${terms[@]}")
fi

# The terminal scenarios, in a stable order, and the GUI's. Not on macOS:
# gui-focus (AppKit sends no presses for keys held on focus-in) and gui-ime
# (-I, fcitx5, is Linux's).
judged=(session terminal terminal-extras tabs floats resize float-wide expect links preview search select
  select-autoscroll viewer viewer-stress synctex synctex-reload reload)
gui_judged=(gui gui-focus gui-ime)
if [[ $(uname) == Darwin ]]; then
  gui_judged=(gui)
fi

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
    synctex-reload)
      mkdir -p target/e2e-sync-reload
      cp target/fixtures/sample.pdf target/fixtures/sample.synctex.gz target/e2e-sync-reload/
      pdf target/e2e-sync-reload/sample.pdf
      ;;
    reload)
      mkdir -p target/e2e-reload
      cp target/fixtures/sample.pdf target/e2e-reload/doc.pdf
      pdf target/e2e-reload/doc.pdf
      ;;
    gui) cmd=(target/debug/temzdf-gui --clean target/fixtures/sample.pdf) ;;
    gui-focus | gui-ime)
      cmd=(env SHELL=/bin/sh 'PS1=$ ' target/debug/temzdf-gui --clean)
      if [[ $1 == gui-ime ]]; then opts=(-I); fi
      ;;
    expect | links | preview | search | select | select-autoscroll | viewer) pdf ;;
    # Inline transfers, as over SSH.
    viewer-stress) pdf; cmd=(env TERMPIX_MEDIUM=direct "${cmd[@]}") ;;
    *)
      echo "e2e-all.sh: no command for scenario $1" >&2
      return 1
      ;;
  esac
}

# Scenarios that need the CPU to themselves (smooth scrolling and tiles
# arriving within their pauses): after the others, one at a time.
alone=()

# Run scenario $2 in terminal $1; its line of the summary goes to OUT.line.
run_one() {
  local t=$1 s=$2 out=$outdir/$1/$2 t0=$SECONDS rc p=0 f=0 k=0
  rm -rf "$out"
  mkdir -p "$out"
  if ! scenario "$s"; then
    echo "$t $s: no command" >"$out.line"
    ((jobs > 1)) || cat "$out.line"
    return
  fi
  scripts/e2e.sh -t "$t" "${pass[@]}" "${opts[@]}" -o "$out" "tests/e2e/$s.actions" -- "${cmd[@]}" >"$out.log" 2>&1
  rc=$?
  if [[ -f $out/expect.txt ]]; then
    p=$(grep -c '^PASS' "$out/expect.txt") f=$(grep -c '^FAIL' "$out/expect.txt") k=$(grep -c '^SKIP' "$out/expect.txt")
  fi
  # 4: failed expectations (counted); anything else is the run's.
  printf '%-8s %-16s %3ss PASS %2s FAIL %s SKIP %s%s %s\n' "$t" "$s" $((SECONDS - t0)) "$p" "$f" "$k" \
    "$( ((rc != 0 && rc != 4)) && echo " (exit $rc, see $out.log)")" \
    "$(grep '^FAIL' "$out/expect.txt" 2>/dev/null | cut -d: -f1 | cut -d' ' -f2 | tr '\n' ' ')" >"$out.line"
  echo "$p $f $k $rc" >"$out.counts"
  # One at a time: as each ends.
  ((jobs > 1)) || cat "$out.line"
}

# The scenarios and, for each, the terminals it runs in.
start=$SECONDS
declare -A in_terms=()
order=()
for t in "${terms[@]}"; do
  if [[ $t == kitty && -n $in_vm ]]; then
    echo "$t: not in a VM (no OpenGL there)"
    continue
  fi
  if (($# > 0)); then
    list=("$@")
  elif [[ $t == gui ]]; then
    list=("${gui_judged[@]}")
  else
    list=("${judged[@]}")
  fi
  for s in "${list[@]}"; do
    # The GUI runs its own scenarios only (gui*), terminals not those.
    if [[ $t == gui && $s != gui* ]] || [[ $t != gui && $s == gui* ]]; then continue; fi
    [[ -n ${in_terms[$s]:-} ]] || order+=("$s")
    in_terms[$s]+="$t "
    rm -f "$outdir/$t/$s.line" "$outdir/$t/$s.counts"
  done
done
# Every terminal of scenario $1, one after another; with -j on Xvfb
# display 300 + SLOT ($2): Xvfb's own pick of a free display (xvfb-run -a)
# gave two runs started at once the same one.
run_scenario() {
  local t
  if [[ -n ${2:-} ]]; then export E2E_XVFB_DISPLAY=$((300 + $2)); fi
  for t in ${in_terms[$1]}; do run_one "$t" "$1"; done
}
later=() slot_pid=()
for s in "${order[@]}"; do
  if ((jobs > 1)) && [[ " ${alone[*]} " == *" $s "* ]]; then
    later+=("$s")
    continue
  fi
  if ((jobs > 1)); then
    # A free slot: one whose scenario ended.
    while :; do
      for ((slot = 0; slot < jobs; slot++)); do
        [[ -z ${slot_pid[slot]:-} ]] || ! kill -0 "${slot_pid[slot]}" 2>/dev/null && break 2
      done
      wait -n || true
    done
    run_scenario "$s" "$slot" &
    slot_pid[slot]=$!
  else
    run_scenario "$s"
  fi
done
wait
for s in "${later[@]}"; do run_scenario "$s"; done

# The summary (with -j, the lines too), in the order of terminals and
# scenarios.
failed=0 total_pass=0 total_fail=0 total_skip=0
for t in "${terms[@]}"; do
  for s in "${order[@]}"; do
    [[ " ${in_terms[$s]} " == *" $t "* ]] || continue
    if ((jobs > 1)); then cat "$outdir/$t/$s.line" 2>/dev/null || echo "$t $s: no result"; fi
    if read -r p f k rc <"$outdir/$t/$s.counts" 2>/dev/null; then
      ((total_pass += p, total_fail += f, total_skip += k))
      ((rc != 0)) && failed=1
    else
      failed=1
    fi
  done
done
echo "total: PASS $total_pass FAIL $total_fail SKIP $total_skip in $((SECONDS - start))s ($outdir)"
exit "$failed"
