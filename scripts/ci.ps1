[CmdletBinding()]
param(
    [ValidateSet("check", "test", "release", "quick")]
    [string]$Stage = "check",
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# pwsh 7 拉起的 powershell.exe 5.1 子进程会继承 pwsh 的 PSModulePath，导致 5.1
# 自动加载不到系统自带模块（如 Microsoft.PowerShell.Utility 的 Get-FileHash）。
# 在派生任何子进程前补回 5.1 的系统模块目录；宿主本身是 5.1 时该目录已在其中，无副作用。
$windowsPowerShellModules = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\Modules"
if (($env:PSModulePath -split ";") -notcontains $windowsPowerShellModules) {
    $env:PSModulePath = "$windowsPowerShellModules;$env:PSModulePath"
}
# 传给 5.1 子进程的干净模块路径：只含 5.1 自身的系统与机器级目录，确保
# 二进制模块（Microsoft.PowerShell.Utility 等）可被自动加载，不受宿主环境影响。
$win51ModulePath = @(
    $windowsPowerShellModules,
    (Join-Path $env:ProgramFiles "WindowsPowerShell\Modules")
) -join ";"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "app"
$Rust = Join-Path $App "src-tauri"
$Artifacts = Join-Path $Root "artifacts"
$InstallerBuilder = Join-Path $PSScriptRoot "build-windows-installers.ps1"
$env:RUSTUP_TOOLCHAIN = "1.97.0"
$env:CARGO_TERM_COLOR = "always"

function Initialize-CudaBuildEnvironment {
    $vcvars = $null
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    # CUDA 13.3 的 nvcc 只适配到 MSVC 14.4x（VS2022）。runner 镜像可能同时装有
    # VS 18（MSVC 14.5x），-allow-unsupported-compiler 并不能保证其可编译；
    # 因此优先锁定 VS2022，仅在缺失时回退 VSINSTALLDIR 与最新 VS。
    if (Test-Path -LiteralPath $vswhere) {
        $vsRoot = (& $vswhere -latest -products * -version "[17.0,18.0)" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath -format value 2>$null | Select-Object -First 1)
        if ($vsRoot) {
            $candidate = Join-Path ("$vsRoot".Trim()) "VC\Auxiliary\Build\vcvars64.bat"
            if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
        }
    }
    if (-not $vcvars -and $env:VSINSTALLDIR) {
        $candidate = Join-Path $env:VSINSTALLDIR "VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path -LiteralPath $candidate) { $vcvars = $candidate }
    }
    if (-not $vcvars -and (Test-Path -LiteralPath $vswhere)) {
        $vsRoot = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath -format value 2>$null | Select-Object -First 1)
        if ($vsRoot) {
            $candidate = Join-Path ("$vsRoot".Trim()) "VC\Auxiliary\Build\vcvars64.bat"
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
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path -LiteralPath $cargoBin) {
        $env:PATH = "$cudaRoot\bin\x64;$cudaRoot\bin;$msvcBin;$cargoBin;$env:PATH"
    } else {
        $env:PATH = "$cudaRoot\bin\x64;$cudaRoot\bin;$msvcBin;$env:PATH"
    }

    if (-not (Get-Command nvcc -ErrorAction SilentlyContinue)) {
        throw "nvcc is not available after CUDA environment setup"
    }
    if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
        throw "cl.exe is not available after MSVC environment setup"
    }
}

# 'quick' is the CUDA-free fallback stage invoked by the pre-push hook on
# machines without the CUDA toolkit; every other stage still requires CUDA.
if ($Stage -ne "quick") {
    Initialize-CudaBuildEnvironment
}

function Invoke-Step {
    param([string]$Name, [scriptblock]$Action)
    Write-Host "`n==> $Name" -ForegroundColor Cyan
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Action
    if ($LASTEXITCODE -ne 0) { throw "$Name failed with exit code $LASTEXITCODE" }
    $watch.Stop()
    Write-Host ("OK  {0} ({1:n1}s)" -f $Name, $watch.Elapsed.TotalSeconds) -ForegroundColor Green
}

$requiredCommands = if ($Stage -eq "quick") { "git", "node", "npm" } else { "git", "node", "npm", "rustc", "cargo" }
foreach ($command in $requiredCommands) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required command not found: $command"
    }
}

