[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Builder = Join-Path $Root "scripts\build-windows-installers.ps1"
$TauriConfig = Join-Path $Root "app\src-tauri\tauri.conf.json"

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
    throw "Installer builder missing: $Builder"
}

$planJson = & $Builder -PlanOnly
if ($LASTEXITCODE -ne 0) {
    throw "Installer builder plan failed with exit code $LASTEXITCODE"
}
$plan = $planJson | ConvertFrom-Json
$installers = @($plan.installers)

Assert-Equal 3 $installers.Count "installer count"
Assert-Equal "SimpChinese,English" ($plan.languages -join ",") "plan languages"
Assert-Equal "True" ([string]$plan.display_language_selector) "language selector"
Assert-Equal "webview2-offline,webview2-online,webview2-system" ($installers.variant -join ",") "variants"
Assert-Equal "offlineInstaller,downloadBootstrapper,skip" ($installers.webview_install_mode -join ",") "WebView2 modes"
Assert-Equal 3 @($installers.name | Sort-Object -Unique).Count "unique installer names"

foreach ($installer in $installers) {
    if ($installer.name -notmatch '^AI-Chat-Memory_0\.1\.0_x64_webview2-(offline|online|system)-setup\.exe$') {
        throw "Unexpected installer name: $($installer.name)"
    }
}

$config = Get-Content -LiteralPath $TauriConfig -Raw | ConvertFrom-Json
Assert-Equal "SimpChinese,English" ($config.bundle.windows.nsis.languages -join ",") "Tauri NSIS languages"
Assert-Equal "True" ([string]$config.bundle.windows.nsis.displayLanguageSelector) "Tauri language selector"

Write-Host "PASS build-windows-installers plan contract" -ForegroundColor Green
