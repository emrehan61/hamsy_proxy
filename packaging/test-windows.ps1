[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath,
    [Parameter(Mandatory = $true)]
    [string]$RegistryRoot,
    [string]$InstallerPath = (Join-Path $PSScriptRoot '..\install.ps1')
)

$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw "Windows packaging test: $Message" }
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { Fail $Message } }

$BinaryPath = [IO.Path]::GetFullPath($BinaryPath)
if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) { Fail "missing binary: $BinaryPath" }
$root = Join-Path ([IO.Path]::GetTempPath()) ('hamsy-windows-test-' + [Guid]::NewGuid().ToString('N'))
$stateRoot = Join-Path $root 'profile\Hamsy\desktop'
$dataDir = Join-Path $root 'profile\Hamsy\data'
$startMenu = Join-Path $root 'profile\Start Menu\Hamsy.lnk'
$installDir = Join-Path $root 'install'
$registryBase = "Registry::HKEY_CURRENT_USER\$RegistryRoot"
$extensionKey = Join-Path $registryBase '.har'
$default = 'Some.Other.HarViewer'

try {
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    New-Item -Path $extensionKey -Force | Out-Null
    New-ItemProperty -LiteralPath $extensionKey -Name '(default)' -Value $default -PropertyType String -Force | Out-Null

    & $BinaryPath --version | Out-Null
    Assert ($LASTEXITCODE -eq 0) 'hamsy --version failed'
    $har = Join-Path $root 'unsafe name & quoted.har'
    Set-Content -LiteralPath $har -Value '{"log":{"entries":[]}}' -Encoding UTF8
    & $BinaryPath open --no-open --data-dir $dataDir -- $har
    Assert ($LASTEXITCODE -eq 0) 'hamsy HAR CLI smoke test failed'

    # Build a tiny native stand-in to verify the generated launcher's Windows
    # argument quoting without opening a browser or touching a real profile.
    $capture = Join-Path $root 'launcher-argv.txt'
    $fakeSource = @'
using System;
using System.IO;
class FakeHamsy {
    static void Main(string[] args) {
        Console.Out.Write(new string('o', 131072));
        Console.Error.Write(new string('e', 131072));
        File.WriteAllLines(Environment.GetEnvironmentVariable("HAMSY_FAKE_CAPTURE"), args);
    }
}
'@
    $fakeBinary = Join-Path $root 'fake hamsy & cli.exe'
    $fakeSourcePath = Join-Path $root 'FakeHamsy.cs'
    $fakeCompileScript = Join-Path $root 'compile-fake.ps1'
    Set-Content -LiteralPath $fakeSourcePath -Value $fakeSource -Encoding UTF8
    @'
param([string]$SourcePath, [string]$OutputPath)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition (Get-Content -LiteralPath $SourcePath -Raw) -OutputAssembly $OutputPath -OutputType ConsoleApplication
'@ | Set-Content -LiteralPath $fakeCompileScript -Encoding UTF8
    & powershell.exe -NoProfile -File $fakeCompileScript -SourcePath $fakeSourcePath -OutputPath $fakeBinary
    Assert ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $fakeBinary)) 'Windows PowerShell could not compile the fake CLI'
    $env:HAMSY_FAKE_CAPTURE = $capture

    $helper = Join-Path $PSScriptRoot 'windows\install-desktop.ps1'
    & powershell.exe -NoProfile -File $helper -BinaryPath $fakeBinary -RegistryRoot $RegistryRoot -StateRoot $stateRoot -StartMenuPath $startMenu
    Assert ($LASTEXITCODE -eq 0) 'desktop helper failed'
    $registeredDefault = (Get-ItemProperty -LiteralPath $extensionKey).'(default)'
    Assert ($registeredDefault -eq $default) 'existing .har default was changed'
    $progId = Join-Path $registryBase 'Hamsy.Har'
    $command = (Get-ItemProperty -LiteralPath (Join-Path $progId 'shell\open\command')).'(default)'
    Assert ($command -match 'HamsyHarLauncher\.exe" "%1"$') 'Open With command does not target the generated launcher'
    Assert ((Get-ItemProperty -LiteralPath $progId).Owner -eq 'io.hamsy.har-launcher') 'ProgId owner marker missing'
    Assert (Test-Path -LiteralPath (Join-Path $stateRoot 'HamsyHarLauncher.exe')) 'native launcher was not generated'
    Assert (Test-Path -LiteralPath $startMenu) 'Start Menu shortcut was not generated'
    $argv = @(
        (Join-Path $root 'first name & unicode-東京.har'),
        (Join-Path $root 'second%quoted.har')
    )
    foreach ($path in $argv) { Set-Content -LiteralPath $path -Value '{}' -Encoding UTF8 }
    $launcherProcess = Start-Process -FilePath (Join-Path $stateRoot 'HamsyHarLauncher.exe') -ArgumentList ($argv | ForEach-Object { '"{0}"' -f $_ }) -PassThru -WindowStyle Hidden
    if (-not $launcherProcess.WaitForExit(15000)) {
        Stop-Process -Id $launcherProcess.Id -Force -ErrorAction SilentlyContinue
        Fail 'native launcher did not exit within 15 seconds'
    }
    Assert ($launcherProcess.ExitCode -eq 0) "native launcher exited with code $($launcherProcess.ExitCode)"
    $captured = @(Get-Content -LiteralPath $capture)
    Assert ($captured.Count -eq 4 -and $captured[0] -eq 'open' -and $captured[1] -eq '--' -and $captured[2] -eq $argv[0] -and $captured[3] -eq $argv[1]) 'launcher did not preserve HAR filenames'

    # A wrong binary may never remove another installation's integration.
    $other = Join-Path $root 'other.exe'
    Copy-Item -LiteralPath $BinaryPath -Destination $other
    & powershell.exe -NoProfile -File $helper -Uninstall -BinaryPath $other -RegistryRoot $RegistryRoot -StateRoot $stateRoot -StartMenuPath $startMenu
    Assert ((Test-Path -LiteralPath $progId) -and (Test-Path -LiteralPath $startMenu)) 'ownership guard removed a different installation'

    & powershell.exe -NoProfile -File $helper -Uninstall -BinaryPath $fakeBinary -RegistryRoot $RegistryRoot -StateRoot $stateRoot -StartMenuPath $startMenu
    Assert (-not (Test-Path -LiteralPath $progId)) 'uninstall left the owned ProgId'
    Assert (-not (Test-Path -LiteralPath $startMenu)) 'uninstall left the owned Start Menu shortcut'
    Assert ((Get-ItemProperty -LiteralPath $extensionKey).'(default)' -eq $default) 'uninstall changed the existing .har default'

    # Exercise the release installer against a locally created, checksummed
    # ZIP so this test never downloads or writes to a runner user's profile.
    $stage = Join-Path $root 'archive-stage'
    New-Item -ItemType Directory -Path (Join-Path $stage 'packaging') -Force | Out-Null
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $stage 'hamsy.exe')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'windows') -Destination (Join-Path $stage 'packaging') -Recurse
    $archive = Join-Path $root 'hamsy-x86_64-pc-windows-msvc.zip'
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $sums = Join-Path $root 'sha256sums.txt'
    Set-Content -LiteralPath $sums -Value "$hash  ./$([IO.Path]::GetFileName($archive))" -Encoding ASCII
    $archiveRegistry = "$RegistryRoot-Installer"
    & powershell.exe -NoProfile -File $InstallerPath -ArchivePath $archive -ChecksumsPath $sums -InstallDir $installDir -StateRoot (Join-Path $root 'installer-state') -StartMenuPath (Join-Path $root 'installer.lnk') -RegistryRoot $archiveRegistry
    Assert ($LASTEXITCODE -eq 0) 'root PowerShell installer failed'
    Assert (Test-Path -LiteralPath (Join-Path $installDir 'hamsy.exe')) 'root installer did not install hamsy.exe'
    & powershell.exe -NoProfile -File $InstallerPath -Uninstall -InstallDir $installDir -StateRoot (Join-Path $root 'installer-state') -StartMenuPath (Join-Path $root 'installer.lnk') -RegistryRoot $archiveRegistry
    Assert ($LASTEXITCODE -eq 0) 'root PowerShell uninstaller failed'
    Assert (-not (Test-Path -LiteralPath (Join-Path $installDir 'hamsy.exe'))) 'root uninstaller left hamsy.exe'
    Write-Output 'Windows CLI smoke, native launcher generation, Open With ownership, isolated uninstall, and release installer checks passed.'
} finally {
    $env:HAMSY_FAKE_CAPTURE = $null
    if (Test-Path -LiteralPath $dataDir) {
        & $BinaryPath open --stop --data-dir $dataDir 2>$null
    }
    Remove-Item -LiteralPath $registryBase -Recurse -Force -ErrorAction SilentlyContinue
    if ($archiveRegistry) { Remove-Item -LiteralPath ("Registry::HKEY_CURRENT_USER\$archiveRegistry") -Recurse -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue }
}
