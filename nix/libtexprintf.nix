# Overlay: libtexprintf (utftex), which draws TeX math as Unicode text for
# the Markdown viewer's text mode (crates/texprintf). Not in nixpkgs.
# GPL-3.0-or-later, which the repository's AGPL-3.0 can combine with.
#
# Built as a static library (no runtime search path to set up) with a
# pkg-config file of our own; upstream installs none.
final: prev: {
  libtexprintf = final.stdenv.mkDerivation (finalAttrs: {
    pname = "libtexprintf";
    version = "1.31";
    src = final.fetchFromGitHub {
      owner = "bartp5";
      repo = "libtexprintf";
      rev = "v${finalAttrs.version}";
      hash = "sha256-OXDcohfSfik0H1MpoznN267OVTYkW75N+TIF6lRRvZ0=";
    };
    nativeBuildInputs = [ final.autoreconfHook ];
    configureFlags = [
      "--enable-static"
      "--disable-shared"
    ];
    postInstall = ''
      mkdir -p $out/lib/pkgconfig
      cat > $out/lib/pkgconfig/texprintf.pc <<PC
      prefix=$out
      libdir=$out/lib
      includedir=$out/include

      Name: texprintf
      Description: TeX math drawn as Unicode text
      Version: ${finalAttrs.version}
      Libs: -L$out/lib -ltexprintf
      Cflags: -I$out/include
      PC
    '';
    meta = {
      description = "printf-like output of TeX math as Unicode text (utftex)";
      homepage = "https://github.com/bartp5/libtexprintf";
      license = final.lib.licenses.gpl3Plus;
    };
  });
}
