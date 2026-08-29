<#
.SYNOPSIS
Builds one uncompressed debug EXE with the shortest incremental path.

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-dev-portable.ps1

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-dev-portable.ps1 -ReuseFrontend
#>
[CmdletBinding()]
param(
    [switch]$ReuseFrontend,
    [switch]$PlanOnly,
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "app"
$Rust = Join-Path $App "src-tauri"
$FrontendEntry = Join-Path $App "dist\index.html"
$ConfigPath = Join-Path $Rust "tauri.conf.json"
$Config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$Version = $Config.version
$OutputName = "AI-Chat-Memory_${Version}_x64_dev.exe"

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $Root "artifacts\dev"
} elseif (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $Root $OutputDirectory
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
# CI-10: rebuild the frontend by default so a stale app\dist can never be
# embedded into the dev EXE; reuse requires an explicit -ReuseFrontend.
$FrontendEntryExists = Test-Path -LiteralPath $FrontendEntry
if ($ReuseFrontend -and -not $FrontendEntryExists) {
    Write-Host "==> -ReuseFrontend was requested but the frontend dist is missing: $FrontendEntry; building it instead" -ForegroundColor Yellow
}
$FrontendAction = if ($ReuseFrontend -and $FrontendEntryExists) {
    "reuse"
} else {
    "build"
}
$FrontendPreference = if ($ReuseFrontend) { "reuse" } else { "build" }

$Plan = [ordered]@{
    frontend_preference = $FrontendPreference
    frontend_action = $FrontendAction
    frontend_runtime = "embedded"
    cargo_profile = "debug"
    output_name = $OutputName
    output_path = Join-Path $OutputDirectory $OutputName
    output_file_count = 1
}
if ($PlanOnly) {
    $Plan | ConvertTo-Json -Depth 3
    exit 0
}

foreach ($Command in "cargo", "npm") {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) {
        throw "Required command not found: $Command"
    }
}

$env:RUSTUP_TOOLCHAIN = "1.97.0"

# Detect CUDA Toolkit: prefer $env:CUDA_PATH, otherwise pick the newest installed toolkit.
$CudaRoot = $env:CUDA_PATH
if (-not $CudaRoot -or -not (Test-Path -LiteralPath (Join-Path $CudaRoot "bin\nvcc.exe"))) {
    $cudaBase = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA"
    $CudaRoot = Get-ChildItem -LiteralPath $cudaBase -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "bin\nvcc.exe") } |
        Sort-Object Name -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $CudaRoot -or -not (Test-Path -LiteralPath (Join-Path $CudaRoot "bin\nvcc.exe"))) {
    throw "CUDA Toolkit (nvcc) was not found. Set CUDA_PATH or install it under $cudaBase"
}

# Detect Visual Studio 2022 via vswhere so the MSVC patch version is not hardcoded.
$vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw "vswhere.exe was not found: $vswhere"
}
$vsInstall = (& $vswhere -latest -products * -property installationPath).Trim()
if (-not $vsInstall) {
    throw "Visual Studio installation was not found via vswhere"
}
$VcVars = Join-Path $vsInstall "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path -LiteralPath $VcVars)) {
    throw "Visual Studio 2022 build environment was not found: $VcVars"
}

if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $Environment = cmd.exe /d /s /c "`"$VcVars`" >nul && set"
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to initialize MSVC environment"
    }
    foreach ($Line in $Environment) {
        $Separator = $Line.IndexOf('=')
        if ($Separator -gt 0) {
            [Environment]::SetEnvironmentVariable(
                $Line.Substring(0, $Separator),
                $Line.Substring($Separator + 1),
                "Process"
            )
        }
    }
}

$MsvcBin = Split-Path -Parent (Get-Command cl.exe -ErrorAction Stop).Source
$env:CUDA_PATH = $CudaRoot
$env:CUDA_HOME = $CudaRoot
$env:NVCC_CCBIN = $MsvcBin
$env:NVCC_APPEND_FLAGS = "-Xcompiler=/Zc:preprocessor"
$env:PATH = "$CudaRoot\bin\x64;$CudaRoot\bin;$MsvcBin;$env:PATH"

if ($FrontendAction -eq "build") {
    Write-Host "==> Build frontend" -ForegroundColor Cyan
    Push-Location $App
    try {
        & npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw "Frontend build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "==> Reuse existing frontend dist" -ForegroundColor DarkGray
}

Write-Host "==> Build incremental debug executable" -ForegroundColor Cyan
Push-Location $Rust
$PreviousTauriConfig = $env:TAURI_CONFIG
try {
    $env:TAURI_CONFIG = '{"build":{"devUrl":null}}'
    & cargo build --all-features --bin ai-chat-memory-desktop
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed with exit code $LASTEXITCODE" }
} finally {
    $env:TAURI_CONFIG = $PreviousTauriConfig
    Pop-Location
}

$Source = Join-Path $Rust "target\debug\ai-chat-memory-desktop.exe"
if (-not (Test-Path -LiteralPath $Source)) {
    throw "Built executable was not found: $Source"
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputPath = Join-Path $OutputDirectory $OutputName
Copy-Item -LiteralPath $Source -Destination $OutputPath -Force

$Item = Get-Item -LiteralPath $OutputPath
$Stream = [IO.File]::OpenRead($Item.FullName)
try {
    if ($Stream.ReadByte() -ne [byte][char]'M' -or $Stream.ReadByte() -ne [byte][char]'Z') {
        throw "Output is not a Windows PE executable: $OutputPath"
    }
} finally {
    $Stream.Dispose()
}

$Hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Item.FullName).Hash
[pscustomobject]@{
    path = $Item.FullName
    bytes = $Item.Length
    sha256 = $Hash
    frontend = $FrontendAction
    profile = "debug"
}
