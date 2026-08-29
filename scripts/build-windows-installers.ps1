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
$PortableVerifier = Join-Path $PSScriptRoot "verify-portable-archive.ps1"
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

    $sourceInstallers = @(Get-ChildItem -LiteralPath $InputDirectory -File -Filter *.exe)
    $expectedInstallerNames = @($installers.name | Sort-Object)
    $actualInstallerNames = @($sourceInstallers.Name | Sort-Object)
    if ($sourceInstallers.Count -ne $installers.Count -or ($expectedInstallerNames -join "`n") -cne ($actualInstallerNames -join "`n")) {
        throw "Expected exactly these installers: $($expectedInstallerNames -join ', '); found: $($actualInstallerNames -join ', ')"
    }
    if (@($sourceInstallers | Where-Object Length -le 0).Count -gt 0) {
        throw "Installer output contains an empty file"
    }

    $sourceArchives = @(Get-ChildItem -LiteralPath $InputDirectory -File -Filter *.zip)
    $actualArchiveNames = @($sourceArchives.Name | Sort-Object)
    if ($sourceArchives.Count -ne 1 -or $actualArchiveNames[0] -cne $portable.name) {
        throw "Expected exactly this portable archive: $($portable.name); found: $($actualArchiveNames -join ', ')"
    }
    $portableSource = Get-Item -LiteralPath (Join-Path $InputDirectory $portable.name)
    $archiveInfo = & $PortableVerifier -ArchivePath $portableSource.FullName -ExpectedEntryName $portable.entry_name

    $resolvedRustVersion = $RustVersion
    if (-not $RustVersion) {
        $rustOutput = ((& rustc --version 2>&1) -join " ").Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to resolve the Rust version: $rustOutput"
        }
        $resolvedRustVersion = $rustOutput
    }
    $commit = ((& git -C $Root rev-parse HEAD 2>&1) -join " ").Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to resolve the Git commit: $commit"
    }

    $outputFullPath = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\', '/')
    $outputRootPath = [IO.Path]::GetPathRoot($outputFullPath).TrimEnd('\', '/')
    if (-not $outputFullPath -or $outputFullPath -eq $outputRootPath) {
        throw "Refusing to publish to a filesystem root: $OutputDirectory"
    }
    if (Test-Path -LiteralPath $outputFullPath -PathType Leaf) {
        throw "Release output path is not a directory: $outputFullPath"
    }

    $outputParent = Split-Path -Parent $outputFullPath
    $outputName = Split-Path -Leaf $outputFullPath
    New-Item -ItemType Directory -Force -Path $outputParent | Out-Null
    $publishId = [Guid]::NewGuid().ToString('N')
    $publishDirectory = Join-Path $outputParent ".$outputName.publish-$publishId"
    $backupDirectory = Join-Path $outputParent ".$outputName.backup-$publishId"
    $backupCreated = $false
    New-Item -ItemType Directory -Path $publishDirectory | Out-Null
    try {
        $artifactRecords = @()
        foreach ($installer in $installers) {
            $source = Get-Item -LiteralPath (Join-Path $InputDirectory $installer.name)
            $artifact = Copy-Item -LiteralPath $source.FullName -Destination $publishDirectory -PassThru
            $artifactRecords += [ordered]@{
                name = $artifact.Name
                variant = $installer.variant
                webview_install_mode = $installer.webview_install_mode
                bytes = $artifact.Length
                sha256 = Get-Sha256 -LiteralPath $artifact.FullName
            }
        }
        $portableArtifact = Copy-Item -LiteralPath $portableSource.FullName -Destination $publishDirectory -PassThru
        $artifactRecords += [ordered]@{
            name = $portableArtifact.Name
            variant = $portable.variant
            webview_install_mode = $portable.webview_install_mode
            bytes = $portableArtifact.Length
            sha256 = Get-Sha256 -LiteralPath $portableArtifact.FullName
            archive_entry = $archiveInfo.entry_name
            archive_entry_bytes = $archiveInfo.entry_bytes
        }

        $manifest = [ordered]@{
            version = $version
            commit = $commit
            built_at_utc = [DateTime]::UtcNow.ToString("o")
            rust = $resolvedRustVersion
            artifacts = $artifactRecords
        }
        $manifestJson = $manifest | ConvertTo-Json -Depth 5
        $manifestPath = Join-Path $publishDirectory "manifest.json"
        [IO.File]::WriteAllText($manifestPath, $manifestJson, [Text.UTF8Encoding]::new($false))

        $expectedPublishNames = @($installers.name + $portable.name + "manifest.json" | Sort-Object)
        $actualPublishNames = @((Get-ChildItem -LiteralPath $publishDirectory -File).Name | Sort-Object)
        if (($expectedPublishNames -join "`n") -cne ($actualPublishNames -join "`n")) {
            throw "Release staging contains unexpected files: $($actualPublishNames -join ', ')"
        }

        if (Test-Path -LiteralPath $outputFullPath) {
            Move-Item -LiteralPath $outputFullPath -Destination $backupDirectory
            $backupCreated = $true
        }
        try {
            Move-Item -LiteralPath $publishDirectory -Destination $outputFullPath
        } catch {
            if ($backupCreated -and -not (Test-Path -LiteralPath $outputFullPath)) {
                Move-Item -LiteralPath $backupDirectory -Destination $outputFullPath
                $backupCreated = $false
            }
            throw
        }
        if ($backupCreated) {
            Remove-Item -LiteralPath $backupDirectory -Recurse -Force
            $backupCreated = $false
        }
    } finally {
        Remove-Item -LiteralPath $publishDirectory -Recurse -Force -ErrorAction SilentlyContinue
        if ($backupCreated -and (Test-Path -LiteralPath $backupDirectory)) {
            if (-not (Test-Path -LiteralPath $outputFullPath)) {
                Move-Item -LiteralPath $backupDirectory -Destination $outputFullPath
            } else {
                Remove-Item -LiteralPath $backupDirectory -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
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
            build = [ordered]@{
                beforeBuildCommand = ""
            }
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

    $portableSource = Join-Path $Rust "target\release\ai-chat-memory-desktop.exe"
    if (-not (Test-Path -LiteralPath $portableSource) -or (Get-Item -LiteralPath $portableSource).Length -le 0) {
        throw "Portable source executable is missing or empty: $portableSource"
    }
    # Reject a corrupted or truncated executable before it is packaged
    # (mirrors the PE header validation in build-dev-portable.ps1).
    $portableSourceStream = [IO.File]::OpenRead($portableSource)
    try {
        if ($portableSourceStream.ReadByte() -ne [byte][char]'M' -or $portableSourceStream.ReadByte() -ne [byte][char]'Z') {
            throw "Portable source executable is not a Windows PE executable: $portableSource"
        }
    } finally {
        $portableSourceStream.Dispose()
    }
    $portableEntryPath = Join-Path $stagingDirectory $portable.entry_name
    $portableArchivePath = Join-Path $stagingDirectory $portable.name
    Copy-Item -LiteralPath $portableSource -Destination $portableEntryPath -Force
    # Build the portable ZIP with the System.IO.Compression API and a fixed entry LastWriteTime so
    # the archive byte stream is deterministic across builds (Compress-Archive stamps entries with
    # the current time and is therefore non-reproducible).
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $fixedEntryTime = [datetimeoffset]::new(2026, 1, 1, 0, 0, 0, [System.TimeZoneInfo]::Local.GetUtcOffset([datetime]::new(2026, 1, 1)))
    $portableArchive = $null
    try {
        $portableArchive = [IO.Compression.ZipFile]::Open($portableArchivePath, [IO.Compression.ZipArchiveMode]::Create)
        $portableEntry = $portableArchive.CreateEntry($portable.entry_name, [IO.Compression.CompressionLevel]::Optimal)
        $portableEntry.LastWriteTime = $fixedEntryTime
        $entryStream = $portableEntry.Open()
        $fileStream = [IO.File]::OpenRead($portableEntryPath)
        try {
            $fileStream.CopyTo($entryStream)
        } finally {
            $entryStream.Dispose()
            $fileStream.Dispose()
        }
    } finally {
        if ($null -ne $portableArchive) {
            $portableArchive.Dispose()
        }
    }
    Remove-Item -LiteralPath $portableEntryPath -Force
    & $PortableVerifier -ArchivePath $portableArchivePath -ExpectedEntryName $portable.entry_name | Out-Null

    Write-ReleaseManifest -InputDirectory $stagingDirectory -OutputDirectory $ArtifactsDirectory
} finally {
    Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`nRelease artifacts: $ArtifactsDirectory" -ForegroundColor Green
Get-ChildItem -LiteralPath $ArtifactsDirectory | Select-Object Name, Length, LastWriteTime | Format-Table -AutoSize
