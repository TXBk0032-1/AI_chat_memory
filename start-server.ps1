$ErrorActionPreference = "Stop"

$RootDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ServerDir = Join-Path $RootDir "server"
$VenvPython = Join-Path $ServerDir "venv\Scripts\python.exe"
$Entry = Join-Path $ServerDir "main.py"

if (Test-Path $VenvPython) {
    & $VenvPython $Entry
} else {
    python $Entry
}
