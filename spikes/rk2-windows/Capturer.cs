using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Threading;
using System.Windows.Forms;
using Vortice.Direct3D;
using Vortice.Direct3D11;
using Vortice.DXGI;
using Windows.Graphics.Capture;
using Windows.Graphics.DirectX;
using Windows.Graphics.DirectX.Direct3D11;

// Captures one HWND with Windows.Graphics.Capture (GraphicsCaptureItem created by IGraphicsCaptureItemInterop.CreateForWindow).
// No monitor capture and no crop anywhere in this path.
//
// Options:
//   --actions "op@ms;op@ms"  apply a window-state change while the session runs (hide, show, minimize, restore, move:X:Y,
//                            webview-hide, webview-show).
//   --fps N                  set GraphicsCaptureSession.MinUpdateInterval = 1000/N ms (0 = not set).
//   --pool N                 frame pool buffer count (default 2).
//   --readback none|map|full none: arrival timing only (no GPU copy). map: GPU copy + CPU map, no PNG.
//                            full: map + PNG save per frame (default).
// Every FrameArrived is timestamped (arrivals.json) before any readback, so arrival rate and readback cost are separable.
internal sealed class Capturer
{
    private sealed class FrameRec
    {
        public int Index;
        public double TMs;
        public double RelMs;
        public long Hash;
        public double MeanLum;
        public double RedFrac;
        public int W;
        public int H;
    }

    private sealed class Action
    {
        public string Op;
        public long AtMs;
    }

