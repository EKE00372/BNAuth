#requires -Version 5.1
<#
Runs the specified EXE from a new directory containing no authenticator data.
Checks the first visible native window, not merely egui's queued commands.
Example: powershell -NoProfile -File tests/windows-first-visible.ps1 -Executable .build/target/release/BNAuth.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [ValidateRange(1, 20)]
    [int]$Launches = 3,

    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../.build/windows-first-visible')
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This native-window regression test requires Windows.' }
$sourceExe = (Resolve-Path -LiteralPath $Executable).Path
if ([IO.Path]::GetExtension($sourceExe) -ne '.exe') { throw 'Executable must be an EXE file.' }
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)
$runDirectory = Join-Path $outputRoot ('run-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$testExe = Join-Path $runDirectory 'BNAuth.exe'
Copy-Item -LiteralPath $sourceExe -Destination $testExe

if (-not ('BNAuth.FirstVisibleWindowTest' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace BNAuth {
    public static class FirstVisibleWindowTest {
        [StructLayout(LayoutKind.Sequential)]
        public struct Bounds { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)]
        struct Point { public int X, Y; }
        [StructLayout(LayoutKind.Sequential)]
        struct MonitorInfo { public int Size; public Bounds Monitor, Work; public uint Flags; }
        [StructLayout(LayoutKind.Sequential)]
        struct Message {
            public IntPtr Window; public uint Id; public UIntPtr WParam; public IntPtr LParam;
            public uint Time; public Point Position; public uint Private;
        }
        public sealed class Observation {
            public string Source;
            public long Microseconds;
            public uint EventTime;
            public bool Visible, OnPrimaryMonitor;
            public uint Dpi;
            public Bounds Outer;
            public int ClientWidth, ClientHeight;
        }
        public sealed class RunResult {
            public int ProcessId;
            public Bounds PrimaryWorkArea;
            public bool ExitedNormally;
            public int ExitCode;
            public string Failure;
            public List<Observation> Observations = new List<Observation>();
        }
        delegate void EventCallback(IntPtr hook, uint id, IntPtr window, int obj, int child, uint thread, uint time);
        delegate bool WindowCallback(IntPtr window, IntPtr state);

        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")] static extern IntPtr MonitorFromPoint(Point point, uint flags);
        [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
        [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr module, EventCallback callback, uint process, uint thread, uint flags);
        [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr hook);
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
        [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out Bounds bounds);
        [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr window, out Bounds bounds);
        [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr window);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int length);
        [DllImport("user32.dll")] static extern bool EnumWindows(WindowCallback callback, IntPtr state);
        [DllImport("user32.dll")] static extern bool PeekMessage(out Message message, IntPtr window, uint min, uint max, uint flags);
        [DllImport("user32.dll")] static extern bool TranslateMessage(ref Message message);
        [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref Message message);
        [DllImport("user32.dll")] static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

        static void PumpEvents() {
            Message message;
            while (PeekMessage(out message, IntPtr.Zero, 0, 0, 1)) {
                TranslateMessage(ref message);
                DispatchMessage(ref message);
            }
        }

        static bool WithinPixel(double actual, double expected) {
            return Math.Abs(actual - expected) <= 1.0;
        }

        static string Validate(Observation sample, Bounds work) {
            if (!sample.OnPrimaryMonitor) return "Visible window is not on the primary monitor.";
            if (sample.Dpi == 0) return "Could not read the visible window's DPI.";
            double scale = sample.Dpi / 96.0;
            // Match the existing work-area fit: use whole logical points and
            // shrink the default only when the primary work area cannot fit it.
            double left = Math.Ceiling(work.Left / scale);
            double top = Math.Ceiling(work.Top / scale);
            double right = Math.Floor(work.Right / scale);
            double bottom = Math.Floor(work.Bottom / scale);
            double width = Math.Min(400, right - left);
            double height = Math.Min(892, bottom - top);
            double x = Math.Max(left, Math.Min(left + (right - left) * 0.75 - width * 0.5, right - width));
            double y = Math.Max(top, Math.Min((top + bottom - height) * 0.5, bottom - height));
            if (!WithinPixel(sample.Outer.Left, x * scale) || !WithinPixel(sample.Outer.Top, y * scale)) {
                return String.Format("Visible position ({0},{1}) differs from the fixed launch position ({2:F2},{3:F2}).",
                    sample.Outer.Left, sample.Outer.Top, x * scale, y * scale);
            }
            if (!WithinPixel(sample.ClientWidth, width * scale) || !WithinPixel(sample.ClientHeight, height * scale)
                || !WithinPixel(sample.Outer.Right - sample.Outer.Left, width * scale)
                || !WithinPixel(sample.Outer.Bottom - sample.Outer.Top, height * scale)) {
                return String.Format("Visible outer/client size differs from the fitted 400x892 logical launch size ({0:F2}x{1:F2} physical).",
                    width * scale, height * scale);
            }
            return null;
        }

        public static RunResult Run(string executable) {
            RunResult result = new RunResult();
            IntPtr previousDpi = SetThreadDpiAwarenessContext(new IntPtr(-4));
            IntPtr eventHook = IntPtr.Zero;
            IntPtr root = IntPtr.Zero;
            Process process = null;
            EventCallback callback = null;
            Stopwatch timer = Stopwatch.StartNew();
            long firstVisibleAt = -1;
            bool firstShowObserved = false;
            string previousSample = null;
            try {
                IntPtr primary = MonitorFromPoint(new Point(), 1);
                MonitorInfo monitor = new MonitorInfo();
                monitor.Size = Marshal.SizeOf(typeof(MonitorInfo));
                if (!GetMonitorInfo(primary, ref monitor)) throw new Exception("Primary monitor work area is unavailable.");
                result.PrimaryWorkArea = monitor.Work;

                Action<IntPtr, string, uint> observe = (window, source, eventTime) => {
                    uint owner;
                    GetWindowThreadProcessId(window, out owner);
                    if (owner != result.ProcessId) return;
                    StringBuilder title = new StringBuilder(256);
                    GetWindowText(window, title, title.Capacity);
                    if (title.ToString() != "BNAuth") return;
                    root = window;
                    Bounds outer, client;
                    if (!GetWindowRect(window, out outer) || !GetClientRect(window, out client)) return;
                    bool visible = IsWindowVisible(window);
                    string signature = visible + ":" + outer.Left + "," + outer.Top + "," + outer.Right + "," + outer.Bottom;
                    if (source == "sample" && previousSample == signature) return;
                    if (source == "sample") previousSample = signature;
                    Observation sample = new Observation {
                        Source = source,
                        Microseconds = (long)(timer.ElapsedTicks * 1000000.0 / Stopwatch.Frequency),
                        EventTime = eventTime,
                        Visible = visible,
                        OnPrimaryMonitor = MonitorFromWindow(window, 2) == primary,
                        Dpi = GetDpiForWindow(window),
                        Outer = outer,
                        ClientWidth = client.Right - client.Left,
                        ClientHeight = client.Bottom - client.Top
                    };
                    result.Observations.Add(sample);
                    if (visible) {
                        if (firstVisibleAt < 0) firstVisibleAt = timer.ElapsedMilliseconds;
                        if (source == "show") firstShowObserved = true;
                        string failure = Validate(sample, monitor.Work);
                        if (result.Failure == null && failure != null) result.Failure = source + ": " + failure;
                    }
                };

                // Install before Process.Start so the first ShowWindow cannot
                // precede the observer. Keep pumping without other test work.
                callback = (hook, id, window, obj, child, thread, time) => {
                    if (window != IntPtr.Zero && obj == 0 && child == 0 && (id == 0x8002 || id == 0x800B)) {
                        observe(window, id == 0x8002 ? "show" : "location", time);
                    }
                };
                eventHook = SetWinEventHook(0x8000, 0x800B, IntPtr.Zero, callback, 0, 0, 2);
                if (eventHook == IntPtr.Zero) throw new Exception("Could not install the first-visible window observer.");
                ProcessStartInfo start = new ProcessStartInfo(executable);
                start.WorkingDirectory = System.IO.Path.GetDirectoryName(executable);
                start.UseShellExecute = false;
                start.WindowStyle = ProcessWindowStyle.Hidden;
                process = Process.Start(start);
                result.ProcessId = process.Id;
                while (!process.HasExited && timer.ElapsedMilliseconds < 10000
                    && (firstVisibleAt < 0 || timer.ElapsedMilliseconds - firstVisibleAt < 500)) {
                    PumpEvents();
                    EnumWindows((window, state) => { observe(window, "sample", 0); return true; }, IntPtr.Zero);
                    // Sleep(1) can last a full Windows timer tick and miss the
                    // first visible frame. Yield while acquiring it and for
                    // its first 50ms; later checks only watch settled bounds.
                    if (firstVisibleAt < 0 || timer.ElapsedMilliseconds - firstVisibleAt < 50) Thread.Yield();
                    else Thread.Sleep(1);
                }
                PumpEvents();
                if (firstVisibleAt < 0 && result.Failure == null) result.Failure = "No visible BNAuth window was observed.";
                if (!firstShowObserved && result.Failure == null) result.Failure = "The first native show event was not observed.";
            }
            catch (Exception error) {
                if (result.Failure == null) result.Failure = error.Message;
            }
            finally {
                if (eventHook != IntPtr.Zero) UnhookWinEvent(eventHook);
                // Only the isolated child receives WM_CLOSE. Never terminate a
                // user's running BNAuth process or force-kill a failed test.
                if (process != null) {
                    if (root != IntPtr.Zero && !process.HasExited) PostMessage(root, 0x0010, IntPtr.Zero, IntPtr.Zero);
                    long closingAt = timer.ElapsedMilliseconds;
                    while (!process.HasExited && timer.ElapsedMilliseconds - closingAt < 5000) {
                        PumpEvents();
                        Thread.Sleep(5);
                    }
                    result.ExitedNormally = process.HasExited;
                    if (result.ExitedNormally) {
                        result.ExitCode = process.ExitCode;
                        if (result.ExitCode != 0 && result.Failure == null) result.Failure = "The isolated process exited with code " + result.ExitCode + ".";
                    } else {
                        result.Failure = "The isolated process did not close through WM_CLOSE; process id " + result.ProcessId + ". " + result.Failure;
                    }
                    process.Dispose();
                }
                GC.KeepAlive(callback);
                SetThreadDpiAwarenessContext(previousDpi);
            }
            return result;
        }
    }
}
'@
}

$results = @()
for ($index = 1; $index -le $Launches; $index++) {
    $result = [BNAuth.FirstVisibleWindowTest]::Run($testExe)
    $results += $result
    if (-not $result.ExitedNormally) { break }
}
$reportPath = Join-Path $runDirectory 'results.json'
$report = [pscustomobject]@{
    Executable = $sourceExe
    Sha256 = (Get-FileHash -LiteralPath $testExe -Algorithm SHA256).Hash.ToLowerInvariant()
    RequestedLaunches = $Launches
    Results = $results
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding UTF8
$failures = @($results | Where-Object { $_.Failure })
if ($results.Count -ne $Launches -or $failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Output ('FAIL process {0}: {1}' -f $_.ProcessId, $_.Failure) }
    throw ('First-visible native-window regression failed. Report: ' + $reportPath)
}
Write-Output ('PASS: {0} launches showed the fixed position and size from the first native show event. Report: {1}' -f $Launches, $reportPath)
