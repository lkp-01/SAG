# Config Dictionary (new additions)

## Storage
- `SAG_STORAGE_BACKEND`: `sqlite` (default) or `postgres`; applies to Edge persistence services and is intentionally not consumed by `sag-connector`.
- `SAG_STORAGE_DB_PATH`: SQLite path when backend=sqlite; applies to Edge persistence services.
- `SAG_POSTGRES_DSN`: PostgreSQL DSN when backend=postgres; applies to Edge persistence services and is intentionally not consumed by `sag-connector`.

### PostgreSQL pool

| Variable | Meaning | Production rule |
|---|---|---|
| `SAG_POSTGRES_POOL_MAX_SIZE` | Maximum connections owned by one process | Must be finite and greater than zero; default 16 |
| `SAG_POSTGRES_POOL_ACQUIRE_TIMEOUT_MS` | Maximum wait for a pooled connection | Must be greater than zero; default 2000 ms |
| `SAG_POSTGRES_CONNECT_TIMEOUT_MS` | TCP/PostgreSQL connection establishment bound | Must be greater than zero; default 5000 ms |
| `SAG_POSTGRES_QUERY_TIMEOUT_MS` | PostgreSQL session `statement_timeout` | Must be greater than zero; default 5000 ms |
| `SAG_POSTGRES_REPLICA_BUDGET` | Total PostgreSQL-using process replicas in the release topology | Update before adding replicas |
| `SAG_POSTGRES_RESERVED_CONNECTIONS` | Connections reserved for administration/failover | Keep outside application pools; default 10 |
| `SAG_POSTGRES_MAX_CONNECTIONS` | Documented server/cluster connection ceiling | Must match the deployed PostgreSQL tier; default 100 |

Every PostgreSQL-using process refuses startup when
`replica budget × pool max + reserved > max_connections`. This is a declared
topology budget, not automatic discovery: a scale change must update the count
before the new replica starts.

### Audit writer

| Variable | Meaning | Default |
|---|---|---|
| `SAG_AUDIT_QUEUE_CAPACITY` | Maximum records resident in one process audit channel | 4096 |
| `SAG_AUDIT_BATCH_SIZE` | Maximum records written by the single audit worker per transaction | 100 |
| `SAG_AUDIT_FLUSH_INTERVAL_MS` | Maximum normal batching delay | 250 ms |
| `SAG_AUDIT_DRAIN_TIMEOUT_MS` | Shutdown deadline for flushing accepted records | 5000 ms |

All values must be greater than zero. Data-plane calls use non-blocking
`try_record`; a full or closed channel is counted in
`audit_dropped_total{reason}`. Security-relevant management mutations do not
use this best-effort path: the business row and audit row commit in one storage
transaction or both roll back.

## Redis queue connection and recovery

| Variable | Meaning | Production rule |
|---|---|---|
| `SAG_REDIS_PASSWORD` | Password used by the single-node development Compose Redis | Non-empty; release override requires an operator-provided value |
| `SAG_BRIDGE_REDIS_URL` | Direct/managed Redis URL and master auth/TLS/db template for Sentinel | Production uses authenticated `rediss://`; DB 2 is reserved for the Bridge queue |
| `SAG_BRIDGE_REDIS_SENTINELS` | Comma-separated authenticated `redis://` or `rediss://` Sentinel endpoints | Empty for direct/managed primary; otherwise configure together with service name |
| `SAG_BRIDGE_REDIS_SENTINEL_SERVICE` | Sentinel master service name | Required exactly when Sentinel endpoints are present |
| `SAG_BRIDGE_REDIS_CONNECT_TIMEOUT_MS` | Bound for each Redis connection attempt | Default 2000; greater than zero |
| `SAG_BRIDGE_REDIS_COMMAND_TIMEOUT_MS` | Bound for Redis commands | Default 5000; must exceed the queue worker's 2000 ms blocking read |
| `SAG_BRIDGE_REDIS_RECONNECT_RETRIES` | Maximum bounded connection attempts | Default 6; greater than zero |
| `SAG_BRIDGE_REDIS_RECONNECT_BASE_MS` | Exponential reconnect base delay | Default 100; greater than zero |
| `SAG_BRIDGE_REDIS_RECONNECT_MAX_MS` | Reconnect delay cap | Default 2000; not less than base |
| `SAG_BRIDGE_READ_ONLY_SYNC_FALLBACK_ON_QUEUE_ERROR` | Explicit emergency fallback for GET/HEAD/OPTIONS only | Default false; mutations always fail closed with 503 |

The Bridge logs only deployment mode, credential-free endpoint, and timeout
bounds. Sentinel connection loss invalidates the cached master connection; the
failed command is not replayed because a response loss may mean the write was
already applied. The next operation performs bounded rediscovery. The local
Compose Redis has AOF and a volume, but is a development single point and is
not a production HA topology.

## Data-plane memory budgets

Every data-plane process validates this conservative startup formula with
checked arithmetic:

```text
reserved + ingress_concurrency * max_request_body
         + response_concurrency * max_response_body
         + queue_capacity * max_enqueued_bytes
         + stream_capacity * max_frame_bytes
         <= SAG_MEMORY_BUDGET_BYTES * 80%
```

Zero/unbounded body limits, multiplication overflow, and an over-budget
configuration stop startup. Compose maps service-specific host variables to
the binary-level `SAG_MEMORY_BUDGET_BYTES` variable.

| Process | Default budget | Primary bounded inputs |
|---|---:|---|
| public-edge | 512 MiB | `PUBLIC_EDGE_MAX_INFLIGHT=32`, request 1 MiB, response 4 MiB; connect 3s, first byte 10s, total 60s |
| Bridge | 512 MiB | hard ingress 128, sync 24, workers 16, request/response 1 MiB, queued frame 256 KiB |
| Agent | 768 MiB | pending 128, bidirectional stream buffers 128, request/response 1 MiB |
| Connector | 1536 MiB | inflight 256, accept queue 256, stream 128, request/response 1 MiB |

Relevant variables are `PUBLIC_EDGE_MAX_{REQUEST,RESPONSE}_BODY_BYTES`,
`SAG_BRIDGE_MAX_{BODY,RESPONSE_BODY}_BYTES`,
`SAG_AGENT_MAX_{REQUEST,RESPONSE}_BODY_BYTES`, and
`SAG_CONNECTOR_MAX_{REQUEST,RESPONSE}_BODY_BYTES`. Increasing any capacity or
body bound without increasing and revalidating the process budget is rejected.
Known oversized request bodies return 413; capacity and known oversized
responses return 503. For a chunked public-edge response with no declared
length, the proxy streams instead of buffering the entire response and aborts
the body stream if the cumulative cap is crossed.

### Repeated response headers and coordinated upgrade

`ForwardResponse.header_values` is an additive ordered repeated field. The
Connector populates it with append semantics and the Bridge prefers it over the
legacy map, so multiple `Set-Cookie` values survive. The Agent also persists it
in idempotency results. An old Agent decodes and discards unknown fields before
re-encoding, so this is **not** safe for mixed-version live traffic despite
being protobuf-additive. Stop ingress, drain Redis/Bridge/Agent pending work,
upgrade Agent → Connector → Bridge as one coordinated set, then reopen ingress.
Rollback uses the same drain and whole-set order; never roll back only one hop.

## Route sync
- `SAG_CONTROL_PLANE_SYNC_ENDPOINT`: single URL or comma-separated URLs.
- `SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK`: disable automatic localhost prepend.

## Compose defaults
- `SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true` on control-plane-admin in compose.
- APISIX admin key in compose sample: `your-admin-key`.
