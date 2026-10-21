<#
.SYNOPSIS
    Publishes a Cinder release to Discord via Webhook.
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Version,

    [Parameter(Position = 1)]
    [string]$Summary,

    [string]$ExePath,

    [string]$WebhookUrl,

    [switch]$DryRun,

    [switch]$NoUpload
)

$pyScript = Join-Path $PSScriptRoot "notify_discord.py"
$pyArgs = @()
if ($DryRun) { $pyArgs += "--dry-run" }
if ($NoUpload) { $pyArgs += "--no-upload" }
if ($Version) { $pyArgs += @("--version", $Version) }
if ($Summary) { $pyArgs += @("--summary", $Summary) }
if ($ExePath) { $pyArgs += @("--exe", $ExePath) }
if ($WebhookUrl) { $pyArgs += @("--webhook", $WebhookUrl) }

python $pyScript @pyArgs
exit $LASTEXITCODE
