[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Builder = Join-Path $Root "scripts\build-dev-portable.ps1"

function Assert-Equal {
    param(
        [Parameter(Mandatory)]$Expected,
        [Parameter(Mandatory)]$Actual,
        [Parameter(Mandatory)][string]$Label
    )

    if ([string]$Expected -cne [string]$Actual) {
        throw "$Label expected '$Expected', got '$Actual'"
    }
}

if (-not (Test-Path -LiteralPath $Builder)) {
    throw "Development builder missing: $Builder"
}

$defaultPlan = & $Builder -PlanOnly | ConvertFrom-Json
Assert-Equal "build" $defaultPlan.frontend_preference "default frontend preference"
Assert-Equal "build" $defaultPlan.frontend_action "default frontend action"
Assert-Equal "debug" $defaultPlan.cargo_profile "Cargo profile"
Assert-Equal "embedded" $defaultPlan.frontend_runtime "frontend runtime"
Assert-Equal "AI-Chat-Memory_0.1.0_x64_dev.exe" $defaultPlan.output_name "output name"
Assert-Equal "1" $defaultPlan.output_file_count "output file count"

$reusePlan = & $Builder -PlanOnly -ReuseFrontend | ConvertFrom-Json
Assert-Equal "reuse" $reusePlan.frontend_preference "explicit reuse frontend preference"
$frontendEntry = Join-Path $Root "app\dist\index.html"
$reuseExpectedAction = if (Test-Path -LiteralPath $frontendEntry) { "reuse" } else { "build" }
Assert-Equal $reuseExpectedAction $reusePlan.frontend_action "explicit reuse frontend action"
Assert-Equal "embedded" $reusePlan.frontend_runtime "reused frontend runtime"

$source = Get-Content -LiteralPath $Builder -Raw
$embeddedRuntimeFragments = @(
    '$PreviousTauriConfig = $env:TAURI_CONFIG',
    '$env:TAURI_CONFIG = ''{"build":{"devUrl":null}}''',
    '$env:TAURI_CONFIG = $PreviousTauriConfig'
)
foreach ($fragment in $embeddedRuntimeFragments) {
    if (-not $source.Contains($fragment)) {
        throw "Development builder does not enforce embedded frontend runtime: $fragment"
    }
}
if ($source -match 'tauri\s+build') {
    throw "Development builder must not invoke tauri build"
}
if ($source -match 'Compress-Archive|System\.IO\.Compression|\.zip') {
    throw "Development builder must not compress its output"
}
if ($source -notmatch 'cargo\s+build') {
    throw "Development builder does not invoke cargo build"
}

Write-Host "PASS build-dev-portable contract" -ForegroundColor Green
