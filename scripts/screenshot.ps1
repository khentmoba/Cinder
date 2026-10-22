# Takes a screenshot of the screen, so the tray popover can be looked at.
#
#   powershell -File scripts/screenshot.ps1 <out.png> [x y w h]
#
# Without a rectangle the whole screen is captured. The image is written with
# the same pixel data that is on the display, so what is in the file is what
# the user sees.
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$X = 0,
    [int]$Y = 0,
    [int]$W = 0,
    [int]$H = 0
)

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
if ($W -le 0 -or $H -le 0) {
    $X = $bounds.X
    $Y = $bounds.Y
    $W = $bounds.Width
    $H = $bounds.Height
}

$bmp = New-Object System.Drawing.Bitmap($W, $H)
$graphics = [System.Drawing.Graphics]::FromImage($bmp)
$graphics.CopyFromScreen($X, $Y, 0, 0, (New-Object System.Drawing.Size($W, $H)))
$graphics.Dispose()

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "wrote $Out ($W x $H)"
