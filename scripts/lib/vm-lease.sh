# shellcheck shell=bash
# Sourced by the scripts that drive a development VM (scripts/windows-vm.sh,
# scripts/macos-vm.sh): a hand-held lock, one per host, so that two sessions
# (two worktrees) never use the VMs at once.
#
#   source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/vm-lease.sh"
#   lease_require              # refuse (exit 3) unless this holder has the lock
#   lease_cli lock|unlock|hold ARGS...   # the commands the scripts offer
#
# Like pacman's db.lck, the lock has no expiry and is never released by
# itself (except by `hold`, which releases what it took). A leftover lock is
# removed by a person with `unlock --force`.
#
# The lock is one file, ${TEMZDF_VM_LOCK:-${XDG_STATE_HOME:-~/.local/state}/temzdf/vm.lock}
# (TEMZDF_VM_LOCK is for tests), of key=value lines: id, name, purpose, since,
# script. It is written to a temporary file beside it and `ln`ed into place,
# which is atomic, fails when the lock exists and shows the whole content the
# moment it appears. Release is `rm`.
#
# The holder is the worktree: the first 16 hex digits of the SHA-256 of its
# top directory (physical path), or of $TEMZDF_VM_HOLDER when that is set.
# The path itself is never written. The name (--as) is only for display.
#
# Exit status 3: the lock is held by another holder (or, for lease_require,
# nobody holds it).

lease_file=${TEMZDF_VM_LOCK:-${XDG_STATE_HOME:-$HOME/.local/state}/temzdf/vm.lock}
lease_script=scripts/$(basename "$0")

lease_die() { echo "$(basename "$0"): $*" >&2; }

# This holder's id.
lease_id() {
  local top
  if [[ -n ${TEMZDF_VM_HOLDER:-} ]]; then
    top=$TEMZDF_VM_HOLDER
  else
    top=$(cd "$(git rev-parse --show-toplevel)" && pwd -P) || return 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    printf %s "$top" | sha256sum | cut -c1-16
  else
    printf %s "$top" | shasum -a 256 | cut -c1-16
  fi
}

# A field of the lock ("" when there is no lock or no such field).
lease_field() { sed -n "s/^$1=//p" "$lease_file" 2>/dev/null | head -n1 || true; }

# Whether this holder holds the lock.
lease_mine() { [[ -e $lease_file && $(lease_field id) == "$(lease_id)" ]]; }

# Who holds it, and how to release it (stderr).
lease_who() {
  {
    echo "$(basename "$0"): the VM lock is held by $(lease_field name): $(lease_field purpose) (since $(lease_field since), taken by $(lease_field script))"
    echo "  the holder releases it with: $lease_script unlock"
    echo "  a leftover one is removed with: $lease_script unlock --force"
  } >&2
}

# lease_try NAME PURPOSE: take the lock if it is free; 0 taken, 1 not.
lease_try() {
  local tmp rc=0
  mkdir -p "$(dirname "$lease_file")" || return 2
  tmp=$(mktemp "$lease_file.XXXXXX") || return 2
  {
    echo "id=$(lease_id)"
    echo "name=${1//$'\n'/ }"
    echo "purpose=${2//$'\n'/ }"
    echo "since=$(date +%Y-%m-%dT%H:%M:%S%z)"
    echo "script=$lease_script"
  } >"$tmp"
  ln "$tmp" "$lease_file" 2>/dev/null || rc=1
  rm -f "$tmp"
  return "$rc"
}

# lease_lock NAME PURPOSE WAIT: WAIT non-empty waits until the lock is free.
lease_lock() {
  local told='' gone=0 rc
  while :; do
    lease_mine && return 0
    rc=0
    lease_try "$1" "$2" || rc=$?
    ((rc == 0)) && return 0
    ((rc == 2)) && { lease_die "cannot write $lease_file"; return 1; }
    if [[ ! -e $lease_file ]]; then
      # Released between the two steps (on Windows a removed file can keep
      # its name for a moment while another process still has it open), or
      # ln failed for another reason: try again for a couple of seconds.
      ((++gone < 20)) && { sleep 0.1; continue; }
      lease_die "cannot create $lease_file"
      return 1
    fi
    gone=0
    [[ -n $3 ]] || { lease_who; return 3; }
    [[ -n $told ]] || { lease_who; echo "$(basename "$0"): waiting for it" >&2; told=1; }
    sleep "${TEMZDF_VM_LOCK_POLL:-10}"
  done
}

# lease_unlock FORCE: FORCE non-empty removes another holder's lock too.
lease_unlock() {
  [[ -e $lease_file ]] || return 0
  if [[ -z $1 ]] && ! lease_mine; then
    lease_who
    echo "$(basename "$0"): not yours; not removed" >&2
    return 3
  fi
  rm -f "$lease_file"
}

# lease_hold NAME PURPOSE WAIT -- CMD...: CMD under the lock; the lock is
# released afterwards (also on INT and TERM) unless it was held already.
lease_hold() {
  local name=$1 purpose=$2 wait=$3
  shift 4
  if lease_mine; then
    "$@"
    return
  fi
  (
    lease_lock "$name" "$purpose" "$wait" || exit $?
    trap 'lease_unlock ""' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    # Not exec: the trap is what releases the lock.
    "$@"
  )
}

# Refuse unless this holder holds the lock.
lease_require() {
  if [[ ! -e $lease_file ]]; then
    {
      echo "$(basename "$0"): the VM lock is not held: take one first with"
      echo "  $lease_script lock --as NAME --why TEXT"
      echo "or run the command under it with"
      echo "  $lease_script hold --as NAME --why TEXT -- CMD"
    } >&2
    exit 3
  fi
  lease_mine || { lease_who; exit 3; }
}

# One line: who holds the lock.
lease_status() {
  if [[ ! -e $lease_file ]]; then
    echo "lock: free"
    return 0
  fi
  echo "lock: held by $(lease_field name): $(lease_field purpose) (since $(lease_field since))$(lease_mine && echo ' (you)')"
}

# lease_cli lock|unlock|hold ARGS...: the arguments of the three commands.
lease_cli() {
  local sub=$1 as='' why='' wait='' force=''
  shift
  while (($# > 0)); do
    case $1 in
      --as) (($# >= 2)) || { lease_die "--as needs a value"; return 2; }; as=$2; shift ;;
      --why) (($# >= 2)) || { lease_die "--why needs a value"; return 2; }; why=$2; shift ;;
      --wait) wait=1 ;;
      --force) force=1 ;;
      --) shift; break ;;
      *) lease_die "$sub: unknown argument $1"; return 2 ;;
    esac
    shift
  done
  case $sub in
    lock)
      (($# == 0)) && [[ -n $as && -n $why ]] || { lease_die "usage: lock --as NAME --why TEXT [--wait]"; return 2; }
      lease_lock "$as" "$why" "$wait"
      ;;
    unlock)
      (($# == 0)) || { lease_die "usage: unlock [--force]"; return 2; }
      lease_unlock "$force"
      ;;
    hold)
      (($# > 0)) && [[ -n $as && -n $why ]] || { lease_die "usage: hold --as NAME --why TEXT [--wait] -- CMD..."; return 2; }
      lease_hold "$as" "$why" "$wait" -- "$@"
      ;;
    *) lease_die "unknown lock command $sub"; return 2 ;;
  esac
}
