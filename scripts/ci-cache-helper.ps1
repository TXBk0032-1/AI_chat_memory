[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

function Get-InputFingerprint {
    param(
        [Parameter(Mandatory)][string]$RepoRoot,
        [Parameter(Mandatory)][string[]]$Paths
    )

    # 1. Get git blob hash of tracked files (disable quotepath to avoid octal escapes on non-ASCII paths)
    $entries = @(git -C $RepoRoot -c core.quotepath=false ls-files -s -- $Paths 2>$null)

    # 2. Check working tree modifications and untracked files
    $dirty = @(git -C $RepoRoot -c core.quotepath=false status --porcelain -uall -- $Paths 2>$null)
    if ($dirty.Count -gt 0) {
        $dirtyMap = @{}
        foreach ($line in $dirty) {
            if ($line.Length -lt 4) { continue }
            $statusCode = $line.Substring(0, 2).Trim()
            $rawPath = $line.Substring(3).Trim()

            # Handle rename or copy: "orig -> dest"
            $targetPath = $rawPath
            $oldPath = $null
            if ($rawPath.Contains(' -> ')) {
                $arrowIndex = $rawPath.IndexOf(' -> ')
                $oldPart = $rawPath.Substring(0, $arrowIndex).Trim()
                $newPart = $rawPath.Substring($arrowIndex + 4).Trim()
                if ($oldPart.StartsWith('"') -and $oldPart.EndsWith('"') -and $oldPart.Length -ge 2) {
                    $oldPart = $oldPart.Substring(1, $oldPart.Length - 2)
                }
                if ($newPart.StartsWith('"') -and $newPart.EndsWith('"') -and $newPart.Length -ge 2) {
                    $newPart = $newPart.Substring(1, $newPart.Length - 2)
                }
                $oldPath = $oldPart
                $targetPath = $newPart
            } else {
                if ($targetPath.StartsWith('"') -and $targetPath.EndsWith('"') -and $targetPath.Length -ge 2) {
                    $targetPath = $targetPath.Substring(1, $targetPath.Length - 2)
                }
            }

            $mapKey = if ($oldPath) { "$oldPath -> $targetPath" } else { $targetPath }
            $fullPath = Join-Path $RepoRoot $targetPath
            $isLeaf = $false
            try {
                $isLeaf = Test-Path -LiteralPath $fullPath -PathType Leaf -ErrorAction SilentlyContinue
            } catch {
                $isLeaf = $false
            }

            if ($isLeaf) {
                $hash = (git -C $RepoRoot hash-object -- $fullPath 2>$null)
                if (-not $hash) {
                    try {
                        $stream = [System.IO.FileStream]::new($fullPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
                        try {
                            $shaCalc = [System.Security.Cryptography.SHA256]::Create()
                            try {
                                $hash = [BitConverter]::ToString($shaCalc.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
                            } finally {
                                $shaCalc.Dispose()
                            }
                        } finally {
                            $stream.Dispose()
                        }
                    } catch {
                        $hash = "UNREADABLE"
                    }
                }
                $dirtyMap[$mapKey] = "$statusCode $hash"
            } else {
                $dirtyMap[$mapKey] = "$statusCode DELETED"
            }
        }
        $dirtyLines = @($dirtyMap.Keys | Sort-Object | ForEach-Object { "$($_):$($dirtyMap[$_])" })
        $allLines = $entries + $dirtyLines
    } else {
        $allLines = $entries
    }

    $bytes = [System.Text.Encoding]::UTF8.GetBytes(($allLines -join "`n"))
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}

function Test-ReleaseArtifactsValid {
    param(
        [Parameter(Mandatory)][string]$ArtifactsDirectory,
        [Parameter(Mandatory)][string]$ExpectedVersion
    )

    $manifestPath = Join-Path $ArtifactsDirectory "manifest.json"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        return $false
    }
    try {
        $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding utf8 | ConvertFrom-Json
    } catch {
        return $false
    }

    if ($manifest.version -ne $ExpectedVersion) {
        return $false
    }

    if (-not $manifest.artifacts -or $manifest.artifacts.Count -lt 4) {
        return $false
    }

    $expectedNames = @(
        "AI-Chat-Memory_${ExpectedVersion}_x64_webview2-offline-setup.exe",
        "AI-Chat-Memory_${ExpectedVersion}_x64_webview2-online-setup.exe",
        "AI-Chat-Memory_${ExpectedVersion}_x64_webview2-system-setup.exe",
        "AI-Chat-Memory_${ExpectedVersion}_x64_portable.zip"
    )
    $actualNames = @($manifest.artifacts | ForEach-Object { $_.name })
    foreach ($exp in $expectedNames) {
        if ($actualNames -notcontains $exp) {
            return $false
        }
    }

    foreach ($art in $manifest.artifacts) {
        $filePath = Join-Path $ArtifactsDirectory $art.name
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            return $false
        }
        $fileItem = Get-Item -LiteralPath $filePath
        if ($fileItem.Length -ne $art.bytes -or $fileItem.Length -le 0) {
            return $false
        }
    }

    return $true
}

function Test-FrontendCacheValid {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$CurrentFingerprint,
        [Parameter(Mandatory)][string]$AppDir,
        [Parameter(Mandatory)][string]$Stage
    )

    $cacheFile = Join-Path $CacheDir "frontend.json"
    if (-not (Test-Path -LiteralPath $cacheFile -PathType Leaf)) {
        return $false
    }
    $distIndex = Join-Path $AppDir "dist\index.html"
    if (-not (Test-Path -LiteralPath $distIndex -PathType Leaf)) {
        return $false
    }
    try {
        $cache = Get-Content -LiteralPath $cacheFile -Raw -Encoding utf8 | ConvertFrom-Json
        if ($cache.fingerprint -ne $CurrentFingerprint) {
            return $false
        }
        if ($Stage -in "test", "release") {
            return ($cache.tested -eq $true)
        }
        return ($cache.built -eq $true -or $cache.tested -eq $true)
    } catch {
        return $false
    }
}

