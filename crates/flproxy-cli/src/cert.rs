//! `flproxy cert` subcommands: inspecting the MITM root certificate
//! authority and (best-effort) installing/removing it from the OS trust
//! store.
//!
//! These subcommands operate directly on the on-disk CA in the flproxy data
//! directory; there is no IPC with a running `flproxy run` process.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Subcommand;
use flproxy_proxy::CertAuthority;

use crate::resolve_data_dir;

/// `flproxy cert` subcommands.
#[derive(Subcommand, Debug)]
pub enum CertCommand {
    /// Print the path to the CA certificate file.
    Path,
    /// Export the CA certificate to a file or stdout (PEM by default).
    Export {
        /// Write DER instead of the default PEM.
        #[arg(long)]
        der: bool,
        /// Destination file; defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print the CA certificate's SHA-256 fingerprint.
    Fingerprint,
    /// Add the CA certificate to the OS trust store.
    Install,
    /// Remove the CA certificate from the OS trust store.
    Uninstall,
}

/// Dispatches a [`CertCommand`].
pub fn dispatch(cmd: CertCommand) -> Result<()> {
    match cmd {
        CertCommand::Path => {
            let (_ca, cert_path) = load_ca()?;
            println!("{}", cert_path.display());
            Ok(())
        }
        CertCommand::Export { der, out } => export(der, out),
        CertCommand::Fingerprint => {
            let (ca, _path) = load_ca()?;
            println!("{}", ca.fingerprint_sha256());
            Ok(())
        }
        CertCommand::Install => install(),
        CertCommand::Uninstall => uninstall(),
    }
}

/// Loads (generating if necessary) the CA rooted at the CLI's data
/// directory, returning it alongside the path to its PEM certificate file.
fn load_ca() -> Result<(CertAuthority, PathBuf)> {
    let dir = resolve_data_dir(None);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create data dir {}", dir.display()))?;
    let ca = CertAuthority::load_or_generate(&dir).context("failed to load or generate the CA")?;
    let cert_path = dir.join("ca.pem");
    Ok((ca, cert_path))
}

fn export(der: bool, out: Option<PathBuf>) -> Result<()> {
    let (ca, _path) = load_ca()?;
    if der {
        let bytes = ca.der();
        match out {
            Some(path) => std::fs::write(&path, &bytes)
                .with_context(|| format!("failed to write {}", path.display()))?,
            None => std::io::stdout()
                .write_all(&bytes)
                .context("failed to write to stdout")?,
        }
    } else {
        let pem = ca.pem();
        match out {
            Some(path) => std::fs::write(&path, &pem)
                .with_context(|| format!("failed to write {}", path.display()))?,
            None => print!("{pem}"),
        }
    }
    Ok(())
}

/// Runs `cmd` with `args`, returning its stdout as a `String` on success (a
/// non-zero exit status is treated as failure). Mirrors the private `run()`
/// helper in `flproxy_api::sysproxy`.
fn run_command(cmd: &str, args: &[&str]) -> std::result::Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `{cmd}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`{cmd} {}` failed: {stderr}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Builds the `security add-trusted-cert` argv used to install the CA into
/// the macOS System keychain.
fn macos_install_args(cert_path: &Path) -> Vec<String> {
    vec![
        "security".to_string(),
        "add-trusted-cert".to_string(),
        "-d".to_string(),
        "-r".to_string(),
        "trustRoot".to_string(),
        "-k".to_string(),
        "/Library/Keychains/System.keychain".to_string(),
        cert_path.display().to_string(),
    ]
}

/// Builds the `security remove-trusted-cert` argv used to remove the CA from
/// the macOS System keychain.
fn macos_uninstall_args(cert_path: &Path) -> Vec<String> {
    vec![
        "security".to_string(),
        "remove-trusted-cert".to_string(),
        "-d".to_string(),
        cert_path.display().to_string(),
    ]
}

/// Builds the `certutil -addstore` argv used to install the CA into the
/// Windows `ROOT` store.
fn windows_install_args(cert_path: &Path) -> Vec<String> {
    vec![
        "certutil".to_string(),
        "-addstore".to_string(),
        "-f".to_string(),
        "ROOT".to_string(),
        cert_path.display().to_string(),
    ]
}

/// Builds the `certutil -delstore` argv used to remove the CA from the
/// Windows `ROOT` store, matching by the CA's common name (`"flproxy CA"`,
/// see `flproxy_proxy::ca::generate_ca_pem`).
fn windows_uninstall_args() -> Vec<String> {
    vec![
        "certutil".to_string(),
        "-delstore".to_string(),
        "ROOT".to_string(),
        "flproxy CA".to_string(),
    ]
}

/// The two shell commands (`cp`, `update-ca-certificates`) needed to
/// manually install the CA into the Linux system trust store.
fn linux_install_commands(cert_path: &Path) -> (String, String) {
    (
        format!(
            "cp {} /usr/local/share/ca-certificates/flproxy-ca.crt",
            cert_path.display()
        ),
        "update-ca-certificates".to_string(),
    )
}

/// The two shell commands (`rm`, `update-ca-certificates`) needed to
/// manually remove the CA from the Linux system trust store.
fn linux_uninstall_commands() -> (String, String) {
    (
        "rm /usr/local/share/ca-certificates/flproxy-ca.crt".to_string(),
        "update-ca-certificates".to_string(),
    )
}

/// The `certutil` command to add the CA to the NSS database Chrome/Firefox
/// use on Linux, independent of whether the system-wide install succeeded.
fn nss_install_command(cert_path: &Path) -> String {
    format!(
        "certutil -d sql:$HOME/.pki/nssdb -A -t \"C,,\" -n flproxy -i {}",
        cert_path.display()
    )
}

fn linux_install_ca_file(cert_path: &Path) -> std::result::Result<(), String> {
    let dest = Path::new("/usr/local/share/ca-certificates/flproxy-ca.crt");
    std::fs::copy(cert_path, dest).map_err(|e| format!("copy to {}: {e}", dest.display()))?;
    run_command("update-ca-certificates", &[])?;
    Ok(())
}

fn linux_uninstall_ca_file() -> std::result::Result<(), String> {
    let dest = Path::new("/usr/local/share/ca-certificates/flproxy-ca.crt");
    std::fs::remove_file(dest).map_err(|e| format!("remove {}: {e}", dest.display()))?;
    run_command("update-ca-certificates", &[])?;
    Ok(())
}

fn install() -> Result<()> {
    let (ca, cert_path) = load_ca()?;
    println!("SHA-256 fingerprint: {}", ca.fingerprint_sha256());

    if cfg!(target_os = "macos") {
        let args = macos_install_args(&cert_path);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_command(arg_refs[0], &arg_refs[1..]) {
            Ok(_) => println!("Installed the flproxy CA into the macOS System keychain."),
            Err(e) => {
                eprintln!("Could not install automatically ({e}).");
                eprintln!("Run this yourself:\n  sudo {}", args.join(" "));
            }
        }
    } else if cfg!(target_os = "linux") {
        match linux_install_ca_file(&cert_path) {
            Ok(()) => println!("Installed the flproxy CA into the system trust store."),
            Err(e) => {
                eprintln!("Could not install automatically ({e}).");
                let (cp, update) = linux_install_commands(&cert_path);
                eprintln!("Run this yourself:\n  sudo {cp}\n  sudo {update}");
            }
        }
        eprintln!("\nFor Chrome/Firefox (which use their own NSS certificate store), also run:");
        eprintln!("  {}", nss_install_command(&cert_path));
    } else if cfg!(target_os = "windows") {
        let args = windows_install_args(&cert_path);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_command(arg_refs[0], &arg_refs[1..]) {
            Ok(_) => println!("Installed the flproxy CA into the Windows ROOT store."),
            Err(e) => {
                eprintln!("Could not install automatically ({e}).");
                eprintln!(
                    "Re-run this command from an elevated (Administrator) prompt:\n  {}",
                    args.join(" ")
                );
            }
        }
    } else {
        eprintln!("Automatic trust-store installation isn't supported on this platform.");
        eprintln!("The CA certificate is at: {}", cert_path.display());
    }

    println!("\nFor iOS/Android, use the web UI's Setup page (/api/setup) instead.");
    Ok(())
}

fn uninstall() -> Result<()> {
    let (ca, cert_path) = load_ca()?;
    println!("SHA-256 fingerprint: {}", ca.fingerprint_sha256());

    if cfg!(target_os = "macos") {
        let args = macos_uninstall_args(&cert_path);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_command(arg_refs[0], &arg_refs[1..]) {
            Ok(_) => println!("Removed the flproxy CA from the macOS System keychain."),
            Err(e) => {
                eprintln!("Could not remove automatically ({e}).");
                eprintln!("Run this yourself:\n  sudo {}", args.join(" "));
            }
        }
    } else if cfg!(target_os = "linux") {
        match linux_uninstall_ca_file() {
            Ok(()) => println!("Removed the flproxy CA from the system trust store."),
            Err(e) => {
                eprintln!("Could not remove automatically ({e}).");
                let (rm, update) = linux_uninstall_commands();
                eprintln!("Run this yourself:\n  sudo {rm}\n  sudo {update}");
            }
        }
    } else if cfg!(target_os = "windows") {
        let args = windows_uninstall_args();
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_command(arg_refs[0], &arg_refs[1..]) {
            Ok(_) => println!("Removed the flproxy CA from the Windows ROOT store."),
            Err(e) => {
                eprintln!("Could not remove automatically ({e}).");
                eprintln!(
                    "Re-run this command from an elevated (Administrator) prompt:\n  {}",
                    args.join(" ")
                );
            }
        }
    } else {
        eprintln!("Automatic trust-store removal isn't supported on this platform.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_install_args_shape() {
        let args = macos_install_args(Path::new("/tmp/ca.pem"));
        assert_eq!(
            args,
            vec![
                "security",
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
                "/tmp/ca.pem",
            ]
        );
    }

    #[test]
    fn windows_uninstall_args_match_ca_common_name() {
        let args = windows_uninstall_args();
        assert_eq!(args, vec!["certutil", "-delstore", "ROOT", "flproxy CA"]);
    }

    #[test]
    fn linux_commands_reference_expected_paths() {
        let (cp, update) = linux_install_commands(Path::new("/tmp/ca.pem"));
        assert_eq!(
            cp,
            "cp /tmp/ca.pem /usr/local/share/ca-certificates/flproxy-ca.crt"
        );
        assert_eq!(update, "update-ca-certificates");
        let (rm, update2) = linux_uninstall_commands();
        assert_eq!(rm, "rm /usr/local/share/ca-certificates/flproxy-ca.crt");
        assert_eq!(update2, "update-ca-certificates");
    }

    #[test]
    fn nss_command_includes_cert_path() {
        let cmd = nss_install_command(Path::new("/tmp/ca.pem"));
        assert!(cmd.contains("/tmp/ca.pem"));
        assert!(cmd.contains("certutil"));
    }
}
