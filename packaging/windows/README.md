# Hamsy Windows integration

`install-desktop.ps1` registers Hamsy for the current user under
`HKCU\Software\Classes`. It adds Hamsy to the `.har` Open With list without
changing the user's default application. The registered command invokes the
generated launcher, which forwards selected filenames to `hamsy.exe open --`
with Windows argument quoting.

From an extracted Windows ZIP, register its binary with:

```powershell
powershell.exe -NoProfile -File packaging/windows/install-desktop.ps1 -BinaryPath "C:\path\to\hamsy.exe"
```

Run the same helper with `-Uninstall` to remove its integration.

The helper also compiles a small Windows GUI launcher with the inbox
PowerShell/.NET Framework toolchain. The launcher opens a file chooser from
the Start Menu, or forwards Open With arguments directly to Hamsy, and does
not retain a console window. It writes ownership markers and checks them
before refresh or uninstall; changed or foreign entries are left in place.
No administrator privileges, VBScript, execution-policy changes, or
certificate trust changes are required.
