using System;
using System.Drawing;
using System.IO;
using System.Windows.Forms;
using Microsoft.Web.WebView2.WinForms;

// Test target: a WinForms window hosting WebView2 with a visible step id and a per-animation-frame counter.
// Control file commands (one per line, polled every 200 ms): webview-hide, webview-show, exit.
internal static class TargetApp
{
    private const string Page = @"<!doctype html><html><head><meta charset='utf-8'><style>
html,body{margin:0;height:100%;background:#ffffff;color:#000000;font-family:Arial,sans-serif;overflow:hidden}
#step{position:absolute;left:40px;top:40px;font-size:120px;font-weight:bold}
#frame{position:absolute;left:40px;top:220px;font-size:72px;font-weight:bold}
#bar{position:absolute;left:0;bottom:0;height:30px;background:#1f5fbf;width:0px}
</style></head><body>
<div id='step'>STEP-07</div>
<div id='frame'>FRAME 00000</div>
<div id='bar'></div>
<script>
var n = 0;
function tick() {
  n++;
  document.getElementById('frame').textContent = 'FRAME ' + ('00000' + n).slice(-5);
  document.getElementById('bar').style.width = ((n * 7) % 800) + 'px';
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
</script></body></html>";

    public static int Run(string[] a)
    {
        string hwndFile = Program.Opt(a, "hwnd-file");
        string ready = Program.Opt(a, "ready-file");
        string control = Program.Opt(a, "control");
        var bounds = Bounds(a);
        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.Run(new TargetForm(hwndFile, ready, control, bounds));
        return 0;
    }

    public static int RunMarker(string[] a)
    {
        string control = Program.Opt(a, "control");
        var bounds = Bounds(a);
        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.Run(new MarkerForm(control, bounds, Program.Opt(a, "hwnd-file")));
        return 0;
    }

    internal static Rectangle Bounds(string[] a)
    {
        return new Rectangle(
            int.Parse(Program.Opt(a, "x", "100")),
            int.Parse(Program.Opt(a, "y", "100")),
            int.Parse(Program.Opt(a, "w", "800")),
            int.Parse(Program.Opt(a, "h", "520")));
    }

    internal static string TakeCommand(string control, ref string last)
    {
        if (string.IsNullOrEmpty(control) || !File.Exists(control)) return null;
        string cmd;
        try { cmd = File.ReadAllText(control).Trim(); } catch { return null; }
        if (cmd == last) return null;
        last = cmd;
        return cmd;
    }

    private sealed class TargetForm : Form
    {
        private readonly WebView2 _web = new WebView2();
        private readonly Timer _poll = new Timer();
        private readonly string _hwndFile, _ready, _control;
        private string _last = "";

        public TargetForm(string hwndFile, string ready, string control, Rectangle bounds)
        {
            _hwndFile = hwndFile; _ready = ready; _control = control;
            Text = "RK2 target window";
            StartPosition = FormStartPosition.Manual;
            Bounds = bounds;
            _web.Dock = DockStyle.Fill;
            Controls.Add(_web);
            _poll.Interval = 200;
            _poll.Tick += (s, e) => Poll();
            Shown += async (s, e) =>
            {
                File.WriteAllText(_hwndFile, Handle.ToInt64().ToString());
                _poll.Start();
                try
                {
                    await _web.EnsureCoreWebView2Async();
                    _web.CoreWebView2.NavigationCompleted += (s2, e2) =>
                        File.WriteAllText(_ready, "ready success=" + e2.IsSuccess);
                    _web.NavigateToString(Page);
                }
                catch (Exception ex)
                {
                    File.WriteAllText(_ready, "error " + ex.Message);
                }
            };
        }

        private void Poll()
        {
            string cmd = TakeCommand(_control, ref _last);
            if (cmd == null) return;
            try
            {
                switch (cmd)
                {
                    // WinForms WebView2 forwards Visible to the controller's IsVisible.
                    case "webview-hide": _web.Visible = false; break;
                    case "webview-show": _web.Visible = true; break;
                    case "exit": Close(); break;
                }
                File.WriteAllText(_control + ".ack", cmd + " applied webViewVisible=" + _web.Visible);
            }
            catch (Exception ex)
            {
                File.WriteAllText(_control + ".ack", cmd + " error " + ex.Message);
            }
        }
    }

    private sealed class MarkerForm : Form
    {
        private readonly Label _label = new Label();
        private readonly Timer _tick = new Timer();
        private readonly Timer _poll = new Timer();
        private readonly string _control;
        private readonly string _hwndFile;
        private string _last = "";
        private int _n;

        public MarkerForm(string control, Rectangle bounds, string hwndFile)
        {
            _control = control;
            _hwndFile = hwndFile;
            FormBorderStyle = FormBorderStyle.None;
            TopMost = true;
            ShowInTaskbar = false;
            StartPosition = FormStartPosition.Manual;
            Bounds = bounds;
            BackColor = Color.FromArgb(230, 0, 0);
            _label.Dock = DockStyle.Fill;
            _label.ForeColor = Color.White;
            _label.Font = new Font("Arial", 44, FontStyle.Bold);
            _label.TextAlign = ContentAlignment.MiddleCenter;
            Controls.Add(_label);
            _tick.Interval = 100;
            _tick.Tick += (s, e) => { _n++; _label.Text = "LEAK-MARKER-7F3A\n" + _n; };
            _poll.Interval = 200;
            _poll.Tick += (s, e) =>
            {
                if (TakeCommand(_control, ref _last) == "exit") Close();
            };
            Shown += (s, e) =>
            {
                if (!string.IsNullOrEmpty(_hwndFile)) File.WriteAllText(_hwndFile, Handle.ToInt64().ToString());
                _tick.Start();
                _poll.Start();
            };
        }

        protected override bool ShowWithoutActivation { get { return true; } }
    }
}
