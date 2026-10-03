Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class IconEx {
  [DllImport("shell32.dll", CharSet = CharSet.Auto)]
  public static extern uint ExtractIconEx(string lpszFile, int nIconIndex, IntPtr[] phiconLarge, IntPtr[] phiconSmall, uint nIcons);
  [DllImport("user32.dll")]
  public static extern bool DestroyIcon(IntPtr hIcon);
}
"@
$targets = @(
  @{ name = 'antigravity'; path = 'C:\Users\Admin\AppData\Local\Programs\antigravity\Antigravity.exe' },
  @{ name = 'opencode';    path = 'C:\Users\Admin\AppData\Local\Programs\@opencode-aidesktop\OpenCode.exe' }
)
foreach ($t in $targets) {
  $large = New-Object IntPtr[] 1
  $count = [IconEx]::ExtractIconEx($t.path, 0, $large, $null, 1)
  if ($count -lt 1 -or $large[0] -eq [IntPtr]::Zero) { Write-Output "extract failed: $($t.name)"; continue }
  $ico = [System.Drawing.Icon]::FromHandle($large[0])
  $bmp = $ico.ToBitmap()
  $big = New-Object System.Drawing.Bitmap 256, 256
  $g = [System.Drawing.Graphics]::FromImage($big)
  $g.InterpolationMode = 'HighQualityBicubic'
  $g.PixelOffsetMode = 'HighQuality'
  $g.DrawImage($bmp, 0, 0, 256, 256)
  $out = "C:\APPLICATIONS\KhentUsage\src\icons\$($t.name).png"
  $big.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  Write-Output "$($t.name): source $($bmp.Width)x$($bmp.Height) -> $out"
  $g.Dispose(); $big.Dispose(); $bmp.Dispose(); $ico.Dispose()
  [IconEx]::DestroyIcon($large[0]) | Out-Null
}
