# App loop recorder

Records the website's product clips (`palette`, `search`, `navigate`) from the
real Foldex app, then encodes them as seamless loops and regenerates the
keycast timings used by `web/src/components/LoopVideo.astro`.

It does not screen-record the desktop. `PrintWindow` paints only the demo
window into a bitmap, so another app can never appear in a take. Input is sent
with `SendKeys`, and every keystroke is refused if the demo window is not in
front.

```
scripts/demo/
  record.ps1     entry point; staging, launching, capture, encoding
  scenes.ps1     <- edit this: one block per clip
  Recorder.ps1   PrintWindow capture engine and input helpers
  demo.py        fictional workspace, session, preferences and themes
  encode.py      crossfade, crop, MP4/WebM/poster, keycast timings
```

## Quick start

Requirements: Windows, a built release app, Python 3, and FFmpeg/FFprobe with
libx264 and libvpx-vp9.

```powershell
cargo build --release

# Record and publish every scene.
powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1

# Re-record one clip after changing its feature.
powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1 -Only palette

# Encode a preview without touching web/ (good while tuning a scene).
powershell -ExecutionPolicy Bypass -File scripts/demo/record.ps1 -Only palette -NoPublish
```

`pwsh -File scripts/demo/record.ps1` works too. `-Build` runs
`cargo build --release` first. `-SkipEncode` keeps the raw takes for a later
`python scripts/demo/encode.py ...` run. `-KeepApp` leaves the last demo window
open for inspection.

Defaults are the ones used for the current site: 25 fps, a 1300x780 window at
(120, 60), a 0.4 s crossfade, and the demo workspace at `C:\FoldexDemo`. Pass
`-DemoRoot`, `-AppExe`, `-Ffmpeg`, `-Ffprobe`, `-Python`, `-Scenes` or `-Repo`
to override them.

## Editing a scene

Open `scenes.ps1`. Each entry is a hashtable. The three current clips are direct
transcriptions of the original takes, so they can be re-recorded as-is.

```powershell
@{
    Name       = 'palette'
    Theme      = 'dark'
    SinglePane = $true
    Crop       = '1280:768:0:0'   # w:h:x:y in the captured frame
    Width      = 1280             # output width; height follows the crop
    Fade       = 0.4              # seconds crossfaded into the tail
    Publish    = $true            # copy to web/ and rewrite loops.ts
    Steps      = {
        Wait 1200
        Send '^+p'; Wait 500
        Type 'split' 130; Wait 700
        Send '{ENTER}'; Wait 2000
    }
}
```

| Field | Meaning |
| --- | --- |
| `Name` | Take and output name. Must be unique. |
| `Note` | Comment for humans; not used. |
| `Theme` | `dark` (default) or `light`. Rebuilt for every take. |
| `SinglePane` | Close the right pane in the first tab before recording. |
| `Crop` | `w:h:x:y`, applied before scaling. Omit to keep the whole window. |
| `Width` | Output width. Omit to keep the cropped width. |
| `Fade` | Crossfade length in seconds. `0` disables it. |
| `Publish` | Copy the result into `web/` and update `loops.ts`. |
| `Window` | `@{ X; Y; Width; Height }` for the main window. |
| `Capture` | Fixed `@{ Width; Height; Fill }` for windows that resize. |
| `Setup` | Runs before capture starts; not part of the clip or keycast. |
| `Steps` | Runs while recording; every `Send` is timed for the keycast. |
| `After` | Runs after capture stops, before the app is closed. |
| `Enabled` | Set `$false` to keep a scene in the file but skip it. |

### Steps

`Wait`, `Send` and `Type` are the common case:

```powershell
Wait 800                       # milliseconds
Send '^f'                      # Ctrl+F (SendKeys notation)
Send '%{UP}'                   # Alt+Up
Type 'hello world' 90          # literal text, 90 ms per character +/- jitter
```

`Type` escapes SendKeys syntax for you. SendKeys modifiers are `^` Ctrl, `+`
Shift, `%` Alt; named keys are `{ENTER}`, `{ESC}`, `{TAB}`, `{BACKSPACE}`,
`{UP}`, `{DOWN}`, `{LEFT}`, `{RIGHT}`, `{HOME}`, `{END}`, `{DEL}`, `{F1}` and so
on. A literal `+`, `^`, `%`, `~`, `(`, `)`, `{`, `}` or `[`/`]` is wrapped in
braces by `Type`.

Any PowerShell works in `Steps`, `Setup` and `After`, so a scene can copy a
theme file, run a helper, or inspect state. The helpers are defined in
`Recorder.ps1`:

