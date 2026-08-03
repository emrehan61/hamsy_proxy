//! Command-line entry point for flproxy: a local MITM HTTP(S) debugging
//! proxy. Wires together `flproxy-core`, `flproxy-proxy`, and `flproxy-api`
//! into a runnable binary (`flproxy run`), plus `cert`/`rules`/`proxy`
//! management subcommands that operate directly on flproxy's on-disk state
//! (no IPC with a running `flproxy run` process).

mod cert;
mod hooks;
mod rules;
mod run;
mod shutdown;
mod sysproxy_cmd;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use cert::CertCommand;
use rules::RulesCommand;
use sysproxy_cmd::ProxyCommand;

/// flproxy: a local MITM HTTP(S) debugging proxy.
///
/// Running with no subcommand is equivalent to `flproxy run`.
#[derive(Parser, Debug)]
#[command(name = "flproxy", version, about = "Local MITM HTTP(S) debugging proxy", long_about = None)]
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
}

/// Resolves the flproxy data directory: `explicit` if given, otherwise
/// [`flproxy_core::data_dir`] (which itself honors `$FLPROXY_HOME`).
pub(crate) fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    match explicit {
        Some(dir) => dir.to_path_buf(),
        None => flproxy_core::data_dir(),
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
    tracing_subscriber::fmt().with_env_filter(filter).init();
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
        match std::panic::catch_unwind(|| flproxy_api::sysproxy_state::restore_if_marked(&data_dir))
        {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                eprintln!("flproxy: failed to restore the system proxy after a panic: {err}")
            }
            Err(_) => eprintln!(
                "flproxy: panicked again while restoring the system proxy after a panic (ignored)"
            ),
        }
        previous(info);
    }));
}

fn main() {
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
        Command::Cert { command } => cert::dispatch(command),
        Command::Rules { command } => rules::dispatch(command),
        Command::Proxy { command } => sysproxy_cmd::dispatch(command),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
