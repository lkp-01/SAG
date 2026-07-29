[CmdletBinding()]
param(
    [ValidateRange(1, 10000)]
    [int]$Jobs = 100
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$suffix = "{0}-{1}" -f $PID, ([Guid]::NewGuid().ToString("N").Substring(0, 8))
$container = "sag-queue-recovery-$suffix"
$volume = "sag-queue-recovery-$suffix"
$password = "queue-test-$suffix"
$prefix = "sag:recovery:$suffix"
$stream = "$prefix`:queue"
$dlq = "$prefix`:dlq"
$group = "bridge-workers"
$entries = New-Object System.Collections.Generic.List[object]

function Invoke-Docker {
    param([Parameter(Mandatory = $true)][string[]]$DockerArgs)
    $output = & docker @DockerArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "docker $($DockerArgs -join ' ') failed: $($output -join [Environment]::NewLine)"
    }
    return $output
}

function Invoke-Redis {
    param([Parameter(Mandatory = $true)][string[]]$RedisArgs)
    return Invoke-Docker -DockerArgs (@(
            "exec", "-e", "REDISCLI_AUTH=$password", $container,
            "redis-cli", "--no-auth-warning", "--raw", "-n", "2"
        ) + $RedisArgs)
}

function Wait-Redis {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        $output = & docker exec -e "REDISCLI_AUTH=$password" $container redis-cli --no-auth-warning ping 2>$null
        if ($LASTEXITCODE -eq 0 -and ($output -join "") -eq "PONG") {
            return
        }
        Start-Sleep -Milliseconds 250
    }
    throw "Redis did not become healthy"
}

function Stop-AtCheckpoint {
    param([Parameter(Mandatory = $true)][string]$Name)
    # appendfsync everysec permits roughly one second of acknowledged queue writes to be lost.
    # Waiting two fsync intervals makes this test deterministic; it does not claim RPO=0.
    Start-Sleep -Seconds 2
    Invoke-Docker -DockerArgs @("kill", $container) | Out-Null
    Write-Host "checkpoint=$Name redis=SIGKILL"
    Invoke-Docker -DockerArgs @("start", $container) | Out-Null
    Wait-Redis
}

