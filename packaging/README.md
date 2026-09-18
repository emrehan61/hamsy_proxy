Hamsy HAR file integration

Keep the hamsy executable in a permanent location, then register the launcher:

  bash packaging/install-desktop.sh --binary /absolute/path/to/hamsy

On Windows x64, use the helper included with the Windows ZIP:

  powershell.exe -NoProfile -File packaging/windows/install-desktop.ps1 -BinaryPath "C:\path\to\hamsy.exe"

It generates a local HAR chooser executable and a Start Menu shortcut. The
main Windows installer is install.ps1, included at the ZIP root; it can also
download and verify a release. See packaging/windows/README.md for details.

The helper installs for the current user. It registers Hamsy as an available
HAR viewer and does not replace an existing default application. On macOS,
use Get Info > Open with > Hamsy > Change All on a .har file. On Linux,
choose Hamsy in the file manager's Open With/default application settings.
On Windows, choose Hamsy in Open with and set it as the default if desired.

To refresh after moving the executable, run the same command with its new
path. Binary-only `hamsy update` keeps the launcher working at the same path;
rerun the main installer to receive launcher changes in a later release.

To remove integration without deleting Hamsy or its data:

  bash packaging/install-desktop.sh --uninstall

On Windows, invoke the same PowerShell helper with -Uninstall. Its retained
copy is under %LOCALAPPDATA%\Hamsy\desktop\integration by default, so the
downloaded archive does not need to be kept.

The prebuilt installer (`install.sh`, with `install_source.sh` retained as a
compatibility alias) downloads the binary and generates integration locally.
Pass `--no-desktop` to omit it. Source builds are explicit with
`install.sh --from-source` or `install-from-source.sh`; source `--no-ui`
installations also omit integration.

The release payload carries the macOS AppleScript and icon; the installer uses
the host's `osacompile`, `sips`, `iconutil`, `PlistBuddy`, and `codesign` to
generate the app locally. The launcher is ad-hoc signed, not Developer ID
signed or notarized. If local generation fails or macOS blocks the launcher,
use `hamsy open /path/to/session.har` in a terminal.
The launcher uses the current user's installed executable and opens the
browser; it does not contain a second copy of the Hamsy engine.
These installers do not use a developer-account signing service or
disable operating-system security policies. Local generation is not a
guarantee that downloaded software will open without security prompts.
