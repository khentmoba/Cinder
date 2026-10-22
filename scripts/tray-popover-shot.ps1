# Opens the tray popover the way a user opens it - by clicking the tray icon -
# and takes a picture of it, so the popover can be looked at on a machine
# where nobody is going to click anything.
#
#   powershell -File scripts/tray-popover-shot.ps1 <out.png> [x y w h]

param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$X = 1520, [int]$Y = 430, [int]$W = 400, [int]$H = 615
)

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$root = [System.Windows.Automation.AutomationElement]::RootElement
$buttons = $root.FindAll(
    [System.Windows.Automation.TreeScope]::Descendants,
    (New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)))

# The tray icon carries the tooltip, which starts with the app's name.
$target = $null
foreach ($b in $buttons) {
    if ($b.Current.Name -like 'Cinder*') { $target = $b; break }
}
if (-not $target) {
    Write-Output "no Cinder icon in the notification area"
    exit 1
}
$target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Write-Output "clicked the tray icon"

Start-Sleep -Milliseconds 1500
& "$PSScriptRoot\screenshot.ps1" $Out $X $Y $W $H
