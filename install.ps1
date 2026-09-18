[CmdletBinding()]
param(
    [string]$Version,
    [string]$Repository = 'emrehan61/hamsy_proxy',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Hamsy\bin'),
    [string]$BinaryPath,
    [string]$ArchivePath,
    [string]$ChecksumsPath,
    [string]$RegistryRoot = 'Software\Classes',
    [string]$StateRoot = (Join-Path $env:LOCALAPPDATA 'Hamsy\desktop'),
    [string]$StartMenuPath = (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Hamsy.lnk'),
    [switch]$NoDesktop,
    [switch]$NoStartMenu,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$script:Owner = 'io.hamsy.installer'
$script:Target = 'x86_64-pc-windows-msvc'

function Fail([string]$Message) { throw "Hamsy installer: $Message" }
function Full([string]$Path) { return [IO.Path]::GetFullPath($Path) }
function Ensure-Directory([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { New-Item -ItemType Directory -Path $Path -Force | Out-Null }
}
function Get-StatePath { return (Join-Path $InstallDir 'install-state.json') }
function Read-InstallState {
    $path = Get-StatePath
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    try { return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json }
    catch { Fail "cannot read installer state at $path" }
}

function Get-ExpectedHash([string]$Path, [string]$ArchiveName) {
    $hashes = @()
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.TrimStart().StartsWith('#')) { continue }
        if ($line -notmatch '^\s*([0-9a-fA-F]{64})\s+(.+?)\s*$') {
            Fail "malformed checksum line in $Path"
        }
        $name = $Matches[2].Trim()
        if ($name.StartsWith('*')) { $name = $name.Substring(1) }
        $name = $name -replace '^\./', ''
        $name = $name -replace '/', '\\'
        if ($name -eq $ArchiveName) { $hashes += $Matches[1].ToLowerInvariant() }
    }
    if ($hashes.Count -ne 1) { Fail "sha256sums.txt must contain exactly one entry for $ArchiveName" }
    return $hashes[0]
}

function Verify-Archive([string]$Archive, [string]$Sums) {
    $name = Split-Path -Leaf $Archive
    $expected = Get-ExpectedHash $Sums $name
    $actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Fail "checksum mismatch for $name (expected $expected, got $actual)" }
    Write-Output "Checksum verified for $name."
}

function Assert-SafeZip([string]$Archive) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName -replace '/', '\\'
            if ([IO.Path]::IsPathRooted($name) -or $name -match '(^|\\)\.\.?(?:\\|$)') {
                Fail "archive contains an unsafe path: $($entry.FullName)"
            }
        }
    } finally { $zip.Dispose() }
}

function Download([string]$Url, [string]$Destination) {
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Destination
    } catch { Fail "download failed for $Url`: $($_.Exception.Message)" }
}

function Invoke-Desktop([string]$Script, [switch]$Remove) {
    $helperArgs = @('-NoProfile', '-File', $Script,
              '-RegistryRoot', $RegistryRoot, '-StateRoot', $StateRoot)
    if (-not [string]::IsNullOrWhiteSpace($StartMenuPath)) { $helperArgs += @('-StartMenuPath', $StartMenuPath) }
    if ($NoStartMenu) { $helperArgs += '-NoStartMenu' }
    if ($Remove) { $helperArgs += '-Uninstall'; if ($BinaryPath) { $helperArgs += @('-BinaryPath', $BinaryPath) } }
    else { $helperArgs += @('-BinaryPath', $BinaryPath) }
    # Windows PowerShell 5.1 is intentional: it supplies the inbox .NET
    # Framework compiler and Windows Forms used by the generated launcher.
    # The host's normal execution policy applies; no bypass is requested.
    & powershell.exe @helperArgs
    if ($LASTEXITCODE -ne 0) { Fail 'desktop integration helper failed' }
}

if ($env:OS -ne 'Windows_NT') { Fail 'this installer runs on Windows only' }
if ($Repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { Fail 'Repository must be OWNER/NAME' }
if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64' -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    Fail 'this release currently supports Windows x64 only; ARM64 and 32-bit Windows are not supported'
}

