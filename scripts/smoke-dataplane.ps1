<#
.SYNOPSIS
  数据面南北向分层冒烟（Windows / PowerShell）。

.DESCRIPTION
  自北向南依次探测可 HTTP 触达的模块；每层单独打印标题与 PASS/FAIL，便于定位。
  管理面（admin / auth / policy）默认探测本机 8090/8080/8081 的 /health，可用环境变量改地址或跳过。

  在 sag-cloud 目录执行:
    .\scripts\smoke-dataplane.ps1

  双机从 Windows 一键（新 Edge + 默认 Intra）:
    .\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1

  常用环境变量:
    BRIDGE_URL, ZENTINEL_URL, PATH_REQ, HDR_APP, HDR_USER, HDR_ROLES
    EDGE_BASE_URL             两机部署：外网侧基础地址（例如 http://edge.sag.intra 或 http://10.0.0.10）
    INTRA_APISIX_DATA_BASE_URL 两机部署：内网侧 APISIX 数据面直连（可选；例如 http://intra.sag.intra:9080）
    MOCK_BASE_URL        默认 http://127.0.0.1:18080
    APISIX_DATA_BASE_URL 默认 http://127.0.0.1:9080（直连数据面，验证 route→上游，不经隧道）
    SMOKE_CONTROL_PLANE_BASE, SMOKE_AUTH_BASE, SMOKE_POLICY_BASE
    SMOKE_SKIP_MANAGEMENT=1   跳过管理面三层
    SMOKE_SKIP_APISIX_DIRECT=1 跳过直连 APISIX
    SMOKE_SKIP_MOCK_DIRECT=1    跳过直连 mock
    PUBLIC_EDGE_BASE_URL        若设置则多测一层 public-edge

  HTTPS（Zentinel）使用系统 curl.exe -k，无需 PowerShell 7。
#>
$ErrorActionPreference = "Continue"

$path = if ($env:PATH_REQ) { $env:PATH_REQ } else { "/dev/" }
$hdrApp = if ($env:HDR_APP) { $env:HDR_APP } else { "app-001" }
$hdrUser = if ($env:HDR_USER) { $env:HDR_USER } else { "u-admin" }
$hdrRoles = if ($env:HDR_ROLES) { $env:HDR_ROLES } else { "admin" }

$dataHeaders = @{
    "x-sag-app-id"     = $hdrApp
    "x-sag-user-id"    = $hdrUser
    "x-sag-user-roles" = $hdrRoles
}

$bridge = if ($env:BRIDGE_URL) { $env:BRIDGE_URL.TrimEnd('/') } else { "http://127.0.0.1:9000" }
$zent = if ($env:ZENTINEL_URL) { $env:ZENTINEL_URL.TrimEnd('/') } else { "https://127.0.0.1:10080" }
$mockBase = if ($env:MOCK_BASE_URL) { $env:MOCK_BASE_URL.TrimEnd('/') } elseif ($env:INTRA_MOCK_BASE_URL) { $env:INTRA_MOCK_BASE_URL.TrimEnd('/') } else { "http://127.0.0.1:18080" }
$apisixData = if ($env:APISIX_DATA_BASE_URL) { $env:APISIX_DATA_BASE_URL.TrimEnd('/') } else { "http://127.0.0.1:9080" }
$ctrl = if ($env:SMOKE_CONTROL_PLANE_BASE) { $env:SMOKE_CONTROL_PLANE_BASE.TrimEnd('/') } else { "http://127.0.0.1:8090" }
$authB = if ($env:SMOKE_AUTH_BASE) { $env:SMOKE_AUTH_BASE.TrimEnd('/') } else { "http://127.0.0.1:8080" }
$polB = if ($env:SMOKE_POLICY_BASE) { $env:SMOKE_POLICY_BASE.TrimEnd('/') } else { "http://127.0.0.1:8081" }

# Optional parameterization for dual-host deployments.
# If set, will override base URLs with an "edge" base, while keeping direct south checks optional.
$edgeBase = if ($env:EDGE_BASE_URL) { $env:EDGE_BASE_URL.TrimEnd('/') } else { "" }
$intraApsisix = if ($env:INTRA_APISIX_DATA_BASE_URL) { $env:INTRA_APISIX_DATA_BASE_URL.TrimEnd('/') } else { "" }
$appCasesRaw = if ($env:APP_CASES) { $env:APP_CASES } else { "app-dev:/dev/,app-ci:/ci/,app-finance:/finance/,app-oa:/oa/,app-hr:/hr/,app-bi:/bi/,app-vendor:/vendor/" }
if ($edgeBase) {
    if (-not $env:SMOKE_CONTROL_PLANE_BASE) { $ctrl = "$($edgeBase):8090" }
    if (-not $env:SMOKE_AUTH_BASE) { $authB = "$($edgeBase):8080" }
    if (-not $env:SMOKE_POLICY_BASE) { $polB = "$($edgeBase):8081" }
    if (-not $env:BRIDGE_URL) { $bridge = "$($edgeBase):9000" }
    if (-not $env:ZENTINEL_URL) {
        $edgeHost = $edgeBase -replace '^https?://', ''
        $zent = "https://$($edgeHost):10080"
    }
}
if ($intraApsisix -and -not $env:APISIX_DATA_BASE_URL) {
    $apisixData = $intraApsisix
}

