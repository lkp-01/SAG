# Connector Session Leases and Failover Design

## Goal

Replace the independent heartbeat timestamp and single outbound stream maps with a generation-bound Connector session pool. A route is healthy only when at least one currently registered session has a fresh heartbeat, and expiration or stream loss must immediately fail only the requests assigned to that session.

## Chosen approach

The Agent keeps Connector availability in process memory because the bidirectional gRPC stream itself is process-local state. Each `connector_endpoint` becomes a logical Connector group containing one or more sessions. Every accepted stream receives an internal monotonically increasing generation. Registration, heartbeat freshness, outbound sender, close signal, Connector identity, and pending attempts all refer to that generation.

This approach supports rolling deployment and active-active Connector replicas without adding Redis or PostgreSQL to the data-plane liveness path. A Connector still opens one stream to every explicitly configured Agent endpoint. Each Agent independently selects a healthy local session; no distributed lease is required for the current explicit multi-Agent topology.

Alternatives rejected:

- Only reduce the 120-second window: improves detection latency but leaves split-brain heartbeats, single-stream replacement races, and no Connector failover.
- Store leases in PostgreSQL or Redis: cannot move the in-memory gRPC sender between Agent processes and adds an external dependency to the failure detector.
- New protocol-level registration acknowledgement and fencing token immediately: architecturally clean, but forces a coordinated protocol rollout. Stream-local generation binding provides the required safety without a protobuf compatibility break; an acknowledgement can be added later.

## Session lifecycle

1. A Connector opens `CreateTunnel`; its peer certificate fingerprint is captured from the TLS connection.
2. The first tunnel message must be `ConnectorRegister`.
3. The Agent authorizes the certificate fingerprint for the claimed `connector_endpoint`, then adds a new session to that endpoint's pool.
4. Subsequent heartbeats are accepted only when their endpoint and Connector ID match the stream-local registered session. The registry updates only that generation.
5. Request selection uses round-robin across sessions whose heartbeat age is below the lease window.
6. Stream loss removes that generation and drops only its pending response senders.
7. A one-second reaper expires stale generations, signals their server streams to close, and drops their pending response senders. The Connector then reconnects through its existing maintenance loop.

Multiple sessions for the same endpoint are intentional. Operators should give replicas unique `connector_id` values and unique client certificates while setting the same `SAG_CONNECTOR_ENDPOINT` group key.

## Health semantics

The default heartbeat remains two seconds. The default lease window becomes ten seconds (five missed heartbeats). Request admission and session selection both enforce freshness, preventing the check/send race from selecting an expired stream. Stream presence and heartbeat freshness are no longer separate sources of truth.

The lease represents tunnel liveness, not full downstream readiness. This change lays the required session foundation; richer readiness fields and per-app passive circuit breaking remain a follow-up because they require a deliberate protocol extension and APISIX failure classification.

## Security boundary

When gRPC mTLS is enabled, Connector certificate binding is required by default. `SAG_CONNECTOR_CERT_BINDINGS` maps an endpoint to one or more SHA-256 certificate fingerprints using repeated comma-separated `endpoint=fingerprint` entries. Multiple fingerprints allow independent certificates for replicas in the same group.

The Agent must fail startup if mTLS is enabled, binding is required, and no valid bindings are configured. It must also fail startup when the client CA cannot be read; silently falling back to server-only TLS is forbidden. Plaintext development mode defaults certificate binding off.

Repository TLS files remain explicitly test-only. The all-in-one development compose declares the test certificate fingerprint. Dual-host/production configuration must provide external certificate paths and fingerprints and must rotate any previously deployed repository keys.

## Failure handling

- Clean disconnect or TCP reset: generation is removed immediately; assigned requests fail immediately.
- Network blackhole: heartbeat lease expires in at most about eleven seconds with the one-second reaper, independent of the library's implicit HTTP/2 ping timeout.
- Stopped forwarding with live heartbeat: bounded queues and deadlines continue to protect resources; readiness/circuit-breaker work is tracked separately.
- One replica fails: only its assigned in-flight attempts fail; new requests select remaining healthy sessions. Mutating requests are never automatically replayed.
- Invalid heartbeat or second registration on one stream: close that stream as a protocol violation.

## Verification

Unit tests cover multi-session failover, generation-bound heartbeats, stale-session expiration, old-generation unregister safety, and pending cleanup. Configuration tests cover binding parsing and authorization. Project verification also runs Rust formatting, relevant package tests, and Docker Compose configuration checks when the host toolchain is available.

