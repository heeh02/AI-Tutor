param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $taskRoot
$taskBindgen = Join-Path $taskRoot 'runtime\tools\wasm-bindgen\wasm-bindgen-0.2.100-x86_64-pc-windows-msvc\wasm-bindgen.exe'
if (-not (Test-Path -LiteralPath $taskBindgen)) {
    throw 'Install the official wasm-bindgen 0.2.100 Windows CLI under runtime/tools/wasm-bindgen, as documented in rust/tutor-ui/README.md.'
}
& cargo build --locked -p tutor-ui --target-dir target/ui --target wasm32-unknown-unknown --release
if ($LASTEXITCODE -ne 0) { throw 'Rust WebAssembly UI compilation failed.' }
$taskDist = if ($OutputDirectory) { [IO.Path]::GetFullPath($OutputDirectory) } else { Join-Path $PSScriptRoot 'dist' }
$taskAssets = Join-Path $taskDist 'assets'
New-Item -ItemType Directory -Force -Path $taskAssets | Out-Null
& $taskBindgen --target web --no-typescript --out-dir $taskAssets --out-name tutor_ui (Join-Path $taskRoot 'target\ui\wasm32-unknown-unknown\release\tutor-ui.wasm')
if ($LASTEXITCODE -ne 0) { throw 'WebAssembly binding generation failed.' }
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'index.html') -Destination (Join-Path $taskDist 'index.html') -Force
foreach ($taskAsset in @('style.css','home.css','activity-gallery.css','parent-center.css','home-environment.css','layout.css','bootstrap.js','robot-companion.js','home-environment.js','app-icon.svg','manifest.webmanifest')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $taskAsset) -Destination (Join-Path $taskAssets $taskAsset) -Force
}
$taskThreeAssets = Join-Path $taskAssets 'vendor\three'
New-Item -ItemType Directory -Force -Path $taskThreeAssets | Out-Null
foreach ($taskAsset in Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'vendor\three') -File) {
    Copy-Item -LiteralPath $taskAsset.FullName -Destination (Join-Path $taskThreeAssets $taskAsset.Name) -Force
}
