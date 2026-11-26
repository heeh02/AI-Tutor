param([switch]$FunctionsOnly)
$ErrorActionPreference = 'Stop'

function Get-ProjectPath([string]$Root, [string]$Relative) {
    $taskBase = [IO.Path]::GetFullPath($Root).TrimEnd('\')
    $taskPath = [IO.Path]::GetFullPath((Join-Path $taskBase $Relative))
    if (-not $taskPath.StartsWith($taskBase + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Update path leaves the project folder.' }
    $taskParent = [IO.Path]::GetDirectoryName($taskPath)
    while ($taskParent -and $taskParent.Length -gt $taskBase.Length) {
        if ((Test-Path -LiteralPath $taskParent) -and ((Get-Item -LiteralPath $taskParent).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Update folders must not redirect outside the project.' }
        $taskParent = [IO.Path]::GetDirectoryName($taskParent)
    }
    return $taskPath
}

function Get-ProjectInstances([string]$Root, [object[]]$Candidates) {
    $taskExpected = Get-ProjectPath $Root 'bin\ai-tutor.exe'
    if (-not $PSBoundParameters.ContainsKey('Candidates')) { $Candidates = @(Get-Process -Name 'ai-tutor' -ErrorAction SilentlyContinue) }
    foreach ($taskProcess in $Candidates) {
        try { $taskExecutable = $taskProcess.Path } catch { throw 'Cannot verify a running AI Tutor executable. Close this project with Stop AI Tutor.cmd before updating.' }
        if ($taskExecutable -and [string]::Equals([IO.Path]::GetFullPath($taskExecutable), $taskExpected, [StringComparison]::OrdinalIgnoreCase)) { $taskProcess }
    }
}

function Get-LocalSpeechProgramFiles {
    # Program assets only. Model weights, environment pointers, policies,
    # reference audio and credentials remain explicit local dependencies.
    foreach ($taskName in @('managed_tts.py','tts_gpu_lease.sh','gpu_admission.py','cuda_link_env.sh','start_qwen_omni.sh','start_cosy_omni.sh','qwen3_tts_12gb.yaml','cosyvoice3_12gb_candidate.yaml')) {
        [PSCustomObject]@{Source=('scripts\speech-lab\'+$taskName);Relative=('bin\local-speech\'+$taskName)}
    }
}

function Get-LocalAsrProgramFiles {
    # The owned recognition service uses these sibling program files only.
    # Its interpreter, model, policy and credentials are not deployment assets.
    foreach ($taskName in @('managed_asr.py','asr_service.py','recognizer.py')) {
        [PSCustomObject]@{Source=('scripts\speech-service\'+$taskName);Relative=('bin\local-asr\'+$taskName)}
    }
}

function Get-FinalAsrProgramFiles {
    # Fixed model/SDK manifests travel with the service; environments, weights
    # and the opt-in policy remain local dependencies.
    foreach ($taskName in @('managed_final_asr.py','qwen_final_engine.py','qwen_final_service.py','qwen-model.json','qwen-sdk.json')) {
        [PSCustomObject]@{Source=('scripts\final-asr\'+$taskName);Relative=('bin\final-asr\'+$taskName)}
    }
    [PSCustomObject]@{Source='scripts\speech-lab\gpu_admission.py';Relative='bin\final-asr\gpu_admission.py'}
}

function Get-LocalMemoryProgramFiles {
    # The opt-in semantic retrieval owner needs both sibling files. Models,
    # environments and policies remain local dependencies, not update assets.
    foreach ($taskName in @('managed_embedding.py','embedding_worker.py')) {
        [PSCustomObject]@{Source=('scripts\local-memory\'+$taskName);Relative=('bin\local-memory\'+$taskName)}
    }
}

function Stop-ProjectInstances([string]$Root) {
    $taskRunning = @(Get-ProjectInstances $Root)
    if ($taskRunning.Count -eq 0) { return }
    $taskDescriptorPath = Get-ProjectPath $Root 'runtime\server.json'
    if (-not (Test-Path -LiteralPath $taskDescriptorPath)) { throw 'This project is running without its local descriptor. Stop it normally before updating; no process was forced to close.' }
    try { $taskDescriptor = Get-Content -LiteralPath $taskDescriptorPath -Raw -Encoding UTF8 | ConvertFrom-Json } catch { throw 'The local service descriptor is invalid. Stop this project normally before updating.' }
    if (-not ($taskRunning | Where-Object { $_.Id -eq $taskDescriptor.pid })) { throw 'The local descriptor does not identify this project instance. Update stopped without closing another program.' }
    Write-Host 'Build complete. Asking this project to stop normally before updating...'
    $taskExecutable = Get-ProjectPath $Root 'bin\ai-tutor.exe'
    # One shared deadline includes the GPU-final runtime's bounded cleanup and
    # the Windows service's final audio/provider shutdown.
    $taskDeadline = [DateTime]::UtcNow.AddSeconds(50)
    # Root is a Windows filesystem path and cannot contain a quote; one quoted argument preserves spaces.
    $taskStop = Start-Process -FilePath $taskExecutable -ArgumentList @('--stop','--root',('"' + $Root.TrimEnd('\') + '"')) -WindowStyle Hidden -PassThru
    try {
        $taskRemaining = [int][Math]::Max(0, ($taskDeadline - [DateTime]::UtcNow).TotalMilliseconds)
        if (-not $taskStop.WaitForExit($taskRemaining)) { throw 'The normal stop request did not finish. Update cancelled; the existing program and data are retained.' }
        if ($taskStop.ExitCode -ne 0) { throw 'The normal stop request failed. Update cancelled; use Stop AI Tutor.cmd and retry.' }
    } finally { $taskStop.Dispose() }
    foreach ($taskProcess in $taskRunning) {
        $taskRemaining = [int][Math]::Max(0, ($taskDeadline - [DateTime]::UtcNow).TotalMilliseconds)
        if (-not $taskProcess.WaitForExit($taskRemaining)) { throw 'The existing service is still releasing resources. Update cancelled without force-closing it.' }
    }
    if (@(Get-ProjectInstances $Root).Count -ne 0) { throw 'This project started again during the update. Close it normally and retry.' }
}

function Publish-ProjectFiles([string]$Root, [object[]]$Files, [scriptblock]$BeforeReplace) {
    if (-not $Files -or $Files.Count -eq 0) { throw 'No update files were provided.' }
    $taskUpdateId = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N')
    $taskStage = Get-ProjectPath $Root ('runtime\program-updates\' + $taskUpdateId)
    $taskBackup = Get-ProjectPath $Root ('runtime\program-backups\' + $taskUpdateId)
    $taskEntries = @()
    $taskSeen = @{}
    # Validate and copy every new file before touching installed files.
    foreach ($taskFile in $Files) {
        $taskDestination = Get-ProjectPath $Root $taskFile.Relative
        if ($taskSeen.ContainsKey($taskDestination)) { throw 'Duplicate update destination.' }
        $taskSeen[$taskDestination] = $true
        $taskSource = Get-ProjectPath $Root $taskFile.Source
        if (-not (Test-Path -LiteralPath $taskSource -PathType Leaf)) { throw 'A built update file is missing. The installed program was not changed.' }
        $taskStaged = Get-ProjectPath $Root ('runtime\program-updates\' + $taskUpdateId + '\' + $taskFile.Relative)
        $taskPrevious = Get-ProjectPath $Root ('runtime\program-backups\' + $taskUpdateId + '\' + $taskFile.Relative)
        New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($taskStaged)) -Force | Out-Null
        Copy-Item -LiteralPath $taskSource -Destination $taskStaged
        if ((Get-FileHash -LiteralPath $taskSource -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $taskStaged -Algorithm SHA256).Hash) { throw 'Staged update checksum differs. The installed program was not changed.' }
        $taskEntries += [PSCustomObject]@{ Destination=$taskDestination; Staged=$taskStaged; Previous=$taskPrevious; HadOld=$false; NewInstalled=$false; Touched=$false }
    }
    # The normal stop happens only after every staged copy is ready and verified.
    if ($BeforeReplace) { & $BeforeReplace }
    try {
        foreach ($taskEntry in $taskEntries) {
            New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($taskEntry.Destination)) -Force | Out-Null
            if (Test-Path -LiteralPath $taskEntry.Destination) {
                New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($taskEntry.Previous)) -Force | Out-Null
                Move-Item -LiteralPath $taskEntry.Destination -Destination $taskEntry.Previous
                $taskEntry.HadOld = $true
            }
            $taskEntry.Touched = $true
            Move-Item -LiteralPath $taskEntry.Staged -Destination $taskEntry.Destination
            $taskEntry.NewInstalled = $true
        }
    } catch {
        $taskRestoreFailed = $false
        for ($taskIndex=$taskEntries.Count-1; $taskIndex -ge 0; $taskIndex--) {
            $taskEntry=$taskEntries[$taskIndex]
            if (-not $taskEntry.Touched) { continue }
            try {
                if ($taskEntry.NewInstalled) { Remove-Item -LiteralPath $taskEntry.Destination -Force }
                if ($taskEntry.HadOld) { Move-Item -LiteralPath $taskEntry.Previous -Destination $taskEntry.Destination }
            } catch { $taskRestoreFailed=$true }
        }
        if ($taskRestoreFailed) { throw ('Update could not restore every program file. Old files remain under ' + $taskBackup + '. Learning data was not changed.') }
        throw 'Update failed; all replaced program files were restored. Learning data was not changed.'
    }
    return $taskBackup
}

if ($FunctionsOnly) { return }
$taskRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Set-Location -LiteralPath $taskRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Please install official Rust and Windows C++ Build Tools first.' }
# Read-only ownership check now; keep the running service available during compilation.
$taskExisting = @(Get-ProjectInstances $taskRoot)
if ($taskExisting.Count) { Write-Host 'This project is running. It will be stopped normally after the build succeeds.' }
function Get-VerifiedFile([string]$Url,[string]$Path,[string]$Sha) {
    if ((Test-Path -LiteralPath $Path) -and (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLower() -eq $Sha) { return }
    $taskPartial = $Path + '.partial'
    & curl.exe --fail --location --connect-timeout 8 --max-time 180 --retry 1 --output $taskPartial $Url
    if ($LASTEXITCODE -ne 0) { throw 'Required file download failed.' }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $taskPartial).Hash.ToLower() -ne $Sha) { throw 'Downloaded file failed pinned SHA256 verification.' }
    Move-Item -LiteralPath $taskPartial -Destination $Path -Force
}
New-Item -ItemType Directory -Force 'models\native','runtime-tools','runtime\tools\wasm-bindgen','bin' | Out-Null
$taskFace=Get-Content 'models\manifest.json' -Raw -Encoding UTF8 | ConvertFrom-Json
Get-VerifiedFile $taskFace.models[0].download_url $taskFace.models[0].file $taskFace.models[0].sha256
$taskWhisper=Get-Content 'models\whisper-rust-manifest.json' -Raw -Encoding UTF8 | ConvertFrom-Json
Get-VerifiedFile $taskWhisper.download_transport $taskWhisper.file $taskWhisper.sha256
$taskDlls=@{ 'onnxruntime.dll'='2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492'; 'onnxruntime_providers_shared.dll'='6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138' }
$taskNeedDll=$false
foreach($taskName in $taskDlls.Keys){$taskPath=Join-Path 'models\native' $taskName; if(-not(Test-Path -LiteralPath $taskPath) -or (Get-FileHash -Algorithm SHA256 -LiteralPath $taskPath).Hash.ToLower() -ne $taskDlls[$taskName]){$taskNeedDll=$true}}
if($taskNeedDll){
    & curl.exe --fail --location --connect-timeout 8 --max-time 180 --output 'runtime-tools\onnxruntime.zip' 'https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-win-x64-1.23.2.zip'
    if($LASTEXITCODE -ne 0){throw 'Official ONNX Runtime download failed.'}
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $taskZip=[IO.Compression.ZipFile]::OpenRead((Join-Path $taskRoot 'runtime-tools\onnxruntime.zip'))
    try { foreach($taskName in $taskDlls.Keys){$taskEntry=$taskZip.GetEntry('onnxruntime-win-x64-1.23.2/lib/'+$taskName);if(-not $taskEntry){throw 'Expected native DLL not found in official archive.'};$taskStream=$taskEntry.Open();$taskTarget=[IO.File]::Create((Join-Path $taskRoot ('models\native\'+$taskName)));try{$taskStream.CopyTo($taskTarget)}finally{$taskTarget.Dispose();$taskStream.Dispose()};if((Get-FileHash -Algorithm SHA256 -LiteralPath ('models\native\'+$taskName)).Hash.ToLower() -ne $taskDlls[$taskName]){throw 'Native DLL digest differs from reviewed runtime.'}} } finally {$taskZip.Dispose()}
}
$taskClang='runtime-tools\libclang\libclang-18.1.1.data\platlib\clang\native\libclang.dll'
if(-not(Test-Path -LiteralPath $taskClang)){
    $taskMeta=Invoke-RestMethod 'https://pypi.org/pypi/libclang/18.1.1/json'
    $taskWheel=$taskMeta.urls | Where-Object {$_.filename -like '*win_amd64.whl'} | Select-Object -First 1
    Get-VerifiedFile $taskWheel.url 'runtime-tools\libclang.whl' $taskWheel.digests.sha256
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory((Join-Path $taskRoot 'runtime-tools\libclang.whl'),(Join-Path $taskRoot 'runtime-tools\libclang'))
}
$taskBindgen='runtime\tools\wasm-bindgen\wasm-bindgen-0.2.100-x86_64-pc-windows-msvc\wasm-bindgen.exe'
if(-not(Test-Path -LiteralPath $taskBindgen)){
    Get-VerifiedFile 'https://github.com/wasm-bindgen/wasm-bindgen/releases/download/0.2.100/wasm-bindgen-0.2.100-x86_64-pc-windows-msvc.tar.gz' 'runtime-tools\wasm-bindgen.tar.gz' '54a3fb947464388a468ade86d65ffa334d6d2c74b7982723b34ecf6ec8c213d8'
    & tar.exe -xzf 'runtime-tools\wasm-bindgen.tar.gz' -C 'runtime\tools\wasm-bindgen'
    if($LASTEXITCODE -ne 0){throw 'wasm-bindgen tool extraction failed.'}
}
& rustup target add wasm32-unknown-unknown
if($LASTEXITCODE -ne 0){throw 'Rust WebAssembly target unavailable.'}
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\build-rust.ps1 build --locked -p ai-tutor --bin ai-tutor --bin ptz-read-probe --bin tutorctl --bin ptz-calibrate-probe --release
if($LASTEXITCODE -ne 0){throw 'Native Rust build failed.'}
$taskUiStage = 'target\install-ui-' + [Guid]::NewGuid().ToString('N')
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File frontend\build.ps1 -OutputDirectory (Get-ProjectPath $taskRoot $taskUiStage)
if($LASTEXITCODE -ne 0){throw 'Rust UI build failed.'}
$taskFiles = @('ai-tutor.exe','ptz-read-probe.exe','tutorctl.exe','ptz-calibrate-probe.exe') | ForEach-Object { [PSCustomObject]@{ Source=('target\release\'+$_); Relative=('bin\'+$_) } }
$taskFiles += @(Get-LocalSpeechProgramFiles)
$taskFiles += @(Get-LocalAsrProgramFiles)
$taskFiles += @(Get-FinalAsrProgramFiles)
$taskFiles += @(Get-LocalMemoryProgramFiles)
$taskUiRoot = Get-ProjectPath $taskRoot $taskUiStage
foreach ($taskFile in Get-ChildItem -LiteralPath $taskUiRoot -File -Recurse) {
    $taskRelative = $taskFile.FullName.Substring($taskUiRoot.Length).TrimStart('\')
    $taskFiles += [PSCustomObject]@{Source=($taskUiStage+'\'+$taskRelative);Relative=('frontend\dist\'+$taskRelative)}
}
foreach ($taskFile in $taskFiles) { if (-not (Test-Path -LiteralPath (Get-ProjectPath $taskRoot $taskFile.Source) -PathType Leaf)) { throw 'A staged build output is missing. Existing service remains unchanged.' } }
$taskBackup = Publish-ProjectFiles $taskRoot $taskFiles -BeforeReplace { Stop-ProjectInstances $taskRoot }
Write-Host ('Previous program files retained at: ' + $taskBackup)
$taskVerify = Start-Process -FilePath (Get-ProjectPath $taskRoot 'bin\ai-tutor.exe') -ArgumentList @('--verify-kimi','--root',('"'+$taskRoot+'"')) -WindowStyle Hidden -Wait -PassThru
try { if ($taskVerify.ExitCode -ne 0) { throw 'Complete normal Kimi login, then run bin\ai-tutor.exe --verify-kimi --root . . Program and learning data are retained.' } } finally { $taskVerify.Dispose() }
Write-Host 'Rust AI Tutor ready. Open Start AI Tutor.cmd.'
