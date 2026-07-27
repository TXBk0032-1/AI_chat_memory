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

$testRoot = Join-Path ([IO.Path]::GetTempPath()) "ai-chat-memory-installer-tests"
$sourceDirectory = Join-Path $testRoot "source"
$artifactsDirectory = Join-Path $testRoot "artifacts"
Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $sourceDirectory, $artifactsDirectory | Out-Null
try {
    foreach ($installer in $installers) {
        [IO.File]::WriteAllBytes((Join-Path $sourceDirectory $installer.name), [byte[]](1, 2, 3, 4))
    }

    & $Builder -ManifestOnly -SourceDirectory $sourceDirectory -ArtifactsDirectory $artifactsDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Manifest-only build failed with exit code $LASTEXITCODE"
    }

    $manifestPath = Join-Path $artifactsDirectory "manifest.json"
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        throw "Manifest was not created: $manifestPath"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    Assert-Equal 3 @($manifest.artifacts).Count "manifest artifact count"
    Assert-Equal "webview2-offline,webview2-online,webview2-system" (($manifest.artifacts.variant) -join ",") "manifest variants"
    foreach ($artifact in $manifest.artifacts) {
        $path = Join-Path $artifactsDirectory $artifact.name
        if (-not (Test-Path -LiteralPath $path)) {
            throw "Manifest artifact missing: $path"
        }
        Assert-Equal 4 $artifact.bytes "$($artifact.name) byte count"
        Assert-Equal ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()) $artifact.sha256 "$($artifact.name) hash"
    }

    [IO.File]::WriteAllBytes((Join-Path $sourceDirectory "unexpected.exe"), [byte[]](9))
    $extraFailed = $false
    try { & $Builder -ManifestOnly -SourceDirectory $sourceDirectory -ArtifactsDirectory $artifactsDirectory } catch { $extraFailed = $true }
    if (-not $extraFailed) { throw "Manifest-only build accepted an extra installer" }
    Remove-Item -LiteralPath (Join-Path $sourceDirectory "unexpected.exe") -Force

    Remove-Item -LiteralPath (Join-Path $sourceDirectory $installers[0].name) -Force
    $missingFailed = $false
    try { & $Builder -ManifestOnly -SourceDirectory $sourceDirectory -ArtifactsDirectory $artifactsDirectory } catch { $missingFailed = $true }
    if (-not $missingFailed) { throw "Manifest-only build accepted a missing installer" }
} finally {
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "PASS build-windows-installers plan contract" -ForegroundColor Green
