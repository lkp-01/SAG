<#
.SYNOPSIS
  七条门户路径冒烟（与「用户门户」网关探测一致：app-001 + /dev/ /ci/ …）。

.DESCRIPTION
  一键体检默认用 /api/test + app-001（经 proxy-rewrite 到 /test/）；门户「网关探测」用 /dev/、/ci/ 等。
  本脚本对七条路径各测：N=Zentinel HTTPS、T=Bridge、S=APISIX 直连（双机时设 INTRA）、P=经 admin-next :3001（与浏览器同源）。
  P 层在 Windows 上使用 curl.exe -L（与 .sh 一致），避免 Invoke-WebRequest 对 Next 308 跟随不一致。

  在 sag-cloud 目录执行：

    # Edge 本机（默认 127.0.0.1）
    .\scripts\smoke-portal-seven.ps1

    # 从 Windows 指远端 Edge + Intra（与 smoke-remote-windows 类似）
    $env:EDGE_BASE_URL = "http://172.16.9.107"
    $env:INTRA_APISIX_DATA_BASE_URL = "http://192.168.9.26:9080"
    $env:ADMIN_NEXT_BASE_URL = "http://172.16.9.107:3001"   # 复现门户经 Next 反代；不设则跳过 P
    .\scripts\smoke-portal-seven.ps1

  环境变量（可选）：
    HDR_USER, HDR_ROLES（默认 u-admin / admin）
    SMOKE_BEARER_TOKEN — 若经 3001 必须带 JWT 时设置（门户会带 Authorization）

  快捷参数（与 smoke-remote-windows 一致风格）：

    .\scripts\smoke-portal-seven.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -IncludeAdminNext
#>
param(
    [string]$EdgeHost = "",
    [string]$IntraHost = "192.168.9.26",
    [switch]$IncludeAdminNext
)

$ErrorActionPreference = "Continue"

if ($EdgeHost) {
    $eb = $EdgeHost.Trim().TrimEnd('/')
    if ($eb -notmatch '^https?://') { $eb = "http://$eb" }
    $env:EDGE_BASE_URL = $eb
    $ih = $IntraHost.Trim()
    $env:INTRA_APISIX_DATA_BASE_URL = "http://${ih}:9080"
    if ($IncludeAdminNext) {
        $hOnly = $eb -replace '^https?://', ''
        $env:ADMIN_NEXT_BASE_URL = "http://${hOnly}:3001"
    }
}

$hdrApp = "app-001"
$hdrUser = if ($env:HDR_USER) { $env:HDR_USER } else { "u-admin" }
$hdrRoles = if ($env:HDR_ROLES) { $env:HDR_ROLES } else { "admin" }
$bearer = $env:SMOKE_BEARER_TOKEN

$edgeBase = if ($env:EDGE_BASE_URL) { $env:EDGE_BASE_URL.TrimEnd('/') } else { "" }
$bridge = if ($env:BRIDGE_URL) { $env:BRIDGE_URL.TrimEnd('/') } elseif ($edgeBase) { "$edgeBase`:9000" } else { "http://127.0.0.1:9000" }
$zent = if ($env:ZENTINEL_URL) { $env:ZENTINEL_URL.TrimEnd('/') } elseif ($edgeBase) {
    $h = $edgeBase -replace '^https?://', ''
    "https://${h}:10080"
} else { "https://127.0.0.1:10080" }
$apisixData = if ($env:APISIX_DATA_BASE_URL) { $env:APISIX_DATA_BASE_URL.TrimEnd('/') } elseif ($env:INTRA_APISIX_DATA_BASE_URL) { $env:INTRA_APISIX_DATA_BASE_URL.TrimEnd('/') } else { "http://127.0.0.1:9080" }
$adminNext = if ($env:ADMIN_NEXT_BASE_URL) { $env:ADMIN_NEXT_BASE_URL.TrimEnd('/') } else { "" }

$tiles = @(
    @{ id = "dev";    name = "研发门户"; path = "/dev/" },
    @{ id = "ci";     name = "持续集成"; path = "/ci/" },
    @{ id = "finance"; name = "财务系统"; path = "/finance/" },
    @{ id = "oa";     name = "OA办公"; path = "/oa/" },
    @{ id = "hr";     name = "人事系统"; path = "/hr/" },
    @{ id = "bi";     name = "老板看板"; path = "/bi/" },
    @{ id = "vendor"; name = "外包交付"; path = "/vendor/" }
)

