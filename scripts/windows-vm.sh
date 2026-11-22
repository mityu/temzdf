#!/usr/bin/env bash
# One disposable Windows 11 VM (QEMU/KVM, through vitro) for the Windows port,
# so that the ConPTY experiments and later the Windows e2e runs need no
# Windows machine. A thin wrapper over vitro (https://github.com/lambdalisue/vitro,
# pinned below). See "End-to-end tests in a VM" in doc/survey/windows-port.md
# and "Windows in a VM" in doc/dev/testing.md.
#
# Usage: scripts/windows-vm.sh COMMAND [ARGS...]
#   tools            build vitro and the OVMF firmware (pinned; nix build) into
#                    the state directory, and write vitro's config; the other
#                    commands do this themselves when it is missing
#   start            start the VM from the golden image (a fresh disposable
#                    overlay) and wait until it answers and has a desktop
#   stop             shut the VM down and delete its overlay (the same as
#                    destroy: the overlay is the VM)
#   destroy          the same
#   reset            destroy the VM and start a fresh one from the golden image
#   run CMD...       run CMD (PowerShell text; the words are joined with
#                    spaces) in the guest, output as UTF-8, exiting with the
#                    status of the last native program CMD ran
#   cp-in HOST GUEST   copy a file or directory to the guest (a relative GUEST
#                    starts in the account's home)
#   cp-out GUEST HOST  copy one back
#   screenshot [PATH]  the guest's screen as a PNG
#                    (default target/windows-vm/screen-TIME.png)
#   status           the VM, the golden image, the tools
#
# Environment: TEMZDF_WINDOWS_VM (the VM's name, default temzdf-windows),
# TEMZDF_WINDOWS_GOLDEN (the golden image, default the newest
# windows-*.qcow2 in vitro's golden directory), TEMZDF_WINDOWS_STATE (the
# state directory, default ${XDG_STATE_HOME:-~/.local/state}/temzdf/windows-vm).
#
# The state directory holds the nix build links (vitro, OVMF) and a config
# for vitro of this script's own. They live outside the work tree and target/
# so that `cargo clean` or moving a worktree cannot break them. vitro reads
# only ~/.config/vitro/config.toml (or $XDG_CONFIG_HOME/vitro/config.toml);
# the script points XDG_CONFIG_HOME at the state directory, so the user's own
# config is neither read nor changed (the SSH key, the golden directory and
# vitro's VM records stay where they are).
#
# Not wrapped yet: `vitro build` of the golden image (provisioning is for
# later). Known problems of the pinned vitro, and what to do:
#  - x86-64 q35 cannot use vitro's `firmware` setting (its 64 MiB variable
#    store is refused by QEMU). The generated config gives OVMF as a ROM
#    (`-bios`) through extra_args instead.
#  - The unattended install is fixed to en-US: with a Japanese ISO it stops at
#    the language and keyboard pages (send alt-n over QMP), so prefer an
#    English ISO.
#  - `vitro exec` does not pass a native program's exit code (PowerShell 5.1
#    makes it 1): `run` appends `; exit $LASTEXITCODE`.
#  - `build` waits 180 s at most for the guest's shutdown, which "updates are
#    in progress" can exceed: the finished image is then thrown away.
#  - The guest here is Japanese: output is Shift_JIS, so `run` switches the
#    console to UTF-8; its clock is 9 hours off (`-rtc base=localtime` would
#    fix it, not tried); the keyboard is US 101.
# Requires /dev/kvm, QEMU and OpenSSH on PATH (in the dev shell) and nix.
set -euo pipefail
cd "$(dirname "$(dirname "${BASH_SOURCE[0]}")")"

vm=${TEMZDF_WINDOWS_VM:-temzdf-windows}
image=windows
state=${TEMZDF_WINDOWS_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/temzdf/windows-vm}
out=target/windows-vm
# Pinned: vitro 0.6.3-3-ga87071b; OVMF is nixpkgs' (this repository's
# flake.lock decides which).
vitro_rev=a87071b
vitro_flake=github:lambdalisue/vitro/$vitro_rev
ssh_user=dev
ssh_key=${HOME}/.config/vitro/id_ed25519
golden_dir=${XDG_DATA_HOME:-$HOME/.local/share}/vitro/golden
# nix names the link after the output: ovmf-fd.
ovmf=$state/ovmf-fd/FV/OVMF.fd

