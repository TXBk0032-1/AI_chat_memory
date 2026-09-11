[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } elseif ($MyInvocation.MyCommand.Path) { Split-Path -Parent $MyInvocation.MyCommand.Path } else { (Get-Location).Path }
$Root = Split-Path -Parent (Split-Path -Parent $ScriptDir)
$CacheHelper = Join-Path $Root "scripts\ci-cache-helper.ps1"
$FinishTaskScript = Join-Path $Root "scripts\finish-task.ps1"
$CiScript = Join-Path $Root "scripts\ci.ps1"

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

if (-not (Test-Path -LiteralPath $CacheHelper -PathType Leaf)) {
    throw "ci-cache-helper.ps1 missing: $CacheHelper"
}
. $CacheHelper

# 1. Parameter contract verification
$finishTaskCommand = Get-Command -Name $FinishTaskScript
Assert-Equal "True" ([string]($finishTaskCommand.Parameters.ContainsKey('Force'))) "finish-task.ps1 has Force parameter"
Assert-Equal "True" ([string]($finishTaskCommand.Parameters.ContainsKey('Clean'))) "finish-task.ps1 has Clean parameter"

$ciCommand = Get-Command -Name $CiScript
Assert-Equal "True" ([string]($ciCommand.Parameters.ContainsKey('Force'))) "ci.ps1 has Force parameter"
Assert-Equal "True" ([string]($ciCommand.Parameters.ContainsKey('Clean'))) "ci.ps1 has Clean parameter"
Assert-Equal "True" ([string]($ciCommand.Parameters.ContainsKey('Stage'))) "ci.ps1 has Stage parameter"

