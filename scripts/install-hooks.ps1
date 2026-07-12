[CmdletBinding()]
param([switch]$Remove)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$HookDirectory = Join-Path $Root ".githooks"

if ($Remove) {
    git -C $Root config --unset core.hooksPath
    Write-Host "Local Git hooks disabled."
    exit 0
}

New-Item -ItemType Directory -Force $HookDirectory | Out-Null
git -C $Root config core.hooksPath .githooks
Write-Host "Local Git hooks enabled. pre-push will run the test pipeline."
