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
$InstallerBuilder = Join-Path $PSScriptRoot "build-windows-installers.ps1"
$env:RUSTUP_TOOLCHAIN = "1.97.0"
$env:CARGO_TERM_COLOR = "always"

function Initialize-CudaBuildEnvironment {
    $vcvars = $null
    if ($env:VSINSTALLDIR) {
        $candidate = Join-Path $env:VSINSTALLDIR "VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not $vcvars -and (Test-Path -LiteralPath $vswhere)) {
        $vsRoot = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
        if ($vsRoot) {
            $candidate = Join-Path $vsRoot "VC\Auxiliary\Build\vcvars64.bat"
            if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
        }
    }
    if (-not $vcvars) {
        throw "Visual Studio C++ build environment was not found"
    }

    $environment = cmd.exe /d /s /c "`"$vcvars`" >nul && set"
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to initialize MSVC environment via $vcvars"
    }
    foreach ($line in $environment) {
        $separator = $line.IndexOf('=')
        if ($separator -gt 0) {
            [Environment]::SetEnvironmentVariable($line.Substring(0, $separator), $line.Substring($separator + 1), "Process")
        }
    }

    $cudaRoot = $env:CUDA_PATH
    if (-not $cudaRoot -or -not (Test-Path -LiteralPath (Join-Path $cudaRoot "bin\nvcc.exe"))) {
        $nvccCommand = Get-Command nvcc -ErrorAction SilentlyContinue
        if ($nvccCommand) {
            $cudaRoot = Split-Path -Parent (Split-Path -Parent $nvccCommand.Source)
        }
    }
    if (-not $cudaRoot) {
        $cudaBase = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA"
        $cudaRoot = Get-ChildItem -LiteralPath $cudaBase -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "bin\nvcc.exe") } |
            Sort-Object Name -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $cudaRoot) {
        throw "CUDA Toolkit with nvcc was not found"
    }
    $nvcc = Join-Path $cudaRoot "bin\nvcc.exe"
    if (-not (Test-Path -LiteralPath $nvcc)) {
        throw "CUDA Toolkit with nvcc was not found"
    }
    $clCommand = Get-Command cl -ErrorAction SilentlyContinue
    if (-not $clCommand) { throw "cl.exe is not available after MSVC environment setup" }
    $msvcBin = Split-Path -Parent $clCommand.Source

    $env:CUDA_PATH = $cudaRoot
    $env:CUDA_HOME = $cudaRoot
    $env:NVCC_CCBIN = $msvcBin
    # CUDA 13.x CCCL requires MSVC's conforming preprocessor.
    $env:NVCC_APPEND_FLAGS = "-Xcompiler=/Zc:preprocessor"
    # CUDA 13 ships runtime DLLs under bin\x64; keep both for nvcc and runtime load.
    $env:PATH = "$cudaRoot\bin\x64;$cudaRoot\bin;$msvcBin;$env:PATH"

    if (-not (Get-Command nvcc -ErrorAction SilentlyContinue)) {
        throw "nvcc is not available after CUDA environment setup"
    }
    if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
        throw "cl.exe is not available after MSVC environment setup"
    }
}

Initialize-CudaBuildEnvironment

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

Invoke-Step "Run userscript tests" {
    node --test (Join-Path $Root "userscript/tests/capture.test.mjs")
}

Invoke-Step "Test Windows installer contract" {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "tests\build-windows-installers.Tests.ps1")
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
    Invoke-Step "Run frontend tests" {
        Push-Location $App
        try { npm test } finally { Pop-Location }
    }
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
    Invoke-Step "Build Windows NSIS installers" {
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $InstallerBuilder -ArtifactsDirectory $Artifacts -RustVersion $rustVersion
    }
}

Write-Host "`nLocal CI stage '$Stage' passed." -ForegroundColor Green
