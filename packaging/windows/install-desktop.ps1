[CmdletBinding()]
param(
    [Parameter()]
    [string]$BinaryPath,

    [Parameter()]
    [switch]$Uninstall,

    # The default is the real per-user Classes hive. Tests may provide a
    # child of HKCU\Software so they never touch the user's file associations.
    [Parameter()]
    [string]$RegistryRoot = 'Software\Classes',

    [Parameter()]
    [string]$StateRoot = (Join-Path $env:LOCALAPPDATA 'Hamsy\desktop'),

    [Parameter()]
    [string]$StartMenuPath = (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Hamsy.lnk'),

    [Parameter()]
    [switch]$NoStartMenu
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    throw "Hamsy desktop integration: $Message"
}

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Path)) { Fail "$Label is required" }
    $item = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    if ($null -eq $item -or -not ($item.PSIsContainer -eq $false)) {
        Fail "$Label does not name a file: $Path"
    }
    return $item.FullName
}

function Normalize-Path([string]$Path) {
    return [IO.Path]::GetFullPath($Path)
}

function Assert-RegistryRoot([string]$Root) {
    if ([string]::IsNullOrWhiteSpace($Root) -or $Root -notmatch '^Software(?:\\|$)' -or $Root -match '(^|\\)\.\.?(?:\\|$)' -or $Root.Contains(':')) {
        Fail 'RegistryRoot must be a child of HKCU\Software and must not contain traversal or drive syntax'
    }
}

function Registry-Path([string]$Relative) {
    return "Registry::HKEY_CURRENT_USER\$RegistryRoot\$Relative"
}

function Open-WritableRegistryKey([string]$Relative) {
    return [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("$RegistryRoot\$Relative", $true)
}

function Read-RegistryValue([string]$Path, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $property = Get-ItemProperty -LiteralPath $Path -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $property) { return $null }
    return $property.$Name
}

function Set-RegistryDefault([string]$Path, [string]$Value) {
    New-Item -Path $Path -Force | Out-Null
    New-ItemProperty -LiteralPath $Path -Name '(default)' -Value $Value -PropertyType String -Force | Out-Null
}