    public static int Run(string[] a)
    {
        IntPtr hwnd = new IntPtr(long.Parse(Program.Opt(a, "hwnd")));
        int seconds = int.Parse(Program.Opt(a, "seconds", "4"));
        string dir = Program.Opt(a, "dir");
        string label = Program.Opt(a, "label", "unlabeled");
        string actionSpec = Program.Opt(a, "actions", "");
        string control = Program.Opt(a, "control");
        int fps = int.Parse(Program.Opt(a, "fps", "0"));
        int poolSize = int.Parse(Program.Opt(a, "pool", "2"));
        string readback = Program.Opt(a, "readback", "full");
        // --monitor: capture the primary monitor (CreateForMonitor) instead of the HWND. Evidence screenshots only.
        bool monitorMode = Program.Opt(a, "monitor") != null;
        int maxSave = int.Parse(Program.Opt(a, "maxsave", "2000000000"));
        Directory.CreateDirectory(dir);

        var sum = new Dictionary<string, object>
        {
            ["label"] = label,
            ["hwnd"] = hwnd.ToInt64(),
            ["windowsBuild"] = Environment.OSVersion.Version.Build,
            ["requestedSeconds"] = seconds,
            ["actionsSpec"] = actionSpec,
            ["fpsRequested"] = fps,
            ["poolSize"] = poolSize,
            ["readback"] = readback,
            ["captureTarget"] = monitorMode ? "monitor:primary" : "hwnd",
            ["maxSave"] = maxSave,
        };
        var frames = new List<FrameRec>();
        var arrivals = new List<double>();
        var actionLog = new List<object>();
        var gate = new object();
        bool closing = false;
        ID3D11Device device = null;
        ID3D11DeviceContext ctx = null;
        Direct3D11CaptureFramePool pool = null;
        GraphicsCaptureSession session = null;
        var sw = new Stopwatch();

        try
        {
            Native.GetWindowRect(hwnd, out Native.RECT rc);
            var rect = Rectangle.FromLTRB(rc.Left, rc.Top, rc.Right, rc.Bottom);
            sum["windowRectBefore"] = rect.ToString();
            sum["isIconicBefore"] = Native.IsIconic(hwnd);
            sum["isWindowVisibleBefore"] = Native.IsWindowVisible(hwnd);
            sum["windowIntersectsAnyMonitorBefore"] = Screen.AllScreens.Any(s => s.Bounds.IntersectsWith(rect));

            try
            {
                sum["graphicsCaptureAccessBorderless"] = GraphicsCaptureAccess
                    .RequestAccessAsync(GraphicsCaptureAccessKind.Borderless).AsTask().GetAwaiter().GetResult().ToString();
            }
            catch (Exception e)
            {
                sum["graphicsCaptureAccessBorderless"] = "exception: " + e.GetType().Name + " " + e.Message;
            }

            sum["stage"] = "CreateForWindow";
            IntPtr itemPtr;
            try
            {
                itemPtr = monitorMode
                    ? Native.CreateItem(Native.MonitorFromPoint(new Native.POINT { X = 0, Y = 0 }, 1), 4)
                    : Native.CreateGraphicsCaptureItemForWindow(hwnd);
            }
            catch (Exception) { sum["nativeStep"] = Native.Step; throw; }
            sum["stage"] = "item-marshal";
            GraphicsCaptureItem item = WinRT.MarshalInspectable<GraphicsCaptureItem>.FromAbi(itemPtr);
            sum["itemDisplayName"] = item.DisplayName;
            sum["itemWidth"] = item.Size.Width;
            sum["itemHeight"] = item.Size.Height;

            sum["stage"] = "d3d11-device";
            int adapterIndex = int.Parse(Program.Opt(a, "adapter", "-1"));
            if (adapterIndex < 0)
            {
                D3D11.D3D11CreateDevice(null, DriverType.Hardware, DeviceCreationFlags.BgraSupport, null, out device, out ctx).CheckError();
            }
            else
            {
                // Explicit DXGI adapter (hybrid laptops: the panel's adapter may differ from the default device's).
                using (var factory = DXGI.CreateDXGIFactory1<IDXGIFactory1>())
                {
                    factory.EnumAdapters1((uint)adapterIndex, out IDXGIAdapter1 adapter).CheckError();
                    sum["adapterIndex"] = adapterIndex;
                    sum["adapterDescription"] = adapter.Description1.Description;
                    D3D11.D3D11CreateDevice(adapter, DriverType.Unknown, DeviceCreationFlags.BgraSupport, null, out device, out ctx).CheckError();
                    adapter.Dispose();
                }
            }

            using (var dxgi = device.QueryInterface<IDXGIDevice>())
            {
                sum["stage"] = "winrt-device-wrap";
                int hr = Native.CreateDirect3D11DeviceFromDXGIDevice(dxgi.NativePointer, out IntPtr winPtr);
                Marshal.ThrowExceptionForHR(hr);
                IDirect3DDevice winDevice = WinRT.MarshalInterface<IDirect3DDevice>.FromAbi(winPtr);
                sum["stage"] = "frame-pool";
                pool = Direct3D11CaptureFramePool.CreateFreeThreaded(
                    winDevice, DirectXPixelFormat.B8G8R8A8UIntNormalized, poolSize, item.Size);
            }

            sum["stage"] = "capture-session";
            session = pool.CreateCaptureSession(item);
            try
            {
                session.IsBorderRequired = false;
                sum["isBorderRequiredSetFalse"] = "ok";
            }
            catch (Exception e)
            {
                sum["isBorderRequiredSetFalse"] = "exception: " + e.GetType().Name + " " + e.Message;
            }
            try { sum["isBorderRequiredReadback"] = session.IsBorderRequired; }
            catch (Exception e) { sum["isBorderRequiredReadback"] = "exception: " + e.Message; }
            try { session.IsCursorCaptureEnabled = false; sum["cursorCaptureDisabled"] = true; }
            catch (Exception e) { sum["cursorCaptureDisabled"] = "exception: " + e.Message; }
            // GraphicsCaptureSession.MinUpdateInterval is not in the Microsoft.Windows.SDK.NET.Ref metadata
            // (10.0.22000, 10.0.22621, 10.0.26100), so no capture-side rate cap is applied. Delivered rate follows content.
            sum["minUpdateIntervalAvailable"] = false;
            sum["fpsLabel"] = fps;

            pool.FrameArrived += (sender, _) =>
            {
                // Timestamp first, so arrival timing is independent of readback cost.
                double arr = sw.Elapsed.TotalMilliseconds;
                try
                {
                    using (var frame = sender.TryGetNextFrame())
                    {
                        if (frame == null) return;
                        lock (gate)
                        {
                            if (closing) return;
                            arrivals.Add(arr);
                            if (readback == "none") return;
                            var rec = ReadFrame(device, ctx, frame, dir, frames.Count + 1, readback == "full" && frames.Count < maxSave);
                            rec.TMs = arr;
                            rec.RelMs = frame.SystemRelativeTime.TotalMilliseconds;
                            frames.Add(rec);
                        }
                    }
                }
                catch (Exception e)
                {
                    lock (gate) { sum["lastHandlerError"] = e.GetType().Name + ": " + e.Message + " @ " + (e.StackTrace ?? "").Split('\n')[0].Trim(); }
                }
            };

            var actions = ParseActions(actionSpec);
            sum["stage"] = "start-capture";
            sw.Start();
            session.StartCapture();
            sum["stage"] = "capturing";

            Thread actionThread = null;
            if (actions.Count > 0)
            {
                actionThread = new Thread(() =>
                {
                    foreach (var act in actions)
                    {
                        long wait = act.AtMs - sw.ElapsedMilliseconds;
                        if (wait > 0) Thread.Sleep((int)wait);
                        bool ok; string err = null;
                        try { ok = ApplyOp(hwnd, act.Op, control, out err); }
                        catch (Exception e) { ok = false; err = e.Message; }
                        lock (gate) { actionLog.Add(new { op = act.Op, plannedMs = act.AtMs, actualMs = sw.ElapsedMilliseconds, ok, err }); }
                    }
                }) { IsBackground = true };
                actionThread.Start();
            }

            long end = seconds * 1000L;
            while (sw.ElapsedMilliseconds < end) Thread.Sleep(20);
            actionThread?.Join(2000);
            double captureEnd = sw.Elapsed.TotalMilliseconds;
            sum["captureDurationMs"] = Math.Round(captureEnd, 1);

            lock (gate) { closing = true; }
            session.Dispose();
            session = null;
            pool.Dispose();
            pool = null;
        }
        catch (Exception e)
        {
            sum["error"] = e.GetType().Name + ": " + e.Message;
        }
        finally
        {
            session?.Dispose();
            pool?.Dispose();
            ctx?.Dispose();
            device?.Dispose();
        }

        lock (gate)
        {
            // Window state at the end of the timeline (shows whether the requested state was applied).
            if (Native.GetWindowRect(hwnd, out Native.RECT endRc))
            {
                var endRect = Rectangle.FromLTRB(endRc.Left, endRc.Top, endRc.Right, endRc.Bottom);
                sum["windowRectEnd"] = endRect.ToString();
                sum["isIconicEnd"] = Native.IsIconic(hwnd);
                sum["isWindowVisibleEnd"] = Native.IsWindowVisible(hwnd);
            }
            sum["actionLog"] = actionLog;

            // Delivered-frame intervals (arrival-time based, before readback).
            var iv = IntervalStats(arrivals);
            foreach (var kv in iv) sum["arrival." + kv.Key] = kv.Value;
            Program.WriteJson(Path.Combine(dir, "arrivals.json"), arrivals.Select(t => Math.Round(t, 3)).ToList());

            var hashes = frames.Select(f => f.Hash).ToList();
            sum["framesArrived"] = arrivals.Count;
            sum["framesReadBack"] = frames.Count;
            sum["distinctFrameHashes"] = hashes.Distinct().Count();
            int identicalPairs = 0;
            for (int i = 1; i < hashes.Count; i++) if (hashes[i] == hashes[i - 1]) identicalPairs++;
            sum["consecutiveIdenticalFramePairs"] = identicalPairs;
            sum["blackFrames(meanLum<8)"] = frames.Count(f => f.MeanLum < 8);
            sum["maxRedFraction"] = frames.Count > 0 ? frames.Max(f => f.RedFrac) : 0.0;
            sum["firstFrameTMs"] = arrivals.Count > 0 ? Math.Round(arrivals[0], 3) : -1;
            sum["lastFrameTMs"] = arrivals.Count > 0 ? Math.Round(arrivals[arrivals.Count - 1], 3) : -1;
            Program.WriteJson(Path.Combine(dir, "summary.json"), sum);
            Program.WriteJson(Path.Combine(dir, "frames.json"), frames.Select(f => new
            {
                f.Index,
                file = string.Format("f{0:0000}.png", f.Index),
                tMs = Math.Round(f.TMs, 3),
                relMs = Math.Round(f.RelMs, 1),
                f.W,
                f.H,
                hash = f.Hash,
                meanLum = Math.Round(f.MeanLum, 2),
                redFraction = Math.Round(f.RedFrac, 4),
            }).ToList());
        }
        return 0;
    }

