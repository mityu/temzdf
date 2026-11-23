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
#   build [ISO]      build a new golden image from a Windows 11 ISO (default
#                    $TEMZDF_WINDOWS_ISO; English, x64) with vitro's unattended
#                    install, plus scripts/windows/build-prepare.ps1; the
#                    image lands beside the others in vitro's golden
#                    directory, the older ones are kept
#   provision        start a fresh VM, run scripts/windows/provision.ps1 in
#                    it (Git, Node, Zig, Python, MSYS2's UCRT64 with Rust,
#                    GCC, Clang and the WASI toolchain, Biome, esbuild; no sleep or screen
#                    lock; UTC),
#                    restart it and keep it as the next golden image
#                    (vitro promote); the versions go to
#                    target/windows-vm/provision-versions.txt
#   check [REV] [-- ARGS...]  copy REV (default HEAD; `git archive`, so only
#                    committed files; a one-commit repository, for the
#                    gate's `git diff`) into a fresh VM's C:\t, npm ci, and run
#                    `cargo xtask check --no-miri ARGS` there in UCRT64's
#                    login shell (as the CI's `shell: msys2`: MSYS2's
#                    usr/bin, with make for mupdf-sys, on PATH); the output
#                    goes to target/windows-vm/check.log, exits with its
#                    status
#
# Environment: TEMZDF_WINDOWS_VM (the VM's name, default temzdf-windows),
# TEMZDF_WINDOWS_GOLDEN (the golden image, default the newest
# windows-*.qcow2 in vitro's golden directory), TEMZDF_WINDOWS_STATE (the
# state directory, default ${XDG_STATE_HOME:-~/.local/state}/temzdf/windows-vm),
# TEMZDF_WINDOWS_CPUS (default 4; the memory is 8 GiB).
#
# The state directory holds the nix build links (vitro, OVMF) and a config
# for vitro of this script's own, one per VM name and written again on every
# call (so that TEMZDF_WINDOWS_GOLDEN always applies and two VMs of other
# names do not change each other's). They live outside the work tree and
# target/ so that `cargo clean` or moving a worktree cannot break them. vitro
# reads only ~/.config/vitro/config.toml (or $XDG_CONFIG_HOME/vitro/config.toml);
# the script points XDG_CONFIG_HOME into the state directory, so the user's
# own config is neither read nor changed (the SSH key, the golden directory
# and vitro's VM records stay where they are).
#
# Known problems of the pinned vitro, and what to do:
#  - x86-64 q35 cannot use vitro's `firmware` setting (its 64 MiB variable
#    store is refused by QEMU). The generated config gives OVMF as a ROM
#    (`-bios`) through extra_args instead.
#  - The unattended install is fixed to en-US: with a Japanese ISO it stops at
#    the language and keyboard pages (send alt-n over QMP), so prefer an
#    English ISO.
#  - `vitro exec` does not pass a native program's exit code (PowerShell 5.1
#    makes it 1): `run` appends `; exit $LASTEXITCODE`.
#  - `build` (and `promote`) wait 180 s at most for the guest's shutdown,
#    which "updates are in progress" can exceed: the finished image is then
#    thrown away. scripts/windows/build-prepare.ps1 turns automatic updates
#    off before that shutdown; `build` passes --keep-failed, so a failed
#    build's disk stays in vitro's builds directory for a look.
#  - Windows reads QEMU's clock (UTC) as local time: provision.ps1 sets the
#    time zone to UTC. A Japanese guest writes Shift_JIS, so `run` switches
#    the console to UTF-8.
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
config=$state/config-$vm
configured=
# The ISO for `build` (set there); empty: a config for running the VM.
iso=
virtio_iso=https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso

usage() { sed -n '2,${ /^set -euo/q; p; }' "${BASH_SOURCE[0]}"; exit 2; }
die() { echo "windows-vm.sh: $*" >&2; exit 1; }

golden() {
  local g=${TEMZDF_WINDOWS_GOLDEN:-}
  # shellcheck disable=SC2012
  [[ -n $g ]] || g=$(ls -1t "$golden_dir"/windows-*.qcow2 2>/dev/null | head -n1 || true)
  [[ -n $g && -e $g ]] || die "no golden image (windows-*.qcow2 in $golden_dir; scripts/windows-vm.sh build makes one)"
  echo "$g"
}

tools() {
  mkdir -p "$config/vitro"
  # Never `builtins.getFlake` on the repository: nixpkgs comes from this
  # flake's lock through --inputs-from.
  [[ -e $state/vitro/bin/vitro ]] || nix build "$vitro_flake" -o "$state/vitro"
  [[ -e $ovmf ]] || nix build nixpkgs#OVMF.fd --inputs-from . -o "$state/ovmf"
  [[ -e $ovmf ]] || die "$ovmf is not there after building OVMF"
  [[ -e $ssh_key ]] || die "no SSH key $ssh_key (the golden image carries it; vitro keygen makes one for new images)"
  local g='' build=''
  if [[ -n $iso ]]; then
    # Without `golden`, vitro takes the newest image it built.
    build="
[images.$image.build]
kind = \"unattended-install\"
source = \"$iso\"
virtio_iso = \"$virtio_iso\"
disk_size = \"120G\"
build_timeout = \"90m\"
provision = \"$PWD/scripts/windows/build-prepare.ps1\""
  else
    g="golden = \"$(golden)\""
  fi
  cat >"$config/vitro/config.toml" <<TOML
# Written by scripts/windows-vm.sh on every call; do not edit.
[defaults]
ssh_user = "$ssh_user"
ssh_key = "$ssh_key"

[images.$image]
guest = "windows"
$g
cpus = ${TEMZDF_WINDOWS_CPUS:-4}
memory = "8G"
extra_args = ["-bios", "$ovmf", "-vga", "none"]
$build
TOML
}

