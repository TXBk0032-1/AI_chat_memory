[CmdletBinding()]
param(
    [ValidateSet("check", "test", "release")]
    [string]$Stage = "check",
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "app"
$Rust = Join-Path $App "src-tauri"
$Artifacts = Join-Path $Root "artifacts"
$env:RUSTUP_TOOLCHAIN = "stable"
$env:CARGO_TERM_COLOR = "always"

function Invoke-Step {
    param([string]$Name, [scriptblock]$Action)
    Write-Host "`n==> $Name" -ForegroundColor Cyan
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Action
    if ($LASTEXITCODE -ne 0) { throw "$Name failed with exit code $LASTEXITCODE" }
    $watch.Stop()
    Write-Host ("OK  {0} ({1:n1}s)" -f $Name, $watch.Elapsed.TotalSeconds) -ForegroundColor Green
}

foreach ($command in "git", "node", "npm", "rustc", "cargo") {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required command not found: $command"
    }
}

$rustVersion = (& rustc --version 2>&1) -join " "
if ($rustVersion -notmatch '^rustc 1\.97\.') {
    throw "Expected Rust 1.97.x, found: $rustVersion"
}

if ($Clean) {
    Invoke-Step "Clean generated output" {
        Push-Location $Rust
        try { cargo clean } finally { Pop-Location }
        Remove-Item -LiteralPath (Join-Path $App "dist") -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $Artifacts -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$nodeModules = Join-Path $App "node_modules"
$installMarker = Join-Path $nodeModules ".package-lock.json"
$lockFile = Join-Path $App "package-lock.json"
if (-not (Test-Path $installMarker) -or (Get-Item $lockFile).LastWriteTimeUtc -gt (Get-Item $installMarker).LastWriteTimeUtc) {
    Invoke-Step "Install locked frontend dependencies" {
        Push-Location $App
        try { npm ci --prefer-offline --no-audit --no-fund } finally { Pop-Location }
    }
} else {
    Write-Host "==> Frontend dependencies are current" -ForegroundColor DarkGray
}

Invoke-Step "Validate userscript syntax" {
    node --check (Join-Path $Root "userscript/dist/ai-chat-memory.user.js")
}

Invoke-Step "Check Rust formatting" {
    Push-Location $Rust
    try { cargo fmt --check } finally { Pop-Location }
}

Invoke-Step "Lint Rust" {
    Push-Location $Rust
    try { cargo clippy --all-targets --all-features -- -D warnings } finally { Pop-Location }
}

Invoke-Step "Type-check and build frontend" {
    Push-Location $App
    try { npm run build } finally { Pop-Location }
}

if ($Stage -in "test", "release") {
    Invoke-Step "Run Rust tests" {
        Push-Location $Rust
        try { cargo test --all-features } finally { Pop-Location }
    }
}

if ($Stage -eq "release") {
    $runningApp = Get-CimInstance Win32_Process -Filter "Name = 'ai-chat-memory-desktop.exe'" -ErrorAction SilentlyContinue
    if ($runningApp) {
        $ids = ($runningApp.ProcessId -join ", ")
        throw "Close AI Chat Memory before release build (running process IDs: $ids)"
    }
    Invoke-Step "Build Windows MSI" {
        Push-Location $App
        try { npm run tauri build -- --bundles msi } finally { Pop-Location }
    }

    New-Item -ItemType Directory -Force $Artifacts | Out-Null
    $msi = Get-ChildItem (Join-Path $Rust "target/release/bundle/msi") -Filter *.msi |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    $exe = Get-Item (Join-Path $Rust "target/release/ai-chat-memory-desktop.exe")
    if (-not $msi -or -not $exe) { throw "Release build completed without expected artifacts" }

    Copy-Item $msi.FullName $Artifacts -Force
    Copy-Item $exe.FullName $Artifacts -Force
    $commit = (git -C $Root rev-parse HEAD).Trim()
    $version = (Get-Content (Join-Path $App "package.json") -Raw | ConvertFrom-Json).version
    $manifest = [ordered]@{
        version = $version
        commit = $commit
        built_at_utc = [DateTime]::UtcNow.ToString("o")
        rust = $rustVersion
        artifacts = @(Get-ChildItem $Artifacts -File | ForEach-Object {
            [ordered]@{ name = $_.Name; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
        })
    }
    $manifestJson = $manifest | ConvertTo-Json -Depth 5
    $manifestPath = Join-Path $Artifacts "manifest.json"
    [System.IO.File]::WriteAllText($manifestPath, $manifestJson, [System.Text.UTF8Encoding]::new($false))
    Write-Host "`nRelease artifacts: $Artifacts" -ForegroundColor Green
    Get-ChildItem $Artifacts | Select-Object Name, Length, LastWriteTime | Format-Table -AutoSize
}

Write-Host "`nLocal CI stage '$Stage' passed." -ForegroundColor Green
