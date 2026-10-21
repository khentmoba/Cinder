# Finds the Cinder icon in the Windows notification area and clicks it, so the
# tray popover can be opened the way a user opens it.
#
#   powershell -File scripts/tray-click.ps1
#
# The icon is found through UI Automation, the same way the shell finds it.
param([string]$Name = 'Cinder')

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = [System.Windows.Automation.AutomationElement]::RootElement
$buttons = $root.FindAll(
    [System.Windows.Automation.TreeScope]::Descendants,
    (New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)))

foreach ($b in $buttons) {
    $r = $b.Current.BoundingRectangle
    Write-Output ("{0,-40} {1},{2} {3}x{4}" -f $b.Current.Name, [int]$r.X, [int]$r.Y, [int]$r.Width, [int]$r.Height)
}

$target = $null
foreach ($b in $buttons) {
    if ($b.Current.Name -eq $Name) { $target = $b; break }
}
if (-not $target) {
    Write-Output "no button named $Name"
    exit 1
}
$target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Write-Output "clicked $($target.Current.Name)"
