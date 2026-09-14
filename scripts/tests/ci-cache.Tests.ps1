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
    & git -C $tempTestDir config core.autocrlf false

    $gitIgnore = Join-Path $tempTestDir ".gitignore"
    Set-Content -LiteralPath $gitIgnore -Value ".ci-cache/`nartifacts/`napp/`n" -Encoding utf8
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
    Remove-Item -LiteralPath $fileC -Force

    # Non-ASCII / Chinese filename handling
    $chineseFilename = -join [char[]]@(0x6D4B, 0x8BD5, 0x0020, 0x6587, 0x4EF6, 0x002E, 0x0074, 0x0078, 0x0074) # "ceshi wenjian.txt"
    $fileChinese = Join-Path $tempTestDir $chineseFilename
    $contentChinese1 = -join [char[]]@(0x4E2D, 0x6587, 0x5185, 0x5BB9) # "zhongwen neirong"
    $contentChinese2 = -join [char[]]@(0x4FEE, 0x6539, 0x540E, 0x7684, 0x4E2D, 0x6587, 0x5185, 0x5BB9) # "xiugaihou de zhongwen neirong"
    Set-Content -LiteralPath $fileChinese -Value $contentChinese1 -Encoding utf8
    & git -C $tempTestDir add .
    & git -C $tempTestDir commit -m "add chinese file" --quiet
    $fpChinese1 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @($chineseFilename)
    Set-Content -LiteralPath $fileChinese -Value $contentChinese2 -Encoding utf8
    $fpChinese2 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @($chineseFilename)
    if ($fpChinese1 -eq $fpChinese2) {
        throw "fingerprint must change when non-ASCII file is modified"
    }
    & git -C $tempTestDir checkout -- $chineseFilename

    # Git rename handling (R  old -> new) must not crash with illegal characters
    & git -C $tempTestDir mv fileA.txt fileA_renamed.txt
    $fpRenamed = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("fileA.txt", "fileA_renamed.txt")
    if (-not $fpRenamed) {
        throw "fingerprint must succeed on renamed files"
    }
    & git -C $tempTestDir reset --hard HEAD --quiet

    # Pathspec exclusion test: "dir", ":(exclude)dir/sub"
    $subDir = Join-Path $tempTestDir "app_nested"
    $subIgnore = Join-Path $subDir "src-tauri"
    New-Item -ItemType Directory -Force -Path $subIgnore | Out-Null
    $fNested = Join-Path $subDir "file1.txt"
    $fIgnored = Join-Path $subIgnore "file2.txt"
    Set-Content -LiteralPath $fNested -Value "nested content" -Encoding utf8
    Set-Content -LiteralPath $fIgnored -Value "ignored content" -Encoding utf8
    & git -C $tempTestDir add .
    & git -C $tempTestDir commit -m "add nested" --quiet

    $fpExcluded1 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("app_nested", ":(exclude)app_nested/src-tauri")
    Set-Content -LiteralPath $fIgnored -Value "ignored changed" -Encoding utf8
    $fpExcluded2 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("app_nested", ":(exclude)app_nested/src-tauri")
    Assert-Equal $fpExcluded1 $fpExcluded2 "excluded path change does not alter fingerprint"

    Set-Content -LiteralPath $fNested -Value "nested changed" -Encoding utf8
    $fpExcluded3 = Get-InputFingerprint -RepoRoot $tempTestDir -Paths @("app_nested", ":(exclude)app_nested/src-tauri")
    if ($fpExcluded1 -eq $fpExcluded3) {
        throw "included path change must alter fingerprint"
    }
    & git -C $tempTestDir reset --hard HEAD --quiet

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
    $headCommit = ((& git -C $tempTestDir rev-parse HEAD 2>$null) -join " ").Trim()

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
        commit = $headCommit
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
    Save-ReleaseCache -CacheDir $testCacheDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -Version $ver -Commit $headCommit
    Assert-Equal "True" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache valid"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff2" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache invalid when frontend changed"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf2" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache invalid when rust changed"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver -Force)) "release cache invalid when Force is specified"

    # Test release cache commit mismatch with manifest
    Save-ReleaseCache -CacheDir $testCacheDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -Version $ver -Commit "oldcommit"
    Assert-Equal "False" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "release cache invalid when cache commit does not match manifest commit"

    # Test fallback mechanism when release.json is missing but manifest matches clean HEAD
    Remove-Item -LiteralPath (Join-Path $testCacheDir "release.json") -Force
    Assert-Equal "True" ([string](Test-ReleaseCacheValid -CacheDir $testCacheDir -RepoRoot $tempTestDir -FrontendFingerprint "ff1" -RustFingerprint "rf1" -ArtifactsDirectory $testArtifactsDir -ExpectedVersion $ver)) "fallback restores release cache on clean matching commit"
} finally {
    Remove-Item -LiteralPath $tempTestDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "PASS ci-cache contract" -ForegroundColor Green
