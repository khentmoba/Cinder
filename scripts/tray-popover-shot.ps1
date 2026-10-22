# Opens the tray popover the way a user opens it - by clicking the tray icon -
# and takes a picture of it, so the popover can be looked at on a machine
# where nobody is going to click anything.
#
#   powershell -File scripts/tray-popover-shot.ps1 <out.png> [x y w h]
#
# The icon is found through UI Automation, the same way the shell finds it.
# When it is behind the notification area's chevron, the chevron is opened
# first - that is what a user does too.

param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$X = 1520, [int]$Y = 430, [int]$W = 400, [int]$H = 615
)

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = [System.Windows.Automation.AutomationElement]::RootElement

function Find-TrayIcon {
    $buttons = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button)))
    $fallback = $null
    foreach ($b in $buttons) {
        $r = $b.Current.BoundingRectangle
        if ($r.IsEmpty) { continue }
        if ($b.Current.Name -notlike 'Cinder*') { continue }
        # The icon in the notification area is small; the taskbar button for
        # the window carries the same name and is not it.
        if ($r.Width -gt 48 -or $r.Height -gt 48) { continue }
        if ($b.Current.ClassName -like 'SystemTray*') { return $b }
        $fallback = $b
    }
    return $fallback
}

$target = Find-TrayIcon
if (-not $target) {
    # Open the hidden icons and look again.
    $buttons = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button)))
    foreach ($b in $buttons) {
        if ($b.Current.Name -like 'Show Hidden Icons*') {
            $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            break
        }
    }
    Start-Sleep -Milliseconds 900
    $target = Find-TrayIcon
}
if (-not $target) {
    Write-Output "no Cinder icon in the notification area"
    exit 1
}

$clicked = $false
try {
    $target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    $clicked = $true
} catch {
    # Some shell icons do not take an invoke; click them where they are.
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Mouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, IntPtr e);
}
"@
    $r = $target.Current.BoundingRectangle
    [Mouse]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)) | Out-Null
    [Mouse]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
    [Mouse]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
    $clicked = $true
}
Write-Output "clicked the tray icon"

Start-Sleep -Milliseconds 1800
& "$PSScriptRoot\screenshot.ps1" $Out $X $Y $W $H
