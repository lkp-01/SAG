<#
.SYNOPSIS
  Seed minimal company demo data.

.DESCRIPTION
  Writes routes/upstreams/policies/users via HTTP APIs:
  - control-plane-admin
  - sag-policy
  - sag-auth
#>

param(
    [string]$AdminBase = $(if ($env:SAG_ADMIN_BASE_URL) { $env:SAG_ADMIN_BASE_URL } else { "http://127.0.0.1:8090" }),
    [string]$PolicyBase = $(if ($env:SAG_POLICY_BASE_URL) { $env:SAG_POLICY_BASE_URL } else { "http://127.0.0.1:8081" }),
    [string]$AuthBase = $(if ($env:SAG_AUTH_BASE_URL) { $env:SAG_AUTH_BASE_URL } else { "http://127.0.0.1:8080" }),
    [string]$AdminUser = "admin",
    [string]$AdminPassword = "Admin@123",
    [string]$ConnectorEndpoint = "connector-local-001:stream",
    [string]$DefaultUpstream = "company-demo-sites:28080",
    [string]$DefaultScheme = "http"
)

$ErrorActionPreference = "Stop"
$admin = $AdminBase.TrimEnd('/')
$policy = $PolicyBase.TrimEnd('/')
$auth = $AuthBase.TrimEnd('/')

function Write-Step([string]$s) {
    Write-Host "==> $s" -ForegroundColor Cyan
}

function Invoke-JsonPost([string]$url, $body, [string]$token = "") {
    $json = $body | ConvertTo-Json -Depth 8
    $headers = @{}
    if ($token) { $headers["Authorization"] = "Bearer $token" }
    Invoke-RestMethod -Method Post -Uri $url -ContentType "application/json; charset=utf-8" -Headers $headers -Body $json | Out-Null
}

function Invoke-JsonPut([string]$url, $body, [string]$token = "") {
    $json = $body | ConvertTo-Json -Depth 8
    $headers = @{}
    if ($token) { $headers["Authorization"] = "Bearer $token" }
    Invoke-RestMethod -Method Put -Uri $url -ContentType "application/json; charset=utf-8" -Headers $headers -Body $json | Out-Null
}

function New-Cn([int[]]$codepoints) {
    return ($codepoints | ForEach-Object { [char]$_ }) -join ''
}

$apps = @(
    @{ host = "dev.internal.com";      app_id = "app-dev";      path_prefix = "/dev/";      note = "dev portal" }
    @{ host = "ci.internal.com";       app_id = "app-ci";       path_prefix = "/ci/";       note = "CI/CD" }
    @{ host = "finance.internal.com";  app_id = "app-finance";  path_prefix = "/finance/";  note = "finance" }
    @{ host = "oa.internal.com";       app_id = "app-oa";       path_prefix = "/oa/";       note = "office automation" }
    @{ host = "hr.internal.com";       app_id = "app-hr";       path_prefix = "/hr/";       note = "hr" }
    @{ host = "bi.internal.com";       app_id = "app-bi";       path_prefix = "/bi/";       note = "boss dashboard" }
    @{ host = "vendor.internal.com";   app_id = "app-vendor";   path_prefix = "/vendor/";   note = "vendor portal" }
)

$policies = @(
    # Admin/Boss: full access across all apps
    @{ id="p-allow-admin-all"; effect="ALLOW"; subjects=@("role:admin"); app_id="*"; path_prefix="/"; priority=6000 }
    @{ id="p-allow-boss-all";  effect="ALLOW"; subjects=@("role:boss");  app_id="*"; path_prefix="/"; priority=5000 }

    # Tech: only dev / CI / OA
    @{ id="p-allow-tech-dev"; effect="ALLOW"; subjects=@("role:tech"); app_id="app-dev"; path_prefix="/"; priority=3000 }
    @{ id="p-allow-tech-ci";  effect="ALLOW"; subjects=@("role:tech"); app_id="app-ci";  path_prefix="/"; priority=3000 }
    @{ id="p-allow-tech-oa";  effect="ALLOW"; subjects=@("role:tech"); app_id="app-oa";  path_prefix="/"; priority=2500 }

    # Finance
    @{ id="p-allow-finance-core"; effect="ALLOW"; subjects=@("role:finance"); app_id="app-finance"; path_prefix="/"; priority=3200 }
    @{ id="p-allow-finance-oa";   effect="ALLOW"; subjects=@("role:finance"); app_id="app-oa";      path_prefix="/"; priority=2500 }

    # External vendor
    @{ id="p-allow-vendor-only"; effect="ALLOW"; subjects=@("role:vendor"); app_id="app-vendor"; path_prefix="/"; priority=2800 }

    # Portal cards share app-001 when only bootstrap tunnel exists (see frontend portal services).
    @{ id="p-allow-sandbox-app001"; effect="ALLOW"; subjects=@("role:tech","role:finance","role:vendor"); app_id="app-001"; path_prefix="/"; priority=4500 }

    # Explicit deny (high priority)
    @{ id="p-deny-vendor-finance"; effect="DENY"; subjects=@("role:vendor"); app_id="app-finance"; path_prefix="/"; priority=9000 }
    @{ id="p-deny-vendor-hr";      effect="DENY"; subjects=@("role:vendor"); app_id="app-hr";      path_prefix="/"; priority=9000 }
    @{ id="p-deny-tech-finance";   effect="DENY"; subjects=@("role:tech");   app_id="app-finance"; path_prefix="/"; priority=8500 }
    @{ id="p-deny-tech-hr";        effect="DENY"; subjects=@("role:tech");   app_id="app-hr";      path_prefix="/"; priority=8500 }
    @{ id="p-deny-tech-bi";        effect="DENY"; subjects=@("role:tech");   app_id="app-bi";      path_prefix="/"; priority=8500 }
    @{ id="p-deny-tech-vendor";    effect="DENY"; subjects=@("role:tech");   app_id="app-vendor";  path_prefix="/"; priority=8500 }
)

