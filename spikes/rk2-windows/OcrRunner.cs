using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Windows.Storage;

// Runs Windows.Media.Ocr on every captured frame (f*.png) in a directory and writes ocr.json.
internal static class OcrRunner
{
    public static int Run(string[] a)
    {
        string dir = Program.Opt(a, "dir");
        var summary = new Dictionary<string, object>();
        var engine = OcrEngine.TryCreateFromUserProfileLanguages();
        if (engine == null)
        {
            summary["error"] = "no OCR engine for user profile languages";
            Program.WriteJson(Path.Combine(dir, "ocr.json"), summary);
            return 1;
        }
        summary["ocrLanguage"] = engine.RecognizerLanguage.DisplayName;

        var files = Directory.GetFiles(dir, "f*.png").OrderBy(x => x, StringComparer.Ordinal).ToArray();
        var rows = new List<object>();
        int stepFrames = 0, leakFrames = 0, counterFrames = 0, advancing = 0, pairs = 0;
        int? prev = null;
        var counters = new List<int>();
        foreach (var f in files)
        {
            string text = "";
            string err = null;
            try { text = Recognize(engine, f); }
            catch (Exception e) { err = e.GetType().Name + ": " + e.Message; }

            string step = null, counter = null;
            var ms = Regex.Match(text, @"STEP\s*-?\s*\d+", RegexOptions.IgnoreCase);
            if (ms.Success) { step = ms.Value; stepFrames++; }
            var mc = Regex.Match(text, @"FRAME\s*(\d+)", RegexOptions.IgnoreCase);
            if (mc.Success)
            {
                counter = mc.Groups[1].Value;
                counterFrames++;
                int v = int.Parse(counter);
                counters.Add(v);
                if (prev.HasValue) { pairs++; if (v > prev.Value) advancing++; }
                prev = v;
            }
            bool leak = text.IndexOf("LEAK", StringComparison.OrdinalIgnoreCase) >= 0 ||
                        text.IndexOf("MARKER", StringComparison.OrdinalIgnoreCase) >= 0;
            if (leak) leakFrames++;

            rows.Add(new { file = Path.GetFileName(f), step, counter, leakTokens = leak, text = text.Replace("\r", "").Replace("\n", " | "), error = err });
        }

        summary["framesOcrd"] = files.Length;
        summary["stepIdReadableFrames"] = stepFrames;
        summary["counterParsedFrames"] = counterFrames;
        summary["counterStrictlyIncreasingPairs"] = advancing;
        summary["counterComparedPairs"] = pairs;
        summary["counterFirst"] = counters.Count > 0 ? counters[0] : (int?)null;
        summary["counterLast"] = counters.Count > 0 ? counters[counters.Count - 1] : (int?)null;
        summary["framesWithLeakTokens"] = leakFrames;
        Program.WriteJson(Path.Combine(dir, "ocr.json"), new { summary, rows });
        return 0;
    }

    private static string Recognize(OcrEngine engine, string path)
    {
        var sf = StorageFile.GetFileFromPathAsync(path).AsTask().GetAwaiter().GetResult();
        using (var fs = sf.OpenAsync(Windows.Storage.FileAccessMode.Read).AsTask().GetAwaiter().GetResult())
        {
            var decoder = BitmapDecoder.CreateAsync(fs).AsTask().GetAwaiter().GetResult();
            using (var sb = decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied).AsTask().GetAwaiter().GetResult())
            {
                var result = engine.RecognizeAsync(sb).AsTask().GetAwaiter().GetResult();
                return result.Text ?? "";
            }
        }
    }
}
