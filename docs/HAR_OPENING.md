# HAR file opening

## Design

The normal capture application and the HAR viewer use the same browser UI. `hamsy open FILE...` provides the file-manager entry point. It reads explicit local file arguments, validates the HAR documents, and hands them to a local viewer through authenticated IPC. The viewer does not start a proxy or touch certificate trust or the capture instance's settings.

The browser receives a URL containing an unguessable, short-lived ticket. `GET /api/har/open/{ticket}` returns `{ "files": [{ "name": "session.har", "text": "..." }] }`; the UI imports these into its existing independent HAR sessions and removes the opening ticket from the URL. Imported sessions continue to use the browser's existing IndexedDB storage. The HTTP endpoint never accepts a filesystem path.

The viewer's state identifies it with `viewerOnly: true`. Capture controls and setup/rules/settings navigation are unavailable in this mode, and the server rejects capture mutations independently of the UI.

## Service lifecycle and limits

`hamsy open [--no-open] [--data-dir DIR] -- FILE...` accepts up to 32 files with a combined size of 64 MiB per invocation. Missing, non-regular, invalid UTF-8, invalid JSON, and invalid HAR files are rejected before starting a viewer. The source files are never modified.

The viewer listens only on `127.0.0.1`. Discovery, a lifetime process lock, and the log live under `<data-dir>/har-viewer`; Unix permissions or Windows access controls restrict access to this private state. The discovery record contains a protocol version, process information, loopback port, and a random authentication token. Separate CLI invocations use the lifetime lock and authenticated health check to converge on one viewer process.

Before disclosing its bearer credential or HAR contents, the CLI verifies an HMAC-SHA256 proof of a fresh challenge using the private discovery token. This prevents an unrelated process that has occupied a stale remembered port from impersonating the viewer. The proof also covers the protocol and application versions.

Queued handoffs expire after five minutes and have a combined 128 MiB limit. Reopening a source file creates a fresh ticket. Tickets are served with `Cache-Control: no-store`; the viewer also sends `Referrer-Policy: no-referrer`. The browser clears the ticket from its URL and persists imported sessions using the existing IndexedDB implementation.

The service exits after 30 minutes without an accepted HTTP request. The browser's regular state polling keeps it alive while a page is active. Its next startup attempts to reuse the previous port, preserving the browser origin and its saved HAR sessions. If another application occupies that port, it chooses another; browser storage at the previous origin is not migrated automatically. Export important sessions as HAR files.

The viewer writes diagnostics to `<data-dir>/har-viewer/viewer.log`, without the HAR payload or ticket request tracing. The log is bounded between service starts. Browser launch errors report the viewer URL, and `--no-open` prints that URL without launching the browser.

`hamsy open --stop [--data-dir DIR]` sends an authenticated shutdown request and waits for the viewer to release its service lock. It succeeds without creating a profile when no viewer is running. It cannot be combined with input files or `--no-open`. A running viewer from a different Hamsy version must be stopped and reopened after an update; the CLI reports that action instead of silently opening an outdated embedded UI.

## Implementation sequence

1. Add the `open` command, private discovery and startup coordination, authenticated handoff, expiring ticket retrieval, and a viewer service with no proxy side effects.
2. Add browser ticket import, safe URL cleanup, multi-file tabs, and viewer-only navigation.
3. Package launcher sources and resources alongside the root `hamsy` executable (or `hamsy.exe` on Windows), preserving the release archive and self-updater contract.
4. Generate, refresh, and remove desktop integration on the user's computer. Preserve user choice of the default HAR application.
5. Verify command behavior, service reuse, invalid input, IPC access restrictions, UI imports, and staged installation/removal.

## Distribution

The release workflow builds the embedded web UI into a native CLI executable. macOS and Linux release tarballs keep `hamsy` at the archive root. Windows x64 releases use a ZIP with `hamsy.exe` at its root. Launcher sources and resources are included alongside the executable; an install does not rebuild the Rust engine or web UI.

On macOS, installation compiles the AppleScript source into `~/Applications/Hamsy.app`, adds the icon and HAR document metadata, and applies a local ad-hoc signature for bundle integrity. On Linux, installation generates the desktop entry and installs its wrapper, icon, and MIME registration. On Windows, a PowerShell installer creates per-user desktop integration from the release resources. Windows does not require Bash or WSL.

`install.sh` is the prebuilt installer for macOS and Linux. `install_source.sh` remains a compatibility entry point for that same operation. Source builds are explicit through `install-from-source.sh` or `install.sh --from-source` in a checkout. Windows uses `install.ps1`.

The installation helper records the absolute executable path rather than assuming a shell PATH or a particular user's home directory. This supports custom binary prefixes. Registering Hamsy as an opener does not forcibly replace Charles or another chosen default.

The launcher delegates to the canonical installed binary, so `hamsy update` continues to update the viewer implementation. Launcher resources are refreshed by rerunning the installer. Direct archive users can run the desktop helper included in the archive. Keep the executable at its installed location, or rerun integration with the new absolute path.

No Apple Developer ID, notarization, or Windows publisher signing is configured. Local launcher creation does not turn the downloaded engine into trusted publisher-signed software: operating-system security policies may still require approval or block execution. Installers do not disable these policies or remove quarantine markers. Installing a HAR opener does not require trusting the MITM CA; certificate setup remains a separate capture concern.

## Verification checklist

- Open one or several valid HAR files, including spaces, Unicode, and shell metacharacters in names.
- Reject missing, malformed, oversized, and non-HAR inputs without changing capture settings or certificates.
- Reuse the same viewer for repeated opens; handle simultaneous initial opens and stale process discovery.
- Reject unauthorized handoff requests and unknown/expired tickets. Do not expose arbitrary local files.
- Import into independent HAR tabs; retain normal browser upload, drag/drop, export, search, and pop-out behavior.
- Hide and reject capture-only operations in viewer mode.
- Build the embedded UI and launcher package, preserving the root binary for existing updaters.
- Stage installation and removal in temporary directories, including a custom prefix, without changing the developer's file associations.
- Verify Finder, Linux file-manager, and Windows Open With integration on their native platforms before public distribution.

Useful local checks:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
(cd ui && pnpm test && pnpm build)
bash packaging/build-desktop.sh /tmp/hamsy-desktop-payload
python3 packaging/test-desktop.py
# On macOS, also validate its staged bundle:
python3 packaging/test-desktop.py --macos-payload /tmp/hamsy-desktop-payload
```

The packaging tests use temporary installation directories and suppress desktop registration, so they do not change your actual default applications. Linux launcher checks can run on macOS with the platform command mocked; that does not replace a real Linux file-manager check.
