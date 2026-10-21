# Checks Cinder's numbers against the raw logs, with an implementation that
# shares no code with the collector.
#
#   powershell -File scripts/check-logs.ps1 [YYYY-MM-DD ...]
#
# For every day given, it reads ~/.claude/projects the way Claude Code writes
# it (scripts/check-claude-logs.mjs) and asks the collector for the same
# numbers (the ignored real_home_day_check test), then compares them line for
# line. The two have to be identical for the numbers to be right.
param()

# With -File, only the first argument binds to a parameter, so the days are
# read from the argument list itself.
$Days = @($args)
if ($Days.Count -eq 0) {
    # The two days before yesterday: today is still being written to.
    $Days = @(
        (Get-Date).AddDays(-2).ToUniversalTime().ToString('yyyy-MM-dd'),
        (Get-Date).AddDays(-3).ToUniversalTime().ToString('yyyy-MM-dd')
    )
}

$failed = $false
foreach ($day in $Days) {
    Write-Output "=== $day ==="

    $mine = node "$PSScriptRoot\check-claude-logs.mjs" $day |
        Where-Object { $_ -like "ROW`t*" } |
        Sort-Object
    if (-not $mine) {
        Write-Output "  no Claude Code usage that day"
        continue
    }

    $env:CINDER_CHECK_DAY = $day
    $env:CINDER_CHECK_AGENT = 'Claude Code'
    $theirs = cargo test --manifest-path src-tauri\Cargo.toml -- --ignored --nocapture real_home_day_check 2>&1 |
        Where-Object { $_ -like "ROW`t*" } |
        ForEach-Object { $_.Trim() } |
        Sort-Object

    $theirs = @($theirs)
    if ($theirs.Count -ne $mine.Count) {
        Write-Output "  MISMATCH: the collector has $($theirs.Count) models, the logs have $($mine.Count)"
        $failed = $true
        continue
    }
    $same = $true
    for ($i = 0; $i -lt $mine.Count; $i++) {
        if ($mine[$i] -ne $theirs[$i]) {
            Write-Output "  MISMATCH"
            Write-Output "    logs:     $($mine[$i])"
            Write-Output "    collector: $($theirs[$i])"
            $same = $false
        }
    }
    if ($same) {
        Write-Output "  OK: $($mine.Count) models, the collector and the raw logs agree"
        $mine | ForEach-Object { Write-Output "    $_" }
    } else {
        $failed = $true
    }
}

if ($failed) { exit 1 }
Write-Output "every day agrees with the raw logs"
