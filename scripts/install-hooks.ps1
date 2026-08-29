[CmdletBinding()]
param([switch]$Remove)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$HookDirectory = Join-Path $Root ".githooks"

if ($Remove) {
    # CI-6: 'git config --unset' exits with code 5 when the key was never set,
    # which used to surface as an error under $ErrorActionPreference = "Stop".
    # Only unset when the local configuration actually defines core.hooksPath.
    $currentHooksPath = git -C $Root config --local --get core.hooksPath 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace(([string]$currentHooksPath))) {
        Write-Host "Local Git hooks are not installed (core.hooksPath is not set); nothing to remove."
        exit 0
    }
    git -C $Root config --unset core.hooksPath
    Write-Host "Local Git hooks disabled."
    exit 0
}

New-Item -ItemType Directory -Force $HookDirectory | Out-Null
git -C $Root config core.hooksPath .githooks
Write-Host "Local Git hooks enabled. pre-push will run the test pipeline."
