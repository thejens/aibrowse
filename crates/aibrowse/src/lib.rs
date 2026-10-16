//! aibrowse: the aibrowse daemon. By default it serves MCP over stdio, so an agent host
//! (e.g. Claude Code) launches it as a local MCP server. `aibrowse serve --http ADDR`
//! serves the same tools over streamable HTTP instead, for agents on other machines.
//!
//! Developer subcommands:
//!   aibrowse reload                                    MCP front that hot-reloads new builds
//!   aibrowse observe <url> [--full] [--headless]       print the Page Model for a URL
//!   aibrowse fixture <url> <name> [--headless]         record raw CDP inputs to fixtures/
//!     (both also take --profile NAME; default "dev")
//!   aibrowse eval navigator [--sites TEXT] [--goals a,b] run the A4 navigator eval
//!   aibrowse eval detectors [--headful] [--local]      run the A2 bot-detector harness
//!   aibrowse trace [dir] [--no-open]                   HTML viewer for a session trace
//!   aibrowse persona show|fit [--profile NAME]         recorded kinematics; fit the persona

mod announce;
mod api_docs;
mod changes;
mod confirm;
mod downloads;
mod eval;
mod eval_detectors;
mod guide;
mod handoff_tools;
mod hibernate;
mod hooks;
mod instructions;
mod marks;
mod network_tools;
mod persona;
mod private_fs;
mod reload;
mod remote;
mod respond;
mod search_tools;
mod server;
mod session;
mod site_tools;
mod survey_tools;
mod tools;
mod trace;
mod uploads;
mod view_tools;
mod watch;

pub use hooks::{HookFuture, ProfileStore, set_profile_store};

use aibrowse_browser::{Browser, LaunchOptions};
use aibrowse_perception::{Detail, capture, fuse};
use anyhow::{Context, bail};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

/// Sends diagnostics to stderr (stdout carries the MCP protocol), filtered by
/// `AIBROWSE_LOG`.
pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("AIBROWSE_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

/// Runs the command `args` (without the program name) names: serving MCP by default.
/// A build with hooks (hooks.rs) registers them before calling this.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        None | Some("mcp") => serve().await,
        Some("--version" | "version") => {
            println!("aibrowse {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("reload") => serve_reloading().await,
        Some("serve") => remote::run(&args[1..]).await,
        Some("observe") => observe(&args[1..]).await,
        Some("fixture") => fixture(&args[1..]).await,
        Some("eval") => eval::run(&args[1..]).await,
        Some("trace") => trace::viewer::run(&args[1..]),
        Some("persona") => persona::run(&args[1..]),
        Some("search") => search_tools::run(&args[1..]).await,
        Some("survey") => survey_tools::run(&args[1..]).await,
        Some(other) => {
            bail!(
                "unknown command {other:?}; expected mcp, serve, reload, observe, fixture, eval, trace, persona, search or survey"
            )
        }
    }
}

async fn serve() -> anyhow::Result<()> {
    let handler = server::Aibrowse::new();
    handler.watch_idle();
    // Under the reload front the watch link is fixed; serve it from the start so an open
    // watch page reconnects as soon as this worker is up.
    if std::env::var_os("AIBROWSE_WATCH_TOKEN").is_some()
        && let Err(err) = handler.handoff_server().await
    {
        tracing::warn!(%err, "could not start the live view server");
    }
    let service = handler
        .clone()
        .serve(rmcp::transport::stdio())
        .await
        .context("starting MCP server")?;
    // The client going away (stdin closed) and being told to stop (signals) both end
    // the session the same way: close Chrome cleanly. Exiting without that would leave
    // Chrome running with the profile locked.
    let signalled = tokio::select! {
        result = service.waiting() => { result?; false }
        () = termination() => true
    };
    handler.shutdown().await;
    if signalled {
        // The stdio transport's reader thread is blocked on stdin and would keep the
        // runtime from shutting down; the session is closed, so leave now.
        std::process::exit(0);
    }
    Ok(())
}

/// A request to stop: SIGTERM, SIGINT or SIGHUP on macOS and Linux; Ctrl+C, Ctrl+Break
/// or the console closing on Windows.
async fn termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
            _ = hup.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
        let (Ok(mut c), Ok(mut brk), Ok(mut close)) = (ctrl_c(), ctrl_break(), ctrl_close()) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = c.recv() => {}
            _ = brk.recv() => {}
            _ = close.recv() => {}
        }
    }
}

async fn serve_reloading() -> anyhow::Result<()> {
    let service = reload::Reloader::start()
        .await
        .context("starting aibrowse worker")?
        .serve(rmcp::transport::stdio())
        .await
        .context("starting MCP server")?;
    service.waiting().await?;
    Ok(())
}

struct DevArgs {
    positional: Vec<String>,
    profile: String,
    full: bool,
    headless: bool,
}

fn parse_dev_args(args: &[String]) -> anyhow::Result<DevArgs> {
    let mut parsed = DevArgs {
        positional: Vec::new(),
        profile: "dev".into(),
        full: false,
        headless: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--full" => parsed.full = true,
            "--headless" => parsed.headless = true,
            "--profile" => parsed.profile = iter.next().context("--profile needs a value")?.clone(),
            _ => parsed.positional.push(arg.clone()),
        }
    }
    Ok(parsed)
}

async fn observe(args: &[String]) -> anyhow::Result<()> {
    let args = parse_dev_args(args)?;
    let [url] = args.positional.as_slice() else {
        bail!("usage: aibrowse observe <url> [--full] [--profile NAME] [--headless]")
    };
    let browser = Browser::launch(LaunchOptions {
        profile: args.profile,
        headless: args.headless,
        ..LaunchOptions::default()
    })
    .await?;
    let mut tab = browser.open_tab().await?;
    tab.navigate(url).await?;
    let detail = if args.full {
        Detail::Full
    } else {
        Detail::Interactive
    };
    let model = aibrowse_perception::observe(&mut tab, detail, None).await?;
    println!("{}", model.render());
    browser.close().await?;
    Ok(())
}

async fn fixture(args: &[String]) -> anyhow::Result<()> {
    let args = parse_dev_args(args)?;
    let [url, name] = args.positional.as_slice() else {
        bail!("usage: aibrowse fixture <url> <name> [--profile NAME] [--headless]")
    };
    let browser = Browser::launch(LaunchOptions {
        profile: args.profile,
        headless: args.headless,
        ..LaunchOptions::default()
    })
    .await?;
    let mut tab = browser.open_tab().await?;
    tab.navigate(url).await?;
    let raw = capture(&mut tab).await?;
    let path = std::path::Path::new("fixtures/pages").join(format!("{name}.json"));
    std::fs::create_dir_all(path.parent().expect("fixture path has a parent"))?;
    std::fs::write(&path, serde_json::to_vec(&raw)?)?;
    let model = fuse(&raw, Detail::Interactive, None);
    eprintln!(
        "wrote {} ({} frames, {} AX nodes, {} model nodes)",
        path.display(),
        raw.frames.len(),
        raw.frames.iter().map(|f| f.ax.len()).sum::<usize>(),
        model.nodes.len()
    );
    browser.close().await?;
    Ok(())
}
