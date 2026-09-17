//! Command-line entry point for hamsy-proxy: a local MITM HTTP(S) debugging
//! proxy. Wires together `hamsy-core`, `hamsy-proxy`, and `hamsy-api`
//! into a runnable binary (`hamsy run`), plus `cert`/`rules`/`proxy`
//! management subcommands that operate directly on hamsy-proxy's on-disk state
//! (no IPC with a running `hamsy run` process).

mod cert;
mod har_open;
mod hooks;
mod mcp;
mod rules;
mod run;
mod shutdown;
mod sysproxy_cmd;
mod update;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use cert::CertCommand;
use rules::RulesCommand;
use sysproxy_cmd::ProxyCommand;

/// hamsy-proxy: a local MITM HTTP(S) debugging proxy.
///
/// Running with no subcommand is equivalent to `hamsy run`.
#[derive(Parser, Debug)]
#[command(name = "hamsy", version, about = "Local MITM HTTP(S) debugging proxy", long_about = None)]
struct Cli {
    /// Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info).
    /// Overridden by `RUST_LOG` when set.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(flatten)]
    run_args: run::RunArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Run the proxy and web UI (the default when no subcommand is given).
    Run(run::RunArgs),
    /// Serve the bundled MCP beta over stdin/stdout.
    Mcp(mcp::McpArgs),
    /// Discover and call agent tools using JSON without an MCP client.
    Agent(mcp::AgentArgs),
    /// Print the agent guide embedded in this release.
    AgentGuide,
    /// Open HAR files in a browser viewer without starting capture.
    Open(har_open::OpenArgs),
    #[command(hide = true, name = "har-viewer-serve")]
    HarViewerServe(har_open::ServeArgs),
    /// Manage the MITM root certificate authority.
    Cert {
        #[command(subcommand)]
        command: CertCommand,
    },
    /// Manage capture/rewrite rules.
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
    },
    /// Control the OS system HTTP/HTTPS proxy.
    Proxy {
        #[command(subcommand)]
        command: ProxyCommand,
    },
    /// Self-update the `hamsy` binary from GitHub Releases.
    Update(update::UpdateArgs),
}

/// Resolves the hamsy-proxy data directory: `explicit` if given, otherwise
/// [`hamsy_core::data_dir`] (which itself honors `$HAMSY_HOME`).
pub(crate) fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    match explicit {
        Some(dir) => dir.to_path_buf(),
        None => hamsy_core::data_dir(),
    }
}

/// Initializes the global tracing subscriber. `RUST_LOG`, when set, always
/// wins; otherwise the level is derived from `-v`/`-vv` (absent = info).
fn init_tracing(verbosity: u8) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(match verbosity {
            0 => "info",
            1 => "debug",
            _ => "trace",
        })
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Chains onto whatever panic hook was previously installed (so the
/// normal "thread panicked at ..." message still prints) and makes a
/// best-effort attempt to restore the OS system proxy from the marker
/// file before that message prints. This is the only in-process
/// mitigation possible for a panic that happens after the system proxy
/// was enabled but before `run()`'s normal shutdown path gets a chance to
/// run. Must not itself panic: a panicking panic hook aborts the process
/// harder, without even printing the original message, which would make
/// crashes strictly harder to diagnose than doing nothing here at all.
fn install_panic_restore_hook(data_dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        match std::panic::catch_unwind(|| hamsy_api::sysproxy_state::restore_if_marked(&data_dir)) {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                eprintln!("hamsy: failed to restore the system proxy after a panic: {err}")
            }
            Err(_) => eprintln!(
                "hamsy: panicked again while restoring the system proxy after a panic (ignored)"
            ),
        }
        previous(info);
    }));
}

fn main() {
    // Two rustls crypto backends end up in the dependency tree: `ring` (via
    // hamsy-proxy, to avoid aws-lc-sys's cmake/nasm build requirement) and
    // `aws-lc-rs` (transitively, via self_update's reqwest+rustls feature).
    // With both present, rustls 0.23 can't auto-select a process-level
    // CryptoProvider and panics on first TLS use. Install `ring` explicitly
    // before any TLS happens. This only errs if a default was already
    // installed, which is harmless, so the error is ignored rather than
    // unwrapped.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let command = cli.command.unwrap_or(Command::Run(cli.run_args));

    // Only `run` holds the system proxy pointed at a long-lived server that
    // could panic on some unrelated bug hours after enabling it; the other
    // subcommands (`cert`/`rules`/`proxy`) are one-shot and fully synchronous
    // -- if they panic, they haven't left anything running that needs this.
    if let Command::Run(ref args) = command {
        install_panic_restore_hook(resolve_data_dir(args.data_dir.as_deref()));
    }

    let result = match command {
        Command::Run(args) => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(run::run(args)),
            Err(e) => Err(e.into()),
        },
        Command::Mcp(args) => tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(mcp::run(args))),
        Command::Agent(args) => tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(mcp::agent(args))),
        Command::AgentGuide => {
            println!("{}", mcp::GUIDE);
            Ok(())
        }
        Command::Open(args) => tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(har_open::open(args))),
        Command::HarViewerServe(args) => tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(har_open::serve(args))),
        Command::Cert { command } => cert::dispatch(command),
        Command::Rules { command } => rules::dispatch(command),
        Command::Proxy { command } => sysproxy_cmd::dispatch(command),
        Command::Update(args) => update::dispatch(args),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn open_parses_multiple_paths_and_options() {
        let cli = Cli::try_parse_from([
            "hamsy",
            "open",
            "--no-open",
            "--data-dir",
            "/tmp/my data",
            "--",
            "one file.har",
            "-two.har",
        ])
        .unwrap();
        let Some(Command::Open(args)) = cli.command else {
            panic!("expected open command")
        };
        assert!(args.no_open);
        assert_eq!(
            args.files,
            vec![PathBuf::from("one file.har"), PathBuf::from("-two.har")]
        );
        assert_eq!(args.data_dir, Some(PathBuf::from("/tmp/my data")));
        assert!(Cli::try_parse_from(["hamsy", "open"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["hamsy", "open", "--stop"])
                .unwrap()
                .command,
            Some(Command::Open(har_open::OpenArgs { stop: true, .. }))
        ));
        assert!(Cli::try_parse_from(["hamsy", "open", "--stop", "file.har"]).is_err());
        assert!(Cli::try_parse_from(["hamsy", "open", "--stop", "--no-open"]).is_err());
        assert!(Cli::try_parse_from(["hamsy", "--manual"])
            .unwrap()
            .command
            .is_none());
    }
}
