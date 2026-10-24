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
        in
        {
          default = pkgs.mkShell {
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
        }
      );
    };
}
