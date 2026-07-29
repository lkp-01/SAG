[CmdletBinding()]
param(
    [string[]]$EdgeFiles = @(
        "docker-compose.edge.yml",
        "docker-compose.hscale-edge.yml",
        "docker-compose.release.edge.yml"
    ),
    [string[]]$IntraFiles = @(
        "docker-compose.intra.yml",
        "docker-compose.release.intra.yml"
    )
)

$ErrorActionPreference = "Stop"
$RepositoryRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $RepositoryRoot

function Get-PropertyValue {
    param(
        [AllowNull()]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )

    if ($null -eq $Object) {
        return $null
    }

    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function Get-EnvironmentValue {
    param(
        [AllowNull()]$Environment,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $value = Get-PropertyValue -Object $Environment -Name $Name
    if ($null -eq $value) {
        return $null
    }
    return [string]$value
}

function Test-Blank {
    param([AllowNull()]$Value)
    return $null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)
}

function Add-Violation {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations,
        [Parameter(Mandatory = $true)][string]$Message
    )
    $Violations.Add($Message)
}

function Get-ComposeModel {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string[]]$Files,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    $arguments = @("compose")
    foreach ($file in $Files) {
        $arguments += @("-f", $file)
    }
    $arguments += @("config", "--format", "json", "--no-env-resolution")

    # Compose expands `${VAR:?}` while loading YAML anchors even when
    # `--no-env-resolution` is requested. Supply non-routable synthetic values
    # only for missing variables so this static gate never needs production
    # credentials and never weakens the release files' required-variable rules.
    $syntheticEnvironment = @{
        SAG_POSTGRES_PASSWORD = "invariant-postgres-value-7f38"
        SAG_REDIS_PASSWORD = "invariant-redis-value-7f38"
        SAG_FOURA_CLIENT_SECRET = "invariant-foura-value-7f38"
        SAG_POSTGRES_DSN = "postgresql://invariant_user:invariant_value_7f38@invalid.local/sag?sslmode=require"
        SAG_JWT_SECRET = "invariant-jwt-value-7f38"
        SAG_PUBLIC_READONLY_TOKEN = "invariant-readonly-value-7f38"
        SAG_AGENT_SYNC_TOKEN = "invariant-agent-value-7f38"
        SAG_APISIX_ADMIN_API_KEY = "invariant-apisix-value-7f38"
        SAG_BOOTSTRAP_ADMIN_PASSWORD = "invariant-admin-value-7f38"
        SAG_SESSION_REDIS_URL = "rediss://:invariant_value_7f38@invalid.local:6379/0"
        SAG_POLICY_INTERNAL_TOKEN = "invariant-policy-value-7f38"
        SAG_POLICY_CACHE_REDIS_URL = "rediss://:invariant_value_7f38@invalid.local:6379/1"
        SAG_GRPC_TLS_CERT = "C:\invariant\agent.crt"
        SAG_GRPC_TLS_KEY = "C:\invariant\agent.key"
        SAG_GRPC_TLS_CLIENT_CA = "C:\invariant\connector-ca.crt"
        SAG_CONNECTOR_CERT_BINDINGS = "connector-intra-001:stream=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        SAG_BRIDGE_REDIS_URL = "rediss://:invariant_value_7f38@invalid.local:6379/2"
        SAG_GRPC_TLS_CLIENT_CERT = "C:\invariant\client.crt"
        SAG_GRPC_TLS_CLIENT_KEY = "C:\invariant\client.key"
        SAG_GRPC_TLS_CA = "C:\invariant\agent-ca.crt"
        SAG_GRPC_TLS_SERVER_NAME = "agent.invalid.local"
        SAG_GRAFANA_ADMIN_PASSWORD = "invariant-grafana-value-7f38"
        SAG_TUNNEL_ENDPOINTS = "https://stealth-tunnel-agent:50051,https://stealth-tunnel-agent2:50051"
    }
    $injectedNames = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $syntheticEnvironment.GetEnumerator()) {
        if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($entry.Key))) {
            [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value)
            $injectedNames.Add($entry.Key)
        }
    }
    try {
        $rendered = & docker @arguments 2>&1
        $composeExitCode = $LASTEXITCODE
    } finally {
        foreach ($name in $injectedNames) {
            [Environment]::SetEnvironmentVariable($name, $null)
        }
    }
    if ($composeExitCode -ne 0) {
        Add-Violation -Violations $Violations -Message (
            "[$Label] Compose rendering failed with exit code ${composeExitCode}: $($rendered -join ' ')"
        )
        return $null
    }

    try {
        return ($rendered -join [Environment]::NewLine) | ConvertFrom-Json
    } catch {
        Add-Violation -Violations $Violations -Message "[$Label] Compose output was not valid JSON: $($_.Exception.Message)"
        return $null
    }
}

