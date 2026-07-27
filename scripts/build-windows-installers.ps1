[CmdletBinding(DefaultParameterSetName = "Build")]
param(
    [Parameter(ParameterSetName = "Plan")]
    [switch]$PlanOnly,
    [Parameter(Mandatory, ParameterSetName = "Manifest")]
    [switch]$ManifestOnly,
    [Parameter(Mandatory, ParameterSetName = "Manifest")]
    [string]$SourceDirectory,
    [string]$ArtifactsDirectory,
    [string]$RustVersion
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "app"
$Rust = Join-Path $App "src-tauri"
if (-not $ArtifactsDirectory) {
    $ArtifactsDirectory = Join-Path $Root "artifacts"
}
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
$portable = [ordered]@{
    variant = "portable"
    webview_install_mode = "skip"
    name = "AI-Chat-Memory_${version}_x64_portable.zip"
    entry_name = "AI-Chat-Memory_${version}_x64_portable.exe"
}

function Clear-Directory {
    param([Parameter(Mandatory)][string]$LiteralPath)

    $fullPath = [IO.Path]::GetFullPath($LiteralPath).TrimEnd('\', '/')
    $rootPath = [IO.Path]::GetPathRoot($fullPath).TrimEnd('\', '/')
    if (-not $fullPath -or $fullPath -eq $rootPath) {
        throw "Refusing to clear a filesystem root: $LiteralPath"
    }
    New-Item -ItemType Directory -Force -Path $fullPath | Out-Null
    Get-ChildItem -LiteralPath $fullPath -Force | Remove-Item -Recurse -Force
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$LiteralPath)

    return (Get-FileHash -Algorithm SHA256 -LiteralPath $LiteralPath).Hash.ToLowerInvariant()
}

function Write-ReleaseManifest {
    param(
        [Parameter(Mandatory)][string]$InputDirectory,
        [Parameter(Mandatory)][string]$OutputDirectory
    )

    $sourceFiles = @(Get-ChildItem -LiteralPath $InputDirectory -File -Filter *.exe)
    $expectedNames = @($installers.name | Sort-Object)
    $actualNames = @($sourceFiles.Name | Sort-Object)
    if ($sourceFiles.Count -ne $installers.Count -or ($expectedNames -join "`n") -cne ($actualNames -join "`n")) {
        throw "Expected exactly these installers: $($expectedNames -join ', '); found: $($actualNames -join ', ')"
    }
    if (@($sourceFiles | Where-Object Length -le 0).Count -gt 0) {
        throw "Installer output contains an empty file"
    }

    Clear-Directory -LiteralPath $OutputDirectory
    $artifactRecords = @()
    foreach ($installer in $installers) {
        $source = Get-Item -LiteralPath (Join-Path $InputDirectory $installer.name)
        $artifact = Copy-Item -LiteralPath $source.FullName -Destination $OutputDirectory -Force -PassThru
        $artifactRecords += [ordered]@{
            name = $artifact.Name
            variant = $installer.variant
            webview_install_mode = $installer.webview_install_mode
            bytes = $artifact.Length
            sha256 = Get-Sha256 -LiteralPath $artifact.FullName
        }
    }

    if (-not $RustVersion) {
        $RustVersion = ((& rustc --version 2>&1) -join " ").Trim()
    }
    $commit = ((& git -C $Root rev-parse HEAD 2>&1) -join " ").Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to resolve the Git commit: $commit"
    }
    $manifest = [ordered]@{
        version = $version
        commit = $commit
        built_at_utc = [DateTime]::UtcNow.ToString("o")
        rust = $RustVersion
        artifacts = $artifactRecords
    }
    $manifestJson = $manifest | ConvertTo-Json -Depth 5
    $manifestPath = Join-Path $OutputDirectory "manifest.json"
    [IO.File]::WriteAllText($manifestPath, $manifestJson, [Text.UTF8Encoding]::new($false))
}

if ($PlanOnly) {
    [ordered]@{
        version = $version
        languages = $languages
        display_language_selector = $true
        installers = $installers
        portable = $portable
    } | ConvertTo-Json -Depth 5
    exit 0
}

if ($ManifestOnly) {
    Write-ReleaseManifest -InputDirectory $SourceDirectory -OutputDirectory $ArtifactsDirectory
    exit 0
}

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) "ai-chat-memory-installers-$([Guid]::NewGuid().ToString('N'))"
$stagingDirectory = Join-Path $temporaryRoot "staging"
$nsisDirectory = Join-Path $Rust "target\release\bundle\nsis"
New-Item -ItemType Directory -Force -Path $temporaryRoot, $stagingDirectory | Out-Null
try {
    foreach ($installer in $installers) {
        $mode = [ordered]@{ type = $installer.webview_install_mode }
        if ($installer.webview_install_mode -ne "skip") {
            $mode.silent = $true
        }
        $override = [ordered]@{
            bundle = [ordered]@{
                windows = [ordered]@{
                    webviewInstallMode = $mode
                }
            }
        }
        $overridePath = Join-Path $temporaryRoot "$($installer.variant).json"
        [IO.File]::WriteAllText(
            $overridePath,
            ($override | ConvertTo-Json -Depth 5),
            [Text.UTF8Encoding]::new($false)
        )

        Clear-Directory -LiteralPath $nsisDirectory
        Write-Host "`n==> Build $($installer.variant) ($($installer.webview_install_mode))" -ForegroundColor Cyan
        Push-Location $App
        try {
            & npm.cmd run tauri -- build --bundles nsis --config $overridePath --ci
            if ($LASTEXITCODE -ne 0) {
                throw "Tauri build failed for $($installer.variant) with exit code $LASTEXITCODE"
            }
        } finally {
            Pop-Location
        }

        $bundleFiles = @(Get-ChildItem -LiteralPath $nsisDirectory -File -Filter *.exe)
        if ($bundleFiles.Count -ne 1 -or $bundleFiles[0].Length -le 0) {
            throw "Expected one non-empty NSIS installer for $($installer.variant), found $($bundleFiles.Count)"
        }
        Copy-Item -LiteralPath $bundleFiles[0].FullName -Destination (Join-Path $stagingDirectory $installer.name) -Force
    }

    Write-ReleaseManifest -InputDirectory $stagingDirectory -OutputDirectory $ArtifactsDirectory
} finally {
    Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`nRelease artifacts: $ArtifactsDirectory" -ForegroundColor Green
Get-ChildItem -LiteralPath $ArtifactsDirectory | Select-Object Name, Length, LastWriteTime | Format-Table -AutoSize
