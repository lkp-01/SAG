# Archive k6 summary JSON with timestamp + optional git commit (§8)
param(
  [Parameter(Mandatory = $true)][string]$SourceJson,
  [string]$Label = "baseline",
  [string]$DestDir = ".\artifacts"
)

$ErrorActionPreference = "Stop"
$src = Resolve-Path $SourceJson
if (-not (Test-Path $DestDir)) { New-Item -ItemType Directory -Path $DestDir | Out-Null }
$ts = Get-Date -Format "yyyyMMdd-HHmmss"
$safe = ($Label -replace '[^\w\-]', '-')
$dest = Join-Path $DestDir "k6-baseline-$safe-$ts.json"
Copy-Item -LiteralPath $src -Destination $dest -Force
$commit = ""
$repoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
try {
  $commit = (git -C $repoRoot rev-parse --short HEAD 2>$null)
} catch {}
Write-Host "Archived: $dest"
Write-Host "PR note template:"
Write-Host "  k6 baseline: $dest"
if ($commit) { Write-Host "  git: $commit" }
