[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } elseif ($MyInvocation.MyCommand.Path) { Split-Path -Parent $MyInvocation.MyCommand.Path } else { (Get-Location).Path }
$Root = Split-Path -Parent $ScriptDir
$Pipeline = Join-Path $ScriptDir "ci.ps1"

if (-not (Test-Path -LiteralPath $Pipeline)) {
    throw "Local CI pipeline not found: $Pipeline"
}

$changes = @(git -C $Root -c core.quotepath=false status --porcelain --untracked-files=all)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to inspect the Git working tree"
}
if ($changes.Count -gt 0) {
    throw "Commit all task changes before running the completion hook.`n$($changes -join "`n")"
}

Write-Host "==> Task changes are committed; starting release verification" -ForegroundColor Cyan
$pipelineArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $Pipeline, "release")
if ($Force) { $pipelineArgs += "-Force" }
if ($Clean) { $pipelineArgs += "-Clean" }

& powershell.exe @pipelineArgs
if ($LASTEXITCODE -ne 0) {
    throw "Task completion hook failed with exit code $LASTEXITCODE"
}

Write-Host "`nTask completion hook passed." -ForegroundColor Green
