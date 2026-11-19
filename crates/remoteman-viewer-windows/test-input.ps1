# Real input for tests of the Windows viewer with nobody at the keyboard: dot-source this in a
# snippet run in the guest's desktop session (tools/desk.py), then call the functions. Places
# are in the viewer window's client area, in pixels. What is sent goes through Windows' own
# input (mouse_event, keybd_event), so it takes the same way as a hand's.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Threading;
public class RmIn {
  [StructLayout(LayoutKind.Sequential)] public struct P { public int x; public int y; }
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr h, ref P p);
  [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] static extern void mouse_event(uint f, uint dx, uint dy, int data, UIntPtr e);
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint f, UIntPtr e);
  [DllImport("user32.dll")] static extern short VkKeyScanW(char c);
  [DllImport("user32.dll")] static extern uint MapVirtualKeyW(uint code, uint how);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint f);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out R r);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int l, t, r, b; }
  public static IntPtr Window() { SetProcessDPIAware(); return FindWindowW("remoteman", null); }
  // The viewer in front of everything, with the keyboard.
  public static string Top() {
    IntPtr h = Window(); if (h == IntPtr.Zero) return "no window";
    SetWindowPos(h, new IntPtr(-1), 0, 0, 0, 0, 3);
    keybd_event(0x87, 0, 0, UIntPtr.Zero); keybd_event(0x87, 0, 2, UIntPtr.Zero);
    SetForegroundWindow(h); Thread.Sleep(200);
    R r; GetClientRect(h, out r); P p = new P(); ClientToScreen(h, ref p);
    return "client " + r.r + "x" + r.b + " at " + p.x + "," + p.y;
  }
  // The window's outer size, where it is.
  public static void Size(int w, int h) { SetWindowPos(Window(), IntPtr.Zero, 0, 0, w, h, 0x16); Thread.Sleep(100); }
  public static void Move(int x, int y) { P p = new P(); p.x = x; p.y = y; ClientToScreen(Window(), ref p); SetCursorPos(p.x, p.y); mouse_event(1, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(30); }
  public static void Click(int x, int y) { Move(x, y); mouse_event(2, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(40); mouse_event(4, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(120); }
  public static void RightClick(int x, int y) { Move(x, y); mouse_event(8, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(40); mouse_event(16, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(120); }
  public static void Drag(int x0, int y0, int x1, int y1) { Move(x0, y0); mouse_event(2, 0, 0, 0, UIntPtr.Zero); for (int i = 1; i <= 10; i++) { Move(x0 + (x1 - x0) * i / 10, y0 + (y1 - y0) * i / 10); } mouse_event(4, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(120); }
  public static void Wheel(int notches) { mouse_event(0x800, 0, 0, notches * 120, UIntPtr.Zero); Thread.Sleep(80); }
  static void Vk(byte vk, bool down) { keybd_event(vk, (byte)MapVirtualKeyW(vk, 0), down ? 0u : 2u, UIntPtr.Zero); Thread.Sleep(15); }
  public static void Key(byte vk) { Vk(vk, true); Vk(vk, false); Thread.Sleep(40); }
  public static void Chord(byte[] vks) { foreach (byte v in vks) Vk(v, true); Thread.Sleep(40); for (int i = vks.Length - 1; i >= 0; i--) Vk(vks[i], false); Thread.Sleep(80); }
  public static void Text(string s) { foreach (char c in s) { short k = VkKeyScanW(c); bool shift = (k & 0x100) != 0; if (shift) Vk(0x10, true); Key((byte)(k & 0xFF)); if (shift) Vk(0x10, false); } }
}
'@
function Shot($path) {
  $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
  $image = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $image.Size)
  $image.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}
