$ErrorActionPreference = "Stop"

$RootDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ServerDir = Join-Path $RootDir "server"
$VenvPython = Join-Path $ServerDir ".venv\Scripts\python.exe"
$LegacyVenvPython = Join-Path $ServerDir "venv\Scripts\python.exe"
$Entry = Join-Path $ServerDir "main.py"

$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"

if (Test-Path $VenvPython) {
    & $VenvPython $Entry
} elseif (Test-Path $LegacyVenvPython) {
    & $LegacyVenvPython $Entry
} else {
    python $Entry
}
