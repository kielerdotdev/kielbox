//! `ssx`: see the `rcr_cli` library for the implementation.

use clap::Parser;

fn main() {
    // clap exits with 2 on usage errors and 0 for --help / --version.
    let cli = rcr_cli::cli::Cli::parse();
    std::process::exit(rcr_cli::run(cli));
}
