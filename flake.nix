{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      overlays = [
        (import ./nix/libghostty-vt.nix)
        (import rust-overlay)
      ];
      forEachSystem = nixpkgs.lib.genAttrs [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      # What temzdf-gui's winit and wgpu load with dlopen on Linux (the
      # real GPU's Vulkan driver comes from the system: /run/opengl-driver
      # on NixOS).
      guiLibsFor =
        pkgs: with pkgs; [
          vulkan-loader
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
          libxfixes
          libxrender
          libxext
          libxinerama
          libxcb
          libxscrnsaver
        ];
    in
    {
      # `nix build` / `nix run`: the viewer, and the GUI client (temzdf-gui,
      # which finds temzdf next to it). MuPDF and SyncTeX are vendored C
      # sources compiled by the crates' build scripts; tests are left to
      # scripts/check.sh because they need TeX to build the fixtures.
      #
      # `nix build .#temzdf-static` (Linux): `temzdf` alone as one static
      # musl binary that runs on any Linux. temzdf-gui cannot be static: it
      # must dlopen the system's Vulkan driver and X11/Wayland libraries,
      # which a static binary cannot; it links nothing but libc already.
      packages = forEachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system overlays; };
          lib = pkgs.lib;
          temzdfFor =
            pkgs: gui:
            pkgs.rustPlatform.buildRustPackage {
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
              ]
              ++ lib.optionals gui [
                "--package"
                "temzdf-gui"
              ];
              nativeBuildInputs = with pkgs.buildPackages; [
                pkg-config
                clang
                # mupdf-sys runs bindgen, which needs libclang (and, for
                # musl, that target's headers: the hook passes them).
                rustPlatform.bindgenHook
              ];
              # In RUNPATH rather than a wrapper script, so the binary works
              # alone (copied, or a symlink to it) outside the dev shell;
              # dlopen searches the RUNPATH of the executable that calls it.
              postFixup = lib.optionalString (gui && pkgs.stdenv.hostPlatform.isLinux) ''
                patchelf --add-rpath ${lib.makeLibraryPath (guiLibsFor pkgs)} $out/bin/temzdf-gui
              '';
              # libghostty-vt: terminal emulation for terminal panes (temzdf-term).
              buildInputs = [
                pkgs.fontconfig
                pkgs.libghostty-vt.dev
              ];
              doCheck = false;
              meta = {
                description = "Terminal PDF viewer drawn with the kitty graphics protocol";
                homepage = "https://github.com/mityu/temzdf-cli";
                license = lib.licenses.agpl3Only;
                mainProgram = "temzdf";
              };
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isStatic {
              # fontconfig (MuPDF's system fonts) and what it needs, from
              # their static archives too.
              PKG_CONFIG_ALL_STATIC = "1";
            };
          temzdf = temzdfFor pkgs true;
        in
        {
          inherit temzdf;
          default = temzdf;
        }
        // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          temzdf-static = temzdfFor pkgs.pkgsStatic false;
        }
      );

      devShells = forEachSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system overlays; };
          lib = pkgs.lib;
          # The toolchain and what scripts/check.sh runs (also in `ci`).
          build = with pkgs; [
            (rust-bin.selectLatestNightlyWith (toolchain: toolchain.default))
            pkg-config
            clang
            fontconfig
            # Tests inside tmux (crates/tuitest, crates/temzdf/tests/tui.rs).
            tmux
            # Type checking the shipped JavaScript configuration against
            # `temzdf print-types` (crates/temzdf/tests/types.rs).
            typescript
          ];
          # The GUI client (doc/gui-client.md): what winit and wgpu load at
          # run time, a software Vulkan driver (Mesa's lavapipe) for offscreen
          # rendering in tests, and the tools its end-to-end tests drive:
          # an Xfce window manager on Xvfb, headless sway with grim and wtype
          # (Wayland), fcitx5 with Mozc (Japanese input, IME tests).
          guiLibs = guiLibsFor pkgs;
          gui = with pkgs; [
            vulkan-tools
            mesa
            xfwm4
            xwininfo
            xprop
            sway
            grim
            wtype
            wlr-randr
            dbus
            (kdePackages.fcitx5-with-addons.override { addons = [ fcitx5-mozc ]; })
          ];
          # Build, test, and drive kitty end to end. Tools that are large or
          # usually installed globally (TeX, other terminals) are in `alldeps`.
          base = {
            packages = with pkgs; [
              (rust-bin.selectLatestNightlyWith (
                toolchain:
                toolchain.default.override {
                  extensions = [
                    "rust-src"
                    "miri"
                    "rust-analyzer"
                    "clippy"
                  ];
                }
              ))
              agent-browser
              # Scratch scripts during development (not needed by the build).
              python3
            ];
            nativeBuildInputs =
              build
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux (
                with pkgs;
                [
                  # End-to-end testing: run kitty on a virtual X server and drive it.
                  kitty
                  xvfb-run
                  xdotool
                  imagemagick
                  xclip
                ]
                ++ gui
              );
            # Found through pkg-config (temzdf-term).
            buildInputs = [ pkgs.libghostty-vt.dev ];
            # mupdf-sys runs bindgen, which needs libclang.
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            # winit and wgpu open these with dlopen. The real GPU's Vulkan
            # driver comes from the system (/run/opengl-driver on NixOS).
            LD_LIBRARY_PATH = lib.makeLibraryPath guiLibs;
            # Tests render offscreen on lavapipe: VK_DRIVER_FILES=$TEMZDF_LAVAPIPE_ICD.
            TEMZDF_LAVAPIPE_ICD = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.uname.processor}.json";
            # The GUI's default font (UDEV Gothic NF), for tests that must not
            # depend on the fonts installed on the host.
            TEMZDF_TEST_FONTS = "${pkgs.udev-gothic-nf}/share/fonts";
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
            nativeBuildInputs = build ++ [ tex ];
            inherit (base) buildInputs LIBCLANG_PATH;
            FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
          };
          # `nix develop .#alldeps`: every tool development of this CLI uses,
          # including TeX for the fixtures and the other terminals and
          # multiplexers that scripts/e2e.sh can drive (tmux is in `base`).
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
