# The visible top-level windows of a process (and every top-level window over DISPLAY1 above a
# given one in z-order), with class, rectangle and extended style.
param([int]$ProcId, [switch]$Above)
Add-Type -TypeDefinition @'
using System; using System.Text; using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int size);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
'@ -ErrorAction SilentlyContinue
$list = New-Object System.Collections.ArrayList
$cb = [W+EnumProc]{ param($h, $l) [void]$list.Add($h); $true }
[W]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
$n = 0
foreach ($h in $list) {
  $p = 0; [W]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null
  if (-not [W]::IsWindowVisible($h)) { continue }
  $cloaked = 0; [W]::DwmGetWindowAttribute($h, 14, [ref]$cloaked, 4) | Out-Null
  if ($cloaked -ne 0) { continue }
  $r = New-Object W+RECT; [W]::GetWindowRect($h, [ref]$r) | Out-Null
  $onDisplay1 = $r.R -gt 0 -and $r.L -lt 3840 -and $r.B -gt 0 -and $r.T -lt 2160
  $c = New-Object Text.StringBuilder 128; [W]::GetClassName($h, $c, 128) | Out-Null
  $t = New-Object Text.StringBuilder 128; [W]::GetWindowText($h, $t, 128) | Out-Null
  $ex = [W]::GetWindowLongPtr($h, -20).ToInt64()
  $mine = $p -eq $ProcId
  if ($mine -or ($Above -and $onDisplay1)) {
    "{0} pid={1} {2} '{3}' {4},{5},{6},{7} ex=0x{8:x}{9}" -f $n, $p, $c, $t, $r.L, $r.T, $r.R, $r.B, $ex, $(if ($ex -band 8) { ' TOPMOST' } else { '' })
  }
  $n++
  if ($Above -and $mine -and $c.ToString() -eq 'remoteman-engine-full') { break }
}
