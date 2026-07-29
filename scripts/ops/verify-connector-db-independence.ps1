$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-NoText {
    param(
        [string]$RelativePath,
        [string[]]$Forbidden
    )
    $path = Join-Path $repoRoot $RelativePath
    $content = Get-Content -Raw -LiteralPath $path
    foreach ($term in $Forbidden) {
        if ($content.Contains($term)) {
            $failures.Add("$RelativePath still contains '$term'")
        }
    }
}

function Assert-HasText {
    param(
        [string]$RelativePath,
        [string]$Required
    )
    $path = Join-Path $repoRoot $RelativePath
    $content = Get-Content -Raw -LiteralPath $path
    if (-not $content.Contains($Required)) {
        $failures.Add("$RelativePath is missing '$Required'")
    }
}

Assert-NoText "proxy/connectors/sag-connector/src/main.rs" @(
    "shared_storage",
    "AuditJob",
    "SAG_CONNECTOR_AUDIT_QUEUE",
    "connector_audit_dropped_total",
    "resolve_storage_backend",
    "SAG_POSTGRES_DSN"
)
Assert-NoText "proxy/connectors/sag-connector/Cargo.toml" @(
    "shared_storage",
    "uuid.workspace"
)
Assert-NoText "docker-compose.intra.yml" @(
    "SAG_STORAGE_BACKEND:",
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-NoText "docker-compose.yml" @(
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-NoText "intra-host.env.example" @(
    "SAG_POSTGRES_DSN"
)
Assert-NoText ".env.example" @(
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-HasText "docker-compose.edge.yml" '127.0.0.1:5432:5432'

Push-Location $repoRoot
try {
    $tree = (& cargo tree -p sag-connector -e normal 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tree failed:`n$tree"
    }
    if ($tree -match "(?m)shared_storage") {
        $failures.Add("cargo tree still contains shared_storage")
    }
}
finally {
    Pop-Location
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Host "FAIL: $_" -ForegroundColor Red }
    exit 1
}

Write-Host "PASS: sag-connector has no direct database dependency."
