<p align="center">
  <img src="docs/images/logo.png" alt="hamsy-proxy" width="160" />
</p>

# hamsy-proxy

A fast, local HTTP(S) debugging proxy with a web UI — capture, inspect, modify, and mock traffic from your machine, your phone, or anything on your network.

## What it does

- **System-wide HTTPS capture via MITM** — mints a per-host leaf certificate on the fly (signed by a locally-generated root CA) and terminates/re-originates TLS, so any HTTP(S) client that trusts the CA can be intercepted, not just a browser with a proxy extension.
- **Live web UI** — a virtualized traffic list with request/response detail, filters, copy-as-cURL, and HAR export, streamed over a WebSocket as traffic happens.
- **Rule engine** — redirect or rewrite URLs, mock responses, add/remove/rewrite request and response headers and bodies (including JSON‑patch style edits), block requests, delay them, or throttle bandwidth.
- **HAR export/import** — pull a session (or a filtered subset) out as a standard `.har` file, or load one back in.
- **iOS + Android support** — a Setup page walks a device through pointing its Wi‑Fi proxy at hamsy-proxy and trusting the CA, QR code included.
- **Single static binary** — build once with the UI embedded and ship `hamsy` as one file.

<!-- TODO: capture a real screenshot of the Traffic page and save it to docs/images/traffic.png, then restore an image link here -->

## Contents

