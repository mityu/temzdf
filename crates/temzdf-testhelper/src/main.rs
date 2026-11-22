//! See the library's documentation for the commands.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = temzdf_testhelper::run(&args) {
        eprintln!("temzdf-testhelper: {e}");
        std::process::exit(2);
    }
}
