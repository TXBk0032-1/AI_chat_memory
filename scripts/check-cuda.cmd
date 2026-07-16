@echo off
setlocal EnableExtensions
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 (
  echo vcvars64 failed
  exit /b 1
)
set "CUDA_PATH=C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.3"
set "CUDA_HOME=%CUDA_PATH%"
set "PATH=%CUDA_PATH%\bin\x64;%CUDA_PATH%\bin;%PATH%"
set "NVCC_CCBIN=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64"
rem CUDA 13.x + MSVC needs the conforming preprocessor for CCCL headers.
set "NVCC_APPEND_FLAGS=-Xcompiler=/Zc:preprocessor"
set "RUST_BACKTRACE=1"
cd /d "%~dp0.."
cargo check --manifest-path app\src-tauri\Cargo.toml
exit /b %ERRORLEVEL%
