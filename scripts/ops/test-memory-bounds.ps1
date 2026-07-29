[CmdletBinding()]
param(
    [ValidateRange(1, 64)][int]$Concurrency = 8,
    [ValidateRange(2, 1000)][int]$Batches = 25,
    [ValidateRange(1024, 4194304)][int]$BodyBytes = 1048576,
    [ValidateRange(1048576, 1073741824)][long]$MaxRssSpanBytes = 67108864
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$suffix = "{0}-{1}" -f $PID, ([Guid]::NewGuid().ToString("N").Substring(0, 8))
$container = "sag-memory-upstream-$suffix"
$edgePort = 18082
$upstreamPort = 18081
$tempRoot = [IO.Path]::GetTempPath()
$dbPath = Join-Path $tempRoot "sag-memory-$suffix.db"
$outPath = Join-Path $tempRoot "sag-memory-$suffix.out.log"
$errPath = Join-Path $tempRoot "sag-memory-$suffix.err.log"
$edgeProcess = $null

$pythonCode = @"
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
import time
BODY=b'x'*$BodyBytes
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        remaining=int(self.headers.get('content-length','0'))
        while remaining:
            chunk=self.rfile.read(min(65536,remaining))
            if not chunk: break
            remaining-=len(chunk)
        if self.path.endswith('/slow'): time.sleep(3)
        response_body=BODY+b'y' if self.path.endswith('/oversized') else BODY
        self.send_response(200)
        self.send_header('Content-Type','application/octet-stream')
        self.send_header('Content-Length',str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body)
    def log_message(self,*args): pass
ThreadingHTTPServer(('0.0.0.0',18081),H).serve_forever()
"@

try {
    & docker run -d --name $container --label sag.memory-test=true `
        -p "127.0.0.1:${upstreamPort}:18081" `
        python:3.11-alpine python -c $pythonCode | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "failed to start bounded upstream" }

    $env:PUBLIC_EDGE_UPSTREAM_BASE_URL = "http://127.0.0.1:$upstreamPort"
    $env:PUBLIC_EDGE_LISTEN_ADDR = "127.0.0.1:$edgePort"
    $env:PUBLIC_EDGE_MAX_INFLIGHT = "$Concurrency"
    $env:PUBLIC_EDGE_MAX_REQUEST_BODY_BYTES = "$BodyBytes"
    $env:PUBLIC_EDGE_MAX_RESPONSE_BODY_BYTES = "$BodyBytes"
    $env:PUBLIC_EDGE_FIRST_BYTE_TIMEOUT_MS = "2000"
    $env:PUBLIC_EDGE_UPSTREAM_TLS_INSECURE = "false"
    $env:SAG_STORAGE_BACKEND = "sqlite"
    $env:SAG_STORAGE_DB_PATH = $dbPath

    $executable = Join-Path $repoRoot "target\debug\public-edge.exe"
    if (-not (Test-Path -LiteralPath $executable)) {
        throw "build public-edge before running this check: cargo build -p public-edge"
    }
    $edgeProcess = Start-Process -FilePath $executable -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $outPath -RedirectStandardError $errPath

    $ready = $false
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        try {
            $response = Invoke-WebRequest -UseBasicParsing `
                -Uri "http://127.0.0.1:$edgePort/metrics" -TimeoutSec 1
            if ($response.StatusCode -eq 200) { $ready = $true; break }
        }
        catch {}
        Start-Sleep -Milliseconds 250
    }
    if (-not $ready) {
        $errorLog = Get-Content $errPath -Raw -ErrorAction SilentlyContinue
        throw "public-edge did not become ready: $errorLog"
    }

    Add-Type -AssemblyName System.Net.Http
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromSeconds(30)
    $payload = New-Object byte[] $BodyBytes

    $oversizedContent = New-Object System.Net.Http.ByteArrayContent `
        -ArgumentList (,(New-Object byte[] ($BodyBytes + 1)))
    $oversizedRequest = $client.PostAsync(
        "http://127.0.0.1:$edgePort/request-oversized", $oversizedContent
    ).GetAwaiter().GetResult()
    if ([int]$oversizedRequest.StatusCode -ne 413) {
        throw "oversized request expected 413, got $([int]$oversizedRequest.StatusCode)"
    }
    $oversizedRequest.Dispose()
    $oversizedContent.Dispose()

    foreach ($case in @(
            @{ Path = "oversized"; Status = 503 },
            @{ Path = "slow"; Status = 504 }
        )) {
        $caseContent = New-Object System.Net.Http.ByteArrayContent `
            -ArgumentList (,(New-Object byte[] 1))
        $caseResponse = $client.PostAsync(
            "http://127.0.0.1:$edgePort/$($case.Path)", $caseContent
        ).GetAwaiter().GetResult()
        if ([int]$caseResponse.StatusCode -ne $case.Status) {
            throw "$($case.Path) expected $($case.Status), got $([int]$caseResponse.StatusCode)"
        }
        $caseResponse.Dispose()
        $caseContent.Dispose()
    }

    $samples = New-Object System.Collections.Generic.List[long]
    $warmupBatches = [Math]::Min(5, $Batches - 1)

    for ($batch = 0; $batch -lt $Batches; $batch++) {
        $tasks = New-Object System.Collections.Generic.List[System.Threading.Tasks.Task[System.Net.Http.HttpResponseMessage]]
        $contents = New-Object System.Collections.Generic.List[System.Net.Http.ByteArrayContent]
        for ($index = 0; $index -lt $Concurrency; $index++) {
            $content = New-Object System.Net.Http.ByteArrayContent -ArgumentList (,$payload)
            $contents.Add($content)
            $tasks.Add($client.PostAsync("http://127.0.0.1:$edgePort/bounded", $content))
        }
        [System.Threading.Tasks.Task]::WaitAll(
            [System.Threading.Tasks.Task[]]$tasks.ToArray()
        )
        foreach ($task in $tasks) {
            $response = $task.Result
            if ([int]$response.StatusCode -ne 200) {
                throw "unexpected status $([int]$response.StatusCode)"
            }
            $bytes = $response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
            if ($bytes.Length -ne $BodyBytes) {
                throw "unexpected response length $($bytes.Length)"
            }
            $response.Dispose()
        }
        foreach ($content in $contents) { $content.Dispose() }
        if ($batch -ge $warmupBatches) {
            $samples.Add((Get-Process -Id $edgeProcess.Id).WorkingSet64)
        }
    }
    $client.Dispose()

    $minimum = ($samples | Measure-Object -Minimum).Minimum
    $maximum = ($samples | Measure-Object -Maximum).Maximum
    $first = $samples[0]
    $last = $samples[$samples.Count - 1]
    $span = $maximum - $minimum
    if ($span -gt $MaxRssSpanBytes -or ($last - $first) -gt $MaxRssSpanBytes) {
        throw "RSS did not stabilize: first=$first last=$last min=$minimum max=$maximum"
    }
    $requests = $Concurrency * $Batches
    Write-Host "memory bound passed: requests=$requests concurrency=$Concurrency request_bytes=$BodyBytes response_bytes=$BodyBytes rss_min=$minimum rss_max=$maximum rss_span=$span rss_first=$first rss_last=$last"
}
finally {
    if ($null -ne $edgeProcess -and -not $edgeProcess.HasExited) {
        Stop-Process -Id $edgeProcess.Id -Force
    }
    $containerJson = & docker inspect $container 2>$null
    if ($LASTEXITCODE -eq 0) {
        $containerInfo = ($containerJson | ConvertFrom-Json)[0]
        if ($containerInfo.Config.Labels.'sag.memory-test' -eq "true") {
            & docker rm -f $container 2>$null | Out-Null
        }
    }
    foreach ($path in @($dbPath, "$dbPath-shm", "$dbPath-wal", $outPath, $errPath)) {
        $fullPath = [IO.Path]::GetFullPath($path)
        if ($fullPath.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
            (Test-Path -LiteralPath $fullPath)) {
            Remove-Item -LiteralPath $fullPath -Force
        }
    }
}