try {
    Invoke-Docker -DockerArgs @("volume", "create", "--label", "sag.queue-recovery=true", $volume) | Out-Null
    Invoke-Docker -DockerArgs @(
        "run", "-d", "--name", $container,
        "--label", "sag.queue-recovery=true",
        "-e", "REDIS_PASSWORD=$password",
        "-p", "127.0.0.1::6379",
        "-v", "${volume}:/data",
        "redis:7-alpine", "sh", "-ec",
        'printf ''appendonly yes\nappendfsync everysec\nrequirepass %s\n'' "$REDIS_PASSWORD" > /tmp/sag-redis.conf; exec redis-server /tmp/sag-redis.conf'
    ) | Out-Null
    Wait-Redis

    for ($index = 0; $index -lt $Jobs; $index++) {
        $queueId = "job-$suffix-$index"
        $idempotencyKey = "idem-$suffix-$index"
        $entryId = (Invoke-Redis -RedisArgs @(
            "XADD", $stream, "*", "queue_id", $queueId, "idempotency_key", $idempotencyKey
        ) | Select-Object -First 1).Trim()
        Invoke-Redis -RedisArgs @(
            "HSET", "$prefix`:job:$queueId", "status", "pending", "idempotency_key", $idempotencyKey
        ) | Out-Null
        $entries.Add([pscustomobject]@{
                EntryId = $entryId
                QueueId = $queueId
                IdempotencyKey = $idempotencyKey
            })
    }
    Invoke-Redis -RedisArgs @("XGROUP", "CREATE", $stream, $group, "0") | Out-Null

    # The one-shot consumer exits with every entry still pending: this is the worker-killed-at-delivered breakpoint.
    Invoke-Redis -RedisArgs @(
        "XREADGROUP", "GROUP", $group, "worker-delivered", "COUNT", "$Jobs", "STREAMS", $stream, ">"
    ) | Out-Null
    $pending = [int](Invoke-Redis -RedisArgs @("XPENDING", $stream, $group) | Select-Object -First 1)
    if ($pending -ne $Jobs) { throw "delivered checkpoint expected PEL=$Jobs, got $pending" }
    Stop-AtCheckpoint -Name "delivered"

    # Recover each entry, perform one mutation side effect guarded by its unique idempotency key,
    # and persist terminal state without acknowledging the stream entry.
    foreach ($entry in $entries) {
        Invoke-Redis -RedisArgs @(
            "XCLAIM", $stream, $group, "worker-recovered", "1", $entry.EntryId, "JUSTID"
        ) | Out-Null
        $claim = (Invoke-Redis -RedisArgs @(
            "SET", "$prefix`:effect:$($entry.IdempotencyKey)", "1", "NX"
        ) | Select-Object -First 1)
        if ($claim -ne "OK") { throw "duplicate mutation dispatch for $($entry.IdempotencyKey)" }
        Invoke-Redis -RedisArgs @(
            "HSET", "$prefix`:job:$($entry.QueueId)", "status", "done", "result", "ok"
        ) | Out-Null
    }
    Stop-AtCheckpoint -Name "result-persisted"

    # A recovered worker observes durable terminal state and deliberately stops immediately before XACK.
    foreach ($entry in $entries) {
        $status = (Invoke-Redis -RedisArgs @("HGET", "$prefix`:job:$($entry.QueueId)", "status") | Select-Object -First 1)
        $effect = (Invoke-Redis -RedisArgs @("GET", "$prefix`:effect:$($entry.IdempotencyKey)") | Select-Object -First 1)
        if ($status -ne "done" -or $effect -ne "1") {
            throw "terminal replay validation failed for $($entry.QueueId)"
        }
    }
    Stop-AtCheckpoint -Name "before-ack"

    # Terminal replay may ACK/delete only after verifying the durable result; it never dispatches again.
    foreach ($entry in $entries) {
        $status = (Invoke-Redis -RedisArgs @("HGET", "$prefix`:job:$($entry.QueueId)", "status") | Select-Object -First 1)
        if ($status -ne "done") { throw "unknown terminal state for $($entry.QueueId)" }
        Invoke-Redis -RedisArgs @("XACK", $stream, $group, $entry.EntryId) | Out-Null
        Invoke-Redis -RedisArgs @("XDEL", $stream, $entry.EntryId) | Out-Null
    }

    $pending = [int](Invoke-Redis -RedisArgs @("XPENDING", $stream, $group) | Select-Object -First 1)
    $remaining = [int](Invoke-Redis -RedisArgs @("XLEN", $stream) | Select-Object -First 1)
    $dlqCount = [int](Invoke-Redis -RedisArgs @("XLEN", $dlq) | Select-Object -First 1)
    if ($pending -ne 0 -or $remaining -ne 0) {
        throw "recovery incomplete: PEL=$pending stream=$remaining"
    }

    $portLine = (Invoke-Docker -DockerArgs @("port", $container, "6379/tcp") | Select-Object -First 1)
    $hostPort = $portLine.Substring($portLine.LastIndexOf(":") + 1)
    $previousTestUrl = $env:SAG_TEST_REDIS_URL
    try {
        $env:SAG_TEST_REDIS_URL = "redis://:$password@127.0.0.1:$hostPort/15"
        $toolchain = @()
        $installed = (& rustup toolchain list 2>$null) -join "`n"
        if ($installed -match "stable-x86_64-pc-windows-gnu") {
            $toolchain = @("+stable-x86_64-pc-windows-gnu")
        }
        & cargo @toolchain test -p http-tunnel-bridge --test queue_recovery redis_queue_kill_point_matrix -- --ignored --exact
        if ($LASTEXITCODE -ne 0) { throw "Rust queue recovery matrix failed" }
    }
    finally {
        $env:SAG_TEST_REDIS_URL = $previousTestUrl
    }

    Write-Host ("queue recovery passed: completed={0} indeterminate=0 dlq={1} unknown=0 pel=0 duplicate_dispatch=0" -f $Jobs, $dlqCount)
}
finally {
    $containerJson = & docker inspect $container 2>$null
    if ($LASTEXITCODE -eq 0) {
        $containerInfo = ($containerJson | ConvertFrom-Json)[0]
        if ($containerInfo.Config.Labels.'sag.queue-recovery' -eq "true") {
            & docker rm -f $container 2>$null | Out-Null
        }
    }
    $volumeJson = & docker volume inspect $volume 2>$null
    if ($LASTEXITCODE -eq 0) {
        $volumeInfo = ($volumeJson | ConvertFrom-Json)[0]
        if ($volumeInfo.Labels.'sag.queue-recovery' -eq "true") {
            & docker volume rm $volume 2>$null | Out-Null
        }
    }
}
