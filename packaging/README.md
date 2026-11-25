Hamsy HAR file integration

Keep the hamsy executable in a permanent location, then register the launcher:

  bash packaging/install-desktop.sh --binary /absolute/path/to/hamsy

The helper installs for the current user. It registers Hamsy as an available
HAR viewer and does not replace an existing default application. On macOS,
use Get Info > Open with > Hamsy > Change All on a .har file. On Linux,
choose Hamsy in the file manager's Open With/default application settings.

To refresh after moving the executable, run the same command with its new
path. Binary-only `hamsy update` keeps the launcher working at the same path;
rerun the main installer to receive launcher changes in a later release.

To remove integration without deleting Hamsy or its data:

  bash packaging/install-desktop.sh --uninstall

The source and prebuilt installers include integration by default. Pass
--no-desktop to omit it. Source --no-ui installations also omit it.

The macOS launcher is ad-hoc signed, not Developer ID signed or notarized.
macOS may require approval in Privacy & Security for downloaded launchers.
Use `hamsy open /path/to/session.har` in a terminal if the launcher is blocked.
The launcher uses the current user's installed executable and opens the
browser; it does not contain a second copy of the Hamsy engine.