function Test-PublishedSensitivePorts {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)]$Services,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    foreach ($serviceProperty in $Services.PSObject.Properties) {
        $serviceName = $serviceProperty.Name
        $service = $serviceProperty.Value
        foreach ($port in @(Get-PropertyValue -Object $service -Name "ports")) {
            if ($null -eq $port) {
                continue
            }

            $target = [string](Get-PropertyValue -Object $port -Name "target")
            $published = Get-PropertyValue -Object $port -Name "published"
            if (Test-Blank $published) {
                continue
            }

            $isSensitive =
                $serviceName -like "http-tunnel-bridge*" -or
                $serviceName -eq "redis" -or
                $serviceName -eq "etcd" -or
                ($serviceName -eq "apisix" -and $target -eq "9180")
            if (-not $isSensitive) {
                continue
            }

            $hostIp = Get-PropertyValue -Object $port -Name "host_ip"
            if ((Test-Blank $hostIp) -or $hostIp -eq "0.0.0.0" -or $hostIp -eq "::") {
                Add-Violation -Violations $Violations -Message (
                    "[$Label] service '$serviceName' publishes sensitive port $target as $published on all interfaces"
                )
            }
        }
    }
}

function Test-BridgeMtls {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)]$Services,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    $required = @(
        "SAG_GRPC_TLS_CA",
        "SAG_GRPC_TLS_CLIENT_CERT",
        "SAG_GRPC_TLS_CLIENT_KEY",
        "SAG_GRPC_TLS_SERVER_NAME"
    )

    foreach ($serviceProperty in $Services.PSObject.Properties) {
        $serviceName = $serviceProperty.Name
        if ($serviceName -notlike "http-tunnel-bridge*") {
            continue
        }

        $environment = Get-PropertyValue -Object $serviceProperty.Value -Name "environment"
        $enabled = Get-EnvironmentValue -Environment $environment -Name "SAG_GRPC_MTLS_ENABLED"
        if ($enabled -ne "true") {
            Add-Violation -Violations $Violations -Message "[$Label] Bridge '$serviceName' does not set SAG_GRPC_MTLS_ENABLED=true"
        }

        foreach ($name in $required) {
            if (Test-Blank (Get-EnvironmentValue -Environment $environment -Name $name)) {
                Add-Violation -Violations $Violations -Message "[$Label] Bridge '$serviceName' has an empty or missing $name"
            }
        }
    }
}

function Test-ServiceRuntimeGuards {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)]$Services,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    foreach ($serviceProperty in $Services.PSObject.Properties) {
        $serviceName = $serviceProperty.Name
        $service = $serviceProperty.Value
        $restart = [string](Get-PropertyValue -Object $service -Name "restart")
        if (Test-Blank $restart -or $restart -eq "no") {
            Add-Violation -Violations $Violations -Message "[$Label] service '$serviceName' has no production restart policy"
        }

        $healthcheck = Get-PropertyValue -Object $service -Name "healthcheck"
        $healthDisabled = Get-PropertyValue -Object $healthcheck -Name "disable"
        if ($null -eq $healthcheck -or $healthDisabled -eq $true) {
            Add-Violation -Violations $Violations -Message "[$Label] service '$serviceName' has no enabled healthcheck"
        }

        $deploy = Get-PropertyValue -Object $service -Name "deploy"
        $resources = Get-PropertyValue -Object $deploy -Name "resources"
        $limits = Get-PropertyValue -Object $resources -Name "limits"
        $memory = Get-PropertyValue -Object $limits -Name "memory"
        $cpus = Get-PropertyValue -Object $limits -Name "cpus"
        if (Test-Blank $memory) {
            $memory = Get-PropertyValue -Object $service -Name "mem_limit"
        }
        if (Test-Blank $cpus) {
            $cpus = Get-PropertyValue -Object $service -Name "cpus"
        }
        if ((Test-Blank $memory) -or (Test-Blank $cpus)) {
            Add-Violation -Violations $Violations -Message "[$Label] service '$serviceName' must set both CPU and memory resource limits"
        }
    }
}

