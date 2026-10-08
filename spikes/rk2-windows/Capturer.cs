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
// Optional timeline: --actions "op@ms;op@ms" applies a window-state change at a scheduled time while the
// capture session is already running. Ops: hide, show, minimize, restore, move:X:Y, webview-hide, webview-show.
// Frames are stamped with tMs (ms since StartCapture) so before/after segments can be compared.
internal sealed class Capturer
{
    private sealed class FrameRec
    {
        public int Index;
        public long TMs;
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
        Directory.CreateDirectory(dir);

        var sum = new Dictionary<string, object>
        {
            ["label"] = label,
            ["hwnd"] = hwnd.ToInt64(),
            ["windowsBuild"] = Environment.OSVersion.Version.Build,
            ["requestedSeconds"] = seconds,
            ["actionsSpec"] = actionSpec,
        };
        var frames = new List<FrameRec>();
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
            try { itemPtr = Native.CreateGraphicsCaptureItemForWindow(hwnd); }
            catch (Exception) { sum["nativeStep"] = Native.Step; throw; }
            sum["stage"] = "item-marshal";
            GraphicsCaptureItem item = WinRT.MarshalInspectable<GraphicsCaptureItem>.FromAbi(itemPtr);
            sum["itemDisplayName"] = item.DisplayName;
            sum["itemWidth"] = item.Size.Width;
            sum["itemHeight"] = item.Size.Height;

            sum["stage"] = "d3d11-device";
            D3D11.D3D11CreateDevice(null, DriverType.Hardware, DeviceCreationFlags.BgraSupport, null, out device, out ctx).CheckError();

            using (var dxgi = device.QueryInterface<IDXGIDevice>())
            {
                sum["stage"] = "winrt-device-wrap";
                int hr = Native.CreateDirect3D11DeviceFromDXGIDevice(dxgi.NativePointer, out IntPtr winPtr);
                Marshal.ThrowExceptionForHR(hr);
                IDirect3DDevice winDevice = WinRT.MarshalInterface<IDirect3DDevice>.FromAbi(winPtr);
                sum["stage"] = "frame-pool";
                pool = Direct3D11CaptureFramePool.CreateFreeThreaded(
                    winDevice, DirectXPixelFormat.B8G8R8A8UIntNormalized, 2, item.Size);
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

            pool.FrameArrived += (sender, _) =>
            {
                try
                {
                    using (var frame = sender.TryGetNextFrame())
                    {
                        if (frame == null) return;
                        lock (gate)
                        {
                            if (closing) return;
                            var rec = ReadFrame(device, ctx, frame, dir, frames.Count + 1);
                            rec.TMs = sw.ElapsedMilliseconds;
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

            var hashes = frames.Select(f => f.Hash).ToList();
            sum["framesArrived"] = frames.Count;
            sum["distinctFrameHashes"] = hashes.Distinct().Count();
            int identicalPairs = 0;
            for (int i = 1; i < hashes.Count; i++) if (hashes[i] == hashes[i - 1]) identicalPairs++;
            sum["consecutiveIdenticalFramePairs"] = identicalPairs;
            sum["blackFrames(meanLum<8)"] = frames.Count(f => f.MeanLum < 8);
            sum["maxRedFraction"] = frames.Count > 0 ? frames.Max(f => f.RedFrac) : 0.0;
            if (frames.Count > 1)
            {
                double span = frames[frames.Count - 1].RelMs - frames[0].RelMs;
                sum["measuredFps"] = span > 0 ? Math.Round((frames.Count - 1) / (span / 1000.0), 2) : 0.0;
            }
            sum["firstFrameTMs"] = frames.Count > 0 ? frames[0].TMs : -1;
            sum["lastFrameTMs"] = frames.Count > 0 ? frames[frames.Count - 1].TMs : -1;
            Program.WriteJson(Path.Combine(dir, "summary.json"), sum);
            Program.WriteJson(Path.Combine(dir, "frames.json"), frames.Select(f => new
            {
                f.Index,
                file = string.Format("f{0:0000}.png", f.Index),
                tMs = f.TMs,
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
                                             Direct3D11CaptureFrame frame, string dir, int index)
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
                    using (var bmp = new Bitmap(dw, dh, PixelFormat.Format32bppArgb))
                    {
                        var bd = bmp.LockBits(new Rectangle(0, 0, dw, dh), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
                        for (int y = 0; y < dh; y++)
                            Buffer.MemoryCopy(src + y * rowPitch, (byte*)bd.Scan0.ToPointer() + (long)y * bd.Stride, dw * 4, dw * 4);
                        bmp.UnlockBits(bd);
                        bmp.Save(Path.Combine(dir, string.Format("f{0:0000}.png", index)), ImageFormat.Png);
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
