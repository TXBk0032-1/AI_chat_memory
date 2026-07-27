[CmdletBinding()]
param(
    [switch]$PlanOnly
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "app"
$version = (Get-Content -LiteralPath (Join-Path $App "package.json") -Raw | ConvertFrom-Json).version
$languages = @("SimpChinese", "English")
$installerDefinitions = @(
    [ordered]@{
        variant = "webview2-offline"
        webview_install_mode = "offlineInstaller"
        output_suffix = "webview2-offline-setup"
    },
    [ordered]@{
        variant = "webview2-online"
        webview_install_mode = "downloadBootstrapper"
        output_suffix = "webview2-online-setup"
    },
    [ordered]@{
        variant = "webview2-system"
        webview_install_mode = "skip"
        output_suffix = "webview2-system-setup"
    }
)

$installers = @($installerDefinitions | ForEach-Object {
    [ordered]@{
        variant = $_.variant
        webview_install_mode = $_.webview_install_mode
        name = "AI-Chat-Memory_${version}_x64_$($_.output_suffix).exe"
    }
})

if ($PlanOnly) {
    [ordered]@{
        version = $version
        languages = $languages
        display_language_selector = $true
        installers = $installers
    } | ConvertTo-Json -Depth 5
    exit 0
}

throw "Installer build mode is not implemented yet"