    // Interval statistics over arrival timestamps (ms). Gaps are intervals above 1.5x the median.
    private static Dictionary<string, object> IntervalStats(List<double> t)
    {
        var d = new Dictionary<string, object>();
        d["count"] = t.Count;
        if (t.Count < 2) return d;
        var iv = new List<double>();
        for (int i = 1; i < t.Count; i++) iv.Add(t[i] - t[i - 1]);
        var s = iv.OrderBy(x => x).ToList();
        double median = Pct(s, 0.50);
        d["intervalCount"] = iv.Count;
        d["meanMs"] = Math.Round(iv.Average(), 3);
        d["p50Ms"] = Math.Round(median, 3);
        d["p95Ms"] = Math.Round(Pct(s, 0.95), 3);
        d["maxMs"] = Math.Round(s[s.Count - 1], 3);
        d["minMs"] = Math.Round(s[0], 3);
        d["gapsOver1.5xMedian"] = iv.Count(x => x > 1.5 * median);
        d["deliveredFps"] = Math.Round((t.Count - 1) / ((t[t.Count - 1] - t[0]) / 1000.0), 2);
        return d;
    }

    private static double Pct(List<double> sorted, double p)
    {
        int idx = (int)Math.Ceiling(p * sorted.Count) - 1;
        idx = Math.Max(0, Math.Min(sorted.Count - 1, idx));
        return sorted[idx];
    }