$users = @(
    @{ id="u-boss-001";    username="boss";      roles=@("boss");    display_name=(New-Cn @(0x8D75,0x603B));      title=(New-Cn @(0x603B,0x7ECF,0x7406));                     dept=(New-Cn @(0x7BA1,0x7406,0x5C42)); password="Boss@123" }
    @{ id="u-tech-001";    username="alice";     roles=@("tech");    display_name=(New-Cn @(0x5F20,0x6668));      title=(New-Cn @(0x540E,0x7AEF,0x5DE5,0x7A0B,0x5E08));         dept=(New-Cn @(0x7814,0x53D1));       password="Tech@123" }
    @{ id="u-tech-002";    username="bob";       roles=@("tech");    display_name=(New-Cn @(0x674E,0x7136));      title=(New-Cn @(0x524D,0x7AEF,0x5DE5,0x7A0B,0x5E08));         dept=(New-Cn @(0x7814,0x53D1));       password="Tech@123" }
    @{ id="u-fin-001";     username="cathy";     roles=@("finance"); display_name=(New-Cn @(0x738B,0x654F));      title=(New-Cn @(0x8D22,0x52A1,0x4E3B,0x7BA1));               dept=(New-Cn @(0x8D22,0x52A1));       password="Fin@123" }
    @{ id="u-vendor-001";  username="vendor_a";  roles=@("vendor");  display_name=(New-Cn @(0x9648,0x5916,0x534F));title=(New-Cn @(0x5916,0x5305,0x5B9E,0x65BD,0x987E,0x95EE)); dept=(New-Cn @(0x5916,0x534F));       password="Vendor@123" }
)

Write-Step "Health check: control-plane-admin / sag-policy / sag-auth"
Invoke-RestMethod -Method Get -Uri "$admin/health" | Out-Null
Invoke-RestMethod -Method Get -Uri "$policy/health" | Out-Null
Invoke-RestMethod -Method Get -Uri "$auth/health" | Out-Null

Write-Step "Login as admin for protected APIs"
$loginRes = Invoke-RestMethod -Method Post -Uri "$auth/api/v1/auth/login" -ContentType "application/json; charset=utf-8" -Body (@{
    username = $AdminUser
    password = $AdminPassword
} | ConvertTo-Json)
$adminToken = $loginRes.token

Write-Step "Seeding routes and upstreams"
foreach ($a in $apps) {
    Invoke-JsonPost "$admin/api/v1/agent/routes" @{
        host = $a.host
        app_id = $a.app_id
        connector_endpoint = $ConnectorEndpoint
        require_healthy_tunnel = $true
    } $adminToken

    Invoke-JsonPut "$admin/api/v1/agent/intranet-upstreams?app_id=$($a.app_id)" @{
        upstream = $DefaultUpstream
        scheme = $DefaultScheme
    } $adminToken
}

Write-Step "Seeding policies"
foreach ($p in $policies) {
    Invoke-JsonPost "$policy/api/v1/policies" $p $adminToken
}

Write-Step "Seeding users into sag-auth"
foreach ($u in $users) {
    Invoke-JsonPost "$auth/api/v1/users" @{
        id = $u.id
        username = $u.username
        password = $u.password
        roles = $u.roles
        display_name = $u.display_name
        title = $u.title
        enabled = $true
    }
}

$usersPath = Join-Path $PSScriptRoot "..\infra\storage-seed\company_users.sample.json"
$usersJson = $users | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText((Resolve-Path $usersPath), $usersJson, [System.Text.Encoding]::UTF8)

Write-Host ""
Write-Host "Seed completed." -ForegroundColor Green
Write-Host "Apps: $($apps.Count), Policies: $($policies.Count)"
Write-Host "Sample users file: $((Resolve-Path $usersPath).Path)" -ForegroundColor DarkGray
Write-Host "Users written: $($users.Count)" -ForegroundColor DarkGray