$failures = New-Object System.Collections.Generic.List[string]

function Write-LayerHeader {
    param([string]$Id, [string]$Title, [string]$Detail)
    Write-Host ""
    Write-Host "=== [$Id] $Title ===" -ForegroundColor Cyan
    if ($Detail) { Write-Host "    $Detail" -ForegroundColor DarkGray }
}

function Test-HttpLayer {
    param(
        [string]$Id,
        [string]$Title,
        [string]$Url,
        [hashtable]$Headers = @{},
        [string]$Detail = ""
    )
    Write-LayerHeader -Id $Id -Title $Title -Detail $(if ($Detail) { $Detail } else { $Url })
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $r = Invoke-WebRequest -Uri $Url -Headers $Headers -Method GET -TimeoutSec 30 -UseBasicParsing
        $sw.Stop()
        $code = [int]$r.StatusCode
        $snippet = if ($r.Content.Length -gt 200) { $r.Content.Substring(0, 200) + "..." } else { $r.Content }
        $snippet = $snippet -replace "[\r\n]+", " "
        Write-Host "    PASS  HTTP $code  $($sw.ElapsedMilliseconds) ms" -ForegroundColor Green
        if ($r.Headers["x-sag-connector"]) {
            Write-Host "    note  x-sag-connector: $($r.Headers['x-sag-connector'])" -ForegroundColor DarkGray
        }
        Write-Host "    body  $snippet" -ForegroundColor DarkGray
        return $true
    }
    catch {
        $sw.Stop()
        Write-Host "    FAIL  $($_.Exception.Message)" -ForegroundColor Red
        [void]$failures.Add("[$Id] $Title -> $($_.Exception.Message)")
        return $false
    }
}

