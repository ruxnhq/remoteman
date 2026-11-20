# A small window that takes the keyboard, so a test can see how remoteman looks unfocused:
#   thief.ps1 X Y SECONDS LOG    writes the foreground window's title into LOG once it is up.
param([int]$X = 1300, [int]$Y = 80, [int]$Seconds = 10, [string]$Log = "C:\rm\thief.log")
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; using System.Text; public class Fg { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n); }'
# Start it normally (a hidden start would hide the form too) and put its console away here.
[void][Fg]::ShowWindow([Fg]::GetConsoleWindow(), 0)
$f = New-Object Windows.Forms.Form
$f.Text = "focus thief"; $f.StartPosition = "Manual"; $f.Location = New-Object Drawing.Point($X, $Y); $f.Size = New-Object Drawing.Size(420, 260)
$t = New-Object Windows.Forms.Timer; $t.Interval = 1500
$t.add_Tick({ $sb = New-Object Text.StringBuilder 256; [Fg]::GetWindowText([Fg]::GetForegroundWindow(), $sb, 256) | Out-Null; Add-Content $Log ("foreground: " + $sb.ToString()); $t.Stop() })
$k = New-Object Windows.Forms.Timer; $k.Interval = $Seconds * 1000; $k.add_Tick({ $f.Close() })
$f.add_Shown({ $f.Activate(); $t.Start(); $k.Start() })
[void]$f.ShowDialog()
