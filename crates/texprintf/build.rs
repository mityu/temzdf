//! Link libtexprintf, found through pkg-config (`nix/libtexprintf.nix`
//! provides it in the dev shell and the package), statically.

fn main() {
    if let Err(e) = pkg_config::Config::new().statik(true).probe("texprintf") {
        panic!("libtexprintf not found through pkg-config ({e}); the Nix dev shell provides it (nix/libtexprintf.nix)");
    }
}