    private static List<Action> ParseActions(string spec)
    {
        var list = new List<Action>();
        if (string.IsNullOrWhiteSpace(spec)) return list;
        foreach (var part in spec.Split(';'))
        {
            var at = part.Split('@');
            list.Add(new Action { Op = at[0].Trim(), AtMs = long.Parse(at[1].Trim()) });
        }
        return list.OrderBy(x => x.AtMs).ToList();
    }

    private static bool ApplyOp(IntPtr h, string op, string control, out string err)
    {
        err = null;
        var p = op.Split(':');
        switch (p[0])
        {
            // ShowWindow returns the previous visibility, so the call itself is the operation.
            case "hide": Native.ShowWindow(h, Native.SW_HIDE); return true;
            case "show": Native.ShowWindow(h, Native.SW_SHOW); return true;
            case "minimize": Native.ShowWindow(h, Native.SW_MINIMIZE); return true;
            case "restore": Native.ShowWindow(h, Native.SW_RESTORE); return true;
            case "move":
                return Native.SetWindowPos(h, IntPtr.Zero, int.Parse(p[1]), int.Parse(p[2]), 0, 0,
                    Native.SWP_NOSIZE | Native.SWP_NOZORDER | Native.SWP_NOACTIVATE);
            case "webview-hide":
            case "webview-show":
                if (control == null) { err = "no control file"; return false; }
                File.WriteAllText(control, p[0]);
                return true;
            default:
                err = "unknown op " + op;
                return false;
        }
    }

    private static unsafe FrameRec ReadFrame(ID3D11Device device, ID3D11DeviceContext ctx,
                                             Direct3D11CaptureFrame frame, string dir, int index, bool savePng)
    {
        IntPtr texPtr = Native.GetD3D11TexturePointer(frame.Surface);
        using (var tex = new ID3D11Texture2D(texPtr))
        {
            Texture2DDescription d = tex.Description;
            int dw = (int)d.Width, dh = (int)d.Height;
            var sd = new Texture2DDescription
            {
                Width = (uint)dw,
                Height = (uint)dh,
                MipLevels = 1,
                ArraySize = 1,
                Format = Format.B8G8R8A8_UNorm,
                SampleDescription = new SampleDescription(1, 0),
                Usage = ResourceUsage.Staging,
                CPUAccessFlags = CpuAccessFlags.Read,
            };
            using (var staging = device.CreateTexture2D(sd))
            {
                ctx.CopyResource(staging, tex);
                MappedSubresource m = ctx.Map(staging, 0, MapMode.Read, Vortice.Direct3D11.MapFlags.None);
                try
                {
                    var rec = new FrameRec { Index = index, W = dw, H = dh };
                    byte* src = (byte*)m.DataPointer.ToPointer();
                    long rowPitch = (long)m.RowPitch;
                    if (savePng)
                    {
                        using (var bmp = new Bitmap(dw, dh, PixelFormat.Format32bppArgb))
                        {
                            var bd = bmp.LockBits(new Rectangle(0, 0, dw, dh), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
                            for (int y = 0; y < dh; y++)
                                Buffer.MemoryCopy(src + y * rowPitch, (byte*)bd.Scan0.ToPointer() + (long)y * bd.Stride, dw * 4, dw * 4);
                            bmp.UnlockBits(bd);
                            bmp.Save(Path.Combine(dir, string.Format("f{0:0000}.png", index)), ImageFormat.Png);
                        }
                    }
                    // Stats over a sampled grid of pixels (BGRA bytes).
                    ulong hash = 1469598103934665603UL;
                    long lum = 0, red = 0, n = 0;
                    for (int y = 0; y < dh; y += 4)
                    {
                        byte* row = src + y * rowPitch;
                        for (int x = 0; x < dw; x += 4)
                        {
                            byte* p = row + x * 4;
                            int B = p[0], G = p[1], R = p[2];
                            lum += (R + G + B) / 3;
                            if (R > 180 && G < 90 && B < 90) red++;
                            hash = (hash ^ (ulong)(B | (G << 8) | (R << 16))) * 1099511628211UL;
                            n++;
                        }
                    }
                    rec.Hash = (long)hash;
                    rec.MeanLum = n > 0 ? (double)lum / n : 0;
                    rec.RedFrac = n > 0 ? (double)red / n : 0;
                    return rec;
                }
                finally
                {
                    ctx.Unmap(staging, 0);
                }
            }
        }
    }
}
