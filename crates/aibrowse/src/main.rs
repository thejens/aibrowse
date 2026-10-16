//! The aibrowse binary: the server and its commands (lib.rs), without hooks.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aibrowse::init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    aibrowse::run(&args).await
}
