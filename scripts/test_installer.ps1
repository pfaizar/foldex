# This mutates a disposable Windows CI user's installer registrations and PATH.
# It intentionally never starts the graphical application or contacts releases.
param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [string]$Iscc = 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
    throw 'Installer smoke tests are restricted to a disposable GitHub Actions runner.'
}
$uninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Foldex.Desktop_is1'
$existing = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($uninstallKey)
if ($existing) { $existing.Dispose(); throw 'Refusing to overwrite an existing Foldex installation.' }
$desktopLink = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'Foldex.lnk'
$startLink = Join-Path ([Environment]::GetFolderPath('Programs')) 'Foldex.lnk'
if ((Test-Path $desktopLink) -or (Test-Path $startLink)) { throw 'A Foldex shortcut already exists.' }
$environmentKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
$originalPath = $environmentKey.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$originalKind = if ($null -ne $originalPath) { $environmentKey.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
$testRoot = Join-Path $env:RUNNER_TEMP ('foldex-installer-' + [Guid]::NewGuid().ToString('N'))
$stage = Join-Path $testRoot 'stage'
$installDir = Join-Path $testRoot 'Custom location Ω\Foldex'
$otherDir = Join-Path $testRoot 'Do not create a second installation'
$dataDir = Join-Path $env:LOCALAPPDATA 'Foldex\data'
$sentinel = Join-Path $dataDir 'installer-smoke-sentinel.txt'
if (Test-Path $sentinel) { throw 'The installer test sentinel already exists.' }

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Wait-SetupIdle {
    # Inno's uninstaller can keep its cleanup process alive after its parent exits.
    # Wait for that process to release SetupMutex before starting the next setup.
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        [Threading.Mutex]$remaining = $null
        if (-not [Threading.Mutex]::TryOpenExisting('Global\Foldex.Setup', [ref]$remaining)) { return }
        $remaining.Dispose()
        if ($timer.ElapsedMilliseconds -ge 15000) { throw 'Setup cleanup did not release its mutex.' }
        Start-Sleep -Milliseconds 100
    }
}
function Run-Setup([string]$Executable, [string[]]$Extra, [bool]$Success = $true) {
    $log = Join-Path $testRoot ('setup-' + [Guid]::NewGuid().ToString('N') + '.log')
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART', "/LOG=`"$log`"") + $Extra
    $process = Start-Process -FilePath $Executable -ArgumentList $arguments -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(90000)) { throw "Setup timed out. Inspect $log" }
    if ($Success) {
        Assert-True ($process.ExitCode -eq 0) "Setup failed with exit code $($process.ExitCode). Inspect $log"
        Wait-SetupIdle
    }
    else { Assert-True ($process.ExitCode -ne 0) 'A blocked setup unexpectedly succeeded.' }
}
function Installed-Version {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($uninstallKey)
    if (-not $key) { return $null }
    try { return $key.GetValue('DisplayVersion') } finally { $key.Dispose() }
}
function Build-Setup([string]$Version) {
    $output = Join-Path $testRoot "Foldex-$Version-windows-x64-setup.exe"
    & $Iscc "/DAppVersion=$Version" "/DSourceDir=$stage" "/DReleaseDir=$testRoot" packaging/windows/foldex.iss | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'The Inno Setup script failed to compile.' }
    return $output
}
function Remove-Installation {
    $uninstaller = Join-Path $installDir 'unins000.exe'
    if (Test-Path $uninstaller) { Run-Setup $uninstaller @() }
}

