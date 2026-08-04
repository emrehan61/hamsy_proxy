//! `hamsy update`: self-updates the `hamsy` binary in place from GitHub
//! Releases (github.com/emrehan61/hamsy_proxy).
//!
//! Release contract this relies on: tags are `v<semver>`, and each release
//! carries assets named `hamsy-<rust-triple>.tar.gz` (a gzipped tar with the
//! `hamsy` binary at its root) plus a `sha256sums.txt`. `self_update`'s
//! default target detection (`self_update::get_target()`, backed by Cargo's
//! `TARGET` build-script env var) returns exactly the rust triples used in
//! those asset names, so it's left unset here rather than pinned via
//! `cfg!`/`env!` in this crate.
//!
//! There is no IPC with a running `hamsy run` process: this only replaces
//! the binary on disk, so an already-running instance keeps executing its
//! already-loaded code and must be restarted to pick up the new version.

use anyhow::{Context, Result};
use clap::Args;
use self_update::backends::github::Update;
use self_update::update::ReleaseUpdate;
use self_update::{cargo_crate_version, Status};

const REPO_OWNER: &str = "emrehan61";
const REPO_NAME: &str = "hamsy_proxy";
const BIN_NAME: &str = "hamsy";

/// `hamsy update` flags.
#[derive(Args, Debug, Clone, Default)]
pub struct UpdateArgs {
    /// Only report whether a newer release is available; don't install it.
    #[arg(long)]
    pub check: bool,
    /// Skip the confirmation prompt before replacing the running binary.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

/// Dispatches [`UpdateArgs`].
pub fn dispatch(args: UpdateArgs) -> Result<()> {
    if args.check {
        check()
    } else {
        install(args.yes)
    }
}

/// Builds the `self_update` GitHub updater against this repo, per the
/// release-workflow contract in the module docs above.
fn configure(no_confirm: bool, show_download_progress: bool) -> Result<Box<dyn ReleaseUpdate>> {
    Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(cargo_crate_version!())
        .no_confirm(no_confirm)
        .show_download_progress(show_download_progress)
        .build()
        .context("failed to configure the updater")
}

fn check() -> Result<()> {
    let current = cargo_crate_version!();
    let updater = configure(true, false)?;
    let release = updater
        .get_latest_release()
        .with_context(update_check_hint)?;

    if self_update::version::bump_is_greater(current, &release.version)
        .context("failed to compare release versions")?
    {
        println!(
            "A newer hamsy release is available: v{} (current: v{current}).",
            release.version
        );
        println!("Run `hamsy update` to install it.");
    } else {
        println!("hamsy is up to date (v{current}).");
    }
    Ok(())
}

fn install(yes: bool) -> Result<()> {
    let updater = configure(yes, true)?;
    // `update()` itself reports "up to date" / prints progress and (unless
    // `no_confirm`) prompts before replacing the binary, matching the
    // `-y/--yes` contract without any extra confirmation logic here.
    let status = updater.update().with_context(update_check_hint)?;
    match status {
        Status::UpToDate(v) => println!("hamsy is up to date (v{v})."),
        Status::Updated(v) => {
            println!("Updated hamsy to v{v}.");
            println!(
                "Note: a running `hamsy run` instance keeps its old code loaded until it is restarted."
            );
        }
    }
    Ok(())
}

/// Context message for a failed release lookup/update. Covers "no releases
/// published yet" (a 404 from the GitHub API) and "no release asset for this
/// platform" without needing to pattern-match `self_update`'s error variants.
fn update_check_hint() -> String {
    format!(
        "hamsy update failed for target `{}`; this can happen if no releases have \
         been published yet, or if the latest release doesn't include a build for \
         this platform",
        self_update::get_target(),
    )
}
