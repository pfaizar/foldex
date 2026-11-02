# Window-only recorder used by record.ps1.
#
# PrintWindow paints one window into a bitmap on a worker thread; the raw
# BGRA frames are piped straight into FFmpeg and stored losslessly (FFV1).
# Nothing outside that window can end up in the take, even if another app
# covers it.
#
# Input helpers use SendKeys and refuse to type if the demo window lost focus,
# so a stray keystroke can never land in another application.

Add-Type -AssemblyName System.Drawing, System.Windows.Forms

if (-not ('Rec' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing, System.Windows.Forms -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Threading;

public static class Rec
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int L, T, R, B; }

    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
    [DllImport("user32.dll")] public static extern void keybd_event(byte k, byte s, uint f, UIntPtr e);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int s);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);

    static Thread worker;
    static volatile bool running;
    public static int Frames;
    public static int Dropped;
    public static int Width;
    public static int Height;
    public static string Error;

    // outW/outH > 0 fixes the video size for windows that resize while
    // recording. The frame is top-aligned on `fill` (0xRRGGBB).
    public static void Start(IntPtr h, string ffmpeg, string output, int fps,
                             int outW = 0, int outH = 0, int fill = 0)
    {
        RECT wr0, fr0;
        GetWindowRect(h, out wr0);
        DwmGetWindowAttribute(h, 9, out fr0, 16);
        int w = (outW > 0 ? outW : fr0.R - fr0.L) & ~1;
        int hh = (outH > 0 ? outH : fr0.B - fr0.T) & ~1;

        var psi = new ProcessStartInfo(ffmpeg, string.Format(
            "-hide_banner -loglevel error -y -skip_initial_bytes 3 -f rawvideo -pix_fmt bgr0 -s {0}x{1} -r {2} -i - -c:v ffv1 -pix_fmt bgr0 \"{3}\"",
            w, hh, fps, output));
        psi.UseShellExecute = false;
        psi.RedirectStandardInput = true;
        psi.CreateNoWindow = true;
        var proc = Process.Start(psi);
        var stdin = proc.StandardInput.BaseStream;

        Width = w;
        Height = hh;
        running = true;
        Frames = 0;
        Dropped = 0;
        Error = null;

        worker = new Thread(() =>
        {
            try
            {
                Bitmap full = null;
                Graphics g = null;
                var buf = new byte[w * hh * 4];
                var bg = BitConverter.GetBytes(fill | unchecked((int)0xFF000000));
                var sw = Stopwatch.StartNew();
                double step = 1000.0 / fps;
                long next = 0;

                while (running)
                {
                    RECT wr, fr;
                    GetWindowRect(h, out wr);
                    DwmGetWindowAttribute(h, 9, out fr, 16);
                    int fw = wr.R - wr.L;
                    int fh = wr.B - wr.T;

                    if (full == null || full.Width != fw || full.Height != fh)
                    {
                        if (g != null) { g.Dispose(); full.Dispose(); }
                        full = new Bitmap(fw, fh, PixelFormat.Format32bppArgb);
                        g = Graphics.FromImage(full);
                    }

                    IntPtr dc = g.GetHdc();
                    PrintWindow(h, dc, 2);
                    g.ReleaseHdc(dc);

                    int cw = Math.Min(w, fr.R - fr.L);
                    int ch = Math.Min(hh, fr.B - fr.T);
                    for (int i = 0; i < buf.Length; i += 4)
                    {
                        buf[i] = bg[0]; buf[i + 1] = bg[1];
                        buf[i + 2] = bg[2]; buf[i + 3] = 255;
                    }

                    var data = full.LockBits(
                        new Rectangle(fr.L - wr.L, fr.T - wr.T, cw, ch),
                        ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
                    for (int y = 0; y < ch; y++)
                        Marshal.Copy(data.Scan0 + y * data.Stride, buf, y * w * 4, cw * 4);
                    full.UnlockBits(data);

                    // Write one frame for every slot that elapsed, so playback
                    // keeps real time even if a frame took too long.
                    do { stdin.Write(buf, 0, buf.Length); Frames++; next++; }
                    while (sw.Elapsed.TotalMilliseconds > next * step + step);

                    double wait = next * step - sw.Elapsed.TotalMilliseconds;
                    if (wait > 0) Thread.Sleep((int)wait); else Dropped++;
                }

                stdin.Close();
                proc.WaitForExit();
                if (proc.ExitCode != 0) Error = "ffmpeg exited with " + proc.ExitCode;
            }
            catch (Exception e) { Error = e.ToString(); }
        });
        worker.Start();
    }

    public static void Stop()
    {
        if (worker == null) return;
        running = false;
        worker.Join();
        worker = null;
    }

    // Windows only lets the foreground process take focus, so tap Alt first.
    public static bool Focus(IntPtr h)
    {
        for (int i = 0; i < 6 && GetForegroundWindow() != h; i++)
        {
            keybd_event(0x12, 0, 0, UIntPtr.Zero);
            keybd_event(0x12, 0, 2, UIntPtr.Zero);
            BringWindowToTop(h);
            SetForegroundWindow(h);
            Thread.Sleep(250);
        }
        return GetForegroundWindow() == h;
    }
}
'@
}