function Test-RedisDurability {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)]$Services,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    $redisProperty = $Services.PSObject.Properties["redis"]
    if ($null -eq $redisProperty) {
        return
    }

    $redis = $redisProperty.Value
    $hasDataVolume = $false
    foreach ($volume in @(Get-PropertyValue -Object $redis -Name "volumes")) {
        if ($null -ne $volume -and (Get-PropertyValue -Object $volume -Name "target") -eq "/data") {
            $hasDataVolume = $true
        }
    }
    if (-not $hasDataVolume) {
        Add-Violation -Violations $Violations -Message "[$Label] Redis has no persistent volume mounted at /data"
    }

    $commandText = ((@(Get-PropertyValue -Object $redis -Name "command") | ForEach-Object { [string]$_ }) -join " ")
    if ($commandText -notmatch "(?i)(--appendonly\s+yes|appendonly\s+yes)") {
        Add-Violation -Violations $Violations -Message "[$Label] Redis does not enable AOF (appendonly yes)"
    }

    $redisEnvironment = Get-PropertyValue -Object $redis -Name "environment"
    $redisPassword = Get-EnvironmentValue -Environment $redisEnvironment -Name "REDIS_PASSWORD"
    if (Test-Blank $redisPassword -or $commandText -notmatch "(?i)(--requirepass|requirepass)") {
        Add-Violation -Violations $Violations -Message "[$Label] Redis does not enforce a non-empty password"
    }
}

function Test-KnownSecrets {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)]$Services,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Violations
    )

    $knownValues = @(
        "postgres",
        "dev-jwt-secret",
        "your-admin-key",
        "Admin@123",
        "demo-readonly-token",
        "sag-agent-sync-dev-token",
        "dev-policy-internal-token",
        "sag-admin",
        "sag-local-secret",
        "changeme"
    )

    foreach ($serviceProperty in $Services.PSObject.Properties) {
        $serviceName = $serviceProperty.Name
        $environment = Get-PropertyValue -Object $serviceProperty.Value -Name "environment"
        if ($null -eq $environment) {
            continue
        }

        foreach ($environmentProperty in $environment.PSObject.Properties) {
            $name = $environmentProperty.Name
            if ($name -notmatch "(?i)(PASSWORD|SECRET|TOKEN|API_KEY|PRIVATE_KEY|CREDENTIAL|POSTGRES_DSN)") {
                continue
            }

            $value = [string]$environmentProperty.Value
            $known = $knownValues -contains $value
            $placeholder = $value -match "(?i)(REPLACE_WITH|postgres:postgres@|example-secret|test-secret)"
            if ($known -or $placeholder) {
                Add-Violation -Violations $Violations -Message "[$Label] service '$serviceName' resolves $name to a known example credential"
            }
        }
    }
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Error "docker is required to parse the resolved Compose model"
    exit 127
}

$violations = [System.Collections.Generic.List[string]]::new()
$configurations = @(
    @{ Label = "edge"; Files = $EdgeFiles },
    @{ Label = "intra"; Files = $IntraFiles }
)

foreach ($configuration in $configurations) {
    $label = [string]$configuration.Label
    $model = Get-ComposeModel -Label $label -Files $configuration.Files -Violations $violations
    if ($null -eq $model) {
        continue
    }
    $services = Get-PropertyValue -Object $model -Name "services"
    Test-PublishedSensitivePorts -Label $label -Services $services -Violations $violations
    Test-BridgeMtls -Label $label -Services $services -Violations $violations
    Test-ServiceRuntimeGuards -Label $label -Services $services -Violations $violations
    Test-RedisDurability -Label $label -Services $services -Violations $violations
    Test-KnownSecrets -Label $label -Services $services -Violations $violations
}

if ($violations.Count -gt 0) {
    Write-Host "Production invariant violations ($($violations.Count)):" -ForegroundColor Red
    foreach ($violation in $violations) {
        Write-Host " - $violation" -ForegroundColor Red
    }
    exit 1
}

Write-Host "Production Compose invariants passed for Edge and Intra configurations."
exit 0
