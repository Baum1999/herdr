# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=codex
# HERDR_INTEGRATION_VERSION=9

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

function Fail-Report([string]$category) {
    [Console]::Error.WriteLine("herdr Codex session report $category (pane $env:HERDR_PANE_ID)")
    exit 1
}

if ([string]::IsNullOrWhiteSpace($env:HERDR_SOCKET_PATH)) { Fail-Report "unavailable" }

try {
    $payload = [Console]::In.ReadToEnd() | ConvertFrom-Json -ErrorAction Stop
} catch {
    Fail-Report "invalid hook input"
}
if ([string]::IsNullOrWhiteSpace($payload.hook_event_name)) { Fail-Report "invalid hook input" }
if ($payload.hook_event_name -ne "SessionStart") { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($payload.transcript_path)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($sessionId) -or
    $payload.source -notin @("startup", "resume", "clear", "compact")) {
    Fail-Report "invalid hook input"
}
if (-not [string]::IsNullOrWhiteSpace($env:CODEX_THREAD_ID) -and $env:CODEX_THREAD_ID -ne $sessionId) { exit 0 }

$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    $output = & $herdr pane report-codex-session $env:HERDR_PANE_ID --agent-session-id $sessionId --session-start-source $payload.source 2>$null
    $status = "$output".Trim()
    if ($LASTEXITCODE -eq 0 -and $status -in @("applied", "unchanged")) { exit 0 }
    if ($status -in @("invalidated", "rejected")) { Fail-Report $status }
    Fail-Report "outcome unknown"
} catch {
    Fail-Report "outcome unknown"
}
