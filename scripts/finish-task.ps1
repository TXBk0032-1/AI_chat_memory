[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Pipeline = Join-Path $PSScriptRoot "ci.ps1"

if (-not (Test-Path -LiteralPath $Pipeline)) {
    throw "Local CI pipeline not found: $Pipeline"
}

$changes = @(git -C $Root status --porcelain --untracked-files=all)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to inspect the Git working tree"
}
if ($changes.Count -gt 0) {
    throw "Commit all task changes before running the completion hook.`n$($changes -join "`n")"
}

Write-Host "==> Task changes are committed; starting release verification" -ForegroundColor Cyan
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Pipeline release
if ($LASTEXITCODE -ne 0) {
    throw "Task completion hook failed with exit code $LASTEXITCODE"
}

Write-Host "`nTask completion hook passed." -ForegroundColor Green
