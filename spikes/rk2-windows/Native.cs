using System;
using System.Runtime.InteropServices;

internal static class Native
{
    public const int SW_HIDE = 0, SW_SHOW = 5, SW_MINIMIZE = 6, SW_RESTORE = 9;
    public const uint SWP_NOSIZE = 0x0001, SWP_NOZORDER = 0x0004, SWP_NOACTIVATE = 0x0010;

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);

    // d3d11.dll: wraps an IDXGIDevice as a WinRT IDirect3DDevice (returns an IInspectable pointer).
    [DllImport("d3d11.dll", ExactSpelling = true)]
    public static extern int CreateDirect3D11DeviceFromDXGIDevice(IntPtr dxgiDevice, out IntPtr graphicsDevice);

    // Windows.Graphics.Capture HWND interop (IGraphicsCaptureItemInterop).
    // IInspectable-derived; declared as IUnknown with the three IInspectable slots as placeholders so vtable indexes match.
    [ComImport, Guid("3628E81B-3CAC-4C60-B7F4-23CE0E0C3356"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IGraphicsCaptureItemInterop
    {
        void GetIids(out int iidCount, out IntPtr iids);
        void GetRuntimeClassName(out IntPtr className);
        void GetTrustLevel(out int trustLevel);
        // Native: HRESULT CreateForWindow(HWND, REFIID, void** result)
        void CreateForWindow([In] IntPtr window, [In] ref Guid iid, out IntPtr result);
        void CreateForMonitor([In] IntPtr monitor, [In] ref Guid iid, out IntPtr result);
    }

    // Surface -> DXGI/D3D11 texture access.
    [ComImport, Guid("A9B3D012-3DF2-4EE3-B8D1-8695F457D3C1"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IDirect3DDxgiInterfaceAccess
    {
        IntPtr GetInterface([In] ref Guid iid);
    }

    // WinRT activation: get the IActivationFactory for a runtime class name (combase.dll).
    [DllImport("combase.dll", CharSet = CharSet.Unicode, PreserveSig = true)]
    public static extern int WindowsCreateString(string sourceString, int length, out IntPtr hstring);
    [DllImport("combase.dll", PreserveSig = true)]
    public static extern int WindowsDeleteString(IntPtr hstring);
    [DllImport("combase.dll", PreserveSig = true)]
    public static extern int RoGetActivationFactory(IntPtr activatableClassId, ref Guid iid, out IntPtr factory);
    public static readonly Guid IActivationFactoryIid = new Guid("00000035-0000-0000-C000-000000000046");

    // IGraphicsCaptureItemInterop (derives from IUnknown only): CreateForWindow is vtable slot 3.
    // Raw calls avoid the RCW/ComWrappers path, which CsWinRT replaces with its own wrapper.
    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate int CreateForWindowFn(IntPtr self, IntPtr window, ref Guid iid, out IntPtr result);

    public static string Step = "";

    public static IntPtr CreateGraphicsCaptureItemForWindow(IntPtr hwnd)
    {
        const string className = "Windows.Graphics.Capture.GraphicsCaptureItem";
        Marshal.ThrowExceptionForHR(WindowsCreateString(className, className.Length, out IntPtr hs));
        IntPtr factory = IntPtr.Zero, interop = IntPtr.Zero;
        try
        {
            Guid factoryIid = IActivationFactoryIid;
            Step = "RoGetActivationFactory";
            Marshal.ThrowExceptionForHR(RoGetActivationFactory(hs, ref factoryIid, out factory));
            Guid interopIid = new Guid("3628E81B-3CAC-4C60-B7F4-23CE0E0C3356");
            Step = "QueryInterface-interop";
            Marshal.ThrowExceptionForHR(Marshal.QueryInterface(factory, ref interopIid, out interop));
            IntPtr vtbl = Marshal.ReadIntPtr(interop);
            IntPtr fn = Marshal.ReadIntPtr(vtbl, 3 * IntPtr.Size);
            var create = Marshal.GetDelegateForFunctionPointer<CreateForWindowFn>(fn);
            Guid itemIid = GraphicsCaptureItemIid;
            Step = "vtable CreateForWindow";
            int hr = create(interop, hwnd, ref itemIid, out IntPtr item);
            Marshal.ThrowExceptionForHR(hr);
            return item;
        }
        finally
        {
            if (interop != IntPtr.Zero) Marshal.Release(interop);
            if (factory != IntPtr.Zero) Marshal.Release(factory);
            WindowsDeleteString(hs);
        }
    }

    public static readonly Guid GraphicsCaptureItemIid = new Guid("79C3F95B-31F7-4EC2-A464-632EF5D30760");
    public static readonly Guid ID3D11Texture2DIid = new Guid("6f15aaf2-d208-4e89-9ab4-489535d34f9c");
    public static readonly Guid DxgiInterfaceAccessIid = new Guid("A9B3D012-3DF2-4EE3-B8D1-8695F457D3C1");

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate int GetInterfaceFn(IntPtr self, ref Guid iid, out IntPtr result);

    // Frame surface (CsWinRT IInspectable) -> ID3D11Texture2D pointer, via raw IDirect3DDxgiInterfaceAccess::GetInterface (slot 3).
    // The returned pointer carries one reference owned by the caller.
    public static IntPtr GetD3D11TexturePointer(object surface)
    {
        IntPtr surf = ((WinRT.IInspectable)surface).ThisPtr;
        Guid accessIid = DxgiInterfaceAccessIid;
        Marshal.ThrowExceptionForHR(Marshal.QueryInterface(surf, ref accessIid, out IntPtr access));
        try
        {
            IntPtr fn = Marshal.ReadIntPtr(Marshal.ReadIntPtr(access), 3 * IntPtr.Size);
            var get = Marshal.GetDelegateForFunctionPointer<GetInterfaceFn>(fn);
            Guid texIid = ID3D11Texture2DIid;
            Marshal.ThrowExceptionForHR(get(access, ref texIid, out IntPtr tex));
            return tex;
        }
        finally { Marshal.Release(access); }
    }
}
