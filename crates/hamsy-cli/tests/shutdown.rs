//! Integration test for the signal-triggered shutdown path (`hamsy run`
//! catching a signal, restoring the system proxy, draining, and exiting
//! `0`).
//!
//! Does NOT cover: any real system-proxy mutation (`--system-proxy` is
//! never passed here, so `sysproxy_state::acquire` is never called and
//! nothing touches the machine's actual OS proxy settings), the marker
//! file/recovery logic (covered by `hamsy-api`'s own unit tests), or
//! Windows signal handling (this file is unix-only; sending SIGHUP/SIGQUIT
//! from Rust without a new dependency is easiest by shelling out to the
//! system `kill` command, which doesn't exist on Windows). It only checks
//! that the process catches a signal and exits cleanly (status `0`) within
//! a bounded time.

#![cfg(unix)]

use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Binds an ephemeral port and immediately drops the listener, handing the
/// caller a port number believed free. Small unavoidable race (something
/// else could grab the port before the real binary binds it) -- standard
/// pattern for this kind of test, acceptable since these tests are
/// `#[ignore]`d and run explicitly, not in every `cargo test`.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

/// A fresh, unique temp directory for one test run, hand-rolled (no
/// `tempfile`/`uuid` dependency in `hamsy-cli`) the same way
/// `hamsy-core/src/settings.rs`'s tests build unique temp paths.
fn fresh_temp_dir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "hamsy-cli-shutdown-test-{label}-{}-{}",
        std::process::id(),
        nanos
    ))
}

/// Polls `127.0.0.1:port` until a TCP connection succeeds (the server is
/// accepting), up to `timeout`. Panics if it never comes up.
fn wait_for_ready(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server on port {port} did not become ready within {timeout:?}");
}

/// Spawns `hamsy run` (never with `--system-proxy`), waits for it to be
/// ready, sends `signal_name` via the system `kill` command, then waits
/// (bounded) for the process to exit and asserts it exited with status
/// `0`. Cleans up the temp data dir afterward, best-effort.
fn run_signal_test(signal_name: &str) {
    let data_dir = fresh_temp_dir(signal_name);
    let proxy_port = free_port();
    let ui_port = free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_hamsy"))
        .args([
            "--data-dir",
            data_dir.to_str().expect("temp dir path is valid UTF-8"),
            "--bind",
            "127.0.0.1",
            "--proxy-port",
            &proxy_port.to_string(),
            "--ui-port",
            &ui_port.to_string(),
            "--no-open",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hamsy");

    wait_for_ready(ui_port, Duration::from_secs(5));

    let status = Command::new("kill")
        .args(["-s", signal_name, &child.id().to_string()])
        .status()
        .expect("run kill(1)");
    assert!(
        status.success(),
        "kill -s {signal_name} {} failed",
        child.id()
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(exit_status) => {
                assert!(
                    exit_status.success(),
                    "hamsy exited with {exit_status:?} after {signal_name}"
                );
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut stderr = String::new();
                    if let Some(mut s) = child.stderr.take() {
                        use std::io::Read;
                        let _ = s.read_to_string(&mut stderr);
                    }
                    panic!("hamsy did not exit within 15s of receiving {signal_name}; stderr: {stderr}");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Slow (spawns a real subprocess and waits up to ~20s total): run with
/// `cargo test -- --ignored` to include it.
#[test]
#[ignore]
fn sigint_triggers_clean_shutdown() {
    run_signal_test("SIGINT");
}

/// Slow (spawns a real subprocess and waits up to ~20s total): run with
/// `cargo test -- --ignored` to include it.
#[test]
#[ignore]
fn sigterm_triggers_clean_shutdown() {
    run_signal_test("SIGTERM");
}
