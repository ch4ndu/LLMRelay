use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = agenticjira::cli::Cli::parse();
    if let Err(error) = agenticjira::cli::run(cli).await {
        eprintln!("llmrelay: {error:#}");
        std::process::exit(1);
    }
}
