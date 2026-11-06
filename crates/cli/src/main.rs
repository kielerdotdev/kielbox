//! `ssx`: see the `cli` library for the implementation.

use clap::Parser;

fn main() {
    // clap exits with 2 on usage errors and 0 for --help / --version.
    let cli = cli::cli::Cli::parse();
    std::process::exit(cli::run(cli));
}
