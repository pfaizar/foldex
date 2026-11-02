# Record the website's app loops from the real Foldex window.
#
#   powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1 -Only palette
#   powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1 -Only search -NoPublish
#
# Edit scenes.ps1 to change what each clip demonstrates. The script stages a
# fictional workspace, records one window with PrintWindow, encodes a seamless
# loop, and regenerates the keycast timings in web/src/data/loops.ts.

[CmdletBinding()]
param(
    [string[]]$Only = @(),
    [switch]$SkipEncode,
    [switch]$NoPublish,
    [switch]$Build,
    [switch]$KeepApp,
    [int]$Fps = 25,
    [string]$DemoRoot = 'C:\FoldexDemo',
    [string]$AppExe = '',
    [string]$Ffmpeg = '',
    [string]$Ffprobe = '',
    [string]$Python = 'python',
    [string]$Scenes = '',
    [string]$Repo = ''
)

$ErrorActionPreference = 'Stop'

if (-not $Repo) {
    $Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
}
if (-not $Scenes) { $Scenes = Join-Path $PSScriptRoot 'scenes.ps1' }
if (-not $AppExe) { $AppExe = Join-Path $Repo 'target\release\foldex.exe' }
if (-not $Ffmpeg) {
    $cmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($cmd) { $Ffmpeg = $cmd.Source }
    else { $Ffmpeg = 'D:\Programs\FFMPeg\bin\ffmpeg.exe' }
}
if (-not $Ffprobe) {
    $cmd = Get-Command ffprobe -ErrorAction SilentlyContinue
    if ($cmd) { $Ffprobe = $cmd.Source }
    else { $Ffprobe = Join-Path (Split-Path $Ffmpeg) 'ffprobe.exe' }
}

$Work = Join-Path $Repo 'target\demo-recording'
$Raw = Join-Path $Work 'raw'
$Out = Join-Path $Work 'out'
$Web = Join-Path $Repo 'web'
$DemoExe = Join-Path $DemoRoot 'app\Foldex.exe'

function Get-SceneValue($Scene, [string]$Name, $Default) {
    if ($Scene.ContainsKey($Name) -and $null -ne $Scene[$Name]) { return $Scene[$Name] }
    return $Default
}

function Stop-DemoApp {
    $name = [IO.Path]::GetFileNameWithoutExtension($DemoExe)
    Get-Process -Name $name -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $DemoExe } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 800
}

function Get-DemoWindow {
    $name = [IO.Path]::GetFileNameWithoutExtension($DemoExe)
    for ($i = 0; $i -lt 40; $i++) {
        $p = Get-Process -Name $name -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $DemoExe -and $_.MainWindowHandle -ne 0 } |
            Select-Object -First 1
        if ($p) { return $p.MainWindowHandle }
        Start-Sleep -Milliseconds 250
    }
    throw "demo window not found; could not start $DemoExe"
}

if (-not (Test-Path $Ffmpeg)) { throw "ffmpeg not found: $Ffmpeg" }
if (-not (Test-Path $Ffprobe)) { throw "ffprobe not found: $Ffprobe" }
if ($Build) {
    Push-Location $Repo
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path $AppExe)) {
    throw "app not built: $AppExe`nRun 'cargo build --release' or pass -Build / -AppExe."
}

New-Item -ItemType Directory -Force $Raw, $Out | Out-Null

. (Join-Path $PSScriptRoot 'Recorder.ps1')
$global:Ffmpeg = $Ffmpeg
$global:RecFps = $Fps

$allScenes = @(& $Scenes)
if ($allScenes.Count -eq 0) { throw "no scenes defined in $Scenes" }
$allScenes = @($allScenes | Where-Object { (Get-SceneValue $_ 'Enabled' $true) })

$names = @($allScenes | ForEach-Object { $_.Name })
$duplicate = $names | Group-Object | Where-Object { $_.Count -gt 1 }
if ($duplicate) { throw "duplicate scene name: $($duplicate.Name -join ', ')" }
$bad = @($names | Where-Object { $_ -notmatch '^[a-z][a-z0-9_]*$' })
if ($bad.Count -gt 0) { throw "scene names must be lower-case identifiers: $($bad -join ', ')" }

if ($Only.Count -gt 0) {
    $missing = @($Only | Where-Object { $names -notcontains $_ })
    if ($missing.Count -gt 0) { throw "unknown scene(s): $($missing -join ', '). Available: $($names -join ', ')" }
    $selected = @($allScenes | Where-Object { $Only -contains $_.Name })
} else {
    $selected = $allScenes
}
if ($selected.Count -eq 0) { throw 'nothing selected' }
$selectedNames = @($selected | ForEach-Object { $_.Name })

Write-Host "demo recorder: $($selectedNames.Count) take(s), $Fps fps"
Write-Host "  app     $DemoExe"
Write-Host "  demo    $DemoRoot"
Write-Host "  ffmpeg  $Ffmpeg"

