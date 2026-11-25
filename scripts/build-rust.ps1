$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $taskRoot
$taskMsvc = 'C:\BuildTools\VC\Tools\MSVC\14.44.35207'
$taskSdk = 'C:\Program Files (x86)\Windows Kits\10'
$env:PATH = "$taskMsvc\bin\Hostx64\x64;C:\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;" + $env:PATH
$env:INCLUDE = "$taskMsvc\include;$taskSdk\Include\10.0.26100.0\ucrt;$taskSdk\Include\10.0.26100.0\um;$taskSdk\Include\10.0.26100.0\shared"
$env:LIB = "$taskMsvc\lib\x64;$taskSdk\Lib\10.0.26100.0\um\x64;$taskSdk\Lib\10.0.26100.0\ucrt\x64"
Remove-Item Env:WHISPER_DONT_GENERATE_BINDINGS -ErrorAction SilentlyContinue
& cargo @args
if ($LASTEXITCODE -ne 0) { throw 'Rust build/check failed.' }
