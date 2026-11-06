#!/usr/bin/env bash
# Build experiments/script-host-bench and measure every form under every
# resource condition (doc/survey/script-host-transport.md). Run it in the
# experiment's shell:
#
#   nix-shell experiments/script-host-bench/shell.nix --run experiments/script-host-bench/run.sh
#
# Conditions use `systemd-run --user --scope` (cgroup v2 with the cpu
# controller delegated; no root): one P-core, one E-core, and one P-core at
# 50% and 25% CPU quota. P_CPU and E_CPU pick the CPUs (defaults fit an
# i7-14700: 0-15 are P-core threads, 16-27 E-cores). KEYS sets the samples.
# Results go to target/script-host-bench/results.csv (and environment.txt).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
out=$root/target/script-host-bench
mkdir -p "$out"
export CARGO_TARGET_DIR=$out/cargo

(cd "$here/guest" && CARGO_TARGET_DIR=$out/guest cargo build --release --target wasm32-wasip1)
(cd "$here" && cargo build --release)
bin=$CARGO_TARGET_DIR/release/script-host-bench
guest=$out/guest/wasm32-wasip1/release/script_host_bench_guest.wasm

p=${P_CPU:-4}
e=${E_CPU:-24}
keys=${KEYS:-2000}
{
  date -Iseconds
  uname -r
  lscpu | grep -E 'Model name|^CPU\(s\)'
  echo "P_CPU=$p E_CPU=$e KEYS=$keys"
} >"$out/environment.txt"

echo "label,form,what,k,n,p50_us,p90_us,p99_us,max_us,mean_us" >"$out/results.csv"
measure() {
  local label=$1
  shift
  "$@" "$bin" run --guest "$guest" --label "$label" --keys "$keys" | tee -a "$out/results.csv"
}
scope() { systemd-run --user --scope --quiet "$@" --; }

measure none
measure p-core scope -p AllowedCPUs="$p"
measure e-core scope -p AllowedCPUs="$e"
measure p-core-50 scope -p AllowedCPUs="$p" -p CPUQuota=50%
measure p-core-25 scope -p AllowedCPUs="$p" -p CPUQuota=25%
echo "results: $out/results.csv"