usage() { sed -n '2,${ /^set -euo/q; p; }' "${BASH_SOURCE[0]}"; exit 2; }
die() { echo "windows-vm.sh: $*" >&2; exit 1; }

golden() {
  local g=${TEMZDF_WINDOWS_GOLDEN:-}
  # shellcheck disable=SC2012
  [[ -n $g ]] || g=$(ls -1t "$golden_dir"/windows-*.qcow2 2>/dev/null | head -n1 || true)
  [[ -n $g && -e $g ]] || die "no golden image (windows-*.qcow2 in $golden_dir; building one is not wrapped yet)"
  echo "$g"
}

tools() {
  mkdir -p "$state/config/vitro"
  # Never `builtins.getFlake` on the repository: nixpkgs comes from this
  # flake's lock through --inputs-from.
  [[ -e $state/vitro/bin/vitro ]] || nix build "$vitro_flake" -o "$state/vitro"
  [[ -e $ovmf ]] || nix build nixpkgs#OVMF.fd --inputs-from . -o "$state/ovmf"
  [[ -e $ovmf ]] || die "$ovmf is not there after building OVMF"
  [[ -e $ssh_key ]] || die "no SSH key $ssh_key (the golden image carries it; vitro keygen makes one for new images)"
  local g
  g=$(golden)
  cat >"$state/config/vitro/config.toml" <<TOML
# Written by scripts/windows-vm.sh; do not edit.
[defaults]
ssh_user = "$ssh_user"
ssh_key = "$ssh_key"

[images.$image]
guest = "windows"
golden = "$g"
extra_args = ["-bios", "$ovmf", "-vga", "none"]
TOML
}

vitro() {
  [[ -e $state/vitro/bin/vitro && -e $ovmf && -e $state/config/vitro/config.toml ]] || tools
  XDG_CONFIG_HOME=$state/config "$state/vitro/bin/vitro" "$@"
}

running() { vitro ls 2>/dev/null | awk -v n="$vm" '$1 == n && $3 == "running" { f = 1 } END { exit !f }'; }

start() {
  mkdir -p "$out"
  if ! running; then
    # A stopped record of the same name would refuse the new VM.
    vitro destroy "$vm" >/dev/null 2>&1 || true
    vitro run --name "$vm" "$image" >>"$out/run.log" 2>&1 || die "vitro run failed (see $out/run.log)"
  fi
  vitro wait --desktop --timeout 180s "$vm"
}

destroy() { vitro destroy "$vm" 2>/dev/null || true; }

# PowerShell text in the guest: UTF-8 output, the native exit code passed on.
run() {
  (($# > 0)) || usage
  start
  vitro exec "$vm" "chcp 65001 > \$null; [Console]::OutputEncoding=[Text.Encoding]::UTF8; $*; exit \$LASTEXITCODE"
}

cmd=${1:-}
[[ -n $cmd ]] || usage
shift
case $cmd in
  tools) tools ;;
  start) start ;;
  stop | destroy) destroy ;;
  reset)
    destroy
    start
    ;;
  run) run "$@" ;;
  cp-in)
    (($# == 2)) || usage
    start
    vitro cp "$1" "$vm:$2"
    ;;
  cp-out)
    (($# == 2)) || usage
    start
    vitro cp "$vm:$1" "$2"
    ;;
  screenshot)
    mkdir -p "$out"
    start
    vitro screenshot "$vm" --out "${1:-$out/screen-$(date +%Y%m%d-%H%M%S).png}"
    ;;
  status)
    echo "state directory: $state"
    echo "vitro: $([[ -e $state/vitro/bin/vitro ]] && echo "$vitro_rev built" || echo "not built (tools)")"
    echo "ovmf: $([[ -e $ovmf ]] && echo built || echo "not built (tools)")"
    echo "golden: $(golden 2>&1 || true)"
    if [[ -e $state/vitro/bin/vitro ]]; then
      echo "vm $vm: $(running && echo running || echo "not running")"
      vitro ls
    fi
    ;;
  *) usage ;;
esac
