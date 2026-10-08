using System;

// Window-state operations applied from outside the target process.
internal static class WindowOps
{
    public static int Run(string[] a)
    {
        var h = new IntPtr(long.Parse(Program.Opt(a, "hwnd")));
        string op = Program.Opt(a, "op", "");
        bool ok;
        switch (op)
        {
            // ShowWindow returns the previous visibility, not success; the call itself is the operation.
            case "hide": Native.ShowWindow(h, Native.SW_HIDE); ok = true; break;
            case "show": Native.ShowWindow(h, Native.SW_SHOW); ok = true; break;
            case "minimize": Native.ShowWindow(h, Native.SW_MINIMIZE); ok = true; break;
            case "restore": Native.ShowWindow(h, Native.SW_RESTORE); ok = true; break;
            case "move":
                ok = Native.SetWindowPos(h, IntPtr.Zero,
                    int.Parse(Program.Opt(a, "x")), int.Parse(Program.Opt(a, "y")), 0, 0,
                    Native.SWP_NOSIZE | Native.SWP_NOZORDER | Native.SWP_NOACTIVATE);
                break;
            default: return 2;
        }
        return ok ? 0 : 1;
    }
}
