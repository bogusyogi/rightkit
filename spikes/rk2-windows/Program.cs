using System;
using System.IO;
using System.Text.Json;

// RK2 Spike B. Subcommands (all output goes to files; the exe is a WinExe so no console is shown):
//   target   --hwnd-file F --ready-file F --control F [--x --y --w --h]   WinForms + WebView2 test app
//   marker   --control F [--x --y --w --h]                              distinctive topmost leak marker
//   capture  --hwnd N --seconds S --dir D --label L                     Windows.Graphics.Capture for HWND
//   ocr      --dir D                                                    Windows.Media.Ocr over captured PNGs
//   window   --hwnd N --op hide|show|minimize|restore|move [--x --y]    ShowWindow / SetWindowPos
internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 0) return 2;
            switch (args[0])
            {
                case "target": return TargetApp.Run(args);
                case "marker": return TargetApp.RunMarker(args);
                case "capture": return Capturer.Run(args);
                case "ocr": return OcrRunner.Run(args);
                case "window": return WindowOps.Run(args);
                case "shot": return Shot.Run(args);
                default: return 2;
            }
        }
        catch (Exception ex)
        {
            try { File.AppendAllText(Path.Combine(AppContext.BaseDirectory, "crash.log"), ex + Environment.NewLine); } catch { }
            return 1;
        }
    }

    internal static string Opt(string[] a, string name, string def = null)
    {
        for (int i = 0; i + 1 < a.Length; i++)
            if (a[i] == "--" + name) return a[i + 1];
        return def;
    }

    internal static void WriteJson(string path, object value)
    {
        File.WriteAllText(path, JsonSerializer.Serialize(value, new JsonSerializerOptions { WriteIndented = true }));
    }
}
