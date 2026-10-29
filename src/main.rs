mod cli;
mod config;
mod extractor;
mod index;
mod packer;
mod scanner;
mod skill;

use std::process;

fn main() {
    if let Err(err) = cli::run_from_args(std::env::args_os()) {
        eprintln!("Error: {err:#}");
        process::exit(1);
    }
}