$state = Read-InstallState
if ($null -ne $state -and $state.owner -ne $Owner) { Fail "refusing to replace an unrecognized installation state at $(Get-StatePath)" }

# Supplying BinaryPath to uninstall is explicitly registration-only. It can
# remove a matching HAR integration, but must never remove the release binary
# in InstallDir, even when that binary has an installer state file.
if ($Uninstall -and $PSBoundParameters.ContainsKey('BinaryPath')) {
    $BinaryPath = Full $BinaryPath
    $desktop = Join-Path $StateRoot 'integration\install-desktop.ps1'
    if (-not (Test-Path -LiteralPath $desktop)) { $desktop = Join-Path $InstallDir 'packaging\windows\install-desktop.ps1' }
    if (-not (Test-Path -LiteralPath $desktop)) { $desktop = Join-Path $PSScriptRoot 'packaging\windows\install-desktop.ps1' }
    if (Test-Path -LiteralPath $desktop) {
        Invoke-Desktop $desktop -Remove
    } else {
        Write-Output 'No Hamsy desktop integration helper was found.'
    }
    exit 0
}

if ($Uninstall) {
    if ($null -eq $state -or $state.owner -ne $Owner) {
        Write-Output 'No Hamsy installation owned by this installer.'
        if (-not $NoDesktop -and (Test-Path -LiteralPath $StateRoot)) {
            $desktop = Join-Path $StateRoot 'integration\install-desktop.ps1'
            if (-not (Test-Path -LiteralPath $desktop)) { $desktop = Join-Path $PSScriptRoot 'packaging\windows\install-desktop.ps1' }
            if (Test-Path -LiteralPath $desktop) {
                & powershell.exe -NoProfile -File $desktop -Uninstall -RegistryRoot $RegistryRoot -StateRoot $StateRoot -StartMenuPath $StartMenuPath
                if ($LASTEXITCODE -ne 0) { Fail 'desktop integration uninstaller failed' }
            }
        }
        exit 0
    }
    if (-not $NoDesktop) {
        $desktopScript = Join-Path $InstallDir 'packaging\windows\install-desktop.ps1'
        $installedPath = Full (Join-Path $InstallDir 'hamsy.exe')
        $ownedStatePath = Full ([string]$state.binary)
        if ($ownedStatePath -ne $installedPath) { Fail 'installer state points outside its owned install path' }
        $BinaryPath = $installedPath
        if (Test-Path -LiteralPath $desktopScript) { Invoke-Desktop $desktopScript -Remove }
    }
    $ownedBinary = Full ([string]$state.binary)
    $installedPath = Full (Join-Path $InstallDir 'hamsy.exe')
    if ((Test-Path -LiteralPath $ownedBinary) -and ((Get-Item -LiteralPath $ownedBinary).FullName -eq $installedPath)) {
        Remove-Item -LiteralPath $ownedBinary -Force
    }
    if ($state.packaging -and (Test-Path -LiteralPath ([string]$state.packaging))) {
        Remove-Item -LiteralPath ([string]$state.packaging) -Recurse -Force
    }
    Remove-Item -LiteralPath (Get-StatePath) -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $InstallDir) {
        $remaining = Get-ChildItem -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue
        if (@($remaining).Count -eq 0) { Remove-Item -LiteralPath $InstallDir -Force }
    }
    Write-Output 'Removed Hamsy installation owned by this installer.'
    exit 0
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ('hamsy-install-' + [Guid]::NewGuid().ToString('N'))
Ensure-Directory $temp
try {
    if ($BinaryPath) {
        if ($ArchivePath) { Fail 'BinaryPath and ArchivePath cannot be used together' }
        $BinaryPath = Full $BinaryPath
        if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) { Fail "BinaryPath does not name a file: $BinaryPath" }
    } else {
        if ($ArchivePath -and -not $ChecksumsPath) { Fail 'ChecksumsPath is required with ArchivePath; downloads are always verified' }
        if (-not $ArchivePath) {
            $archiveName = "hamsy-$script:Target.zip"
            $tag = if ($Version -match '^v') { $Version } else { 'v' + $Version }
            $releasePath = if ($Version) { 'download/' + $tag } else { 'latest/download' }
            $base = "https://github.com/$Repository/releases/$releasePath"
            $ArchivePath = Join-Path $temp $archiveName
            $ChecksumsPath = Join-Path $temp 'sha256sums.txt'
            Write-Output "Downloading $archiveName..."
            Download "$base/$archiveName" $ArchivePath
            Download "$base/sha256sums.txt" $ChecksumsPath
        } else {
            $ArchivePath = Full $ArchivePath
            $ChecksumsPath = Full $ChecksumsPath
        }
        if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf) -or -not (Test-Path -LiteralPath $ChecksumsPath -PathType Leaf)) { Fail 'archive and checksum files must exist' }
        Verify-Archive $ArchivePath $ChecksumsPath
        Assert-SafeZip $ArchivePath
        $extract = Join-Path $temp 'extract'
        Expand-Archive -LiteralPath $ArchivePath -DestinationPath $extract -Force
        $BinaryPath = Join-Path $extract 'hamsy.exe'
        if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) { Fail 'release ZIP does not contain hamsy.exe at its root' }
        $BinaryPath = Full $BinaryPath
    }

    # A caller-supplied binary is a registration-only mode. This supports
    # developers who keep a locally built binary outside the release install.
    if ($PSBoundParameters.ContainsKey('BinaryPath') -and -not $ArchivePath) {
        $desktopScript = Join-Path $PSScriptRoot 'packaging\windows\install-desktop.ps1'
        if (-not $NoDesktop) {
            if (-not (Test-Path -LiteralPath $desktopScript)) { Fail 'packaging/windows/install-desktop.ps1 is missing' }
            Invoke-Desktop $desktopScript
        }
        Write-Output "Using custom Hamsy binary at $BinaryPath."
        exit 0
    }

    $destinationDir = Full $InstallDir
    $destination = Join-Path $destinationDir 'hamsy.exe'
    if ((Test-Path -LiteralPath $destination) -and ($null -eq $state)) { Fail "refusing to replace an unowned existing installation: $destination" }
    Ensure-Directory $destinationDir
    $stagedDestination = Join-Path $destinationDir ('hamsy.exe.' + [Guid]::NewGuid().ToString('N') + '.tmp')
    Copy-Item -LiteralPath $BinaryPath -Destination $stagedDestination -Force
    try { Move-Item -LiteralPath $stagedDestination -Destination $destination -Force }
    catch { Remove-Item -LiteralPath $stagedDestination -Force -ErrorAction SilentlyContinue; Fail "could not replace $destination; close any running Hamsy process and try again" }
    if (-not $ArchivePath) { $ArchivePath = '' }
    $packaging = if ($extract) { Join-Path $extract 'packaging\windows' } else { '' }
    if ($packaging -and (Test-Path -LiteralPath $packaging)) {
        $targetPackaging = Join-Path $destinationDir 'packaging\windows'
        Ensure-Directory (Split-Path -Parent $targetPackaging)
        Ensure-Directory $targetPackaging
        Copy-Item -Path (Join-Path $packaging '*') -Destination $targetPackaging -Recurse -Force
    }
    $binaryHash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
    [ordered]@{ owner = $Owner; binary = $destination; binaryHash = $binaryHash; packaging = $targetPackaging; archive = $ArchivePath; repository = $Repository } |
        ConvertTo-Json | Set-Content -LiteralPath (Get-StatePath) -Encoding UTF8
    if (-not $NoDesktop) {
        $desktopScript = Join-Path $destinationDir 'packaging\windows\install-desktop.ps1'
        if (-not (Test-Path -LiteralPath $desktopScript)) { Fail 'release ZIP does not contain packaging/windows/install-desktop.ps1' }
        $BinaryPath = $destination
        Invoke-Desktop $desktopScript
    }
    Write-Output "Installed Hamsy to $destination."
} finally {
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue }
}
