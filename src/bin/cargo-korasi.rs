use clap::Parser;

use korasi_cli::{opt::Opt, run};

// `cargo` invokes this binary as `cargo-korasi korasi <args>`
// so the parser below is defined with that in mind.
#[derive(Parser, Debug)]
#[clap(bin_name = "cargo")]
enum Cli {
    Korasi(Opt),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let Cli::Korasi(opts) = Cli::parse();

    if opts.debug {
        tracing_subscriber::fmt().init();
    }

    run(opts).await
}
