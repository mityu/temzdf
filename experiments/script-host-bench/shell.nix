# The tools for experiments/script-host-bench (see doc/survey/script-host-transport.md):
# a Rust toolchain with the wasm32-wasip1 target and a WASI C compiler for
# QuickJS (rquickjs-sys) in the WebAssembly guest.
#
#   nix-shell experiments/script-host-bench/shell.nix --run experiments/script-host-bench/run.sh
#
# Not pinned (the results depend on the machine anyway): the channel's
# nixpkgs and rust-overlay's master. Not builtins.getFlake on the repository
# either: as a path it copies the whole directory into the store, target/
# included (over 100 GB), on every evaluation.
let
  rustOverlay = import (builtins.fetchTarball "https://github.com/oxalica/rust-overlay/archive/master.tar.gz");
  pkgs = import <nixpkgs> { overlays = [ rustOverlay ]; };
  rust = pkgs.rust-bin.stable.latest.minimal.override { targets = [ "wasm32-wasip1" ]; };
  wasiCc = pkgs.pkgsCross.wasi32.stdenv.cc;
in
pkgs.mkShell {
  packages = [ rust ];
  # rquickjs-sys would download the WASI SDK; use nixpkgs' clang and
  # wasi-libc, for the wasm target only (the native cc stays stdenv's).
  RQUICKJS_SYS_NO_WASI_SDK = "1";
  CC_wasm32_wasip1 = "${wasiCc}/bin/${wasiCc.targetPrefix}cc";
  AR_wasm32_wasip1 = "${wasiCc.bintools}/bin/${wasiCc.targetPrefix}ar";
}
