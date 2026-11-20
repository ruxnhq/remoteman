# Makes viewer.ico, the icon embedded in remoteman.exe (build.rs), from the viewer's app icon
# ("Slipstream", assets/icon/viewer.svg) as the Apple build has it rasterised full bleed at 1024
# (apple/Support/Assets.xcassets/AppIcon.appiconset/ios-1024.png): a rounded tile, as the app's
# own logo is drawn (corners 22 % of the tile), at the sizes Windows asks for, each stored as PNG.
#   powershell -ExecutionPolicy Bypass -File crates\remoteman-viewer-app-windows\assets\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$here = $PSScriptRoot
$root = Resolve-Path (Join-Path $here '..\..\..')
$source = [System.Drawing.Image]::FromFile((Join-Path $root 'apple\Support\Assets.xcassets\AppIcon.appiconset\ios-1024.png'))
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
$pngs = foreach ($n in $sizes) {
  $bmp = New-Object System.Drawing.Bitmap $n, $n, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'; $g.InterpolationMode = 'HighQualityBicubic'; $g.PixelOffsetMode = 'HighQuality'; $g.CompositingQuality = 'HighQuality'
  $g.Clear([System.Drawing.Color]::Transparent)
  # A little room around the tile at the larger sizes, none at the smallest (every pixel counts).
  $inset = if ($n -ge 48) { $n * 0.03 } else { 0 }
  $side = $n - 2 * $inset
  $r = $side * 0.22
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $path.AddArc($inset, $inset, 2 * $r, 2 * $r, 180, 90)
  $path.AddArc($inset + $side - 2 * $r, $inset, 2 * $r, 2 * $r, 270, 90)
  $path.AddArc($inset + $side - 2 * $r, $inset + $side - 2 * $r, 2 * $r, 2 * $r, 0, 90)
  $path.AddArc($inset, $inset + $side - 2 * $r, 2 * $r, 2 * $r, 90, 90)
  $path.CloseFigure()
  $brush = New-Object System.Drawing.TextureBrush $source
  $scale = $side / $source.Width
  $brush.ScaleTransform($scale, $scale, [System.Drawing.Drawing2D.MatrixOrder]::Append)
  $brush.TranslateTransform($inset, $inset, [System.Drawing.Drawing2D.MatrixOrder]::Append)
  $g.FillPath($brush, $path)
  $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  , $ms.ToArray()
}
$source.Dispose()
# ICONDIR, one ICONDIRENTRY per size (PNG-compressed images, as Windows takes since Vista), then the images.
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $n = $sizes[$i]; $len = $pngs[$i].Length
  $w.Write([byte]($n % 256)); $w.Write([byte]($n % 256)); $w.Write([byte]0); $w.Write([byte]0)
  $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$len); $w.Write([uint32]$offset)
  $offset += $len
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $here 'viewer.ico'), $out.ToArray())
"viewer.ico: $($sizes -join ', ') px, $($out.Length) bytes"
