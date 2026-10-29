{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # `nix build` / `nix run`: the viewer, and the GUI client (temzdf-gui,
  # which finds temzdf next to it). MuPDF and SyncTeX are vendored C
  # sources compiled by the crates' build scripts; tests are left to
  # scripts/check.sh because they need TeX to build the fixtures.
  outputs =
    inputs@{ flake-parts, rust-overlay, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      perSystem =
        {
          pkgs,
          lib,
          system,
          ...
        }:
        let
          inherit (pkgs.stdenv.hostPlatform) isLinux isDarwin;

          # The fonts temzdf-gui embeds (crates/temzdf-gui/build.rs), as WezTerm
          # bundles them: JetBrains Mono, the default and first fallback, then
          # Noto Color Emoji and the Nerd Font symbols.
          bundleFontPath =
            let
              jb = style: "${pkgs.jetbrains-mono}/share/fonts/truetype/JetBrainsMono-${style}.ttf";
            in
            pkgs.lib.concatStringsSep ":" (
              map jb [
                "Regular"
                "Bold"
                "Italic"
                "BoldItalic"
              ]
              ++ [
                "${pkgs.noto-fonts-color-emoji}/share/fonts/noto/NotoColorEmoji.ttf"
                "${pkgs.nerd-fonts.symbols-only}/share/fonts/truetype/NerdFonts/Symbols/SymbolsNerdFontMono-Regular.ttf"
              ]
            );
          # What temzdf-gui's winit and wgpu load with dlopen on Linux (the
          # real GPU's Vulkan driver comes from the system: /run/opengl-driver
          # on NixOS).
          # The GUI client (doc/gui-client.md): what winit and wgpu load at
          # run time, a software Vulkan driver (Mesa's lavapipe) for offscreen
          # rendering in tests, and the tools its end-to-end tests drive:
          # an Xfce window manager on Xvfb, headless sway with grim and wtype
          # (Wayland), fcitx5 with Mozc (Japanese input, IME tests).
          guiLibs = with pkgs; [
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
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [
              (import ./nix/libghostty-vt.nix)
              (import rust-overlay)
            ];
            config.allowUnfreePredicate = pkg: builtins.elem (lib.getName pkg) [ "tart" ];
          };

          devShells =
            let
              buildDevShell =
                {
                  withTeX ? false,
                  withTerminals ? false,
                  enableMiri ? false,
                  enableFixedFontSet ? false,
                  skipTart ? false,
                }:
                pkgs.mkShell (
                  {
                    packages =
                      with pkgs;
                      [
                        agent-browser
                        python3
                        imagemagick # Screenshots compared in scripts/e2e.sh (expect).
                        shellcheck
                      ]
                      ++ (
                        if enableMiri then
                          [
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
                          ]
                        else
                          with pkgs;
                          [
                            cargo
                            rustc
                            rust-analyzer
                            clippy
                            rustfmt
                          ]
                      )
                      ++ lib.optionals withTeX [
                        (pkgs.texliveBasic.withPackages (
                          ps: with ps; [
                            latexmk
                            amsmath
                            amsfonts
                            hyperref
                            geometry
                          ]
                        ))
                      ]
                      ++ lib.optionals withTerminals (
                        with pkgs;
                        [
                          ghostty
                          wezterm
                          zellij
                        ]
                      )

                      ++ lib.optionals isLinux (
                        with pkgs;
                        [
                          # End-to-end testing: run kitty on a virtual X server and drive it.
                          kitty
                          xvfb-run
                          xdotool
                          xclip

                          vulkan-tools
                          mesa
                          xfwm4
                          xwininfo
                          xprop
                          sway
                          grim
                          wtype
                          # The clipboard on Wayland (scripts/e2e.sh -d wayland).
                          wl-clipboard
                          wlr-randr
                          dbus
                          (kdePackages.fcitx5-with-addons.override { addons = [ fcitx5-mozc ]; })
                        ]
                      )
                      ++ lib.optionals (isDarwin && !skipTart) [ pkgs.tart ];

                    nativeBuildInputs = with pkgs; [
                      pkg-config
                      clang
                      fontconfig
                      # Tests inside tmux (crates/tuitest, crates/temzdf/tests/tui.rs).
                      tmux
                      # Type checking the shipped JavaScript configuration against
                      # `temzdf print-types` (crates/temzdf/tests/types.rs).
                      typescript
                    ];

                    # Found through pkg-config (temzdf-term).
                    buildInputs = [ pkgs.libghostty-vt.dev ];

                    # mupdf-sys runs bindgen, which needs libclang.
                    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

                    # Fonts temzdf-gui embeds (see bundledFontsFor).
                    TEMZDF_BUNDLED_FONTS = bundleFontPath;
                  }
                  // lib.optionalAttrs enableFixedFontSet {
                    # `nix develop .#ci`: what scripts/check.sh needs on a bare machine
                    # (.github/workflows/ci.yml): the toolchain, TeX for the fixtures, and
                    # a fixed font set instead of whatever the host has installed.
                    FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
                  }
                  // lib.optionalAttrs isLinux {
                    # winit and wgpu open these with dlopen. The real GPU's Vulkan
                    # driver comes from the system (/run/opengl-driver on NixOS).
                    LD_LIBRARY_PATH = lib.makeLibraryPath guiLibs;
                    # Tests render offscreen on lavapipe: VK_DRIVER_FILES=$TEMZDF_LAVAPIPE_ICD.
                    TEMZDF_LAVAPIPE_ICD = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.uname.processor}.json";
                    # The GUI's default font (UDEV Gothic NF), for tests that must not
                    # depend on the fonts installed on the host.
                    TEMZDF_TEST_FONTS = "${pkgs.udev-gothic-nf}/share/fonts";
                  }
                );
            in
            {
              default = buildDevShell { };
              enable-miri = buildDevShell {
                withTeX = true;
                enableMiri = true;
              };
              ci = buildDevShell {
                withTeX = true;
                enableMiri = true;
                enableFixedFontSet = true;
              };
              alldeps = buildDevShell {
                withTeX = true;
                withTerminals = true;
                enableMiri = true;
              };
            }
            // lib.optionalAttrs isDarwin {
              no-vm = buildDevShell { skipTart = true; };
            };
          packages =
            let
              buildPackage =
                {
                  withGui ? false,
                  enableCheck ? false,
                }:
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
                  ++ lib.optionals withGui [
                    "--package"
                    "temzdf-gui"
                  ];
                  nativeBuildInputs = with pkgs.buildPackages; [
                    pkg-config
                    clang
                    # mupdf-sys runs bindgen, which needs libclang.
                    rustPlatform.bindgenHook
                  ];
                  # In RUNPATH rather than a wrapper script, so the binary works
                  # alone (copied, or a symlink to it) outside the dev shell;
                  # dlopen searches the RUNPATH of the executable that calls it.
                  postFixup = lib.optionalString (withGui && isLinux) ''
                    patchelf --add-rpath ${lib.makeLibraryPath guiLibs} $out/bin/temzdf-gui
                  '';
                  # libghostty-vt: terminal emulation for terminal panes (temzdf-term).
                  buildInputs = [
                    pkgs.fontconfig
                    pkgs.libghostty-vt.dev
                  ];
                  doCheck = enableCheck;
                  meta = {
                    description = "Terminal PDF viewer drawn with the kitty graphics protocol";
                    homepage = "https://github.com/mityu/temzdf-cli";
                    license = lib.licenses.agpl3Only;
                    mainProgram = "temzdf";
                  };
                  # Environment variables of the build (plain attributes are
                  # not exported).
                  env = lib.optionalAttrs withGui {
                    TEMZDF_BUNDLED_FONTS = bundleFontPath;
                  };
                };
            in
            {
              default = buildPackage { withGui = true; };
              with-check = buildPackage {
                withGui = true;
                enableCheck = true;
              };
            };
        };
    };
}
