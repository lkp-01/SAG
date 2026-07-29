[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$CargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $CargoBin) -and ($env:Path -notlike "*$CargoBin*")) {
    $env:Path = "$CargoBin;$env:Path"
}

$Protoc = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter "Google.Protobuf*" -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter "protoc.exe" -ErrorAction SilentlyContinue } |
    Select-Object -First 1
if ($Protoc -and ($env:Path -notlike "*$($Protoc.DirectoryName)*")) {
    $env:Path = "$($Protoc.DirectoryName);$env:Path"
}

if (-not (Get-Command link.exe -ErrorAction SilentlyContinue) -and (Get-Command gcc.exe -ErrorAction SilentlyContinue)) {
    $env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"
    Write-Host "Using Rust GNU toolchain because MSVC link.exe is unavailable."
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Command
    )
    Write-Host "==> $Name"
    & $Command
    $commandExitCode = $LASTEXITCODE
    if ($commandExitCode -ne 0) {
        Write-Error "$Name failed with exit code $commandExitCode"
        exit $commandExitCode
    }
}

if (Get-Command cargo -ErrorAction SilentlyContinue) {
    Invoke-Checked "Rust format" { cargo fmt --all -- --check }
    Invoke-Checked "Rust check" { cargo check --workspace --all-targets }
    Invoke-Checked "Rust clippy" { cargo clippy --workspace --all-targets -- -D warnings }
    Invoke-Checked "Rust tests" { cargo test --workspace --all-targets }
} else {
    Write-Host "SKIPPED Rust checks: cargo is not available"
}

foreach ($frontend in @("frontend", "frontend-portal", "frontend-admin-next")) {
    Invoke-Checked "$frontend typecheck" { npm --prefix $frontend run typecheck }
    Invoke-Checked "$frontend lint" { npm --prefix $frontend run lint }
    Invoke-Checked "$frontend build" { npm --prefix $frontend run build }
}

$composeSets = @(
    @("docker-compose.yml"),
    @("docker-compose.yml", "docker-compose.release.yml"),
    @("docker-compose.edge.yml"),
    @("docker-compose.edge.yml", "docker-compose.edge.perf.yml"),
    @("docker-compose.edge.yml", "docker-compose.hscale-edge.yml"),
    @("docker-compose.edge.yml", "docker-compose.hscale-auth.yml"),
    @("docker-compose.edge.yml", "docker-compose.release.edge.yml")
)

if (Get-Command docker -ErrorAction SilentlyContinue) {
    foreach ($set in $composeSets) {
        $composeArgs = @("compose")
        foreach ($file in $set) {
            $composeArgs += @("-f", $file)
        }
        $composeArgs += @("config", "--quiet")
        Invoke-Checked "Compose config: $($set -join ' + ')" { & docker @composeArgs }
    }

    Invoke-Checked "Compose config: Intra" {
        docker compose -f docker-compose.intra.yml config --quiet --no-env-resolution
    }
    Invoke-Checked "Production invariants" {
        & (Join-Path $PSScriptRoot "ops\verify-production-invariants.ps1")
    }
} else {
    Write-Host "SKIPPED Compose checks: docker CLI is not available"
}

Write-Host "All available project checks passed."