$failures = New-Object System.Collections.Generic.List[string]

function Snip([string]$s, [int]$max = 180) {
    $one = ($s -replace "[\r\n]+", " ").Trim()
    if ($one.Length -gt $max) { return $one.Substring(0, $max) + "..." }
    return $one
}

function Curl-Zentinel {
    param([string]$Url, [hashtable]$ExtraHeaders)
    $tmpB = Join-Path $env:TEMP "sag-p7-$PID-body.txt"
    $tmpH = Join-Path $env:TEMP "sag-p7-$PID-hdr.txt"
    $args = @("-sS", "-k", "--http1.1", "--tlsv1.2", "-o", $tmpB, "-D", $tmpH, "-w", "%{http_code}")
    foreach ($k in $ExtraHeaders.Keys) {
        $args += "-H"; $args += "${k}: $($ExtraHeaders[$k])"
    }
    $args += $Url
    $codeStr = (& curl.exe @args 2>&1 | Out-String).Trim()
    $code = 0
    if (-not [int]::TryParse($codeStr.Trim(), [ref]$code)) { $code = 0 }
    $body = if (Test-Path $tmpB) { Get-Content -Raw -LiteralPath $tmpB } else { "" }
    Remove-Item -LiteralPath $tmpB, $tmpH -ErrorAction SilentlyContinue
    return @{ Code = $code; Body = $body }
}

function Curl-Http {
    param([string]$Url, [hashtable]$ExtraHeaders)
    try {
        $r = Invoke-WebRequest -Uri $Url -Headers $ExtraHeaders -Method GET -TimeoutSec 30 -UseBasicParsing
        return @{ Code = [int]$r.StatusCode; Body = $r.Content }
    }
    catch {
        $resp = $_.Exception.Response
        if ($null -ne $resp) {
            try {
                $sr = New-Object System.IO.StreamReader($resp.GetResponseStream())
                $b = $sr.ReadToEnd()
                $sr.Close()
                return @{ Code = [int]$resp.StatusCode; Body = $b }
            }
            catch { }
        }
        return @{ Code = -1; Body = $_.Exception.Message }
    }
}

# P 层经 Next :3001；Next 可能对尾斜杠返回 308。Windows PowerShell 5.1 的 Invoke-WebRequest 对 308 跟随不一致，
# 易出现空状态码/误判。与 smoke-portal-seven.sh 对齐：curl.exe -L。
function Curl-HttpFollow {
    param([string]$Url, [hashtable]$ExtraHeaders)
    $tmpB = Join-Path $env:TEMP "sag-p7-follow-$PID.body"
    $curlArgs = [System.Collections.Generic.List[string]]::new()
    [void]$curlArgs.AddRange([string[]]@("-sS", "-L", "--http1.1", "-o", $tmpB, "-w", "%{http_code}"))
    if ($Url -match '^(?i)https://') {
        $curlArgs.Clear()
        [void]$curlArgs.AddRange([string[]]@("-sS", "-k", "-L", "--http1.1", "--tlsv1.2", "-o", $tmpB, "-w", "%{http_code}"))
    }
    foreach ($k in $ExtraHeaders.Keys) {
        $curlArgs.Add("-H")
        $curlArgs.Add("${k}: $($ExtraHeaders[$k])")
    }
    $curlArgs.Add($Url)
    $codeStr = (& curl.exe @($curlArgs.ToArray()) 2>&1 | Out-String).Trim()
    $code = 0
    if (-not [int]::TryParse($codeStr.Trim(), [ref]$code)) { $code = 0 }
    $body = if (Test-Path -LiteralPath $tmpB) { Get-Content -Raw -LiteralPath $tmpB } else { "" }
    Remove-Item -LiteralPath $tmpB -ErrorAction SilentlyContinue
    return @{ Code = $code; Body = $body }
}

function Base-Headers {
    $h = @{
        "x-sag-app-id"     = $hdrApp
        "x-sag-user-id"    = $hdrUser
        "x-sag-user-roles" = $hdrRoles
    }
    if ($bearer) { $h["Authorization"] = "Bearer $bearer" }
    return $h
}