function Test-HttpsCurlLayer {
    param(
        [string]$Id,
        [string]$Title,
        [string]$Url,
        [hashtable]$Headers,
        [string]$Detail = ""
    )
    Write-LayerHeader -Id $Id -Title $Title -Detail $(if ($Detail) { $Detail } else { $Url })
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $tmpB = Join-Path $env:TEMP "sag-smoke-body-$PID.txt"
    $tmpH = Join-Path $env:TEMP "sag-smoke-hdr-$PID.txt"
    # Windows curl.exe uses Schannel; some environments fail TLS handshake with Pingora-based servers.
    # We try curl.exe first, and if we hit a Schannel handshake error we fallback to WSL curl (OpenSSL),
    # which is typically more compatible.
    $curlArgs = @("-sS", "-k", "--http1.1", "--tlsv1.2", "-o", $tmpB, "-D", $tmpH, "-w", "%{http_code}")
    foreach ($k in $Headers.Keys) {
        $curlArgs += "-H"
        $curlArgs += "${k}: $($Headers[$k])"
    }
    $curlArgs += $Url
    try {
        $codeStr = (& curl.exe @curlArgs 2>&1 | Out-String).Trim()
        $sw.Stop()
        if ($LASTEXITCODE -ne 0) {
            $err = "curl exit $LASTEXITCODE : $codeStr"
            $maybeSchannel = ($codeStr -match "(?i)schannel") -or ($codeStr -match "\\(35\\)")
            $hasWsl = $false
            try { $hasWsl = [bool](Get-Command wsl.exe -ErrorAction SilentlyContinue) } catch { $hasWsl = $false }
            if ($maybeSchannel -and $hasWsl) {
                # Fallback to WSL curl. We don't use temp files here; parse body + status from stdout.
                $wslArgs = @("curl", "-sS", "-k", "--http1.1", "--tlsv1.2", "-w", "`n%{http_code}", $Url)
                foreach ($k in $Headers.Keys) {
                    $wslArgs += "-H"
                    $wslArgs += "${k}: $($Headers[$k])"
                }
                $out = (& wsl.exe @wslArgs 2>&1 | Out-String)
                $lines = ($out -split "`r?`n")
                $statusLine = $lines[-1].Trim()
                $body = ($lines[0..([Math]::Max(0, $lines.Length - 2))] -join "`n")
                $parsed2 = 0
                if (-not [int]::TryParse($statusLine, [ref]$parsed2)) {
                    throw "curl(schannel) failed: $err ; wsl curl returned unexpected status '$statusLine' output='$out'"
                }
                $code = $parsed2
                $bodyOne = ($body -replace "[\r\n]+", " ").Trim()
                if ($bodyOne.Length -gt 220) { $bodyOne = $bodyOne.Substring(0, 220) + "..." }
                if ($code -ge 200 -and $code -lt 300) {
                    Write-Host "    PASS  HTTP $code  $($sw.ElapsedMilliseconds) ms (wsl curl fallback)" -ForegroundColor Green
                } else {
                    Write-Host "    FAIL  HTTP $code  $($sw.ElapsedMilliseconds) ms (wsl curl fallback)" -ForegroundColor Red
                    [void]$failures.Add("[$Id] $Title -> HTTP $code")
                }
                Write-Host "    body  $bodyOne" -ForegroundColor DarkGray
                return ($code -ge 200 -and $code -lt 300)
            }
            throw $err
        }
        $code = 0
        $parsed = 0
        if (-not [int]::TryParse($codeStr.Trim(), [ref]$parsed)) { $parsed = 0 }
        $code = $parsed
        $hdrBlock = if (Test-Path $tmpH) { Get-Content -Raw -LiteralPath $tmpH } else { "" }
        $body = if (Test-Path $tmpB) { Get-Content -Raw -LiteralPath $tmpB } else { "" }
        Remove-Item -LiteralPath $tmpB, $tmpH -ErrorAction SilentlyContinue
        $bodyOne = ($body -replace "[\r\n]+", " ").Trim()
        if ($bodyOne.Length -gt 220) { $bodyOne = $bodyOne.Substring(0, 220) + "..." }
        if ($code -ge 200 -and $code -lt 300) {
            Write-Host "    PASS  HTTP $code  $($sw.ElapsedMilliseconds) ms" -ForegroundColor Green
        }
        elseif ($code -ge 300) {
            Write-Host "    FAIL  HTTP $code  $($sw.ElapsedMilliseconds) ms" -ForegroundColor Red
            [void]$failures.Add("[$Id] $Title -> HTTP $code")
        }
        else {
            Write-Host "    FAIL  bad status from curl: '$codeStr'" -ForegroundColor Red
            [void]$failures.Add("[$Id] $Title -> parse '$codeStr'")
        }
        if ($hdrBlock -match "(?im)^x-sag-connector:\s*(.+)\r?$") {
            Write-Host "    note  x-sag-connector: $($Matches[1].Trim())" -ForegroundColor DarkGray
        }
        Write-Host "    body  $bodyOne" -ForegroundColor DarkGray
        return ($code -ge 200 -and $code -lt 300)
    }
    catch {
        $sw.Stop()
        Remove-Item -LiteralPath $tmpB, $tmpH -ErrorAction SilentlyContinue
        Write-Host "    FAIL  $($_.Exception.Message)" -ForegroundColor Red
        [void]$failures.Add("[$Id] $Title -> $($_.Exception.Message)")
        return $false
    }
}

function Test-AppCaseLayers {
    param(
        [string]$AppId,
        [string]$AppPath,
        [int]$Index
    )
    $h = @{
        "x-sag-app-id"     = $AppId
        "x-sag-user-id"    = $hdrUser
        "x-sag-user-roles" = $hdrRoles
    }
    Test-HttpsCurlLayer -Id ("V{0}N" -f $Index) -Title "zentinel real path" -Url "$zent$AppPath" -Headers $h -Detail "app=$AppId" | Out-Null
    Test-HttpLayer -Id ("V{0}T" -f $Index) -Title "bridge real path" -Url "$bridge$AppPath" -Headers $h -Detail "app=$AppId" | Out-Null
    if (-not $env:SMOKE_SKIP_APISIX_DIRECT) {
        Test-HttpLayer -Id ("V{0}S" -f $Index) -Title "apisix real path" -Url "$apisixData$AppPath" -Headers $h -Detail "app=$AppId" | Out-Null
    }
}

Write-Host "smoke-dataplane.ps1 — north-to-south + management probes" -ForegroundColor Yellow
Write-Host "PATH_REQ=$path  app=$hdrApp" -ForegroundColor DarkGray

