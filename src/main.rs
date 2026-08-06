//! librespeed-cli — test your Internet speed with LibreSpeed.

mod cli;
mod defs;
mod helper;
mod http;
mod output;
mod ping;
mod report;
mod speedtest;
mod spinner;
mod util;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Err(e) = speedtest::run(&cli).await {
        // `{:#}` renders the whole error chain, so the underlying cause
        // (DNS failure, refused connection, ...) is visible.
        write_error!("{e:#}\n");
        output::fatal("Terminated due to error");
    }
}
