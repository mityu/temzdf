#!/usr/bin/env bash
# One persistent macOS VM (Tart) for scripts/e2e.sh, so that runs neither
# take the host's front window nor its pointer, and the test environment can
# be rebuilt on another Mac the same way. See "Running in a VM" in
# doc/macos.md.
#
# Usage: scripts/macos-vm.sh COMMAND [ARGS...]
#   create        clone the base image ($TEMZDF_VM_IMAGE, pinned below) into
#                 the VM ($TEMZDF_VM, default temzdf-e2e) and size it
#   start         start it headless with the repository shared, wait until
#                 it answers (creates it first if needed)
#   provision     set up the guest (scripts/macos/vm-provision.sh: Nix,
#                 Ghostty, WezTerm; no sleep or screen lock; kitty is
#                 installed but cannot draw in a VM, no OpenGL); rerun
#                 after changing that script
#   permissions   when macOS asks (about every 30 days) whether the agent
#                 may go on recording the screen: restart the VM with its
#                 screen in a window, where a person clicks Allow (then
#                 `check`, `save`)
#   check         whether the guest has the permissions and the tools
#   save          stop and keep the VM as it is now as its golden copy
#   reset         replace the VM with a fresh clone of the golden copy
#   run [--] CMD  run CMD in the guest's copy of the work tree, inside its
#                 dev shell and GUI session; target/e2e* come back to the
#                 host's target/vm/; exits with CMD's status
#   e2e ARGS...   build temzdf and temzdf-gui there, then scripts/e2e.sh ARGS
#   shell         an interactive shell in the guest's work tree
#   stop, status
#
# Nothing but `stop` (and `save`, `reset`, `permissions`) stops the VM: runs
# leave it up for the next one, and for looking into a failure.
# Requires Apple Silicon and Tart (in the dev shell on macOS).
set -euo pipefail
cd "$(dirname "$0")/.."

[[ $(uname) == Darwin ]] || { echo "macos-vm.sh: macOS only" >&2; exit 2; }

vm=${TEMZDF_VM:-temzdf-e2e}
golden=$vm-golden
# The base image, pinned (Cirrus Labs' macOS images: user admin/admin,
# auto-login, SSH, Homebrew, the Command Line Tools, the Tart Guest Agent).
image=${TEMZDF_VM_IMAGE:-ghcr.io/cirruslabs/macos-tahoe-base@sha256:4aaa751fa7538902d73c767f27a066fc87e83f183069da4e4b5022863abd4c77}
cpus=${TEMZDF_VM_CPUS:-4}
memory=${TEMZDF_VM_MEMORY:-6144}
# Scale 1 (px), as scripts/e2e.sh needs it in a VM, large enough for its
# windows (900x600 plus title bar and menu bar).
display=1600x1000px
out=target/macos-vm
share=temzdf
# Where the guest sees the repository, and its own copy of it.
guest_share="/Volumes/My Shared Files/$share"
guest_tree='$HOME/temzdf'
mkdir -p "$out"

