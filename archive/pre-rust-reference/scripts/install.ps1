$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $taskRoot
if (-not (Test-Path -LiteralPath '.venv\Scripts\python.exe')) {
    & python -m venv .venv
    if ($LASTEXITCODE -ne 0) { throw 'Python 3.10 or newer is required.' }
}
$taskPython = Join-Path $taskRoot '.venv\Scripts\python.exe'
& $taskPython -m pip install -r requirements.lock.txt
if ($LASTEXITCODE -ne 0) { throw 'Project dependency installation failed.' }
& $taskPython scripts\setup_vision.py
if ($LASTEXITCODE -ne 0) { throw 'Face weight validation failed.' }
& $taskPython scripts\setup_models.py
if ($LASTEXITCODE -ne 0) { throw 'Local speech model download failed.' }
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File frontend\build.ps1
if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
Set-Location -LiteralPath $taskRoot
& $taskPython scripts\probe_kimi.py
if ($LASTEXITCODE -ne 0) { throw 'Kimi normal login or no-tool verification needs attention. No global configuration was changed.' }
Write-Host 'AI Tutor installed. Open Start AI Tutor.cmd.'