- [What it does](#what-it-does)
- [Install / build](#install--build)
- [Cutting a release](#cutting-a-release)
- [Quick start](#quick-start)
- [CLI reference](#cli-reference)
- [Web UI tour](#web-ui-tour)
- [Rules](#rules)
- [Mobile (iOS/Android)](#mobile-iosandroid)
- [Certificate-pinned apps will not work](#certificate-pinned-apps-will-not-work)
- [Troubleshooting](#troubleshooting)
- [HAR export/import](#har-exportimport)
- [Architecture](#architecture)
- [Configuration](#configuration)
- [Development](#development)
- [Security note](#security-note)
- [License](#license)

## Install / build

Recommended — install the prebuilt binary from the latest GitHub Release, no toolchain required:

```
curl -fsSL https://raw.githubusercontent.com/emrehan61/hamsy_proxy/master/install_source.sh | bash
```

Downloads the latest released `hamsy` binary for your platform, verifies its checksum, and puts it on your PATH — no Rust, no Node, nothing built locally. `install_source.sh --help` for its flags (`--prefix`, `--version vX.Y.Z` to pin a release, `--no-cert`, `--no-path`).

Building from source instead — pipe `install.sh` straight from GitHub:

```
curl -fsSL https://raw.githubusercontent.com/emrehan61/hamsy_proxy/master/install.sh | bash
```

Equivalent to cloning and running `install.sh` below — same script, same result — it just clones the repo into a temp dir for you first.

Or clone and run `install.sh` yourself:

```
git clone https://github.com/emrehan61/hamsy_proxy.git
cd hamsy_proxy
./install.sh
```

(SSH instead: `git@github.com:emrehan61/hamsy_proxy.git`.) The binary the build produces (and that `install.sh` installs) is `hamsy`.

Prerequisites, up front: Rust (stable), Node 22, and pnpm. `install.sh` checks for all three and helps with what it can — missing Rust gets an offer to install it via rustup (prompted, or auto-confirmed under `-y`/`--yes`); pnpm is activated automatically via `corepack enable` + `corepack prepare` (version pinned in `ui/package.json`), no prompt needed. Node is the exception: if it's missing or older than 22, `install.sh` does not install it for you — it fails with a message telling you to install Node >=22 yourself (nvm, brew, or your distro's package manager). Supported platforms are macOS and Linux — `install.sh` is a bash script that checks OS/arch and refuses anything else; on Windows, use the manual `cargo build` steps documented later in this section instead.

Default install location is `$HOME/.local/bin`; it prints an `export PATH=...` line if that's not already on yours. Flags (`./install.sh --help` for the full list): `--prefix DIR` (install somewhere else), `--yes`/`-y` (skip confirmation prompts — e.g. before installing Rust via rustup), `--skip-deps` (only check dependencies, install nothing), `--no-ui` (skip the UI build, build `hamsy` without `--features embed-ui`), `--no-cert` (skip trusting the CA in the OS trust store), `--no-path` (skip offering to add the install dir to your PATH), `--uninstall` (remove the installed binary — asks before touching `~/.hamsy`, and a bare `--yes` won't answer that question for you). `install.sh` also runs `hamsy cert install` and offers to add `$PREFIX` to your PATH automatically (prompted, or automatic under `--yes`), so a fresh install needs no manual follow-up unless those steps are skipped or declined.

The rest of this section is the manual build `install.sh` itself runs — useful if you're iterating on the UI or don't want the script deciding anything for you.

Prerequisites: Rust (stable — see `rust-toolchain.toml`), Node 22, and pnpm (`ui/package.json` pins `pnpm@11.18.0`).

```
cd ui && pnpm install && pnpm build
cargo build --release --features embed-ui -p hamsy-cli
./target/release/hamsy
```

(`cd ui &&` is used instead of `pnpm --dir ui` because `pnpm --dir` still walks up the directory tree for package-manager detection, and an ancestor directory with its own `package.json`/`packageManager` field — e.g. `$HOME` — can make it pick the wrong package manager; running from inside `ui/` avoids that.)

`--features embed-ui` bakes the built `ui/dist` directory into the `hamsy` binary (via `rust-embed`), so the result is a single self-contained executable. The release binary built this way (`cargo build --release --features embed-ui -p hamsy-cli`) is about 14 MB, and was confirmed to run standalone — serving the embedded UI correctly — even when copied to and executed from a directory with no `ui/` anywhere nearby. Without that feature, the binary serves the UI straight off disk, checking (in order) `$HAMSY_UI_DIR`, `./ui/dist`, then `<dir of the executable>/ui/dist` — falling back to a minimal built-in placeholder page if none of those exist. That's the normal dev workflow: `cargo build -p hamsy-cli` (no feature flag) plus a UI build, or run the UI's own dev server for hot reload:

```
cd ui && pnpm dev
```

This serves the UI on `http://localhost:5173` and proxies `/api` and `/cert` to `http://127.0.0.1:9081` (hamsy-proxy's default UI/API port), per `ui/vite.config.ts`.

## Cutting a release

```
./release.sh -s   # patch: X.Y.Z -> X.Y.(Z+1)
./release.sh -m   # minor: X.Y.Z -> X.(Y+1).0
./release.sh -b   # major: X.Y.Z -> (X+1).0.0
```

Bumps the `[workspace.package]` version in the root `Cargo.toml`, refreshes `Cargo.lock`, commits (`Release vX.Y.Z`), tags (`vX.Y.Z`), and pushes — from `master` only, on a clean, up-to-date tree. The pushed tag triggers `.github/workflows/release.yml`, which builds the binaries for all platforms and publishes them as a GitHub release; `hamsy update` compares its own version against the latest release and picks up the new binary from there. Need an exact version instead of a bump (e.g. to match a specific release number)? `./release.sh -v 1.2.3` (or `--set-version v1.2.3`, leading `v` optional) sets it directly — it must be strictly greater than the current version. `./release.sh --dry-run` previews the version change and every command without changing anything; `--no-push` stops after the local commit/tag; `--yes` skips the confirmation prompt. `./release.sh --help` for the full flag list.

## Quick start

```
hamsy                 # same as `hamsy run` — captures system-wide out of the box
```

`install.sh` already runs `hamsy cert install` for you unless you passed `--no-cert` or declined the prompt — only in that case do you need to run it yourself before HTTPS capture works:

```
hamsy cert install     # trust the CA in the OS keychain (best-effort; prints manual steps on failure)
```

That's it: `hamsy` points your OS's HTTP(S) proxy setting at itself on startup (macOS, Windows, and Linux) and restores whatever was there before on shutdown, so there's no separate "turn on the proxy" step anymore — `hamsy proxy on`/`off`/`status` still exist, but only for controlling the system proxy independently of a running `hamsy`. If you'd rather leave your OS proxy settings alone and point clients at hamsy-proxy yourself, run `hamsy --manual` and configure them to use `127.0.0.1:9080`.

Setting the OS proxy automatically is best-effort and a little platform-dependent — a failure here is never fatal, hamsy-proxy just prints a warning with manual instructions and keeps running:

- **macOS** — uses `networksetup`; needs an admin account (some managed/corporate Macs restrict this).
- **Linux** — uses GNOME's `gsettings`; other desktops (KDE, XFCE, headless servers, WSL) don't have it, so hamsy-proxy warns and falls back to telling you to set `http_proxy`/`https_proxy` (or your desktop's own proxy settings) by hand.
- **Windows** — writes the per-user `HKCU\...\Internet Settings` registry keys; apps that were already running on WinINet (some older/native apps) may not notice the change until restarted.

Then open the web UI and browse — traffic starts appearing immediately. Here's the actual startup banner (captured from a local run on the default ports):

```

  hamsy 0.1.0

  Proxy      http://127.0.0.1:9080
  Web UI     http://127.0.0.1:9081
  CA cert    http://10.10.100.182:9081/cert/hamsy-ca.crt
  SHA-256    49:A6:C0:B4:4E:74:2F:F8:E5:51:40:67:3D:EF:1D:86:CB:BC:37:59:8E:DD:F6:F8:31:5C:2F:79:3D:15:24:0A

  Devices on your network: point their proxy at 10.10.100.182:9080
  Press Ctrl-C to stop.
```

(The LAN IP shown is whatever the machine's primary network interface happens to be at capture time.)

## CLI reference

Top level:

```
$ hamsy --help
Local MITM HTTP(S) debugging proxy

Usage: hamsy [OPTIONS] [COMMAND]

Commands:
  run    Run the proxy and web UI (the default when no subcommand is given)
  cert   Manage the MITM root certificate authority
  rules  Manage capture/rewrite rules
  proxy  Control the OS system HTTP/HTTPS proxy
  help   Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...               Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -p, --proxy-port <PROXY_PORT>  Override the proxy listener port
  -u, --ui-port <UI_PORT>        Override the web UI/API listener port
  -b, --bind <BIND>              Override the address both servers bind to
      --data-dir <DATA_DIR>      Override the hamsy data directory (default: `$HAMSY_HOME` or `~/.hamsy`)
      --system-proxy             Force-enable the OS system proxy on start (this is now the default; kept for backward compatibility / to override a persisted opt-out)
      --manual                   Don't touch the OS system proxy; configure your client to use it manually (alias: --no-system-proxy)
      --no-open                  Don't open a browser tab once the servers are listening
      --paused                   Start with capture paused
      --no-https                 Disable HTTPS/TLS interception (blind-tunnel HTTPS instead of MITM'ing it)
  -h, --help                     Print help
  -V, --version                  Print version
```

`hamsy` with no subcommand is exactly `hamsy run` with the same flags.

```
$ hamsy run --help
Run the proxy and web UI (the default when no subcommand is given)

Usage: hamsy run [OPTIONS]

Options:
  -p, --proxy-port <PROXY_PORT>  Override the proxy listener port
  -v, --verbose...               Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -u, --ui-port <UI_PORT>        Override the web UI/API listener port
  -b, --bind <BIND>              Override the address both servers bind to
      --data-dir <DATA_DIR>      Override the hamsy data directory (default: `$HAMSY_HOME` or `~/.hamsy`)
      --system-proxy             Force-enable the OS system proxy on start (this is now the default; kept for backward compatibility / to override a persisted opt-out)
      --manual                   Don't touch the OS system proxy; configure your client to use it manually (alias: --no-system-proxy)
      --no-open                  Don't open a browser tab once the servers are listening
      --paused                   Start with capture paused
      --no-https                 Disable HTTPS/TLS interception (blind-tunnel HTTPS instead of MITM'ing it)
  -h, --help                     Print help
```

Certificate authority management:

```
$ hamsy cert --help
Manage the MITM root certificate authority

Usage: hamsy cert [OPTIONS] <COMMAND>

Commands:
  path         Print the path to the CA certificate file
  export       Export the CA certificate to a file or stdout (PEM by default)
  fingerprint  Print the CA certificate's SHA-256 fingerprint
  install      Add the CA certificate to the OS trust store
  uninstall    Remove the CA certificate from the OS trust store
  help         Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...  Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -h, --help        Print help
```

```
$ hamsy cert export --help
Export the CA certificate to a file or stdout (PEM by default)

Usage: hamsy cert export [OPTIONS]

Options:
      --der         Write DER instead of the default PEM
  -v, --verbose...  Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
      --out <OUT>   Destination file; defaults to stdout
  -h, --help        Print help
```

`cert path`, `cert fingerprint`, `cert install`, and `cert uninstall` take no arguments beyond the global `-v`/`--help`. `install`/`uninstall` try to do the right thing automatically per OS (macOS Keychain via `security`, Windows `ROOT` store via `certutil`, Linux via `update-ca-certificates` plus an NSS hint for Chrome/Firefox) and print the manual command to run yourself if that fails (e.g. because it needs `sudo`).

Rule management (operates directly on `<data-dir>/rules.json`, no running server required):

```
$ hamsy rules --help
Manage capture/rewrite rules

Usage: hamsy rules [OPTIONS] <COMMAND>

Commands:
  list    List all rules (id, name, enabled, priority)
  export  Export rules as pretty JSON, to a file or stdout
  import  Import rules from a JSON file (a `Rule[]` array)
  help    Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...  Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -h, --help        Print help
```

```
$ hamsy rules import --help
Import rules from a JSON file (a `Rule[]` array)

Usage: hamsy rules import [OPTIONS] <FILE>

Arguments:
  <FILE>  Path to a JSON file containing a `Rule[]` array

Options:
      --replace     Replace the entire rule set instead of merging by id
  -v, --verbose...  Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -h, --help        Print help
```

System proxy control (writes/reads the OS proxy settings directly, independent of whether `hamsy run` is currently active):

```
$ hamsy proxy --help
Control the OS system HTTP/HTTPS proxy

Usage: hamsy proxy [OPTIONS] <COMMAND>

Commands:
  on      Enable the OS system proxy, pointed at this instance's configured port
  off     Disable the OS system proxy
  status  Print whether the OS system proxy currently appears to be enabled
  help    Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose...  Increase log verbosity: `-v` = debug, `-vv` = trace (absent = info). Overridden by `RUST_LOG` when set
  -h, --help        Print help
```

hamsy-proxy always restores whatever proxy configuration existed *before* it made any change — not just "off". The instant the system proxy is enabled (by a plain `hamsy run` — the default unless `--manual`/`--no-system-proxy` or a persisted `manualProxy: true` opts out — the web UI's system-proxy toggle, or `hamsy proxy on`), hamsy-proxy takes a snapshot of the OS proxy settings as they stood at that moment and writes it to `<data-dir>/sysproxy-state.json` alongside the pid and target host:port — so if you had a corporate proxy or a different tool's proxy configured, that's what comes back, not a blank "disabled" state. `Ctrl-C`/`SIGINT`, `SIGTERM`, `SIGHUP` (e.g. closing the terminal window), and `SIGQUIT`/`Ctrl-\` on Unix — or `Ctrl-C`, `Ctrl-Break`, console close, logoff, and system shutdown on Windows — all trigger this restore immediately, *before* draining in-flight connections, so the machine is never left pointed at a proxy that's about to go away. A hard kill (`SIGKILL`, a crash, a power cut) is the one thing nothing running in-process can catch; that case is instead recovered automatically (with a logged warning) the next time `hamsy run` starts (including the bare `hamsy`) or `hamsy proxy on` runs, by reading the same marker file — other subcommands (`cert`/`rules`/`proxy off`/`proxy status`) don't touch it. Sending a second signal while a drain is already in progress skips the wait entirely and exits immediately — safe, since the restore already happened before draining started.

## Web UI tour

The UI (`ui/`) is a Vite + [Solid.js](https://www.solidjs.com/) single-page app (see `ui/package.json`'s dependencies — `solid-js`, `@solidjs/router`, `@tanstack/solid-virtual`). It has four pages:

- **Traffic** (`/`) — a virtualized flow list (filter by method, status class, resource type, host, "only modified", and free-text search) next to a detail pane with Overview/Request/Response/Timings/Raw tabs (plus a WebSocket tab for captured WS flows). Toolbar actions: pause/resume capture, clear, replay the selected flow, copy the selected flow as a `curl` command, and export HAR (all flows, just the selection, or just what's currently filtered). Keyboard shortcuts: `/` or `Cmd/Ctrl+K` to search, `j`/`k` or arrow keys to move selection, space to pause, `Cmd/Ctrl+E` to export the filtered set, `Delete` to clear (with confirmation).
- **Rules** (`/rules`) — a master-detail rule editor: pick a rule from the list (or start from a template) and edit it in a Visual tab (structured match/action editors) or a JSON tab (a raw textarea that's a lossless escape hatch for anything the visual editor doesn't have a widget for — same `Rule` JSON either way). Unsaved changes are guarded on navigation and tab close.
- **Settings** (`/settings`) — grouped fields for proxy ports/bind address/upstream proxy, capture (pause, max flows, max body size, WS capture, include/exclude host globs), HTTPS interception (passthrough presets and passthrough hosts — see [Configuration](#configuration) for what the built-in presets cover), system proxy toggle, theme, and data management (HAR export/import, rules export/import, clear all flows). Each field is persisted with a debounced `PUT /api/settings` partial patch as you type.
- **Setup** (`/setup`) — the onboarding page: proxy host/port to configure on a device (with per-OS/per-platform instructions including iOS and Android), a QR code and download links for the CA certificate plus its SHA-256 fingerprint, per-platform trust instructions, and a "check for new traffic" button that diffs the flow count before/after so you can confirm capture is actually working.

## Rules

A rule (`hamsy_core::Rule`) pairs a **matcher** with a list of **actions**. See [`docs/RULES.md`](docs/RULES.md) for the full field reference, phase semantics, evaluation order, and more worked examples. The short version:

- The matcher can check the URL (`urlOp`: `any`/`contains`/`equals`/`startsWith`/`endsWith`/`regex`/`wildcard`), HTTP method, host:port (glob), resource type, request/response headers, and request/response body content.
- Actions are grouped into request-phase (URL/method/header/body rewriting, `redirect`, `mockResponse`, `block`) and response-phase (status/header/body rewriting) — a few (`delay`, `throttle`) apply in whichever phase the rule matched in.
- All matching rules apply, in priority order (lower first, then insertion order for ties) — except `block` and `mockResponse`, which short-circuit any remaining rules in that phase.
- `redirect`/`rewriteUrl` support `$1`–`$9` capture-group substitution from a `regex` URL matcher.
- A `redirect`/`rewriteUrl` to a different host keeps the original request's `Host` header (Map Remote-style) rather than switching it to the new target's host; a `setRequestHeader` action for `Host` still overrides it.

One example per action category (there are more variants — see `docs/RULES.md` for the full catalog):

```json
{"type": "redirect", "to": "http://localhost:4000/$1"}
```
```json
{"type": "mockResponse", "status": 500, "headers": [], "body": "{\"error\":\"boom\"}", "encoding": "text", "delayMs": 0}
```
```json
{"type": "setRequestHeader", "name": "Authorization", "value": "Bearer dev-token"}
```
```json
{"type": "replaceInResponseBody", "find": "\\d{4}-\\d{2}-\\d{2}", "replace": "REDACTED", "regex": true}
```
```json
{"type": "block", "reason": "blocked by policy"}
```
```json
{"type": "delay", "ms": 500}
```
```json
{"type": "throttle", "bytesPerSec": 51200}
```

## Mobile (iOS/Android)

1. Set the device's Wi‑Fi proxy to this machine's LAN IP and the proxy port shown in the startup banner / Settings page.
2. Install the CA from the **Setup** page — it shows a QR code (linking to the certificate download URL) plus `.pem`/`.crt` download buttons and the fingerprint to verify.
3. **iOS**: after installing the profile, you must *also* go to **Settings → General → About → Certificate Trust Settings** and enable full trust for the hamsy-proxy root certificate. This is the single most common reason HTTPS interception silently does nothing on iOS — installing the profile alone is not enough.
4. **Android**: Android 7+ ignores user-installed CAs by default unless an app explicitly opts in via its network security config. Without rooting the device or using an emulator (or a system-store cert install), only browsers and cooperating apps will be interceptable — most third-party apps will simply fail to connect once HTTPS is intercepted.
5. HTTP/2 upstream traffic is captured fine, which matters since many mobile apps/browsers negotiate HTTP/2 by default.

## Certificate-pinned apps will not work

Apps that pin their expected TLS certificate (banking apps, some chat apps) will reject hamsy-proxy's minted certificate and fail to connect, by design, regardless of whether the CA is trusted. This is the same limitation every MITM debugging proxy has — Proxyman, Charles, mitmproxy included.

## Troubleshooting

- **HTTPS sites show a connection error / "your connection is not private"** — the CA isn't trusted by the client yet. Run `hamsy cert install`. On iOS, installing the profile alone is not enough — you also need the Certificate Trust Settings step in [Mobile (iOS/Android)](#mobile-iosandroid).
- **The Traffic list stays empty** — check, in rough order of likelihood: capture might be paused (`--paused` at startup, the UI's pause button, or `paused` in `settings.json`); `captureIncludeHosts` might be set to a non-empty list that doesn't include the host you're hitting (once it's non-empty, *only* matching hosts are captured — see the Configuration table); the system proxy might not actually be enabled (`hamsy proxy status`); or the client itself might be ignoring the OS system proxy — many CLI tools (`curl`, `git`, package managers, ...) look at `$http_proxy`/`$https_proxy` instead:
  ```
  export http_proxy=http://127.0.0.1:9080
  export https_proxy=http://127.0.0.1:9080
  ```
- **`hamsy` refuses to start, printing "port ... is already in use"** — pass `--proxy-port`/`--ui-port` (the error message names the one that's already in use). Both listeners are bound before anything else happens — before the startup banner prints, before the browser opens — so a port conflict is always reported cleanly rather than leaving a half-started process behind.
- **Some app breaks entirely once the proxy/CA is on**, rather than just showing one failed request — almost always certificate pinning; see [Certificate-pinned apps will not work](#certificate-pinned-apps-will-not-work). The common cases (cloud CLIs, container registries, Meta/Apple/Google apps and services) are already excluded by default via `passthroughPresets` — check whether the relevant preset is still enabled before assuming this is a new problem. For anything else, add the host to `passthroughHosts` (Settings, or `settings.json` directly) so it's never MITM'd. Conversely, if you actually want to intercept a host one of these presets covers, turn that preset off in Settings.
- **The web UI shows a bare "Web UI not built" placeholder page** instead of the real UI — the binary was built without `--features embed-ui`, and no built `ui/dist` was found on disk either. See [Install / build](#install--build) for the exact lookup order (`$HAMSY_UI_DIR`, then `./ui/dist`, then `<exe dir>/ui/dist`) and how to build it.
- **The machine seems stuck pointed at a dead hamsy-proxy's proxy settings** (it was `kill -9`'d, crashed, or the machine lost power while running) — `hamsy proxy off` forces a restore. In practice you rarely need to: the next `hamsy run` (including the bare `hamsy`) or `hamsy proxy on` notices the leftover marker file and restores it automatically before doing anything else, so this is usually already fixed by the time you go looking for it.
- **Android apps fail to connect once HTTPS interception is on**, even with the CA installed — expected; see point 4 in [Mobile (iOS/Android)](#mobile-iosandroid) (Android 7+ ignores user-installed CAs by default outside of browsers and cooperating apps).
- **macOS: `hamsy proxy on` / the automatic system-proxy step at startup fails, or `networksetup` seems to hang** — `networksetup` needs permission to change some network services (most commonly seen on a managed/corporate Mac), and hamsy-proxy reports whatever error it returns rather than pretending the change succeeded — the failing `networksetup` command and its stderr are included in the error message.
- **Linux: startup prints a warning instead of setting the system proxy** — hamsy-proxy sets the system proxy via GNOME's `gsettings`; on non-GNOME desktops (KDE, XFCE, a headless server, WSL, ...) that's not available, so this is expected. Set `http_proxy`/`https_proxy` yourself, or configure your desktop's proxy settings manually, then point them at `127.0.0.1:9080`.
- **Windows: the system proxy setting changed but some already-running app hasn't noticed** — hamsy-proxy writes the per-user `HKCU\...\Internet Settings` registry keys directly; apps already running on WinINet may have cached the old settings and need a restart to pick up the change.
- **Don't want hamsy-proxy touching your OS proxy settings at all** — run `hamsy --manual` (or `--no-system-proxy`); it leaves your existing proxy configuration alone and you configure clients yourself, same as hamsy-proxy's previous default behavior.

## HAR export/import

- From the UI: the Traffic page's export buttons, or Settings → Data → Export/Import HAR.
- Directly: `GET /api/har` (optionally `?ids=a,b` to export a subset) returns a HAR 1.2 document as a file download; `POST /api/har/import` accepts a HAR 1.2 document and returns `{"imported": n}`.

## Architecture

| Crate/dir | Role |
|---|---|
| `hamsy-core` | Pure logic: flow/rule/settings types, the rule matching/mutation engine, body codecs, the in-memory flow store, HAR import/export. No networking. |
| `hamsy-proxy` | The MITM engine: accepts connections, terminates/re-originates TLS, applies rules, dispatches upstream, records flows, relays WebSockets. Built directly on `tokio`/`hyper`/`rustls`. |
| `hamsy-api` | The REST + WebSocket API and web UI asset server (`axum`). Depends only on `hamsy-core`; talks to a proxy backend through small `ReplayHook`/`CertHook` traits. |
| `hamsy-cli` | The `hamsy` binary: wires `hamsy-proxy` and `hamsy-api` together for `run`, plus `cert`/`rules`/`proxy` management subcommands that operate on-disk directly. |
| `ui/` | The Solid.js web UI, built with Vite. |

Request lifecycle for a MITM'd HTTPS request: a client `CONNECT`s to hamsy-proxy; hamsy-proxy replies `200` immediately and then peeks the tunneled bytes to tell TLS from plaintext. For TLS, it hand-parses the ClientHello for SNI/ALPN (without pulling in a full TLS parser), mints a leaf certificate for that host on the fly (signed by the CA, cached per-host), and terminates TLS itself. The decrypted request then runs through the rule engine's request phase (URL/header/body rewrites, or a `block`/`mockResponse` short-circuit), goes to the real upstream server over a pooled connection, and the response runs through the rule engine's response phase before being recorded as a `Flow` and sent back to the client — all while a WebSocket-connected UI receives the same event stream in real time. Plain HTTP and hosts covered by `passthroughHosts`/`intercept_https=false` skip the TLS-terminate step and are blind-tunneled (though still recorded as a minimal flow if capture is on). HTTP/2 upstream connections are captured correctly too — verified by observing that requests to github.com and google.com are recorded with protocol `HTTP/2.0` in the flow list.

## Configuration

Data lives under `$HAMSY_HOME` if set, otherwise `~/.hamsy` (`%USERPROFILE%\.hamsy` on Windows):

- `settings.json` — the table below.
- `rules.json` — the rule list, as a JSON array.
- `ca.pem` / `ca-key.pem` — the root CA certificate and private key (key file written `0600` on Unix).

| Setting | Default | Description |
|---|---|---|
| `proxyPort` | `9080` | Port the MITM proxy listens on. |
| `uiPort` | `9081` | Port the UI/API server listens on. |
| `bindAddr` | `0.0.0.0` | Address both servers bind to (`0.0.0.0` so phones on the LAN can reach it). |
| `maxFlows` | `10000` | Maximum number of flows retained in the in-memory store. |
| `maxBodyBytes` | `5242880` (5 MiB) | Maximum body size captured before truncation. |
| `interceptHttps` | `true` | Whether to MITM HTTPS traffic (vs. blind-tunnel it). |
| `passthroughHosts` | `[]` | Host globs that are never MITM'd, even if `interceptHttps` is true, in addition to whatever `passthroughPresets` covers. For your own hosts (e.g. a banking app) — the built-in presets already cover the common pinned/own-CA-bundle cases. |
| `passthroughPresets` | every preset name (see below) | Names of built-in preset groups (`core`, `cloud-cli`, `meta`, `mobile-os`) whose hosts are unioned with `passthroughHosts` for MITM purposes. Covers cloud instance-metadata/in-cluster Kubernetes endpoints, cloud CLIs/registries (gcloud, kubectl, aws, Docker), and apps that pin certificates or ship their own CA bundle (Meta apps, Apple/Google device services) — these break outright under interception, so they're excluded by default. Loopback (`localhost`, `127.0.0.1`, etc.) is deliberately *not* in any preset — MITM'ing your own local dev server so you can see its decrypted traffic is a primary use case for hamsy. Turn a preset off in Settings (or remove its name here) if you actually want to intercept those hosts. See `crates/hamsy-core/src/passthrough_presets.rs` for the exact host list per preset, or `GET /api/presets/passthrough`. |
| `captureIncludeHosts` | `[]` | If non-empty, only these host globs are captured. |
| `captureExcludeHosts` | `[]` | Host globs that are never captured. |
| `manualProxy` | `false` | Whether to leave the OS system proxy alone on startup (opt out of the default system-wide capture). `false` (the default) behaves like always passing `--system-proxy`; `true` behaves like always passing `--manual`. Passing `--manual`/`--system-proxy` on the command line for one run overrides this without changing the saved value. |
| `captureWebsockets` | `true` | Whether to capture WebSocket frames. |
| `theme` | `"dark"` | UI theme name. |
| `upstreamProxy` | `null` | Optional upstream proxy to chain through, e.g. `"http://host:port"`. |
| `paused` | `false` | Whether capture is currently paused. |

## Development

```
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
cd ui && pnpm typecheck
```

`cargo test --workspace` currently passes 188 tests across all four crates (plus 3 empty doc-test suites), with 2 more ignored by default — the end-to-end signal/shutdown tests in `hamsy-cli/tests/shutdown.rs`, which spawn the real binary as a subprocess and send it real signals, so they're opt-in via `cargo test -- --ignored` rather than part of the normal run. `cargo clippy --workspace --all-targets`, `cargo fmt --all --check`, and `cd ui && pnpm typecheck` (`tsc --noEmit`) are all clean.

### Local development

Run `./dev.sh` from the repo root to start the backend (`cargo run -p hamsy-cli`) and the UI dev server together with one command — it prints `http://localhost:5173` to open once both are up, and Ctrl-C stops both cleanly. `./dev.sh build` builds the UI and serves it from a single `hamsy` process instead (no Vite); `./dev.sh backend`/`./dev.sh ui` run just one side. `./dev.sh --help` for details. Every backend it starts passes `--manual`, so a dev run never touches your OS-wide proxy settings — set `HAMSY_DEV_SYSTEM_PROXY=1` to run with `--system-proxy` instead if you specifically need to exercise that behavior.

Formatting is stock rustfmt — there is deliberately no `rustfmt.toml`, so `cargo fmt` with a default toolchain produces exactly what's committed and no per-project setup is needed.

### Contributing

- Start with the Architecture table above for which crate owns what before touching anything.
- Run `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` on whatever you touch before sending it out — both are currently clean on `master`, so a new warning or formatting diff is yours to fix, not a pre-existing one to ignore.
- Tests live next to the code they cover: `hamsy-core` has only inline `#[cfg(test)] mod tests` blocks (no `tests/` directory — it's pure logic, no server to spin up). `hamsy-proxy` and `hamsy-api` each add a `tests/` directory on top of their inline unit tests (`hamsy-proxy/tests/proxy_tests.rs` drives a real MITM proxy instance end-to-end; `hamsy-api/tests/rest_api.rs` and `tests/ws.rs` drive the REST/WebSocket API against a standalone `ApiState`). `hamsy-cli` has inline tests in `cert.rs`/`rules.rs`/`run.rs` plus the `--ignored` end-to-end tests in `tests/shutdown.rs` mentioned above.
- Iterating on the UI: run its dev server for hot reload rather than rebuilding the Rust binary on every change — see `cd ui && pnpm dev` in [Install / build](#install--build).

## Security note

By default hamsy-proxy binds `0.0.0.0` (see `bindAddr` above) so phones and other devices on your LAN can reach it — which also means **anyone on that network can use it as an open HTTP(S) proxy**, and the web UI/API has **no authentication** at all (there's no auth middleware in `hamsy-api`; the router adds only CORS, compression, and tracing layers). If you're not doing mobile/LAN capture, set `bindAddr` to `127.0.0.1`. Don't run hamsy-proxy on a network you don't trust.

Installing the CA certificate means **anyone who obtains `ca-key.pem` can transparently MITM your HTTPS traffic** on any device that trusts that CA. Keep it private (it's written with `0600` permissions on Unix, but back it up carefully if you do), and run `hamsy cert uninstall` to remove the CA from your OS/browser trust store once you're done debugging.

## License

MIT — see [`LICENSE`](LICENSE) (also declared in the workspace `Cargo.toml` via `license = "MIT"`).