$rustVersion = $null
if ($Stage -ne "quick") {
    $rustVersion = (& rustc --version 2>&1) -join " "
    if ($rustVersion -notmatch '^rustc 1\.97\.') {
        throw "Expected Rust 1.97.x, found: $rustVersion"
    }
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

function Invoke-ParallelSteps {
    param(
        [Parameter(Mandatory)]
        [array]$Branches
    )

    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $processes = @()

    foreach ($branch in $Branches) {
        $name = $branch.Name
        $cmd = $branch.Command
        $cwd = if ($branch.WorkingDirectory) { $branch.WorkingDirectory } else { $Root }

        Write-Host "`n==> [Parallel: Start] $name" -ForegroundColor Cyan

        $psi = [System.Diagnostics.ProcessStartInfo]::new()
        $psi.FileName = "cmd.exe"
        $psi.Arguments = "/c $cmd"
        $psi.WorkingDirectory = $cwd
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true
        # 显式固定子进程的模块路径：并行分支里的 powershell.exe 5.1 契约测试
        # 不再依赖继承自 pwsh 7 的 PSModulePath（node/cargo 不受该变量影响）。
        $psi.EnvironmentVariables["PSModulePath"] = $win51ModulePath

        $process = [System.Diagnostics.Process]::new()
        $process.StartInfo = $psi
        $process.Start() | Out-Null

        $outTask = $process.StandardOutput.ReadToEndAsync()
        $errTask = $process.StandardError.ReadToEndAsync()

        $processes += [pscustomobject]@{
            Name = $name
            Process = $process
            OutTask = $outTask
            ErrTask = $errTask
            StartWatch = [System.Diagnostics.Stopwatch]::StartNew()
        }
    }

    $hasFailed = $false
    $failedNames = @()

    foreach ($item in $processes) {
        # WaitForExit(TimeSpan) 仅存在于 .NET 5+（pwsh 7）；Windows PowerShell 5.1
        # 只有毫秒整数重载，传 TimeSpan 会在启动阶段抛 MethodException 中断流水线。
        $timeoutMilliseconds = [int][TimeSpan]::FromMinutes(30).TotalMilliseconds
        if (-not $item.Process.WaitForExit($timeoutMilliseconds)) {
            $item.StartWatch.Stop()
            try { $item.Process.Kill() } catch { }
            Write-Host "FAILED [Parallel: Timeout $($item.Name)] exceeded the 30 minute timeout" -ForegroundColor Red
            throw "$($item.Name) exceeded the 30 minute timeout"
        }
        $item.StartWatch.Stop()
        [System.Threading.Tasks.Task]::WaitAll(@($item.OutTask, $item.ErrTask))

        $outContent = $item.OutTask.Result
        $errContent = $item.ErrTask.Result

        Write-Host "`n--- [Parallel Output] $($item.Name) ($('{0:n1}s' -f $item.StartWatch.Elapsed.TotalSeconds)) ---" -ForegroundColor DarkCyan
        if ($outContent -and $outContent.Trim()) { Write-Host $outContent.TrimEnd() }
        if ($errContent -and $errContent.Trim()) { Write-Host $errContent.TrimEnd() -ForegroundColor Yellow }

        if ($item.Process.ExitCode -ne 0) {
            Write-Host "FAILED [Parallel: Exit $($item.Process.ExitCode)] $($item.Name)" -ForegroundColor Red
            $hasFailed = $true
            $failedNames += "$($item.Name) (exit code $($item.Process.ExitCode))"
        } else {
            Write-Host ("OK  {0} ({1:n1}s)" -f $item.Name, $item.StartWatch.Elapsed.TotalSeconds) -ForegroundColor Green
        }
    }

    $watch.Stop()
    if ($hasFailed) {
        throw "Parallel execution failed: $($failedNames -join ', ')"
    }
    Write-Host ("`nOK  Parallel group completed ({0:n1}s)" -f $watch.Elapsed.TotalSeconds) -ForegroundColor Green
}

$installerContractTest = Join-Path $PSScriptRoot "tests\build-windows-installers.Tests.ps1"
$portableContractTest = Join-Path $PSScriptRoot "tests\build-dev-portable.Tests.ps1"
$userscriptPath = Join-Path $Root "userscript\dist\ai-chat-memory.user.js"
$userscriptTestPath = Join-Path $Root "userscript\tests\capture.test.mjs"

$frontendCmd = "npm run build"
$rustCmd = "node --check `"$userscriptPath`" && node --test `"$userscriptTestPath`" && powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$installerContractTest`" && powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$portableContractTest`" && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings"

if ($Stage -in "test", "release") {
    $frontendCmd = "npm run build && npm test"
    $rustCmd = "$rustCmd && cargo test --all-features"
}

# Lightweight CUDA-free stage used as the pre-push hook fallback. It only
# validates the userscript and rebuilds the frontend; everything that needs the
# CUDA toolkit (cargo build/tests, clippy) stays in the 'test'/'release' stages.
if ($Stage -eq "quick") {
    Write-Host "==> Quick stage: lightweight validation without the CUDA toolkit" -ForegroundColor Cyan
    Invoke-Step "Check userscript syntax" {
        node --check $userscriptPath
    }
    Invoke-Step "Run userscript tests" {
        node --test $userscriptTestPath
    }
    Invoke-Step "Build frontend" {
        Push-Location $App
        try { npm run build } finally { Pop-Location }
    }
    Write-Host "`nLocal CI stage 'quick' passed (CUDA-dependent checks were skipped; run 'ci.ps1 test' on a CUDA machine for the full pipeline)." -ForegroundColor Green
    exit 0
}

Invoke-ParallelSteps @(
    @{
        Name = if ($Stage -in "test", "release") { "Frontend Pipeline (build & test)" } else { "Frontend Pipeline (build)" }
        WorkingDirectory = $App
        Command = $frontendCmd
    },
    @{
        Name = if ($Stage -in "test", "release") { "Rust & Quality Pipeline (lint & test)" } else { "Rust & Quality Pipeline (lint)" }
        WorkingDirectory = $Rust
        Command = $rustCmd
    }
)

if ($Stage -eq "release") {
    $runningApp = Get-CimInstance Win32_Process -Filter "Name = 'ai-chat-memory-desktop.exe'" -ErrorAction SilentlyContinue
    if ($runningApp) {
        $ids = ($runningApp.ProcessId -join ", ")
        throw "Close AI Chat Memory before release build (running process IDs: $ids)"
    }
    Invoke-Step "Build Windows NSIS installers" {
        $previousModulePath = $env:PSModulePath
        $env:PSModulePath = $win51ModulePath
        try {
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $InstallerBuilder -ArtifactsDirectory $Artifacts -RustVersion $rustVersion
        } finally {
            $env:PSModulePath = $previousModulePath
        }
    }
}

Write-Host "`nLocal CI stage '$Stage' passed." -ForegroundColor Green