try {
    New-Item -ItemType Directory -Path $stage, (Join-Path $stage 'licenses'), $dataDir -Force | Out-Null
    Set-Content -LiteralPath $sentinel -Value 'preserve personal data'
    Copy-Item -LiteralPath $Binary -Destination (Join-Path $stage 'Foldex.exe')
    Copy-Item -LiteralPath theme.toml, LICENSE -Destination $stage
    Copy-Item packaging/windows/installed-README.md (Join-Path $stage 'README.md')
    Set-Content (Join-Path $stage 'THIRD-PARTY-NOTICES.md') 'Smoke test fixture; never distributed.'
    Set-Content (Join-Path $stage 'licenses\fixture.txt') 'Smoke test fixture'
    Set-Content (Join-Path $stage 'foldex-installed.ini') '[Installation]'
    $v1 = Build-Setup '91.0.0'
    $rc2 = Build-Setup '91.1.0-rc.2'
    $rc10 = Build-Setup '91.1.0-rc.10'
    $stable = Build-Setup '91.1.0'

    Run-Setup $v1 @("/DIR=`"$installDir`"", '/TASKS=desktopicon,addtopath')
    Assert-True ((Installed-Version) -eq '91.0.0') 'Fresh install registration is missing.'
    Assert-True (Test-Path (Join-Path $installDir 'Foldex.exe')) 'Custom installation directory was ignored.'
    Assert-True ((Test-Path $desktopLink) -and (Test-Path $startLink)) 'Shortcuts were not created.'
    $path = $environmentKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    Assert-True (($path -split ';') -contains $installDir) 'The selected PATH entry is missing.'
    Assert-True ($environmentKey.GetValueKind('Path') -eq $originalKind) 'The PATH registry type changed.'

    $mutexDirectory = [regex]::Replace($installDir, '[A-Z]', { param($match) $match.Value.ToLowerInvariant() })
    $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::Unicode.GetBytes($mutexDirectory))).ToLowerInvariant()
    $mutex = [Threading.Mutex]::new($false, "Global\Foldex.Running.$hash")
    try {
        Run-Setup $rc2 @("/DIR=`"$installDir`"") $false
        Run-Setup (Join-Path $installDir 'unins000.exe') @() $false
        Assert-True ((Installed-Version) -eq '91.0.0') 'Running-app protection changed the installation.'
    } finally { $mutex.Dispose() }
    $mutex = [Threading.Mutex]::new($false, 'Global\Foldex.Setup')
    try { Run-Setup $rc2 @("/DIR=`"$installDir`"") $false } finally { $mutex.Dispose() }

    # Reuse the saved location even when a second installer supplies another DIR.
    Run-Setup $rc2 @("/DIR=`"$otherDir`"")
    Assert-True (-not (Test-Path $otherDir)) 'Upgrade created a duplicate installation.'
    Run-Setup $rc10 @('/UPDATE=1', '/WAITPID=2147483647')
    Assert-True ((Installed-Version) -eq '91.1.0-rc.10') 'Numeric prerelease ordering failed.'
    Run-Setup $rc2 @() $false
    Run-Setup $stable @()
    Run-Setup $rc10 @() $false
    Run-Setup $v1 @() $false
    Run-Setup $stable @() # Same-version repair.
    Assert-True ((Installed-Version) -eq '91.1.0') 'Downgrade or repair broke the installed version.'
    Assert-True (Test-Path $sentinel) 'An upgrade removed personal data.'
    $path = $environmentKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    Assert-True (@($path -split ';' | Where-Object { $_ -eq $installDir }).Count -eq 1) 'Repair duplicated the PATH entry.'
    $unrelated = Join-Path $testRoot 'Unrelated application'
    $environmentKey.SetValue('Path', "$path;$unrelated", $originalKind)
    Remove-Installation
    Assert-True ($null -eq (Installed-Version)) 'Uninstall registration remains.'
    Assert-True (-not (Test-Path (Join-Path $installDir 'Foldex.exe'))) 'Uninstall left the program behind.'
    Assert-True (-not ((Test-Path $desktopLink) -or (Test-Path $startLink))) 'Uninstall left shortcuts behind.'
    Assert-True (Test-Path $sentinel) 'Uninstall removed personal data.'
    $path = $environmentKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    Assert-True (-not (($path -split ';') -contains $installDir)) 'Uninstall left its owned PATH entry behind.'
    Assert-True (($path -split ';') -contains $unrelated) 'Uninstall removed an unrelated PATH entry.'
    $expectedPath = if ([string]::IsNullOrEmpty($originalPath)) { $unrelated } else { "$originalPath;$unrelated" }
    Assert-True ($path -ceq $expectedPath) 'Uninstall changed unrelated PATH text or empty segments.'

    # A PATH entry that predates Setup is never owned or deleted by the installer.
    $environmentKey.SetValue('Path', "$path;$installDir", $originalKind)
    Run-Setup $v1 @("/DIR=`"$installDir`"", '/TASKS=addtopath')
    Remove-Installation
    $path = $environmentKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    Assert-True (($path -split ';') -contains $installDir) 'Uninstall deleted a pre-existing PATH entry.'
    Write-Host 'Installer smoke tests passed.'
} finally {
    try { Remove-Installation } finally {
        if ($null -eq $originalPath) { $environmentKey.DeleteValue('Path', $false) }
        else { $environmentKey.SetValue('Path', $originalPath, $originalKind) }
        $environmentKey.Dispose()
        Remove-Item -LiteralPath $sentinel -Force -ErrorAction SilentlyContinue
        Write-Host "Installer test logs: $testRoot"
    }
}
