Add-Type -AssemblyName System.Drawing
$targets = @(
  @{ name = 'antigravity'; path = 'C:\Users\Admin\AppData\Local\Programs\antigravity\Antigravity.exe' },
  @{ name = 'opencode';    path = 'C:\Users\Admin\AppData\Local\Programs\@opencode-aidesktop\OpenCode.exe' }
)
foreach ($t in $targets) {
  if (-not (Test-Path $t.path)) { Write-Output "missing: $($t.path)"; continue }
  $ico = [System.Drawing.Icon]::ExtractAssociatedIcon($t.path)
  if ($null -eq $ico) { Write-Output "no icon: $($t.name)"; continue }
  $bmp = $ico.ToBitmap()
  $big = New-Object System.Drawing.Bitmap 256, 256
  $g = [System.Drawing.Graphics]::FromImage($big)
  $g.InterpolationMode = 'HighQualityBicubic'
  $g.PixelOffsetMode = 'HighQuality'
  $g.DrawImage($bmp, 0, 0, 256, 256)
  $out = "C:\APPLICATIONS\KhentUsage\src\icons\$($t.name).png"
  $big.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $big.Dispose(); $bmp.Dispose()
  Write-Output "$($t.name) -> $out (source $($ico.Width)x$($ico.Height))"
}