# --- Management plane (optional) ---
if (-not $env:SMOKE_SKIP_MANAGEMENT) {
    Test-HttpLayer -Id "M1" -Title "control-plane-admin /health" -Url "$ctrl/health" | Out-Null
    Test-HttpLayer -Id "M2" -Title "sag-auth /health" -Url "$authB/health" | Out-Null
    Test-HttpLayer -Id "M3" -Title "sag-policy /health" -Url "$polB/health" | Out-Null
    if ($env:SMOKE_ADMIN_BEARER_TOKEN) {
        Write-LayerHeader -Id "M4" -Title "verify control-plane route model" -Detail "$ctrl/api/v1/agent/routes"
        try {
            $h = @{ Authorization = "Bearer $($env:SMOKE_ADMIN_BEARER_TOKEN)" }
            $r = Invoke-WebRequest -Uri "$ctrl/api/v1/agent/routes" -Headers $h -Method GET -TimeoutSec 30 -UseBasicParsing
            if ($r.Content -match '"app_id":"app-dev"' -and $r.Content -match '"app_id":"app-vendor"') {
                Write-Host "    PASS  app route rows visible" -ForegroundColor Green
            } else {
                Write-Host "    FAIL  route rows missing (expected app-dev..app-vendor)" -ForegroundColor Red
                [void]$failures.Add("[M4] route rows missing (expected app-dev..app-vendor)")
            }
        }
        catch {
            Write-Host "    FAIL  $($_.Exception.Message)" -ForegroundColor Red
            [void]$failures.Add("[M4] verify route model -> $($_.Exception.Message)")
        }
    }
}
else {
    Write-Host ""
    Write-Host "=== [M*] management skipped (SMOKE_SKIP_MANAGEMENT=1) ===" -ForegroundColor DarkYellow
}

# --- North: HTTPS ingress (Zentinel -> bridge -> agent -> connector -> APISIX -> mock) ---
Test-HttpsCurlLayer -Id "N1" -Title "north Zentinel HTTPS ingress + full tunnel chain" `
    -Url "$zent$path" -Headers $dataHeaders `
    -Detail "Expect: tunnel + connector + APISIX + mock (body may contain sag-test-workload)" | Out-Null

# --- Tunnel edge (HTTP bridge only; same chain without Zentinel TLS) ---
Test-HttpLayer -Id "T1" -Title "http-tunnel-bridge (gRPC to agent path)" `
    -Url "$bridge$path" -Headers $dataHeaders | Out-Null

# --- South: direct APISIX data plane (no tunnel; validates route/upstream) ---
if (-not $env:SMOKE_SKIP_APISIX_DIRECT) {
    Test-HttpLayer -Id "S1" -Title "south APISIX data plane (direct)" `
        -Url "$apisixData$path" -Headers $dataHeaders `
        -Detail "Bypasses tunnel; needs Route+upstream to mock on $path" | Out-Null
}
else {
    Write-Host ""
    Write-Host "=== [S1] APISIX direct skipped (SMOKE_SKIP_APISIX_DIRECT=1) ===" -ForegroundColor DarkYellow
}

# --- South: mock workload alone ---
if (-not $env:SMOKE_SKIP_MOCK_DIRECT) {
    Test-HttpLayer -Id "S2" -Title "south mock workload /health (upstream only)" `
        -Url "$mockBase/health" -Headers @{} | Out-Null
}
else {
    Write-Host ""
    Write-Host "=== [S2] mock direct skipped (SMOKE_SKIP_MOCK_DIRECT=1) ===" -ForegroundColor DarkYellow
}

# --- Optional public edge ---
if ($env:PUBLIC_EDGE_BASE_URL) {
    $pe = $env:PUBLIC_EDGE_BASE_URL.TrimEnd('/')
    Test-HttpLayer -Id "P1" -Title "public-edge ingress" -Url "$pe$path" -Headers $dataHeaders | Out-Null
}

if (-not $env:SMOKE_SKIP_MULTI_APP) {
    Write-Host ""
    Write-Host "=== [V*] verify 7 app real paths ===" -ForegroundColor Cyan
    $pairs = $appCasesRaw.Split(",", [System.StringSplitOptions]::RemoveEmptyEntries)
    $idx = 1
    foreach ($p in $pairs) {
        $parts = $p.Split(":", 2, [System.StringSplitOptions]::None)
        if ($parts.Length -ne 2) { continue }
        $appId = $parts[0]
        $appPath = $parts[1]
        Test-AppCaseLayers -AppId $appId -AppPath $appPath -Index $idx
        $idx += 1
    }
}

# --- Summary ---
Write-Host ""
Write-Host "=== SUMMARY ===" -ForegroundColor Yellow
if ($failures.Count -eq 0) {
    Write-Host "All executed layers passed." -ForegroundColor Green
    exit 0
}
else {
    Write-Host "Failed ($($failures.Count)):" -ForegroundColor Red
    foreach ($f in $failures) { Write-Host "  - $f" -ForegroundColor Red }
    exit 1
}
