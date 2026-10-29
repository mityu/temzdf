#!/bin/bash
# Runs inside the VM of scripts/macos-vm.sh (a Cirrus Labs macOS base image:
# user admin with sudo, Homebrew, the Command Line Tools), in its copy of
# the work tree. Idempotent.
#
#   vm-provision.sh setup   Nix with flakes, the dev shell built once,
#                           kitty, Ghostty and WezTerm as apps, no sleep,
#                           screen saver or screen lock, the Dock hidden;
#                           versions in ~/temzdf-vm-versions.txt
#   vm-provision.sh check   whether this session may drive apps and take
#                           screenshots, the display, the tools
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

nix_bin=/nix/var/nix/profiles/default/bin

setup() {
  if [[ ! -x "$nix_bin/nix" ]]; then
    curl -fsSL https://nixos.org/nix/install -o /tmp/nix-install
    sh /tmp/nix-install --daemon --yes
  fi
  if ! grep -q '^experimental-features.*flakes' /etc/nix/nix.conf; then
    echo 'experimental-features = nix-command flakes' | sudo tee -a /etc/nix/nix.conf >/dev/null
    sudo launchctl kickstart -k system/org.nixos.nix-daemon
    sleep 2
  fi
  export PATH="$nix_bin:$PATH"

  # Apps, as scripts/e2e.sh opens them (`open -a`); casks are not pinned, so
  # their versions are written down below. Downloads are quarantined: the
  # first `open` would wait for a click on Gatekeeper's question.
  # WezTerm's stable release is old (2024): its nightly, as on the host.
  # Ghostty 1.3.1 (the cask's release; the host's updates itself) takes a
  # program given with -e through `open` for a file to open and asks
  # whether to run it: its tip, which runs it.
  for old in wezterm ghostty; do
    if brew list --cask "$old" >/dev/null 2>&1; then brew uninstall --cask "$old"; fi
  done
  brew install --cask kitty ghostty@tip wezterm@nightly
  # And LaunchServices may not know them yet (`open -a` finds apps there).
  local lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  for app in kitty Ghostty WezTerm; do
    xattr -dr com.apple.quarantine "/Applications/$app.app" 2>/dev/null || true
    "$lsregister" -f "/Applications/$app.app"
  done

  # A run must never meet a locked screen or a sleeping display.
  sudo pmset -a sleep 0 displaysleep 0 disksleep 0
  defaults -currentHost write com.apple.screensaver idleTime -int 0
  sudo sysadminctl -screenLock off -password admin 2>/dev/null || true
  defaults write com.apple.dock autohide -bool true
  killall Dock 2>/dev/null || true
  # The image opens a Terminal window at login, where scripts/e2e.sh puts
  # its window: closed, and no windows restored at login.
  defaults write com.apple.loginwindow TALLogoutSavesState -bool false
  defaults write com.apple.Terminal NSQuitAlwaysKeepsWindows -bool false
  pkill -x Terminal 2>/dev/null || true

  # The dev shell once, so the first run does not build it.
  nix develop --quiet -c true

  {
    echo "provisioned $(date -u +%Y-%m-%dT%H:%MZ)"
    sw_vers | tr '\n' ' '
    echo
    nix --version
    brew list --cask --versions kitty ghostty@tip wezterm@nightly
  } >~/temzdf-vm-versions.txt
  cat ~/temzdf-vm-versions.txt
}

check() {
  local ok=0
  export PATH="$nix_bin:$PATH"
  nix develop --quiet -c scripts/macos-gui.sh build
  target/macos-gui/termctl permissions || ok=1
  read -r _ _ w h scale <<<"$(target/macos-gui/termctl main)"
  echo "display: ${w}x${h} at scale $scale"
  ((scale == 1)) || { echo "display: not at scale 1"; ok=1; }
  for app in kitty Ghostty WezTerm; do
    [[ -d /Applications/$app.app ]] || { echo "missing: $app.app"; ok=1; }
  done
  for tool in magick kitten wezterm cargo; do
    nix develop --quiet -c sh -c "command -v $tool" >/dev/null || { echo "missing: $tool"; ok=1; }
  done
  ((ok == 0)) && echo "all there"
  return "$ok"
}

case ${1:-} in
  setup) setup ;;
  check) check ;;
  *) sed -n '2,${ /^set -euo/q; p; }' "${BASH_SOURCE[0]}"; exit 2 ;;
esac
