param([Parameter(Mandatory=$true)][string]$Url,
      [Parameter(Mandatory=$true)][string]$PartialPath)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (-not $Url.StartsWith('https://huggingface.co/BAAI/bge-m3/resolve/5617a9f61b028005a4858fdac845db406aefb181/')) { throw 'Unexpected asset source' }
if (Test-Path -LiteralPath $PartialPath) { throw 'Preserve existing partial' }
Invoke-WebRequest -Uri $Url -OutFile $PartialPath -UseBasicParsing -TimeoutSec 900
