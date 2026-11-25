param(
    [Parameter(Mandatory=$true)][string]$Root,
    [Parameter(Mandatory=$true)][string]$EvidencePath
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$taskRoot = (Resolve-Path -LiteralPath $Root).Path
$modelDirectory = Join-Path $taskRoot 'models/memory/e5-small'
$targetPath = Join-Path $modelDirectory 'model.safetensors'
$partialPath = $targetPath + '.partial'
$evidenceAbsolute = [System.IO.Path]::GetFullPath($EvidencePath)
if (Test-Path -LiteralPath $evidenceAbsolute) { throw 'Use a new evidence path; do not overwrite previous attempts.' }
$revision = '614241f622f53c4eeff9890bdc4f31cfecc418b3'
$expectedHash = '1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477'
$expectedBytes = 470641600
$sourceUrl = "https://huggingface.co/intfloat/multilingual-e5-small/resolve/$revision/model.safetensors?download=true"
$evidence = [ordered]@{
    model_id = 'intfloat/multilingual-e5-small'
    revision = $revision
    filename = 'model.safetensors'
    source_url = $sourceUrl
    metadata_url = "https://huggingface.co/api/models/intfloat/multilingual-e5-small/tree/$revision`?expand=false"
    expected_bytes = $expectedBytes
    expected_sha256 = $expectedHash
    license = 'MIT'
    license_source = "https://huggingface.co/intfloat/multilingual-e5-small/raw/$revision/README.md"
    local_model_card_sha256 = '0038de97aee16258cecbad7ffda4b4febd6953e747a00e0ddbc8e6ed241e9c1c'
    official_model_card_git_blob = 'e56327af55e9742caf76bff9ba83b6904cd2003d'
    method = 'PowerShell HTTPS, fixed revision, no alternate mirror; partial file verified before atomic rename'
    started_at = [DateTime]::UtcNow.ToString('o')
    status = 'not_started'
    inference_run = $false
    packages_installed = $false
    other_weights_downloaded = $false
}
$failure = $null
try {
    if (-not (Test-Path -LiteralPath $modelDirectory -PathType Container)) { throw 'Existing E5 model directory is required.' }
    $cardHash = (Get-FileHash -LiteralPath (Join-Path $modelDirectory 'README.md') -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($cardHash -ne $evidence.local_model_card_sha256) { throw 'Fixed model-card hash mismatch.' }
    if (Test-Path -LiteralPath $targetPath) {
        $candidate = $targetPath
        $evidence.status = 'verifying_existing_target'
    } elseif (Test-Path -LiteralPath $partialPath) {
        $candidate = $partialPath
        $evidence.status = 'verifying_existing_partial_without_overwrite'
    } else {
        $evidence.status = 'downloading'
        Invoke-WebRequest -Uri $sourceUrl -OutFile $partialPath -UseBasicParsing -TimeoutSec 600
        $candidate = $partialPath
    }
    $evidence.actual_bytes = (Get-Item -LiteralPath $candidate).Length
    $evidence.actual_sha256 = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($evidence.actual_bytes -ne $expectedBytes -or $evidence.actual_sha256 -ne $expectedHash) {
        throw 'Downloaded or existing file does not match the fixed source; it has been preserved.'
    }
    if ($candidate -eq $partialPath) {
        # File.Move with this overload refuses to overwrite a concurrently created target.
        [System.IO.File]::Move($partialPath, $targetPath)
    }
    $evidence.status = 'verified_ready'
    $evidence.target_relative_path = 'models/memory/e5-small/model.safetensors'
} catch {
    $failure = $_
    $evidence.failure_stage = $evidence.status
    $evidence.status = 'failed_preserved'
    $evidence.error_type = $_.Exception.GetType().FullName
    $evidence.partial_exists = Test-Path -LiteralPath $partialPath
    if ($evidence.partial_exists) { $evidence.partial_bytes = (Get-Item -LiteralPath $partialPath).Length }
} finally {
    $evidence.finished_at = [DateTime]::UtcNow.ToString('o')
    $evidenceDirectory = [System.IO.Path]::GetDirectoryName($evidenceAbsolute)
    [System.IO.Directory]::CreateDirectory($evidenceDirectory) | Out-Null
    $file = [System.IO.File]::Open($evidenceAbsolute, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
    $writer = [System.IO.StreamWriter]::new($file, [System.Text.UTF8Encoding]::new($false))
    try { $writer.WriteLine(($evidence | ConvertTo-Json -Depth 5)) } finally { $writer.Dispose() }
}
if ($null -ne $failure) { throw 'Fixed E5 weight acquisition failed; see the preserved evidence/partial file. No revision or mirror fallback was used.' }
$evidence | ConvertTo-Json -Depth 5
