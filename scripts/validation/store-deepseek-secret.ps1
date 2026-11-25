$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security
$secretDirectory = Join-Path $env:LOCALAPPDATA 'AI Tutor\secrets'
$secretPath = Join-Path $secretDirectory 'deepseek.dpapi'
if (Test-Path -LiteralPath $secretPath) {
    throw 'Credential already exists; refusing to overwrite it.'
}
$secureInput = Read-Host 'DeepSeek API key (hidden)' -AsSecureString
$plainPointer = [IntPtr]::Zero
$plainBytes = $null
try {
    $plainPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureInput)
    $plainText = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($plainPointer)
    if ($plainText -notmatch '^sk-[a-zA-Z0-9_-]{16,}$') {
        throw 'Credential format is invalid.'
    }
    $plainBytes = [Text.Encoding]::UTF8.GetBytes($plainText)
    $encrypted = [Security.Cryptography.ProtectedData]::Protect(
        $plainBytes, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    [IO.Directory]::CreateDirectory($secretDirectory) | Out-Null
    $stream = [IO.File]::Open($secretPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
    try { $stream.Write($encrypted, 0, $encrypted.Length) } finally { $stream.Dispose() }
    Write-Output 'Saved CurrentUser DPAPI credential outside the workspace.'
} finally {
    if ($plainBytes) { [Array]::Clear($plainBytes, 0, $plainBytes.Length) }
    $plainText = $null
    if ($plainPointer -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($plainPointer)
    }
    $secureInput.Dispose()
}
