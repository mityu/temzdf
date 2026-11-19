#!/usr/bin/env bash
# What a build on Ubuntu without Nix needs beyond apt and rustup (and Zig,
# esbuild and npm on PATH), installed under PREFIX:
#
#   - libtexprintf (crates/texprintf), built as a static library with a
#     pkg-config file of our own, as nix/libtexprintf.nix builds it;
#   - the shipped scripts' npm packages (crates/temzdf-script/runtime/),
#     installed outside the repository (a node_modules inside it would be
#     picked up by scripts/check-docs.sh).
#
# Prints the environment the build needs as KEY=VALUE lines on stdout (CI
# appends them to $GITHUB_ENV), and its progress on stderr.
#
# Usage: scripts/deps-without-nix.sh PREFIX
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 PREFIX" >&2
    exit 2
fi
mkdir -p "$1"
prefix=$(cd "$1" && pwd)
root=$(cd "$(dirname "$0")/.." && pwd)

# Keep in step with nix/libtexprintf.nix.
texprintf_version=1.31

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

{
    echo "== libtexprintf $texprintf_version"
    git clone --quiet --depth 1 --branch "v$texprintf_version" \
        https://github.com/bartp5/libtexprintf.git "$work/libtexprintf"
    (
        cd "$work/libtexprintf"
        autoreconf -fi
        ./configure --enable-static --disable-shared --prefix="$prefix"
        make -j"$(nproc)"
        make install
    )
    # The same pkg-config file as nix/libtexprintf.nix writes (upstream
    # installs none); change both together.
    mkdir -p "$prefix/lib/pkgconfig"
    cat >"$prefix/lib/pkgconfig/texprintf.pc" <<PC
prefix=$prefix
libdir=$prefix/lib
includedir=$prefix/include

Name: texprintf
Description: TeX math drawn as Unicode text
Version: $texprintf_version
Libs: -L$prefix/lib -ltexprintf
Cflags: -I$prefix/include
PC

    echo "== npm packages of crates/temzdf-script/runtime"
    mkdir -p "$prefix/runtime-npm"
    cp "$root/crates/temzdf-script/runtime/package.json" \
        "$root/crates/temzdf-script/runtime/package-lock.json" "$prefix/runtime-npm/"
    (cd "$prefix/runtime-npm" && npm ci --no-audit --no-fund)
} >&2

echo "PKG_CONFIG_PATH=$prefix/lib/pkgconfig"
echo "TEMZDF_NODE_MODULES=$prefix/runtime-npm/node_modules"