$manifest = [ordered]@{ fps = $Fps; clips = @() }

foreach ($scene in $allScenes) {
    $name = $scene.Name
    $crop = [string](Get-SceneValue $scene 'Crop' '')
    $width = [int](Get-SceneValue $scene 'Width' 0)
    $fade = [double](Get-SceneValue $scene 'Fade' 0.4)
    $publish = [bool](Get-SceneValue $scene 'Publish' $true)

    $manifest.clips += [ordered]@{
        name     = $name
        crop     = $(if ($crop) { $crop } else { $null })
        width    = $width
        fade     = $fade
        publish  = $publish
        recorded = ($selectedNames -contains $name)
    }

    if ($selectedNames -notcontains $name) { continue }

    Write-Host ''
    Write-Host "recording $name" -ForegroundColor Cyan

    $theme = [string](Get-SceneValue $scene 'Theme' 'dark')
    Stop-DemoApp
    & $Python -I (Join-Path $PSScriptRoot 'demo.py') reset `
        --root $DemoRoot --repo $Repo --app-exe $AppExe --theme $theme
    if ($LASTEXITCODE -ne 0) { throw "could not stage the demo workspace (exit $LASTEXITCODE)" }

    if ([bool](Get-SceneValue $scene 'SinglePane' $false)) {
        $sessionPath = Join-Path $DemoRoot 'app\foldex-session.json'
        $j = Get-Content -Raw $sessionPath | ConvertFrom-Json
        $j.workspace.tabs[0].right = $null
        [IO.File]::WriteAllText($sessionPath, ($j | ConvertTo-Json -Depth 20))
    }

    Start-Process -FilePath $DemoExe -WorkingDirectory (Split-Path $DemoExe) | Out-Null
    $h = Get-DemoWindow

    $win = Get-SceneValue $scene 'Window' @{ X = 120; Y = 60; Width = 1300; Height = 780 }
    [Rec]::ShowWindow($h, 9) | Out-Null
    [Rec]::MoveWindow($h, [int]$win.X, [int]$win.Y, [int]$win.Width, [int]$win.Height, $true) | Out-Null
    Wait 1500
    if (-not [Rec]::Focus($h)) { throw "could not focus the demo window for '$name'" }

    Set-RecordTarget $h
    if ($scene.Setup) { & $scene.Setup }

    $capture = Get-SceneValue $scene 'Capture' $null
    $capW = 0; $capH = 0; $fill = 0
    if ($capture) {
        $capW = [int](Get-SceneValue $capture 'Width' 0)
        $capH = [int](Get-SceneValue $capture 'Height' 0)
        $fillValue = Get-SceneValue $capture 'Fill' 0
        $fill = if ($fillValue -is [string]) { [Convert]::ToInt32($fillValue, 16) } else { [int]$fillValue }
    }

    $rawPath = Join-Path $Raw "$name.mkv"
    $keyPath = Join-Path $Raw "$name.keys.json"
    $completed = $false

    try {
        Start-Recording $rawPath $capW $capH $fill
        & $scene.Steps
        $completed = $true
    } finally {
        try {
            try {
                Stop-Recording
            } catch {
                if ($completed) { throw }
                Write-Warning "recorder cleanup for '$name': $($_.Exception.Message)"
            }
            if ($completed) {
                Save-KeyLog $keyPath
                if ($scene.After) { & $scene.After }
            }
        } finally {
            if (-not $KeepApp) { Stop-DemoApp }
        }
    }

    Write-Host ("  {0} frames, {1} dropped, {2:N2}s, capture {3}x{4}" -f [Rec]::Frames, [Rec]::Dropped, ([Rec]::Frames / [double]$Fps), [Rec]::Width, [Rec]::Height)
}

$manifest | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $Work 'manifest.json') -Encoding utf8

if ($SkipEncode) {
    Write-Host ''
    Write-Host "raw takes kept in $Raw" -ForegroundColor Green
    exit 0
}

Write-Host ''
Write-Host 'encoding loops' -ForegroundColor Cyan
$encodeArgs = @(
    '-I', (Join-Path $PSScriptRoot 'encode.py'),
    '--work', $Work,
    '--web', $Web,
    '--ffmpeg', $Ffmpeg,
    '--ffprobe', $Ffprobe,
    '--fps', $Fps
)
if ($Only.Count -gt 0) { $encodeArgs += @('--only', ($selectedNames -join ',')) }
if ($NoPublish) { $encodeArgs += '--no-publish' }

& $Python @encodeArgs
if ($LASTEXITCODE -ne 0) { throw "encode failed with exit code $LASTEXITCODE" }

Write-Host ''
Write-Host "raw takes   $Raw"
Write-Host "encoded     $Out"
if (-not $NoPublish) {
    Write-Host "website     $(Join-Path $Web 'public\media')"
    Write-Host "keycast     $(Join-Path $Web 'src\data\loops.ts')"
}
Write-Host 'done' -ForegroundColor Green