# 2. Get-InputFingerprint idempotency and sensitivity
$tempTestDir = Join-Path ([System.IO.Path]::GetTempPath()) "ci-cache-test-$([System.Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $tempTestDir | Out-Null
try {
    & git -C $tempTestDir init --quiet
    & git -C $tempTestDir config user.name "CI Tester"
    & git -C $tempTestDir config user.email "ci-test@example.com"

    $fileA = Join-Path $tempTestDir "fileA.txt"
    $fileB = Join-Path $tempTestDir "fileB.txt"
    Set-Content -LiteralPath $fileA -Value "hello frontend" -Encoding utf8
    Set-Content -LiteralPath $fileB -Value "hello backend" -Encoding utf8
    & git -C $tempTestDir add .
    & git -C $tempTestDir commit -m "initial commit" --quiet

    $fp1 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileA.txt")
    $fp2 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileA.txt")
    Assert-Equal $fp1 $fp2 "fingerprint is idempotent"

    $fpBackend1 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileB.txt")
    if ($fp1 -eq $fpBackend1) {
        throw "fingerprint of different files must not collide"
    }

    # Modify fileA: fileA fingerprint changes, fileB fingerprint remains unchanged
    Set-Content -LiteralPath $fileA -Value "hello frontend modified" -Encoding utf8
    $fp1Modified = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileA.txt")
    if ($fp1 -eq $fp1Modified) {
        throw "fingerprint must change when file is modified"
    }
    $fpBackend2 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileB.txt")
    Assert-Equal $fpBackend1 $fpBackend2 "backend fingerprint is unaffected by frontend file modification"

    # Add untracked file to frontend set
    $fileC = Join-Path $tempTestDir "fileC.txt"
    Set-Content -LiteralPath $fileC -Value "new untracked file" -Encoding utf8
    $fp1WithC = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileA.txt", "fileC.txt")
    if ($fp1Modified -eq $fp1WithC) {
        throw "fingerprint must change when untracked file is added"
    }

    # 3. Test Test-FrontendCacheValid & Save-FrontendCache
    $testCacheDir = Join-Path $tempTestDir ".ci-cache"
    $testAppDir = Join-Path $tempTestDir "app"
    $testDistDir = Join-Path $testAppDir "dist"
    New-Item -ItemType Directory -Force -Path $testDistDir | Out-Null
    $distIndex = Join-Path $testDistDir "index.html"
    Set-Content -LiteralPath $distIndex -Value "<html></html>" -Encoding utf8

    Assert-Equal "False" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "check")) "cache invalid when file missing"

    Save-FrontendCache -CacheDir $testCacheDir -Fingerprint "hash1" -Tested $false
    Assert-Equal "True" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "check")) "cache valid for check stage"
    Assert-Equal "False" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "test")) "cache invalid for test stage if not tested"
    Assert-Equal "False" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "release")) "cache invalid for release stage if not tested"
    Assert-Equal "False" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash2" -AppDir $testAppDir -Stage "check")) "cache invalid on fingerprint mismatch"

    # Missing dist/index.html invalidates cache
    Remove-Item -LiteralPath $distIndex -Force
    Assert-Equal "False" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "check")) "cache invalid if dist/index.html is missing"
    Set-Content -LiteralPath $distIndex -Value "<html></html>" -Encoding utf8

    Save-FrontendCache -CacheDir $testCacheDir -Fingerprint "hash1" -Tested $true
    Assert-Equal "True" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "test")) "cache valid for test stage when tested"
    Assert-Equal "True" ([string](Test-FrontendCacheValid -CacheDir $testCacheDir -CurrentFingerprint "hash1" -AppDir $testAppDir -Stage "release")) "cache valid for release stage when tested"

    # 4. Test Test-RustCacheValid & Save-RustCache
    Assert-Equal "False" ([string](Test-RustCacheValid -CacheDir $testCacheDir -CurrentFingerprint "rhash1" -Stage "check")) "rust cache invalid when file missing"
    Save-RustCache -CacheDir $testCacheDir -Fingerprint "rhash1" -Tested $false
    Assert-Equal "True" ([string](Test-RustCacheValid -CacheDir $testCacheDir -CurrentFingerprint "rhash1" -Stage "check")) "rust cache valid for check stage"
    Assert-Equal "False" ([string](Test-RustCacheValid -CacheDir $testCacheDir -CurrentFingerprint "rhash1" -Stage "test")) "rust cache invalid for test stage if not tested"
    Save-RustCache -CacheDir $testCacheDir -Fingerprint "rhash1" -Tested $true
    Assert-Equal "True" ([string](Test-RustCacheValid -CacheDir $testCacheDir -CurrentFingerprint "rhash1" -Stage "test")) "rust cache valid for test stage when tested"
    Assert-Equal "False" ([string](Test-RustCacheValid -CacheDir $testCacheDir -CurrentFingerprint "rhash2" -Stage "test")) "rust cache invalid on fingerprint mismatch"

    # 5. Test Test-ReleaseArtifactsValid & Test-ReleaseCacheValid
    $testArtifactsDir = Join-Path $tempTestDir "artifacts"
    New-Item -ItemType Directory -Force -Path $testArtifactsDir | Out-Null
    $ver = "1.0.0"

    Assert-Equal "False" ([string](Test-ReleaseArtifactsValid -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "artifacts invalid with missing manifest"

    $artNames = @(
        "AI-Chat-Memory_${ver}_x64_webview2-offline-setup.exe",
        "AI-Chat-Memory_${ver}_x64_webview2-online-setup.exe",
        "AI-Chat-Memory_${ver}_x64_webview2-system-setup.exe",
        "AI-Chat-Memory_${ver}_x64_portable.zip"
    )
    $artList = @()
    foreach ($an in $artNames) {
        $p = Join-Path $testArtifactsDir $an
        [System.IO.File]::WriteAllBytes($p, [byte[]](1, 2, 3, 4))
        $artList += [ordered]@{
            name = $an
            variant = "test"
            webview_install_mode = "test"
            bytes = 4
            sha256 = "dummy"
        }
    }
    $mockManifest = [ordered]@{
        version = $ver
        commit = "testcommit"
        built_at_utc = [DateTime]::UtcNow.ToString("o")
        artifacts = $artList
    }
    $manifestPath = Join-Path $testArtifactsDir "manifest.json"
    [System.IO.File]::WriteAllText($manifestPath, ($mockManifest | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))

    Assert-Equal "True" ([string](Test-ReleaseArtifactsValid -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "artifacts valid with all files matching manifest"

    # Version mismatch
    Assert-Equal "False" ([string](Test-ReleaseArtifactsValid -ArtifactsDirectory $testArtifactsDir -ExpectedVersion "2.0.0")) "artifacts invalid on version mismatch"

    # File length mismatch / corrupted
    [System.IO.File]::WriteAllBytes((Join-Path $testArtifactsDir $artNames[0]), [byte[]](1, 2))
    Assert-Equal "False" ([string](Test-ReleaseArtifactsValid -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "artifacts invalid on file size mismatch"
    [System.IO.File]::WriteAllBytes((Join-Path $testArtifactsDir $artNames[0]), [byte[]](1, 2, 3, 4))

    # Test-ReleaseCacheValid and Force parameter
    Save-ReleaseCache -CacheDir $testCacheDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -Version $ver
    Assert-Equal "True" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache valid"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff2" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache invalid when frontend changed"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf2" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache invalid when rust changed"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver -Force)) "release cache invalid when Force is specified"
} finally {
    Remove-Item -LiteralPath $tempTestDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "PASS ci-cache contract" -ForegroundColor Green
