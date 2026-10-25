# Overlay: nixpkgs' libghostty-vt built from the Ghostty commit the Rust
# bindings (crate libghostty-vt-sys, see crates/temzdf-term/Cargo.toml) were
# generated from. libghostty's C API is not stable yet, so the library and the
# bindings must come from the same commit; nixpkgs packages a newer one.
#
# Only the source, its Zig package set (Ghostty ships it as build.zig.zon.nix)
# and the Zig version it needs differ; the build itself is nixpkgs'. Drop this
# overlay once nixpkgs and the crate agree on a commit.
final: prev:
let
  rev = "a887df42c56f6de86c0fe6da9c4eeca37931e083";
  version = "0.1.0-unstable-2026-07-11";
  src = final.fetchFromGitHub {
    owner = "ghostty-org";
    repo = "ghostty";
    inherit rev;
    hash = "sha256-1Zz65SCk3rkJ9+Q0MmyNOTNiDSLBRIHRd3IvFM4iNXw=";
  };
in
{
  libghostty-vt = prev.libghostty-vt.overrideAttrs (old: {
    inherit version src;
    deps = final.callPackage "${src}/build.zig.zon.nix" {
      name = "libghostty-vt-cache-${version}";
      # Copies, not symlinks, as nixpkgs' package does (Zig computes relative
      # paths lexically; see the note in nixpkgs' libghostty-vt).
      linkFarm =
        name: entries:
        final.runCommand name { } ''
          mkdir -p $out
          ${final.lib.concatMapStringsSep "\n" (e: "cp -rL ${e.path} $out/${e.name}") entries}
        '';
    };
    # This commit builds with Zig 0.15.
    nativeBuildInputs = map (p: if p == final.zig_0_16 then final.zig_0_15 else p) old.nativeBuildInputs;
  });
}