$global:RecordTarget = [IntPtr]::Zero
$global:RecordRunning = $false
$global:RecFps = 25
$global:KeyLog = New-Object System.Collections.ArrayList

function Clear-KeyLog {
    $global:KeyLog.Clear()
}

function Set-RecordTarget([IntPtr]$Handle) {
    $global:RecordTarget = $Handle
}

function Get-ForegroundTarget {
    $global:RecordTarget = [Rec]::GetForegroundWindow()
    return $global:RecordTarget
}

function Get-WindowProcessPath([IntPtr]$Handle) {
    $processId = [uint32]0
    [Rec]::GetWindowThreadProcessId($Handle, [ref]$processId) | Out-Null
    if ($processId -eq 0) { return $null }
    return (Get-Process -Id $processId -ErrorAction Stop).Path
}

function Assert-RecordTarget {
    if ($global:RecordTarget -eq [IntPtr]::Zero) {
        throw 'no recording target set; call Set-RecordTarget or Get-ForegroundTarget first'
    }
    if ([Rec]::GetForegroundWindow() -ne $global:RecordTarget) {
        if ($global:RecordRunning) { Stop-Recording }
        throw 'focus left the demo window; the take was stopped before sending another key'
    }
}

function Wait([int]$Milliseconds) {
    Start-Sleep -Milliseconds $Milliseconds
}

# Send one SendKeys chord, e.g. '^+p', '{ENTER}', '%{UP}'.
function Send([string]$Keys) {
    Assert-RecordTarget
    if ($global:RecordRunning) {
        $t = [math]::Round([Rec]::Frames / [double]$global:RecFps, 2)
        $global:KeyLog.Add([pscustomobject]@{ t = $t; k = $Keys }) | Out-Null
    }
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
}

# Type literal text, escaping the characters SendKeys treats as syntax.
function Type([string]$Text, [int]$Delay = 95) {
    foreach ($c in $Text.ToCharArray()) {
        if ($c -eq "`n") {
            $key = '{ENTER}'
        } elseif ($c -eq "`t") {
            $key = '{TAB}'
        } elseif ('+^%~(){}[]'.Contains([string]$c)) {
            $key = "{$c}"
        } else {
            $key = [string]$c
        }
        Send $key
        Start-Sleep -Milliseconds ($Delay + (Get-Random -Minimum -25 -Maximum 35))
    }
}

function Start-Recording([string]$Path, [int]$Width = 0, [int]$Height = 0, [int]$Fill = 0) {
    if ($global:RecordTarget -eq [IntPtr]::Zero) { throw 'no recording target set' }
    Clear-KeyLog
    [Rec]::Start($global:RecordTarget, $global:Ffmpeg, $Path, $global:RecFps, $Width, $Height, $Fill)
    $global:RecordRunning = $true
}

function Stop-Recording {
    if (-not $global:RecordRunning) { return }
    [Rec]::Stop()
    $global:RecordRunning = $false
    if ([Rec]::Error) { throw "recorder failed: $([Rec]::Error)" }
    if ([Rec]::Frames -le 0) { throw 'recorder produced no frames' }
}

function Save-KeyLog([string]$Path) {
    ConvertTo-Json -InputObject @($global:KeyLog) -Depth 4 |
        Set-Content -Path $Path -Encoding utf8
}