vitro() {
  [[ -n $configured ]] || { tools; configured=1; }
  XDG_CONFIG_HOME=$config "$state/vitro/bin/vitro" "$@"
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
  build)
    (($# <= 1)) || usage
    iso=${1:-${TEMZDF_WINDOWS_ISO:-}}
    [[ -n $iso ]] || die "build needs an ISO (an argument or TEMZDF_WINDOWS_ISO)"
    [[ -f $iso ]] || die "no ISO $iso"
    iso=$(realpath "$iso")
    mkdir -p "$out"
    # About 40 minutes. The output goes to the terminal and $out/build.log.
    vitro build --keep-failed "$image" 2>&1 | tee -a "$out/build.log"
    ;;
  provision)
    (($# == 0)) || usage
    mkdir -p "$out"
    # A fresh overlay: what is promoted is the golden image plus the script.
    destroy
    start
    vitro exec --script scripts/windows/provision.ps1 "$vm" 2>&1 | tee "$out/provision.log"
    # Installers can leave a reboot pending, and promote waits
    # 180 s at most for the shutdown: get it over with first.
    vitro restart --desktop --timeout 15m "$vm"
    # PATH as a new logon sees it.
    vitro exec "$vm" "chcp 65001 > \$null; [Console]::OutputEncoding=[Text.Encoding]::UTF8; \
      git --version; node --version; npm --version; zig version; python --version; biome --version; esbuild --version; \
      C:/msys64/usr/bin/pacman.exe -Q mingw-w64-ucrt-x86_64-rust mingw-w64-ucrt-x86_64-rust-wasm mingw-w64-ucrt-x86_64-gcc \
        mingw-w64-ucrt-x86_64-clang mingw-w64-ucrt-x86_64-pkgconf make \
        mingw-w64-ucrt-x86_64-wasi-libc mingw-w64-ucrt-x86_64-wasi-compiler-rt mingw-w64-ucrt-x86_64-lld mingw-w64-ucrt-x86_64-llvm; \
      rustc -vV; cargo -vV | Select-Object -First 1; gcc --version | Select-Object -First 1; clang --version | Select-Object -First 1; \
      Get-ChildItem \"\$(rustc --print sysroot)/lib/rustlib\" -Directory | ForEach-Object Name; \
      Get-Command cargo, rustc, gcc, clang, python, git -ErrorAction SilentlyContinue | ForEach-Object Source; \
      if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) { 'no rustup' }; \
      \"LIBCLANG_PATH=\$env:LIBCLANG_PATH\"; \"WASI_SDK=\$env:WASI_SDK\"; \
      Get-TimeZone | Select-Object -ExpandProperty Id; Get-Date -Format o" 2>&1 | tee "$out/provision-versions.txt"
    vitro promote "$vm"
    ;;
  check)
    rev=HEAD
    if (($# > 0)) && [[ $1 != -- ]]; then rev=$1; shift; fi
    [[ ${1:-} != -- ]] || shift
    sha=$(git rev-parse --verify "$rev^{commit}")
    mkdir -p "$out"
    git archive --format=tar -o "$out/tree.tar" "$sha"
    destroy
    start
    vitro cp "$out/tree.tar" "$vm:tree.tar"
    echo "windows-vm.sh: checking $sha in the guest's C:/t" | tee "$out/check.log"
    set +e
    run "Remove-Item -Recurse -Force C:/t -ErrorAction SilentlyContinue; New-Item -ItemType Directory C:/t | Out-Null; \
      tar -xf tree.tar -C C:/t; Set-Location C:/t; \
      git init -q; git add -A; git -c user.name=vm -c user.email=vm@localhost commit -q -m $sha; \
      npm ci --prefix crates/temzdf-script/runtime --no-audit --no-fund; \
      if (\$LASTEXITCODE -ne 0) { exit \$LASTEXITCODE }; \
      \$env:MSYSTEM='UCRT64'; \$env:MSYS2_PATH_TYPE='inherit'; \$env:CHERE_INVOKING='1'; \
      C:/msys64/usr/bin/bash.exe -lc 'cargo xtask check --stream --no-miri $*'" 2>&1 | tee -a "$out/check.log"
    status=${PIPESTATUS[0]}
    set -e
    # The gate's own logs (target/check/ in the guest).
    vitro cp "$vm:C:/t/target/check" "$out/check" >/dev/null 2>&1 || true
    exit "$status"
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
