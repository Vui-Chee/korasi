use clap::Parser;

use korasi_cli::{opt::Opt, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opt::parse();

    if opts.debug {
        tracing_subscriber::fmt().init();
    }

    run(opts).await
}