Write-Host "smoke-portal-seven.ps1 — 7 portal paths × (N Zentinel / T Bridge / S APISIX$(if ($adminNext) { ' / P admin-next' }))" -ForegroundColor Yellow
Write-Host "  ZENTINEL=$zent  BRIDGE=$bridge  APISIX=$apisixData$(if ($adminNext) { "  ADMIN_NEXT=$adminNext" })" -ForegroundColor DarkGray
Write-Host ""

foreach ($t in $tiles) {
    $p = $t.path
    $h = Base-Headers
    Write-Host "=== [$($t.id)] $($t.name)  path=$p ===" -ForegroundColor Cyan

    $nz = Curl-Zentinel -Url "$zent$p" -ExtraHeaders $h
    $okN = ($nz.Code -ge 200 -and $nz.Code -lt 300)
    $lineN = "  N Zentinel  HTTP $($nz.Code)  $(if ($okN) { 'PASS' } else { 'FAIL' })"
    if ($okN) { Write-Host $lineN -ForegroundColor Green } else { Write-Host $lineN -ForegroundColor Red }
    Write-Host ("      body {0}" -f (Snip $nz.Body)) -ForegroundColor DarkGray
    if (-not $okN) { [void]$failures.Add("[$($t.id)] N $($t.name) HTTP $($nz.Code) $(Snip $nz.Body 120)") }

    $tb = Curl-Http -Url "$bridge$p" -ExtraHeaders $h
    $okT = ($tb.Code -ge 200 -and $tb.Code -lt 300)
    $lineT = "  T Bridge    HTTP $($tb.Code)  $(if ($okT) { 'PASS' } else { 'FAIL' })"
    if ($okT) { Write-Host $lineT -ForegroundColor Green } else { Write-Host $lineT -ForegroundColor Red }
    Write-Host ("      body {0}" -f (Snip $tb.Body)) -ForegroundColor DarkGray
    if (-not $okT) { [void]$failures.Add("[$($t.id)] T $($t.name) HTTP $($tb.Code) $(Snip $tb.Body 120)") }

    $ts = Curl-Http -Url "$apisixData$p" -ExtraHeaders $h
    $okS = ($ts.Code -ge 200 -and $ts.Code -lt 300)
    $lineS = "  S APISIX    HTTP $($ts.Code)  $(if ($okS) { 'PASS' } else { 'FAIL' })"
    if ($okS) { Write-Host $lineS -ForegroundColor Green } else { Write-Host $lineS -ForegroundColor Red }
    Write-Host ("      body {0}" -f (Snip $ts.Body)) -ForegroundColor DarkGray
    if (-not $okS) { [void]$failures.Add("[$($t.id)] S $($t.name) HTTP $($ts.Code) $(Snip $ts.Body 120)") }

    if ($adminNext) {
        # 与门户 page.tsx 一致：/api-zentinel/dev/；经 Curl-HttpFollow（curl -L）跟随 Next 308。
        $tp = Curl-HttpFollow -Url "$adminNext/api-zentinel$p" -ExtraHeaders $h
        $okP = ($tp.Code -ge 200 -and $tp.Code -lt 300)
        $lineP = "  P admin-next HTTP $($tp.Code)  $(if ($okP) { 'PASS' } else { 'FAIL' })"
        if ($okP) { Write-Host $lineP -ForegroundColor Green } else { Write-Host $lineP -ForegroundColor Red }
        Write-Host ("      body {0}" -f (Snip $tp.Body)) -ForegroundColor DarkGray
        if (-not $okP) { [void]$failures.Add("[$($t.id)] P $($t.name) HTTP $($tp.Code) $(Snip $tp.Body 120)") }
    }
    Write-Host ""
}

Write-Host "=== SUMMARY ===" -ForegroundColor Yellow
if ($failures.Count -eq 0) {
    Write-Host "All portal path probes passed." -ForegroundColor Green
    if (-not $adminNext) { Write-Host "Tip: set ADMIN_NEXT_BASE_URL=http://<Edge>:3001 to also test Next rewrites (browser path)." -ForegroundColor DarkYellow }
    exit 0
}
Write-Host "Failed ($($failures.Count)):" -ForegroundColor Red
foreach ($f in $failures) { Write-Host "  - $f" -ForegroundColor Red }
exit 1