- `Set-RecordTarget <hwnd>` / `Get-ForegroundTarget` change what is captured.
- `Get-WindowProcessPath <hwnd>` identifies the foreground window safely.
- `Wait`, `Send`, `Type`, `Clear-KeyLog`.

An alternate window (for example the launcher) uses `Setup` to open it and make
it the target, plus a fixed `Capture` size:

```powershell
@{
    Name    = 'launcher'
    Publish = $false
    Capture = @{ Width = 640; Height = 300; Fill = '1c1c1c' }
    Setup   = {
        Send '^%+j'
        Wait 1200
        Get-ForegroundTarget | Out-Null
    }
    Steps   = {
        Wait 1000
        Type '1920*1080/1024' 120
        Wait 1800
    }
}
```

`Send` during `Setup` is not added to the keycast, because the key log starts
with the capture.

### Adding a clip to the site

1. Add a scene to `scenes.ps1` with `Publish = $true`.
2. Record it; this creates `web/public/media/<name>.mp4|webm` and
   `web/src/assets/media/<name>.png`, and adds its timings to
   `web/src/data/loops.ts` automatically.
3. Add the poster import and `posters[name]` entry in
   `web/src/components/LoopVideo.astro`, then use `<LoopVideo name="...">`.
   The `LoopName` union in `loops.ts` is generated, so no manual edit is needed
   there.

`Publish = $false` is useful for a scratch take: it still encodes into
`target/demo-recording/out/`, but leaves `web/` alone.

## How it works

1. **Stage.** `demo.py` deletes and rebuilds `C:\FoldexDemo`: a made-up Studio
   tree with realistic names, sizes and timestamps, a portable copy of
   `Foldex.exe`, a session with three tabs, preferences and an optional light
   theme. No personal files are read.
2. **Launch and frame.** The staged app starts, is restored to 1300x780 at
   (120, 60), and must take focus. `Send` aborts the take if focus moves.
3. **Capture.** A worker thread calls `PrintWindow(hwnd, hdc, 2)` at 25 fps and
   copies the DWM frame into a BGRA buffer. The buffer is piped as raw
   `bgr0` into FFmpeg and stored losslessly as FFV1 in
   `target/demo-recording/raw/<name>.mkv`. If a frame takes too long, the
   recorder writes the same frame again so the clip keeps real time.
4. **Time the keys.** Each `Send` records `Rec.Frames / fps` next to the
   SendKeys chord. That log becomes `<name>.keys.json`.
5. **Loop.** `encode.py` plays from `Fade` to the end, then crossfades the first
   `Fade` seconds into the tail. The last frame now flows into the first, and
   key timestamps shift back by `Fade`. Duration becomes
   `original - Fade`.
6. **Encode.** H.264 MP4 (CRF 21, `-tune animation`, faststart), VP9 WebM
   (CRF 33), and a first-frame PNG poster.
7. **Publish.** MP4/WebM go to `web/public/media/`, posters to
   `web/src/assets/media/`, and `web/src/data/loops.ts` is rewritten in scene
   order. Keycaps are DOM elements synced to `video.currentTime`; they are not
   baked into the video.

Outputs:

```
target/demo-recording/
  manifest.json          scene settings used for this run
  encoded.json           last encoded duration/keys per clip
  raw/<name>.mkv         lossless take
  raw/<name>.keys.json   key log, timestamps from the raw take
  out/<name>.mp4|webm    encoded loop
  out/<name>.png         poster
```

Raw takes are deliberately kept under `target/` (git-ignored) so a bad encode
can be retried without recording again.

## Troubleshooting

- **`focus left the demo window`** — another window stole focus. The take stops
  before the next key. Close popups and re-run; do not click during a take.
- **`app not built`** — run `cargo build --release` or pass `-Build`.
- **`could not stage` / file in use** — a demo `Foldex.exe` is still running.
  The recorder stops only the copy at `<DemoRoot>\app\Foldex.exe`; close any
  other copy manually.
- **`too short for a crossfade`** — the take is shorter than `2 * Fade`; add
  more steps or lower `Fade`.
- **Keys look one step off** — the keycast is timed when `Send` is called, so
  very fast UI transitions may need an extra `Wait` before the next key.
- **Clip is blurry or the wrong size** — adjust `Crop`/`Width` in `scenes.ps1`.
  The current crops assume the default window geometry; changing `Window`
  usually requires new crops.
- **Theme looks stale** — every take is reset from `theme.toml`; change the
  repo theme, not the staged copy.

The demo folder is disposable by design. Do not point `-DemoRoot` at anything
you care about: staging deletes and recreates it.
