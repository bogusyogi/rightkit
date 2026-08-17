using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;

internal static class ManagedRustLauncher
{
    private const int InvalidState = 125;
    private const string Marker = "rightkit.build-control.agent-shims.install.v1";

    private static int Main()
    {
        try
        {
            var executable = Process.GetCurrentProcess().MainModule.FileName;
            var shim = Path.GetFileNameWithoutExtension(executable).ToLowerInvariant();
            if (shim != "cargo" && shim != "rustc" && shim != "rustdoc" && shim != "rustup") return InvalidState;
            var manifestPath = Path.Combine(Path.GetDirectoryName(executable), ".rightkit-managed-agent-shims.json");
            var manifest = File.ReadAllText(manifestPath, Encoding.UTF8);
            if (!Regex.IsMatch(manifest, "\\\"marker\\\"\\s*:\\s*\\\"" + Regex.Escape(Marker) + "\\\"")) return InvalidState;
            var nodePath = JsonString(manifest, "nodePath");
            var runtimeCurrent = JsonString(manifest, "runtimeCurrent");
            if (!Path.IsPathRooted(nodePath) || !Path.IsPathRooted(runtimeCurrent)) return InvalidState;
            var agent = Path.Combine(runtimeCurrent, "build-control-clients", "src", "managed-agent.mjs");
            if (!File.Exists(nodePath) || !File.Exists(agent)) return InvalidState;
            var info = new ProcessStartInfo(nodePath, Quote(agent) + " " + string.Join(" ", Tail(Environment.GetCommandLineArgs())))
            {
                WorkingDirectory = Environment.CurrentDirectory,
                UseShellExecute = false,
                CreateNoWindow = false,
            };
            info.EnvironmentVariables["RIGHTKIT_MANAGED_AGENT_SHIM"] = shim;
            using (var child = Process.Start(info))
            {
                if (child == null) return InvalidState;
                child.WaitForExit();
                return child.ExitCode;
            }
        }
        catch { return InvalidState; }
    }

    private static string JsonString(string json, string key)
    {
        var match = Regex.Match(json, "\\\"" + Regex.Escape(key) + "\\\"\\s*:\\s*\\\"((?:\\\\.|[^\\\"])*)\\\"");
        if (!match.Success) throw new InvalidDataException(key);
        return Regex.Unescape(match.Groups[1].Value);
    }

    private static string[] Tail(string[] args)
    {
        var result = new string[Math.Max(0, args.Length - 1)];
        for (var index = 1; index < args.Length; index++) result[index - 1] = Quote(args[index]);
        return result;
    }

    private static string Quote(string value)
    {
        if (value.Length > 0 && value.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '\"' }) < 0) return value;
        var result = new StringBuilder("\"");
        var slashes = 0;
        foreach (var character in value)
        {
            if (character == '\\') { slashes++; continue; }
            if (character == '\"') result.Append('\\', slashes * 2 + 1);
            else result.Append('\\', slashes);
            result.Append(character);
            slashes = 0;
        }
        result.Append('\\', slashes * 2);
        return result.Append('"').ToString();
    }
}
