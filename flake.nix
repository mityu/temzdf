{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Cargo builds in derivations: dependencies apart from our crates.
    crane.url = "github:ipetkov/crane";
  };

  # `nix build` / `nix run`: the viewer, and the GUI client (temzdf-gui,
  # which finds temzdf next to it). MuPDF and SyncTeX are vendored C
  # sources compiled by the crates' build scripts; tests are left to
  # cargo xtask check because they need TeX to build the fixtures. Built
  # with crane: the dependencies, the script host's guest and its
  # dependencies are derivations of their own, so a change to our crates
  # alone takes them from a cache (CI keeps the store between runs).
  outputs =
    inputs@{
      flake-parts,
      rust-overlay,
      crane,
      ...
    }:
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

          # The fonts temzdf-gui embeds (crates/temzdf-gui/build.rs), from the
          # list in crates/temzdf-gui/fonts.toml (upstream files by URL and
          # sha256, the same ones `cargo xtask fetch-fonts` gets without Nix).
          bundleFonts = map (f: {
            inherit (f) file;
            path = pkgs.fetchurl {
              name = f.file;
              url = pkgs.lib.replaceStrings [ "{tag}" ] [ f.tag ] f.url;
              inherit (f) sha256;
            };
          }) (builtins.fromTOML (builtins.readFile ./crates/temzdf-gui/fonts.toml)).font;
          bundleFontFiles = map (f: "${f.path}") bundleFonts;
          bundleFontPath = pkgs.lib.concatStringsSep ":" bundleFontFiles;
          # The fonts tests see through fontconfig (cargo xtask check sets
          # FONTCONFIG_FILE to it): those fonts alone, every generic family
          # JetBrains Mono first, nothing from the host (its font directories,
          # /etc/fonts), so that text is drawn the same on every machine and
          # in CI. nixpkgs' makeFontsConf adds the host's directories and
          # settings, which made CI and this machine draw differently. No
          # cachedir: scanning six fonts takes milliseconds, and a cache in
          # ~/.cache once went stale and hid Noto Color Emoji.
          testFontConfig =
            let
              dir = pkgs.linkFarm "temzdf-test-fonts" (
                map (f: {
                  name = f.file;
                  inherit (f) path;
                }) bundleFonts
              );
              alias = family: ''
                <alias binding="same">
                  <family>${family}</family>
                  <prefer>
                    <family>JetBrains Mono</family>
                    <family>Noto Color Emoji</family>
                    <family>Symbols Nerd Font Mono</family>
                  </prefer>
                </alias>
              '';
            in
            pkgs.writeText "temzdf-test-fonts.conf" ''
              <?xml version="1.0"?>
              <!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
              <fontconfig>
                <dir>${dir}</dir>
                ${pkgs.lib.concatMapStrings alias [
                  "sans-serif"
                  "serif"
                  "monospace"
                  "system-ui"
                  "cursive"
                  "fantasy"
                ]}
              </fontconfig>
            '';
          # What temzdf-gui's winit and wgpu load with dlopen on Linux (the
          # real GPU's Vulkan driver comes from the system: /run/opengl-driver
          # on NixOS).
          # The GUI client (doc/design/gui-client.md): what winit and wgpu load at
          # run time, a software Vulkan driver (Mesa's lavapipe) for offscreen
          # rendering in tests, and the tools its end-to-end tests drive:
          # an Xfce window manager on Xvfb, headless sway with grim and wtype
          # (Wayland), fcitx5 with Mozc (Japanese input, IME tests).
          # The script host runs as a WebAssembly guest (crates/temzdf-script-guest,
          # built and precompiled by crates/temzdf-script-runner/build.rs): the
          # toolchains carry the wasm32-wasip1 standard library, and QuickJS's C
          # code (rquickjs-sys) is compiled with nixpkgs' WASI clang. That clang
          # is only named in these variables, never put on PATH: there it would
          # replace the native cc.
          wasmTargets = [ "wasm32-wasip1" ];
          wasiCc = pkgs.pkgsCross.wasi32.stdenv.cc;
          wasmEnv = {
            CC_wasm32_wasip1 = "${wasiCc}/bin/${wasiCc.targetPrefix}cc";
            AR_wasm32_wasip1 = "${wasiCc.bintools}/bin/${wasiCc.targetPrefix}ar";
            # rquickjs-sys would otherwise download the WASI SDK.
            RQUICKJS_SYS_NO_WASI_SDK = "1";
          };
          # The shipped scripts (crates/temzdf-script/runtime) are bundled by
          # crates/temzdf-script/build.rs with esbuild; their npm packages
          # come from the lock file, built here so builds need no network.
          # Builds get the dependencies only; the dev shells also the
          # devDependencies (scripts/ts-types.ts bundles declarations).
          runtimeNodeModules =
            dev:
            pkgs.importNpmLock.buildNodeModules {
              npmRoot = ./crates/temzdf-script/runtime;
              nodejs = pkgs.nodejs;
              derivationArgs = lib.optionalAttrs (!dev) { npmInstallFlags = [ "--omit=dev" ]; };
            };
          scriptEnv = dev: {
            TEMZDF_NODE_MODULES = "${runtimeNodeModules dev}/node_modules";
          };
          stableToolchain = pkgs.rust-bin.stable.latest.minimal.override {
            targets = wasmTargets;
            extensions = [
              "clippy"
              "rustfmt"
            ];
          };
          # The dev shells' toolchain also has Windows' standard library, for
          # `cargo clippy --target x86_64-pc-windows-gnu` (the Windows port,
          # built with MSYS2's UCRT64 there, doc/survey/windows-port.md);
          # `nix build` keeps stableToolchain.
          shellToolchain = pkgs.rust-bin.stable.latest.minimal.override {
            targets = wasmTargets ++ [ "x86_64-pc-windows-gnu" ];
            extensions = [
              "clippy"
              "rustfmt"
            ];
          };
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
                  skipTart ? false,
                  # Only what CI runs (cargo xtask check, scripts/miri.sh): no
                  # editor tools, toolchain docs or end-to-end tools, so that
                  # less is downloaded.
                  forCi ? false,
                }:
                pkgs.mkShell (
                  {
                    packages =
                      with pkgs;
                      [
                        shellcheck
                        esbuild
                        biome # Formats and lints the shipped scripts (cargo xtask check).
                        # scripts/ts-types.ts (checked by cargo xtask check), and
                        # npm install --package-lock-only in
                        # crates/temzdf-script/runtime (adding a package).
                        nodejs
                      ]
                      ++ lib.optionals (!forCi) [
                        agent-browser
                        python3
                        imagemagick # Screenshots compared in scripts/e2e.sh (expect).
                      ]
                      ++ (
                        if enableMiri then
                          [
                            (rust-bin.selectLatestNightlyWith (
                              toolchain:
                              # minimal: rustc, cargo and the standard library,
                              # without default's documentation (700 MB unpacked).
                              (if forCi then toolchain.minimal else toolchain.default).override {
                                extensions = [
                                  "rust-src"
                                  "miri"
                                  "clippy"
                                  "rustfmt"
                                ]
                                ++ lib.optionals (!forCi) [ "rust-analyzer" ];
                                targets = wasmTargets;
                              }
                            ))
                          ]
                        else
                          [ shellToolchain ] ++ lib.optionals (!forCi) [ pkgs.rust-analyzer ]
                      )
                      ++ lib.optionals withTeX [
                        (pkgs.texliveBasic.withPackages (
                          ps: with ps; [
                            latexmk
                            amsmath
                            amsfonts
                            hyperref
                            geometry
                            # tests/fixtures/tounicode-controls.tex
                            lm
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

                      ++ lib.optionals (isLinux && !forCi) (
                        with pkgs;
                        [
                          # End-to-end testing: run kitty on a virtual X server and drive it.
                          kitty
                          xvfb-run
                          # Xvfb itself: scripts/e2e/run.ts starts it (-displayfd).
                          xvfb
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
                    buildInputs = [
                      pkgs.libghostty-vt.dev
                    ];

                    # mupdf-sys runs bindgen, which needs libclang.
                    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

                    # Fonts temzdf-gui embeds (see bundleFonts).
                    TEMZDF_BUNDLED_FONTS = bundleFontPath;
                    # The fonts of tests (see testFontConfig).
                    TEMZDF_TEST_FONTCONFIG = testFontConfig;
                  }
                  // wasmEnv
                  // scriptEnv true
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
              # CI (.github/workflows/ci.yml): cargo xtask check with the
              # toolchain of the default shell, and Miri in a job of its own.
              ci = buildDevShell {
                withTeX = true;
                forCi = true;
              };
              # No TeX: Miri runs no test that needs the fixtures (they
              # are ignored under Miri).
              ci-miri = buildDevShell {
                enableMiri = true;
                forCi = true;
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
              craneLib = (crane.mkLib pkgs).overrideToolchain stableToolchain;
              cargoSrc = lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./crates
                  # A workspace member: Cargo reads its manifest even
                  # though the package does not build it.
                  ./xtask
                ];
              };
              # The crates as Cargo.lock names them. crane leaves them in the
              # store, read-only (buildRustPackage copied them first):
              # mupdf-sys copies MuPDF's sources into its OUT_DIR keeping
              # that mode, then patches font.c there, so its copies are made
              # writable.
              cargoVendorDir = craneLib.vendorCargoDeps {
                src = cargoSrc;
                overrideVendorCargoPackage =
                  p: drv:
                  if p.name == "mupdf-sys" then
                    drv.overrideAttrs (_: {
                      postPatch = ''
                        substituteInPlace build.rs --replace-fail \
                          '.map_err(|e| format!("Couldn'"'"'t copy {src_path:?} to {dst_path:?}: {e}"))?;' \
                          '.map_err(|e| format!("Couldn'"'"'t copy {src_path:?} to {dst_path:?}: {e}"))?;
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let mode = fs::metadata(&dst_path)?.permissions().mode();
                                fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode | 0o200))?;
                            }'
                      '';
                    })
                  else
                    drv;
              };
              # What every derivation below shares.
              common = {
                pname = "temzdf";
                version = (lib.importTOML ./Cargo.toml).workspace.package.version;
                src = cargoSrc;
                inherit cargoVendorDir;
                strictDeps = true;
                nativeBuildInputs = with pkgs.buildPackages; [
                  pkg-config
                  clang
                  # mupdf-sys runs bindgen, which needs libclang.
                  rustPlatform.bindgenHook
                  esbuild
                  # Stylo's build script (Blitz, the Markdown viewer's
                  # graphics mode) generates code with Python.
                  python3
                ];
                # libghostty-vt: terminal emulation for terminal panes (temzdf-term).
                buildInputs = [
                  pkgs.fontconfig
                  pkgs.libghostty-vt.dev
                ];
                # Tests are cargo xtask check's: they need TeX, the
                # fixtures, tmux and more than the build's sources.
                doCheck = false;
                # The dependencies only built, not also checked first (a
                # build is all the package needs).
                cargoCheckCommand = "true";
                # bindgenHook copies NIX_CFLAGS_COMPILE, with its
                # -frandom-seed of each derivation, into
                # BINDGEN_EXTRA_CLANG_ARGS, which bindgen's build script
                # watches: without it the package would build bindgen,
                # mupdf-sys and libghostty-vt-sys again.
                preBuild = ''
                  BINDGEN_EXTRA_CLANG_ARGS=$(printf '%s' "$BINDGEN_EXTRA_CLANG_ARGS" | sed 's/-frandom-seed=[^ ]*//g')
                  export BINDGEN_EXTRA_CLANG_ARGS
                '';
                # Environment variables of the build (plain attributes are
                # not exported).
                env =
                  wasmEnv
                  // scriptEnv false
                  // {
                    TEMZDF_BUNDLED_FONTS = bundleFontPath;
                  };
              };

              # The script host's guest (crates/temzdf-script-guest), as
              # crates/temzdf-script-runner/build.rs would build it.
              guestArgs = "--package temzdf-script-guest --target wasm32-wasip1";
              guestCommon = common // {
                pname = "temzdf-script-guest";
                CARGO_PROFILE = "script-guest";
                cargoExtraArgs = guestArgs;
              };
              guestDeps = craneLib.buildDepsOnly guestCommon;
              guest = craneLib.mkCargoDerivation (
                guestCommon
                // {
                  cargoArtifacts = guestDeps;
                  # The .wasm alone, not the build's target directory too.
                  doInstallCargoArtifacts = false;
                  buildPhaseCargoCommand = "cargoWithProfile build ${guestArgs}";
                  installPhaseCommand = ''
                    install -Dm644 target/wasm32-wasip1/script-guest/temzdf_script_guest.wasm \
                      $out/temzdf_script_guest.wasm
                  '';
                }
              );

              packageArgs = "--package temzdf --package temzdf-gui";
              deps = craneLib.buildDepsOnly (common // { cargoExtraArgs = packageArgs; });
              temzdf = craneLib.buildPackage (
                common
                // {
                  cargoArtifacts = deps;
                  cargoExtraArgs = packageArgs;
                  env = common.env // {
                    # Only precompiled by build.rs (no nested cargo).
                    TEMZDF_SCRIPT_GUEST_WASM = "${guest}/temzdf_script_guest.wasm";
                  };
                  # In RUNPATH rather than a wrapper script, so the binary works
                  # alone (copied, or a symlink to it) outside the dev shell;
                  # dlopen searches the RUNPATH of the executable that calls it.
                  postFixup = lib.optionalString isLinux ''
                    patchelf --add-rpath ${lib.makeLibraryPath guiLibs} $out/bin/temzdf-gui
                  '';
                  meta = {
                    description = "Terminal PDF viewer drawn with the kitty graphics protocol";
                    homepage = "https://github.com/mityu/temzdf-cli";
                    license = lib.licenses.agpl3Only;
                    mainProgram = "temzdf";
                  };
                }
              );

              # Windows (x86_64-pc-windows-gnu), cross-built on Linux:
              # `nix build .#windows` gives temzdf.exe and temzdf-gui.exe,
              # which need no DLLs beyond Windows' own. MinGW-w64 with the
              # UCRT, as MSYS2's UCRT64 on Windows (doc/survey/windows-port.md).
              # The script host's guest is the same .wasm as above.
              mingw = pkgs.pkgsCross.ucrt64;
              windowsTarget = "x86_64-pc-windows-gnu";
              craneLibWindows = (crane.mkLib pkgs).overrideToolchain (
                pkgs.rust-bin.stable.latest.minimal.override {
                  targets = wasmTargets ++ [ windowsTarget ];
                }
              );
              windowsCommon = common // {
                pname = "temzdf-windows";
                cargoExtraArgs = "${packageArgs} --target ${windowsTarget}";
                nativeBuildInputs = common.nativeBuildInputs ++ [
                  # x86_64-w64-mingw32-gcc: the linker, and the C compiler of
                  # mupdf-sys, libtexprintf, SyncTeX and the rest.
                  mingw.stdenv.cc
                  # .cargo/config.toml links with lld (the static libghostty-vt).
                  pkgs.buildPackages.lld
                  # libghostty-vt-sys builds libghostty-vt for the target.
                  pkgs.buildPackages.zig_0_15
                ];
                # Nothing of the build machine's (fontconfig, the Linux
                # libghostty-vt) for a Windows build.
                buildInputs = [ ];
                preBuild = common.preBuild + ''
                  # bindgen (mupdf-sys) parses the headers for the target:
                  # MinGW's headers on top of what bindgenHook passes.
                  export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu="$BINDGEN_EXTRA_CLANG_ARGS -isystem ${mingw.windows.mingw_w64_headers}/include"
                '';
                env = common.env // {
                  TEMZDF_SCRIPT_GUEST_WASM = "${guest}/temzdf_script_guest.wasm";
                  # libghostty-vt from the Ghostty commit of nix/libghostty-vt.nix
                  # (the one libghostty-vt-sys was generated from), with its Zig
                  # packages from the store: libghostty-vt-sys runs
                  # `zig build -Dtarget=x86_64-windows-gnu` without network.
                  GHOSTTY_SOURCE_DIR = "${pkgs.libghostty-vt.src}";
                  GHOSTTY_ZIG_SYSTEM_DIR = "${pkgs.libghostty-vt.deps}";
                  # The cc crate would take stdenv's CC (the build machine's).
                  CC_x86_64_pc_windows_gnu = "${mingw.stdenv.cc.targetPrefix}cc";
                  CXX_x86_64_pc_windows_gnu = "${mingw.stdenv.cc.targetPrefix}c++";
                  AR_x86_64_pc_windows_gnu = "${mingw.stdenv.cc.targetPrefix}ar";
                  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc.targetPrefix}cc";
                  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=lld -C link-arg=-no-pie -L native=${mingw.windows.pthreads}/lib";
                };
              };
              windowsDeps = craneLibWindows.buildDepsOnly windowsCommon;
              windows = craneLibWindows.buildPackage (
                windowsCommon
                // {
                  cargoArtifacts = windowsDeps;
                  # The build machine's strip and patchelf know no PE files.
                  dontStrip = true;
                  dontPatchELF = true;
                  postInstall = ''
                    for exe in $out/bin/*.exe; do
                      ${mingw.stdenv.cc.targetPrefix}strip "$exe"
                    done
                  '';
                }
              );
            in
            {
              default = temzdf;
              # The parts, to build or inspect on their own.
              inherit deps guest;
              guest-deps = guestDeps;
            }
            // lib.optionalAttrs isLinux {
              inherit windows;
              windows-deps = windowsDeps;
            };
        };
    };
}
