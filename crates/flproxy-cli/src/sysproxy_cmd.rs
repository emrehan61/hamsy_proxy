//! `flproxy proxy on|off|status`: OS system-proxy control.
//!
//! Operates directly on the on-disk `Settings` (for the configured proxy
//! port) via [`flproxy_api::sysproxy`); there is no IPC with a running
//! `flproxy run` process.

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::resolve_data_dir;

/// `flproxy proxy` subcommands.
#[derive(Subcommand, Debug)]
pub enum ProxyCommand {
    /// Enable the OS system proxy, pointed at this instance's configured port.
    On,
    /// Disable the OS system proxy.
    Off,
    /// Print whether the OS system proxy currently appears to be enabled.
    Status,
}

/// Dispatches a [`ProxyCommand`].
pub fn dispatch(cmd: ProxyCommand) -> Result<()> {
    match cmd {
        ProxyCommand::On => on(),
        ProxyCommand::Off => off(),
        ProxyCommand::Status => status(),
    }
}

/// Bypass list applied when enabling the OS system proxy: traffic to these
/// hosts is left to connect directly rather than through the proxy.
const SYSTEM_PROXY_BYPASS: &[&str] = &["localhost", "127.0.0.1", "::1", "*.local"];

fn on() -> Result<()> {
    let data_dir = resolve_data_dir(None);
    let settings = flproxy_core::Settings::load(&data_dir.join("settings.json"));
    let bypass: Vec<String> = SYSTEM_PROXY_BYPASS.iter().map(|s| s.to_string()).collect();
    flproxy_api::sysproxy_state::acquire(&data_dir, "127.0.0.1", settings.proxy_port, &bypass)
        .map_err(anyhow::Error::msg)
        .context("failed to enable the system proxy")?;
    println!("System proxy enabled: 127.0.0.1:{}", settings.proxy_port);
    Ok(())
}

fn off() -> Result<()> {
    let data_dir = resolve_data_dir(None);
    flproxy_api::sysproxy_state::release(&data_dir)
        .map_err(anyhow::Error::msg)
        .context("failed to disable the system proxy")?;
    println!("System proxy disabled.");
    Ok(())
}

fn status() -> Result<()> {
    match flproxy_api::sysproxy::status() {
        Ok(true) => println!("System proxy is currently enabled."),
        Ok(false) => println!("System proxy is currently disabled."),
        Err(e) => println!("Could not determine system proxy status: {e}"),
    }
    Ok(())
}
