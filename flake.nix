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
      # `nix build` / `nix run`: the viewer. MuPDF and SyncTeX are vendored C
      # sources compiled by the crates' build scripts; tests are left to
      # scripts/check.sh because they need TeX to build the fixtures.
      packages = forEachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;
          temzdf = pkgs.rustPlatform.buildRustPackage {
            pname = "temzdf";
            version = (lib.importTOML ./Cargo.toml).workspace.package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--package"
              "temzdf"
            ];
            nativeBuildInputs = with pkgs; [
              pkg-config
              clang
            ];
            buildInputs = [ pkgs.fontconfig ];
            # mupdf-sys runs bindgen, which needs libclang.
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            doCheck = false;
            meta = {
              description = "Terminal PDF viewer drawn with the kitty graphics protocol";
              homepage = "https://github.com/mityu/temzdf-cli";
              license = lib.licenses.agpl3Only;
              mainProgram = "temzdf";
            };
          };
        in
        {
          inherit temzdf;
          default = temzdf;
        }
      );

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
          # `nix develop .#ci`: what scripts/check.sh needs on a bare machine
          # (.github/workflows/ci.yml): the toolchain, TeX for the fixtures, and
          # a fixed font set instead of whatever the host has installed.
          ci = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              pkg-config
              clang
              fontconfig
              tex
            ];
            inherit (base) LIBCLANG_PATH;
            FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
          };
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
                    tmux
                  ]
                );
            }
          );
        }
      );
    };
}
