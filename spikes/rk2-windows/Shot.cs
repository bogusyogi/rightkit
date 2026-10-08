using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Linq;
using System.Windows.Forms;
using Vortice.DXGI;

// Full virtual-desktop screenshot (all monitors) plus placement evidence for one HWND at the same moment.
// Usage: shot --out <png> --hwnd <decimal HWND>. Writes <png>.json next to it.
internal static class Shot
{
    public static int Run(string[] a)
    {
        string outPath = Program.Opt(a, "out");
        IntPtr h = new IntPtr(long.Parse(Program.Opt(a, "hwnd", "0")));
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(outPath)));

        Rectangle vs = SystemInformation.VirtualScreen;
        var info = new Dictionary<string, object>
        {
            ["timeUtc"] = DateTime.UtcNow.ToString("o"),
            ["virtualScreen"] = vs.ToString(),
            ["monitors"] = Screen.AllScreens.Select(s => new { s.DeviceName, bounds = s.Bounds.ToString(), s.Primary }).ToList(),
        };
        try
        {
            using (var bmp = new Bitmap(vs.Width, vs.Height, PixelFormat.Format32bppArgb))
            using (var g = Graphics.FromImage(bmp))
            {
                g.CopyFromScreen(vs.X, vs.Y, 0, 0, vs.Size, CopyPixelOperation.SourceCopy);
                bmp.Save(outPath, ImageFormat.Png);
                // Mean luminance of the grab, so a black (uncapturable) desktop is visible in the evidence.
                info["screenMeanLuminance"] = Math.Round(MeanLum(bmp), 2);
            }
        }
        catch (Exception e)
        {
            info["screenshotError"] = e.GetType().Name + ": " + e.Message;
        }

        if (h != IntPtr.Zero)
        {
            Native.GetWindowRect(h, out Native.RECT rc);
            var rect = Rectangle.FromLTRB(rc.Left, rc.Top, rc.Right, rc.Bottom);
            info["fixtureHwnd"] = h.ToInt64();
            info["fixtureRect"] = rect.ToString();
            info["fixtureIsWindowVisible"] = Native.IsWindowVisible(h);
            info["fixtureIsIconic"] = Native.IsIconic(h);
            info["fixtureIntersectsAnyMonitor"] = Screen.AllScreens.Any(s => s.Bounds.IntersectsWith(rect));
            info["fixtureIntersectsVirtualScreen"] = vs.IntersectsWith(rect);
        }
        info["gpuAdapters"] = ListAdapters();
        Program.WriteJson(Path.ChangeExtension(outPath, ".json"), info);
        return 0;
    }

    // DXGI adapter list (index, description, dedicated video memory) for the placement evidence.
    private static List<object> ListAdapters()
    {
        var list = new List<object>();
        using (var factory = Vortice.DXGI.DXGI.CreateDXGIFactory1<Vortice.DXGI.IDXGIFactory1>())
        {
            for (uint i = 0; i < 16; i++)
            {
                if (factory.EnumAdapters1(i, out Vortice.DXGI.IDXGIAdapter1 ad).Failure || ad == null) break;
                var d = ad.Description1;
                list.Add(new { index = i, description = d.Description, dedicatedVideoMB = d.DedicatedVideoMemory / 1048576, vendorId = d.VendorId });
                ad.Dispose();
            }
        }
        return list;
    }

    // Sampled mean luminance (every 16th pixel).
    private static double MeanLum(Bitmap bmp)
    {
        long sum = 0, n = 0;
        for (int y = 0; y < bmp.Height; y += 16)
            for (int x = 0; x < bmp.Width; x += 16)
            {
                Color c = bmp.GetPixel(x, y);
                sum += (c.R + c.G + c.B) / 3;
                n++;
            }
        return n > 0 ? (double)sum / n : 0;
    }
}