function Save-FrontendCache {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$Fingerprint,
        [Parameter(Mandatory)][bool]$Tested
    )

    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    $cacheFile = Join-Path $CacheDir "frontend.json"
    $record = [ordered]@{
        fingerprint = $Fingerprint
        built = $true
        tested = $Tested
        updated_at_utc = [DateTime]::UtcNow.ToString("o")
    }
    $json = $record | ConvertTo-Json -Depth 3
    [System.IO.File]::WriteAllText($cacheFile, $json, [System.Text.UTF8Encoding]::new($false))
}

function Test-RustCacheValid {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$CurrentFingerprint,
        [Parameter(Mandatory)][string]$Stage
    )

    $cacheFile = Join-Path $CacheDir "rust.json"
    if (-not (Test-Path -LiteralPath $cacheFile -PathType Leaf)) {
        return $false
    }
    try {
        $cache = Get-Content -LiteralPath $cacheFile -Raw -Encoding utf8 | ConvertFrom-Json
        if ($cache.fingerprint -ne $CurrentFingerprint) {
            return $false
        }
        if ($Stage -in "test", "release") {
            return ($cache.tested -eq $true)
        }
        return ($cache.linted -eq $true -or $cache.tested -eq $true)
    } catch {
        return $false
    }
}

function Save-RustCache {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$Fingerprint,
        [Parameter(Mandatory)][bool]$Tested
    )

    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    $cacheFile = Join-Path $CacheDir "rust.json"
    $record = [ordered]@{
        fingerprint = $Fingerprint
        linted = $true
        tested = $Tested
        updated_at_utc = [DateTime]::UtcNow.ToString("o")
    }
    $json = $record | ConvertTo-Json -Depth 3
    [System.IO.File]::WriteAllText($cacheFile, $json, [System.Text.UTF8Encoding]::new($false))
}

function Test-ReleaseCacheValid {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$RepoRoot,
        [Parameter(Mandatory)][string]$FrontendFingerprint,
        [Parameter(Mandatory)][string]$RustFingerprint,
        [Parameter(Mandatory)][string]$ArtifactsDirectory,
        [Parameter(Mandatory)][string]$ExpectedVersion,
        [switch]$Force
    )

    if ($Force) {
        return $false
    }

    $artifactsOk = Test-ReleaseArtifactsValid -ArtifactsDirectory $ArtifactsDirectory -ExpectedVersion $ExpectedVersion
    if (-not $artifactsOk) {
        return $false
    }

    $manifestPath = Join-Path $ArtifactsDirectory "manifest.json"
    $manifest = $null
    if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
        try {
            $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding utf8 | ConvertFrom-Json
        } catch {
            return $false
        }
    }

    $releaseCacheFile = Join-Path $CacheDir "release.json"
    if (Test-Path -LiteralPath $releaseCacheFile -PathType Leaf) {
        try {
            $rc = Get-Content -LiteralPath $releaseCacheFile -Raw -Encoding utf8 | ConvertFrom-Json
            if ($rc.frontend_fingerprint -eq $FrontendFingerprint -and
                $rc.rust_fingerprint -eq $RustFingerprint -and
                $rc.version -eq $ExpectedVersion) {
                if ($rc.commit -and $manifest -and $manifest.commit -and ($manifest.commit -ne $rc.commit)) {
                    return $false
                }
                return $true
            }
            return $false
        } catch {
            return $false
        }
    }

    # Fallback: only if release.json does not exist, but existing manifest matches clean HEAD commit
    if ($manifest) {
        try {
            $currentCommit = ((& git -C $RepoRoot rev-parse HEAD 2>$null) -join " ").Trim()
            $cleanTree = (@(git -C $RepoRoot -c core.quotepath=false status --porcelain --untracked-files=all 2>$null).Count -eq 0)
            if ($currentCommit -and $manifest.commit -eq $currentCommit -and $cleanTree) {
                Save-ReleaseCache -CacheDir $CacheDir `
                    -FrontendFingerprint $FrontendFingerprint `
                    -RustFingerprint $RustFingerprint `
                    -Version $ExpectedVersion `
                    -Commit $currentCommit
                return $true
            }
        } catch {
            return $false
        }
    }

    return $false
}

function Save-ReleaseCache {
    param(
        [Parameter(Mandatory)][string]$CacheDir,
        [Parameter(Mandatory)][string]$FrontendFingerprint,
        [Parameter(Mandatory)][string]$RustFingerprint,
        [Parameter(Mandatory)][string]$Version,
        [string]$Commit
    )

    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    $cacheFile = Join-Path $CacheDir "release.json"
    $record = [ordered]@{
        frontend_fingerprint = $FrontendFingerprint
        rust_fingerprint = $RustFingerprint
        version = $Version
        commit = $Commit
        updated_at_utc = [DateTime]::UtcNow.ToString("o")
    }
    $json = $record | ConvertTo-Json -Depth 3
    [System.IO.File]::WriteAllText($cacheFile, $json, [System.Text.UTF8Encoding]::new($false))
}