function Ensure-Directory([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Read-State {
    $stateFile = Join-Path $StateRoot 'state.json'
    if (-not (Test-Path -LiteralPath $stateFile)) { return $null }
    try { return Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json }
    catch { Fail "cannot read installation state at $stateFile" }
}

function Write-State([string]$Path) {
    Ensure-Directory $StateRoot
    [ordered]@{
        owner = 'io.hamsy.har-launcher'
        binary = $Path
        launcher = (Join-Path $StateRoot 'HamsyHarLauncher.exe')
        registryRoot = $RegistryRoot
        progId = 'Hamsy.Har'
        startMenu = $(if ($NoStartMenu) { $null } else { $StartMenuPath })
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $StateRoot 'state.json') -Encoding UTF8
}

function CSharp-String([string]$Value) {
    return '"' + ($Value -replace '\\', '\\' -replace '"', '\"') + '"'
}

function New-HarLauncher([string]$Path) {
    $launcherPath = Join-Path $StateRoot 'HamsyHarLauncher.exe'
    $ownerFile = Join-Path $StateRoot 'launcher-owner'
    if ((Test-Path -LiteralPath $launcherPath) -and (-not (Test-Path -LiteralPath $ownerFile))) {
        Fail "refusing to replace an unrecognized launcher: $launcherPath"
    }
    $binaryLiteral = CSharp-String $Path
    $source = @"
using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;

internal static class HamsyHarLauncher {
    private const string Hamsy = $binaryLiteral;

    [STAThread]
    private static int Main(string[] args) {
        if (args == null || args.Length == 0) {
            Application.EnableVisualStyles();
            using (var dialog = new OpenFileDialog()) {
                dialog.Multiselect = true;
                dialog.Filter = "HTTP Archive (*.har)|*.har|All files (*.*)|*.*";
                dialog.Title = "Open HAR with Hamsy";
                if (dialog.ShowDialog() != DialogResult.OK) return 0;
                args = dialog.FileNames;
            }
        }
        if (!File.Exists(Hamsy)) {
            MessageBox.Show("The configured Hamsy executable could not be found:\n" + Hamsy, "Hamsy", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 2;
        }
        var command = new StringBuilder("open --");
        foreach (var arg in args) command.Append(' ').Append(Quote(arg));
        var start = new ProcessStartInfo {
            FileName = Hamsy,
            Arguments = command.ToString(),
            UseShellExecute = false,
            CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden,
            WorkingDirectory = Path.GetDirectoryName(Hamsy),
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        try {
            using (var process = Process.Start(start)) {
                var outputTask = process.StandardOutput.ReadToEndAsync();
                var errorTask = process.StandardError.ReadToEndAsync();
                process.WaitForExit();
                Task.WaitAll(outputTask, errorTask);
                var error = errorTask.Result;
                var output = outputTask.Result;
                if (process.ExitCode != 0) {
                    var detail = String.IsNullOrWhiteSpace(error) ? output : error;
                    MessageBox.Show("Hamsy could not open the HAR file(s).\n\n" + detail, "Hamsy", MessageBoxButtons.OK, MessageBoxIcon.Error);
                    return process.ExitCode;
                }
            }
            return 0;
        } catch (Exception error) {
            MessageBox.Show("Could not start Hamsy:\n\n" + error.Message, "Hamsy", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
    }

    private static string Quote(string value) {
        var result = new StringBuilder("\"");
        var slashes = 0;
        foreach (var ch in value) {
            if (ch == '\\') { slashes++; continue; }
            if (ch == '\"') { result.Append('\\', slashes * 2 + 1).Append('\"'); slashes = 0; continue; }
            result.Append('\\', slashes).Append(ch); slashes = 0;
        }
        result.Append('\\', slashes * 2).Append("\"");
        return result.ToString();
    }
}
"@
    Ensure-Directory $StateRoot
    $tempSource = Join-Path $StateRoot 'HamsyHarLauncher.cs'
    $tempAssembly = Join-Path $StateRoot 'HamsyHarLauncher.new.exe'
    Set-Content -LiteralPath $tempSource -Value $source -Encoding UTF8
    try {
        Add-Type -TypeDefinition (Get-Content -LiteralPath $tempSource -Raw) -OutputAssembly $tempAssembly -OutputType WindowsApplication -ReferencedAssemblies @('System.Windows.Forms.dll', 'System.Drawing.dll')
        Move-Item -LiteralPath $tempAssembly -Destination $launcherPath -Force
        Set-Content -LiteralPath $ownerFile -Value $owner -Encoding ASCII
    } finally {
        Remove-Item -LiteralPath $tempSource -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $tempAssembly -Force -ErrorAction SilentlyContinue
    }
    return $launcherPath
}

function New-StartMenuShortcut([string]$Path) {
    $parent = Split-Path -Parent $Path
    Ensure-Directory $parent
    if (Test-Path -LiteralPath $Path) {
        $existing = New-Object -ComObject WScript.Shell
        $shortcut = $existing.CreateShortcut($Path)
        if (-not $shortcut.TargetPath -or (Normalize-Path $shortcut.TargetPath) -ne (Normalize-Path (Join-Path $StateRoot 'HamsyHarLauncher.exe'))) {
            Fail "refusing to replace an unrecognized Start Menu shortcut: $Path"
        }
    }
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($Path)
    $shortcut.TargetPath = Join-Path $StateRoot 'HamsyHarLauncher.exe'
    $shortcut.WorkingDirectory = Split-Path -Parent $BinaryPath
    $shortcut.Description = 'Open HAR files with Hamsy'
    $shortcut.IconLocation = "$BinaryPath,0"
    $shortcut.Save()
}

function Remove-StartMenuShortcut([string]$Path, [string]$ExpectedBinary) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    try {
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($Path)
        if ($shortcut.TargetPath -and (Normalize-Path $shortcut.TargetPath) -eq (Normalize-Path $ExpectedBinary)) {
            Remove-Item -LiteralPath $Path -Force
        }
    } catch {
        # A user-owned or malformed shortcut must remain untouched.
    }
}

Assert-RegistryRoot $RegistryRoot
$classes = Registry-Path ''
$extensionKey = Registry-Path '.har'
$progIdKey = Registry-Path 'Hamsy.Har'
$openWithKey = Registry-Path '.har\OpenWithProgids'
$commandKey = Registry-Path 'Hamsy.Har\shell\open\command'
$iconKey = Registry-Path 'Hamsy.Har\DefaultIcon'
$owner = 'io.hamsy.har-launcher'

if ($Uninstall) {
    $state = Read-State
    if ($null -eq $state -or $state.owner -ne $owner) {
        Write-Output 'No Hamsy desktop integration owned by this installer.'
        exit 0
    }
    if (-not [string]::IsNullOrWhiteSpace($BinaryPath)) {
        $requested = Normalize-Path $BinaryPath
        if ((Normalize-Path ([string]$state.binary)) -ne $requested) {
            Write-Output 'Desktop integration belongs to another Hamsy installation; keeping it.'
            exit 0
        }
    }
    $expectedCommand = '"{0}" "%1"' -f ([string]$state.launcher)
    $registeredOwner = Read-RegistryValue $progIdKey 'Owner'
    $registeredCommand = Read-RegistryValue $commandKey '(default)'
    if ($registeredOwner -ne $owner -or $registeredCommand -ne $expectedCommand) {
        Write-Output 'Hamsy registry entries were changed or are not owned by this installer; keeping them.'
        exit 0
    }
    $launcherOwnerPath = Join-Path $StateRoot 'launcher-owner'
    if ((-not (Test-Path -LiteralPath $launcherOwnerPath)) -or ((Get-Content -LiteralPath $launcherOwnerPath -Raw).Trim() -ne $owner)) {
        Write-Output 'Hamsy launcher ownership marker is missing or changed; keeping integration.'
        exit 0
    }

    if (Test-Path -LiteralPath $openWithKey) {
        Remove-ItemProperty -LiteralPath $openWithKey -Name 'Hamsy.Har' -ErrorAction SilentlyContinue
    }
    # Remove only values/subkeys created by this installer. If a user added
    # values below the ProgId, preserving the key is safer than deleting them.
    Remove-ItemProperty -LiteralPath $progIdKey -Name 'Owner' -ErrorAction SilentlyContinue
    $progIdWritable = Open-WritableRegistryKey 'Hamsy.Har'
    if ($null -ne $progIdWritable) {
        try { $progIdWritable.DeleteValue('', $false) }
        finally { $progIdWritable.Close() }
    }
    if (Test-Path -LiteralPath $commandKey) { Remove-Item -LiteralPath $commandKey -Force -ErrorAction SilentlyContinue }
    $shellKey = Registry-Path 'Hamsy.Har\shell\open'
    if (Test-Path -LiteralPath $shellKey) { Remove-Item -LiteralPath $shellKey -Recurse -Force -ErrorAction SilentlyContinue }
    $shellParent = Registry-Path 'Hamsy.Har\shell'
    if (Test-Path -LiteralPath $shellParent) {
        $shellKey = Get-Item -LiteralPath $shellParent
        if ($shellKey.GetSubKeyNames().Count -eq 0 -and $shellKey.GetValueNames().Count -eq 0) {
            Remove-Item -LiteralPath $shellParent -Force -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path -LiteralPath $iconKey) { Remove-Item -LiteralPath $iconKey -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $progIdKey) {
        $remainingKey = Get-Item -LiteralPath $progIdKey
        if ($remainingKey.GetSubKeyNames().Count -eq 0 -and $remainingKey.GetValueNames().Count -eq 0) {
            Remove-Item -LiteralPath $progIdKey -Force
        }
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$state.startMenu)) {
        Remove-StartMenuShortcut ([string]$state.startMenu) ([string]$state.launcher)
    }
    Remove-Item -LiteralPath $StateRoot -Recurse -Force
    Write-Output 'Removed Hamsy HAR desktop integration.'
    exit 0
}

$BinaryPath = Normalize-Path (Resolve-ExistingFile $BinaryPath 'BinaryPath')
$existingState = Read-State
if ((Test-Path -LiteralPath $StateRoot) -and $null -eq $existingState) {
    Fail "refusing to replace unrecognized state directory: $StateRoot"
}
if ($null -ne $existingState -and $existingState.owner -ne $owner) {
    Fail "refusing to replace unrecognized state directory: $StateRoot"
}

$launcherPath = Join-Path $StateRoot 'HamsyHarLauncher.exe'
$expectedCommand = '"{0}" "%1"' -f $launcherPath
if (Test-Path -LiteralPath $progIdKey) {
    $existingOwner = Read-RegistryValue $progIdKey 'Owner'
    if ($existingOwner -ne $owner) { Fail "refusing to replace unrecognized registry ProgId: Hamsy.Har" }
    $existingCommand = Read-RegistryValue $commandKey '(default)'
    $previousCommand = if ($null -ne $existingState -and $existingState.launcher) { '"{0}" "%1"' -f ([string]$existingState.launcher) } else { $expectedCommand }
    if ($existingCommand -and $existingCommand -ne $previousCommand -and $existingCommand -ne $expectedCommand) {
        Fail 'the Hamsy Open With command was changed; restore ownership before refreshing'
    }
}
if (Test-Path -LiteralPath $openWithKey) {
    $current = Read-RegistryValue $openWithKey 'Hamsy.Har'
    if ($null -ne $current -and $current -ne '') { Fail 'refusing to replace an unrecognized Hamsy Open With entry' }
}

$stateRootInitiallyPresent = Test-Path -LiteralPath $StateRoot
try {
    $launcherPath = New-HarLauncher $BinaryPath
} catch {
    if (-not $stateRootInitiallyPresent -and $null -eq $existingState -and (Test-Path -LiteralPath $StateRoot)) {
        Remove-Item -LiteralPath $StateRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    throw
}

Ensure-Directory $classes
Set-RegistryDefault $progIdKey 'Hamsy HAR file'
New-ItemProperty -LiteralPath $progIdKey -Name 'Owner' -Value $owner -PropertyType String -Force | Out-Null
Set-RegistryDefault $iconKey "$BinaryPath,0"
Set-RegistryDefault $commandKey $expectedCommand
New-Item -Path $openWithKey -Force | Out-Null
New-ItemProperty -LiteralPath $openWithKey -Name 'Hamsy.Har' -Value '' -PropertyType String -Force | Out-Null

Write-State $BinaryPath
if (-not $NoStartMenu) { New-StartMenuShortcut $StartMenuPath }
$retained = Join-Path $StateRoot 'integration'
Ensure-Directory $retained
$retainedScript = Join-Path $retained 'install-desktop.ps1'
if ((Normalize-Path $PSCommandPath) -ne (Normalize-Path $retainedScript)) {
    Copy-Item -LiteralPath $PSCommandPath -Destination $retainedScript -Force
}
if (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'README.md')) {
    $readme = Join-Path $retained 'README.md'
    if ((Normalize-Path (Join-Path $PSScriptRoot 'README.md')) -ne (Normalize-Path $readme)) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination $readme -Force
    }
}
Write-Output "Registered Hamsy HAR integration for $BinaryPath."
Write-Output 'Existing default applications were preserved.'