usage() { sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"; exit 2; }

# The VM's state in `tart list`: running, stopped, or nothing.
state() { tart list --source local | awk -v n="$1" '$2 == n { print $NF }'; }
exists() { [[ -n $(state "$1") ]]; }
running() { [[ $(state "$vm") == running ]]; }

# A command in the guest, through the Tart Guest Agent (in the logged-in
# user's GUI session), in a login shell with Nix's profile (its installer
# sets it up for interactive shells only).
nix_profile=/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
guest() {
  tart exec "$vm" /bin/zsh -lc "[[ -e $nix_profile ]] && . $nix_profile; export NIX_CONFIG='warn-dirty = false'; $1"
}

wait_ready() {
  local i
  for ((i = 0; i < 180; i++)); do
    tart exec "$vm" /usr/bin/true >/dev/null 2>&1 && return 0
    sleep 1
  done
  echo "macos-vm.sh: $vm does not answer (see $out/run.log)" >&2
  return 1
}

create() {
  exists "$vm" && return 0
  tart clone "$image" "$vm"
  tart set "$vm" --cpu "$cpus" --memory "$memory" --display "$display" --no-display-refit
}

# tart run stays in the foreground for the VM's life: detached here.
boot() {
  local graphics=$1
  local args=(--dir="$share:$PWD" --no-audio)
  [[ $graphics == headless ]] && args+=(--no-graphics)
  nohup tart run "${args[@]}" "$vm" >>"$out/run.log" 2>&1 </dev/null &
  disown
  wait_ready
}

start() {
  create
  running && return 0
  boot headless
}

stop() {
  running || return 0
  tart stop "$vm"
}

# The work tree into the guest's copy: tracked and untracked files, .git
# (the flake reads the tree through git, so target/ never goes into the
# store), the fixtures (no TeX needed there); not the rest of target/.
sync_in() {
  guest "mkdir -p $guest_tree && rsync -a --delete --exclude /target/ '$guest_share/' $guest_tree/ &&
    mkdir -p $guest_tree/target && rsync -a '$guest_share/target/fixtures/' $guest_tree/target/fixtures/"
}

# What e2e runs left (target/e2e*), back to the host's target/vm/.
sync_out() {
  guest "mkdir -p '$guest_share/target/vm' && cd $guest_tree/target &&
    for d in e2e*(N/); do rsync -a --delete \$d/ '$guest_share/target/vm/'\$d/; done"
}

run() {
  local cmd status=0
  [[ ${1:-} == -- ]] && shift
  (($# > 0)) || usage
  start
  # Terminals left open (macOS reopens apps at login) would sit where
  # scripts/e2e.sh puts its window.
  guest "pkill -x ghostty; pkill -x wezterm-gui; pkill -x kitty; pkill -x Terminal; sleep 1; true"
  sync_in
  cmd=$(printf '%q ' "$@")
  guest "cd $guest_tree && nix develop --quiet -c $cmd" || status=$?
  sync_out || true
  return "$status"
}

cmd=${1:-}
[[ -n $cmd ]] || usage
shift
case $cmd in
  create) create ;;
  start) start ;;
  stop) stop ;;
  provision)
    start
    sync_in
    guest "cd $guest_tree && bash scripts/macos/vm-provision.sh setup"
    ;;
  permissions)
    create
    stop
    boot window
    # A screenshot by the agent brings up macOS's question about screen
    # recording (the permission itself is in the image already).
    guest "screencapture -x /tmp/temzdf-vm-permissions.png" || true
    cat <<'EOF'
In the VM's window (Tart's), macOS asks whether "tart-guest-agent" may
record the screen: click Allow. Then
  scripts/macos-vm.sh check   (it should say "all there")
  scripts/macos-vm.sh save    (keeps this VM as the golden copy; stops it)
macOS may ask again after 30 days (and as long after the golden copy was
saved, after a reset): do this again then.
EOF
    ;;
  check)
    start
    sync_in
    guest "cd $guest_tree && bash scripts/macos/vm-provision.sh check"
    ;;
  save)
    stop
    exists "$golden" && tart delete "$golden"
    tart clone "$vm" "$golden"
    echo "saved $golden"
    ;;
  reset)
    exists "$golden" || { echo "macos-vm.sh: no $golden (save one first)" >&2; exit 1; }
    stop
    exists "$vm" && tart delete "$vm"
    tart clone "$golden" "$vm"
    ;;
  run) run "$@" ;;
  # The fixtures come from the host (sync_in): no TeX in the guest.
  e2e) run sh -c 'cargo build -q -p temzdf -p temzdf-gui && scripts/e2e.sh "$@"' e2e "$@" ;;
  shell)
    start
    sync_in
    tart exec -i -t "$vm" /bin/zsh -lc "cd $guest_tree && exec zsh -l"
    ;;
  status)
    echo "vm $vm: $(state "$vm" || true)${image:+ (image $image)}"
    echo "golden $golden: $(exists "$golden" && echo present || echo none)"
    running && echo "ip $(tart ip "$vm")" && guest "cat ~/temzdf-vm-versions.txt 2>/dev/null" || true
    ;;
  *) usage ;;
esac
