$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'install-rust.ps1') -FunctionsOnly

function Assert-Install([bool]$Condition,[string]$Message) { if (-not $Condition) { throw $Message } }
function Write-TestFile([string]$Path,[string]$Text) {
    New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($Path)) | Out-Null
    [IO.File]::WriteAllText($Path,$Text)
}
$taskTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('ai-tutor-install-test-' + [Guid]::NewGuid().ToString('N') + ' with spaces')
New-Item -ItemType Directory -Path $taskTestRoot | Out-Null
try {
    $taskOwned = Join-Path $taskTestRoot 'bin\ai-tutor.exe'
    $taskForeign = Join-Path $taskTestRoot 'other\ai-tutor.exe'
    $taskInstances = @(Get-ProjectInstances $taskTestRoot -Candidates @([PSCustomObject]@{Id=1;Path=$taskOwned},[PSCustomObject]@{Id=2;Path=$taskForeign}))
    Assert-Install ($taskInstances.Count -eq 1 -and $taskInstances[0].Id -eq 1) 'Process ownership must match this exact project executable.'

    $taskFiles = @()
    foreach ($taskName in @('ai-tutor.exe','ptz-read-probe.exe','tutorctl.exe','ptz-calibrate-probe.exe')) {
        Write-TestFile (Join-Path $taskTestRoot ('target\release\'+$taskName)) ('new-'+$taskName)
        Write-TestFile (Join-Path $taskTestRoot ('bin\'+$taskName)) ('old-'+$taskName)
        $taskFiles += [PSCustomObject]@{Source=('target\release\'+$taskName);Relative=('bin\'+$taskName)}
    }
    $taskSpeechFiles = @(Get-LocalSpeechProgramFiles)
    Assert-Install ($taskSpeechFiles.Count -eq 8) 'The managed voice bundle must contain eight reviewed program assets.'
    foreach ($taskFile in $taskSpeechFiles) {
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Source) ('new-'+[IO.Path]::GetFileName($taskFile.Relative))
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Relative) ('old-'+[IO.Path]::GetFileName($taskFile.Relative))
    }
    $taskFiles += $taskSpeechFiles
    $taskAsrFiles = @(Get-LocalAsrProgramFiles)
    Assert-Install ($taskAsrFiles.Count -eq 3) 'The managed recognition bundle must contain three reviewed program assets.'
    $taskAsrNames = @('managed_asr.py','asr_service.py','recognizer.py')
    for ($taskIndex=0; $taskIndex -lt $taskAsrNames.Count; $taskIndex++) {
        $taskFile = $taskAsrFiles[$taskIndex]
        $taskName = $taskAsrNames[$taskIndex]
        Assert-Install ($taskFile.Source -eq ('scripts\speech-service\'+$taskName) -and $taskFile.Relative -eq ('bin\local-asr\'+$taskName)) 'Recognition program paths differ from the runtime bundle contract.'
        Assert-Install (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) $taskFile.Source) -PathType Leaf) 'A real recognition program source is missing.'
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Source) ('new-'+$taskName)
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Relative) ('old-'+$taskName)
    }
    $taskFiles += $taskAsrFiles
    $taskFinalAsrFiles = @(Get-FinalAsrProgramFiles)
    Assert-Install ($taskFinalAsrFiles.Count -eq 6) 'The precise transcription bundle needs six program/manifest files.'
    foreach ($taskFile in $taskFinalAsrFiles) {
        Assert-Install (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) $taskFile.Source) -PathType Leaf) 'A precise transcription source is missing.'
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Source) ('new-'+[IO.Path]::GetFileName($taskFile.Relative))
        Write-TestFile (Join-Path $taskTestRoot $taskFile.Relative) ('old-'+[IO.Path]::GetFileName($taskFile.Relative))
    }
    $taskFiles += $taskFinalAsrFiles
    Write-TestFile (Join-Path $taskTestRoot 'target\test-ui\index.html') 'new-ui'
    Write-TestFile (Join-Path $taskTestRoot 'frontend\dist\index.html') 'old-ui'
    Write-TestFile (Join-Path $taskTestRoot 'runtime\data\learner.sqlite') 'keep-learning-data'
    $taskRetained = @('runtime\speech-cosy\api-key','runtime\speech-gpu\omni-api-key','runtime\speech-gpu\omni-env-path.txt','runtime\local-speech-policy.json','runtime\tts-references.json','models\speech\model.safetensors','runtime\local-asr-policy.json','runtime\speech-service.json','runtime\speech-asr-venv\Scripts\python.exe','models\speech\sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20\encoder.onnx')
    $taskRetained += @('runtime\gpu-resource-policy.json','runtime\local-final-asr-policy.json')
    foreach ($taskRelative in $taskRetained) { Write-TestFile (Join-Path $taskTestRoot $taskRelative) 'keep-local-dependency' }
    $taskFiles += [PSCustomObject]@{Source='target\test-ui\index.html';Relative='frontend\dist\index.html'}
    $taskBackup = Publish-ProjectFiles $taskTestRoot $taskFiles
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'New program was not installed.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskBackup 'bin\ai-tutor.exe'))) -eq 'old-ai-tutor.exe') 'Previous program backup is missing.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'bin\ptz-calibrate-probe.exe'))) -eq 'new-ptz-calibrate-probe.exe') 'Calibration tool was not staged and installed.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskBackup 'bin\ptz-calibrate-probe.exe'))) -eq 'old-ptz-calibrate-probe.exe') 'Previous calibration tool backup is missing.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskBackup 'frontend\dist\index.html'))) -eq 'old-ui') 'Previous UI backup is missing.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'runtime\data\learner.sqlite'))) -eq 'keep-learning-data') 'Learning data changed during deployment.'
    foreach ($taskFile in ($taskSpeechFiles + $taskAsrFiles + $taskFinalAsrFiles)) {
        $taskName = [IO.Path]::GetFileName($taskFile.Relative)
        Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot $taskFile.Relative))) -eq ('new-'+$taskName)) 'A managed speech program asset was not installed.'
        Assert-Install (([IO.File]::ReadAllText((Join-Path $taskBackup $taskFile.Relative))) -eq ('old-'+$taskName)) 'A managed speech program asset backup is missing.'
    }
    foreach ($taskRelative in $taskRetained) {
        Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot $taskRelative))) -eq 'keep-local-dependency') 'A local voice dependency or credential was replaced.'
        Assert-Install (-not (Test-Path -LiteralPath (Join-Path $taskBackup $taskRelative))) 'Local dependencies must not enter program backups.'
    }

    # Lock the second installed file. The first file has already moved when this fails.
    $taskLocked = [IO.File]::Open((Join-Path $taskTestRoot 'bin\ptz-read-probe.exe'),[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $taskRejected = $false
    try {
        Write-TestFile (Join-Path $taskTestRoot 'target\release\ai-tutor.exe') 'second-update'
        try { Publish-ProjectFiles $taskTestRoot $taskFiles | Out-Null } catch { $taskRejected = $true }
    } finally { $taskLocked.Dispose() }
    Assert-Install $taskRejected 'A locked program should abort deployment.'
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'Failed deployment did not restore the first program.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'bin\ptz-calibrate-probe.exe'))) -eq 'new-ptz-calibrate-probe.exe') 'Failed deployment changed the calibration tool.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'runtime\data\learner.sqlite'))) -eq 'keep-learning-data') 'Failed deployment changed learning data.'

    # Fail in the voice bundle after binaries and earlier voice assets moved.
    $taskLocked = [IO.File]::Open((Join-Path $taskTestRoot 'bin\local-speech\start_cosy_omni.sh'),[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $taskRejected = $false
    try {
        Write-TestFile (Join-Path $taskTestRoot 'scripts\speech-lab\managed_tts.py') 'changed-wrapper'
        try { Publish-ProjectFiles $taskTestRoot $taskFiles | Out-Null } catch { $taskRejected = $true }
    } finally { $taskLocked.Dispose() }
    Assert-Install $taskRejected 'A locked voice asset should abort the whole deployment.'
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'Voice bundle failure did not restore the program.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'bin\local-speech\managed_tts.py'))) -eq 'new-managed_tts.py') 'Voice bundle failure did not restore an earlier asset.'

    # Recognition files share the same transaction with native binaries and TTS.
    $taskLocked = [IO.File]::Open((Join-Path $taskTestRoot 'bin\local-asr\recognizer.py'),[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $taskRejected = $false
    try {
        Write-TestFile (Join-Path $taskTestRoot 'scripts\speech-service\managed_asr.py') 'changed-asr-wrapper'
        try { Publish-ProjectFiles $taskTestRoot $taskFiles | Out-Null } catch { $taskRejected = $true }
    } finally { $taskLocked.Dispose() }
    Assert-Install $taskRejected 'A locked recognition asset should abort the whole deployment.'
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'Recognition failure did not restore the program.'
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'bin\local-speech\managed_tts.py'))) -eq 'new-managed_tts.py') 'Recognition failure did not restore the voice bundle.'
    foreach ($taskFile in $taskAsrFiles) {
        Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot $taskFile.Relative))) -eq ('new-'+[IO.Path]::GetFileName($taskFile.Relative))) 'Recognition failure did not restore an earlier recognition asset.'
    }
    Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot 'frontend\dist\index.html'))) -eq 'new-ui') 'Recognition failure changed the installed UI.'

    $taskGate = [PSCustomObject]@{Called=$false;AllStaged=$false}
    $taskRejected=$false
    try {
        Publish-ProjectFiles $taskTestRoot $taskFiles -BeforeReplace {
            $taskGate.Called=$true
            $taskGate.AllStaged=(@($taskEntries | Where-Object { -not (Test-Path -LiteralPath $_.Staged -PathType Leaf) }).Count -eq 0)
            throw 'Simulated normal-stop refusal; no actual process was contacted.'
        } | Out-Null
    } catch { $taskRejected=$true }
    Assert-Install ($taskRejected -and $taskGate.Called -and $taskGate.AllStaged) 'Stop gate did not run after all staged copies were ready.'
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'A failed normal-stop gate changed the installed program.'

    $taskRejected=$false
    try { Publish-ProjectFiles $taskTestRoot @([PSCustomObject]@{Source='target\release\ai-tutor.exe';Relative='..\outside.exe'}) | Out-Null } catch { $taskRejected=$true }
    Assert-Install $taskRejected 'An update path outside the project was accepted.'
    $taskRejected=$false
    try { Publish-ProjectFiles $taskTestRoot @([PSCustomObject]@{Source='target\missing.exe';Relative='bin\ai-tutor.exe'}) | Out-Null } catch { $taskRejected=$true }
    Assert-Install $taskRejected 'A missing build output was accepted.'
    Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'Preflight failure changed the installed program.'

    foreach ($taskFile in $taskAsrFiles) {
        $taskSource = Join-Path $taskTestRoot $taskFile.Source
        $taskOriginal = [IO.File]::ReadAllText($taskSource)
        Remove-Item -LiteralPath $taskSource
        $taskGate.Called=$false
        $taskRejected=$false
        try {
            try { Publish-ProjectFiles $taskTestRoot $taskFiles -BeforeReplace { $taskGate.Called=$true } | Out-Null } catch { $taskRejected=$true }
            Assert-Install ($taskRejected -and -not $taskGate.Called) 'Each missing recognition asset must fail before asking the current service to stop.'
            Assert-Install (([IO.File]::ReadAllText($taskOwned)) -eq 'new-ai-tutor.exe') 'Recognition preflight failure changed the installed program.'
        } finally { Write-TestFile $taskSource $taskOriginal }
    }

    Remove-Item -LiteralPath (Join-Path $taskTestRoot 'scripts\speech-lab\tts_gpu_lease.sh')
    $taskGate.Called=$false
    $taskRejected=$false
    try { Publish-ProjectFiles $taskTestRoot $taskFiles -BeforeReplace { $taskGate.Called=$true } | Out-Null } catch { $taskRejected=$true }
    Assert-Install ($taskRejected -and -not $taskGate.Called) 'Missing voice assets must fail before asking the current service to stop.'

    # Fake objects exercise the actual stop function without launching any process.
    $taskOriginalInstances = (Get-Item Function:\Get-ProjectInstances).ScriptBlock
    $taskOriginalStart = Get-Item Function:\Start-Process -ErrorAction SilentlyContinue
    $taskStopChild = [PSCustomObject]@{ ExitCode=0; Budget=0; Disposed=$false; Completes=$true }
    $taskStopChild | Add-Member ScriptMethod WaitForExit { param([int]$Milliseconds) $this.Budget=$Milliseconds; Start-Sleep -Milliseconds 40; return $this.Completes }
    $taskStopChild | Add-Member ScriptMethod Dispose { $this.Disposed=$true }
    $taskService = [PSCustomObject]@{ Id=777; Budget=0 }
    $taskService | Add-Member ScriptMethod WaitForExit { param([int]$Milliseconds) $this.Budget=$Milliseconds; return $true }
    $taskStopScenario = [PSCustomObject]@{ Queries=0; Child=$taskStopChild; Service=$taskService }
    Write-TestFile (Join-Path $taskTestRoot 'runtime\server.json') '{"pid":777}'
    try {
        function Get-ProjectInstances { param([string]$Root) $taskStopScenario.Queries++; if ($taskStopScenario.Queries -eq 1) { $taskStopScenario.Service } }
        function Start-Process {
            param($FilePath,$ArgumentList,$WindowStyle,[switch]$PassThru)
            Assert-Install ($WindowStyle -eq 'Hidden' -and $FilePath -eq $taskOwned) 'Normal stop must target this project without a visible helper window.'
            Assert-Install ($ArgumentList.Count -eq 3 -and $ArgumentList[0] -eq '--stop' -and $ArgumentList[1] -eq '--root' -and $ArgumentList[2] -eq ('"'+$taskTestRoot+'"')) 'A project root with spaces must remain one quoted stop argument.'
            return $taskStopScenario.Child
        }
        Stop-ProjectInstances $taskTestRoot
        Assert-Install ($taskStopChild.Disposed -and $taskStopChild.Budget -gt 45000 -and $taskStopChild.Budget -le 50000) 'Stop request did not receive the bounded managed cleanup budget.'
        Assert-Install ($taskService.Budget -gt 0 -and $taskService.Budget -lt $taskStopChild.Budget) 'Stop child and main service must share one deadline.'
        $taskStopScenario.Queries=0
        $taskStopChild.Completes=$false
        $taskStopChild.Disposed=$false
        $taskService.Budget=0
        $taskRejected=$false
        try { Stop-ProjectInstances $taskTestRoot } catch { $taskRejected=$true }
        Assert-Install ($taskRejected -and $taskStopChild.Disposed -and $taskService.Budget -eq 0) 'An unfinished stop request must reject the update without forcing service shutdown.'
    } finally {
        Set-Item Function:\Get-ProjectInstances -Value $taskOriginalInstances
        if ($taskOriginalStart) { Set-Item Function:\Start-Process -Value $taskOriginalStart.ScriptBlock } else { Remove-Item Function:\Start-Process }
    }
    foreach ($taskRelative in $taskRetained) {
        Assert-Install (([IO.File]::ReadAllText((Join-Path $taskTestRoot $taskRelative))) -eq 'keep-local-dependency') 'A failed update changed a local model, environment, policy or credential.'
    }
    Write-Host 'PASS: project ownership, staged binaries/UI/eight voice/three streaming ASR/six precise ASR assets, exact bundle paths, dependency retention, rollback, missing files before stop, shared bounded stop deadline, path guards. Only temporary files and fake processes were used.'
} finally {
    $taskResolved = [IO.Path]::GetFullPath($taskTestRoot)
    $taskTempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $taskResolved.StartsWith($taskTempBase,[StringComparison]::OrdinalIgnoreCase) -or -not ([IO.Path]::GetFileName($taskResolved)).StartsWith('ai-tutor-install-test-')) { throw 'Refusing to clean an unverified test path.' }
    Remove-Item -LiteralPath $taskResolved -Recurse -Force
}
