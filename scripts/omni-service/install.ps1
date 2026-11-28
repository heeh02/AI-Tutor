param([switch]$SkipModelDownload)
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$python = Join-Path $taskRoot '.venv\Scripts\python.exe'
$runtime = Join-Path $taskRoot 'runtime\omni-runtime'
$runtimePython = Join-Path $runtime 'Scripts\python.exe'
if (-not (Test-Path -LiteralPath $runtimePython)) {
    & $python -m venv $runtime
    if ($LASTEXITCODE -ne 0) { throw 'Omni environment creation failed.' }
}
$env:PIP_DISABLE_PIP_VERSION_CHECK = '1'
& $runtimePython -m pip install --upgrade pip setuptools wheel
if ($LASTEXITCODE -ne 0) { throw 'Omni packaging tools installation failed.' }
& $runtimePython -m pip install --extra-index-url https://download.pytorch.org/whl/cu128 torch==2.7.1+cu128 torchvision==0.22.1+cu128 torchaudio==2.7.1+cu128
if ($LASTEXITCODE -ne 0) { throw 'Omni CUDA runtime installation failed.' }
& $runtimePython -m pip install -r (Join-Path $PSScriptRoot 'requirements-win32.txt')
if ($LASTEXITCODE -ne 0) { throw 'Omni runtime dependencies installation failed.' }
& $runtimePython (Join-Path $PSScriptRoot 'prepare_gptq.py')
if ($LASTEXITCODE -ne 0) { throw 'Omni GPTQ runtime installation failed.' }
if (-not $SkipModelDownload) {
    & $runtimePython (Join-Path $PSScriptRoot 'download_model.py')
    if ($LASTEXITCODE -ne 0) { throw 'Omni model download or verification failed.' }
}
