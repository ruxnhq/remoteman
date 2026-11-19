using System.Runtime.InteropServices;

namespace RemotemanHost;

/// The icon in the notification area, its menu and its notifications. WinUI 3 has none of its
/// own, so this is Windows' Shell_NotifyIcon with a window nobody sees, on the UI thread (WinUI
/// pumps that thread's Win32 messages, so its procedure is called there).
public sealed unsafe class Tray : IDisposable
{
    const uint WM_APP = 0x8000, CALLBACK = WM_APP + 1;
    const uint NIM_ADD = 0, NIM_MODIFY = 1, NIM_DELETE = 2, NIM_SETVERSION = 4;
    const uint NIF_MESSAGE = 1, NIF_ICON = 2, NIF_TIP = 4, NIF_INFO = 0x10, NIF_SHOWTIP = 0x80;
    const uint WM_LBUTTONUP = 0x202, WM_RBUTTONUP = 0x205, WM_CONTEXTMENU = 0x7B, NIN_SELECT = 0x400, NIN_KEYSELECT = 0x401;
    const uint MF_STRING = 0, MF_GRAYED = 1, MF_SEPARATOR = 0x800;
    const uint TPM_RETURNCMD = 0x100, TPM_RIGHTBUTTON = 2;
    static readonly uint TaskbarCreated = RegisterWindowMessageW("TaskbarCreated");

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct NOTIFYICONDATAW
    {
        public uint cbSize;
        public nint hWnd;
        public uint uID, uFlags, uCallbackMessage;
        public nint hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string szTip;
        public uint dwState, dwStateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string szInfo;
        public uint uVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string szInfoTitle;
        public uint dwInfoFlags;
        public Guid guidItem;
        public nint hBalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct WNDCLASSW
    {
        public uint style;
        public nint lpfnWndProc;
        public int cbClsExtra, cbWndExtra;
        public nint hInstance, hIcon, hCursor, hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
    }

    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }

    delegate nint WndProc(nint hwnd, uint msg, nint w, nint l);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] static extern bool Shell_NotifyIconW(uint message, ref NOTIFYICONDATAW data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern ushort RegisterClassW(ref WNDCLASSW c);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint CreateWindowExW(uint ex, string cls, string name, uint style, int x, int y, int w, int h, nint parent, nint menu, nint inst, nint param);
    [DllImport("user32.dll")] static extern nint DefWindowProcW(nint hwnd, uint msg, nint w, nint l);
    [DllImport("user32.dll")] static extern bool DestroyWindow(nint hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint RegisterWindowMessageW(string name);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint LoadImageW(nint inst, string name, uint type, int cx, int cy, uint flags);
    [DllImport("user32.dll")] static extern nint CreatePopupMenu();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool AppendMenuW(nint menu, uint flags, nint id, string? text);
    [DllImport("user32.dll")] static extern int TrackPopupMenu(nint menu, uint flags, int x, int y, int reserved, nint hwnd, nint rect);
    [DllImport("user32.dll")] static extern bool DestroyMenu(nint menu);
    [DllImport("user32.dll")] static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(nint hwnd);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("kernel32.dll")] static extern nint GetModuleHandleW(nint name);

    readonly WndProc _proc;
    readonly nint _hwnd;
    readonly Dictionary<string, nint> _icons = new();
    string _icon = "sharing", _tip = "remoteman";
    bool _added;

    /// The icon was clicked: open the window.
    public event Action? Open;
    /// The menu, built when it is about to show: its lines, and what each item does (`null`
    /// for a line that only says something; a lone "-" is a separator).
    public Func<List<(string Text, Action? Do)>>? Menu;

    public Tray()
    {
        _proc = Procedure;
        var cls = new WNDCLASSW { lpfnWndProc = Marshal.GetFunctionPointerForDelegate(_proc), hInstance = GetModuleHandleW(0), lpszClassName = "remoteman-host-tray" };
        RegisterClassW(ref cls);
        _hwnd = CreateWindowExW(0, "remoteman-host-tray", "", 0, 0, 0, 0, 0, 0, 0, cls.hInstance, 0);
        foreach (var name in new[] { "sharing", "connected", "paused" })
        {
            var path = Path.Combine(AppContext.BaseDirectory, "Assets", $"{name}.ico");
            // LR_LOADFROMFILE, at the size the notification area draws.
            _icons[name] = LoadImageW(0, path, 1, GetSystemMetrics(49), GetSystemMetrics(50), 0x10);
        }
        Add();
    }

    NOTIFYICONDATAW Data(uint flags) => new()
    {
        cbSize = (uint)Marshal.SizeOf<NOTIFYICONDATAW>(),
        hWnd = _hwnd,
        uID = 1,
        uFlags = flags,
        uCallbackMessage = CALLBACK,
        hIcon = _icons.GetValueOrDefault(_icon),
        szTip = _tip.Length > 127 ? _tip[..127] : _tip,
        szInfo = "",
        szInfoTitle = "",
    };

    void Add()
    {
        var d = Data(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        _added = Shell_NotifyIconW(NIM_ADD, ref d);
        d.uVersion = 4;
        Shell_NotifyIconW(NIM_SETVERSION, ref d);
        Log.Say(_added ? "tray: the icon is shown" : "tray: Windows didn't take the icon");
    }

    /// Which icon (sharing, connected, paused) and what its tooltip says.
    public void Show(string icon, string tip)
    {
        if (icon == _icon && tip == _tip) return;
        (_icon, _tip) = (icon, tip);
        var d = Data(NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        Shell_NotifyIconW(NIM_MODIFY, ref d);
    }

    /// A notification from the icon.
    public void Notify(string title, string text)
    {
        var d = Data(NIF_INFO);
        d.szInfoTitle = title.Length > 63 ? title[..63] : title;
        d.szInfo = text.Length > 255 ? text[..255] : text;
        Shell_NotifyIconW(NIM_MODIFY, ref d);
        Log.Say($"notified: {title}: {text}");
    }

    nint Procedure(nint hwnd, uint msg, nint w, nint l)
    {
        if (msg == CALLBACK)
        {
            uint what = (uint)(l & 0xFFFF);
            if (what is WM_LBUTTONUP or NIN_SELECT or NIN_KEYSELECT) Open?.Invoke();
            else if (what is WM_RBUTTONUP or WM_CONTEXTMENU) ShowMenu();
            return 0;
        }
        // Explorer started again: the icon has to be added again.
        if (msg == TaskbarCreated) Add();
        return DefWindowProcW(hwnd, msg, w, l);
    }

    public void ShowMenu()
    {
        var items = Menu?.Invoke() ?? new();
        var menu = CreatePopupMenu();
        for (int i = 0; i < items.Count; i++)
        {
            var (text, act) = items[i];
            if (text == "-") AppendMenuW(menu, MF_SEPARATOR, 0, null);
            else AppendMenuW(menu, MF_STRING | (act is null ? MF_GRAYED : 0), i + 1, text.Replace("&", "&&"));
        }
        GetCursorPos(out var at);
        // Without this the menu stays open when the user clicks elsewhere.
        SetForegroundWindow(_hwnd);
        int chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, at.X, at.Y, 0, _hwnd, 0);
        DestroyMenu(menu);
        if (chosen > 0 && items[chosen - 1].Do is Action a)
        {
            Log.Say($"tray menu: {items[chosen - 1].Text}");
            a();
        }
    }

    public void Dispose()
    {
        if (_added)
        {
            var d = Data(0);
            Shell_NotifyIconW(NIM_DELETE, ref d);
            _added = false;
        }
        DestroyWindow(_hwnd);
    }
}
