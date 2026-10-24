{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      forEachSystem = nixpkgs.lib.genAttrs [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      devShells = forEachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;
          # Build, test, and drive kitty end to end. Tools that are large or
          # usually installed globally (TeX, other terminals) are in `alldeps`.
          base = {
            nativeBuildInputs =
              with pkgs;
              [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
                pkg-config
                clang
                fontconfig
                # Scratch scripts during development (not needed by the build).
                python3
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                # End-to-end testing: run kitty on a virtual X server and drive it.
                kitty
                xvfb-run
                xdotool
                imagemagick
                xclip
              ];
            # mupdf-sys runs bindgen, which needs libclang.
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          };
          # Just enough LaTeX to build the test fixtures (tests/fixtures/*.tex).
          tex = pkgs.texliveBasic.withPackages (
            ps: with ps; [
              latexmk
              amsmath
              amsfonts
              hyperref
              geometry
            ]
          );
        in
        {
          # `nix develop`: everything except the large, usually global tools.
          default = pkgs.mkShell base;
          # `nix develop .#alldeps`: every tool development of this CLI uses,
          # including TeX for the fixtures and the other terminals and the
          # multiplexer that scripts/e2e.sh can drive.
          alldeps = pkgs.mkShell (
            base
            // {
              nativeBuildInputs =
                base.nativeBuildInputs
                ++ [ tex ]
                ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux (
                  with pkgs;
                  [
                    ghostty
                    wezterm
                    zellij
                  ]
                );
            }
          );
        }
      );
    };
}
