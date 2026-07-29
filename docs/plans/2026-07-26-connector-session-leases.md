# Connector Session Leases Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Connector health generation-bound, fail stale sessions within ten seconds, support multiple Connector replicas per endpoint, and bind registrations to approved mTLS certificate fingerprints.

**Architecture:** Replace the single `endpoint -> stream` map with `endpoint -> session pool`, where every session owns a generation, heartbeat timestamp, outbound sender, and close signal. The gRPC handler binds every heartbeat to its stream-local generation, while a periodic reaper closes expired sessions and wakes their pending requests. Connector registrations are authorized by endpoint-to-certificate-fingerprint bindings.

**Tech Stack:** Rust 2021, Tokio, tonic 0.12 bidirectional streaming, std synchronization primitives, Docker Compose, Prometheus metrics.

---

### Task 1: Session pool behavior

**Files:**
- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`

**Step 1:** Add failing tests for two sessions on one endpoint, generation-bound heartbeat updates, stale expiration, and failover after one generation unregisters.

**Step 2:** Run `cargo test -p stealth-tunnel-agent connector_registry::tests` and confirm the new APIs/tests fail to compile or fail assertions.

**Step 3:** Implement `endpoint -> Vec<SessionEntry>`, round-robin healthy selection, close signals, per-generation pending cleanup, and stale-session reaping.

**Step 4:** Re-run the focused tests and confirm they pass.

### Task 2: Stream-local registration and heartbeat binding

**Files:**
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/manager.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Step 1:** Add tests for heartbeat/session identity matching.

**Step 2:** Make Register the first and only registration message on a stream, bind Heartbeat to the registered generation, and reject protocol violations.

**Step 3:** Replace `TunnelManager.last_heartbeat` checks with registry health checks and pass the lease window into request selection.

**Step 4:** Start a one-second stale-session reaper from Agent main and expose session-count/expiration metrics.

**Step 5:** Run the Agent tests.

### Task 3: mTLS endpoint authorization

**Files:**
- Modify: `proxy/agents/stealth-tunnel-agent/src/config.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Step 1:** Add tests for binding parsing, fingerprint normalization, and endpoint authorization.

**Step 2:** Parse `SAG_REQUIRE_CONNECTOR_CERT_BINDING` and `SAG_CONNECTOR_CERT_BINDINGS`, requiring bindings by default when mTLS is enabled.

**Step 3:** Capture the tonic peer certificate fingerprint and authorize the Register endpoint before adding a session.

**Step 4:** Make a missing/unreadable client CA fatal whenever mTLS is enabled.

**Step 5:** Run configuration and Agent tests.

### Task 4: Safe deployment defaults

**Files:**
- Modify: `docker-compose.yml`
- Modify: `docker-compose.edge.yml`
- Modify: `.env.example`
- Modify: `.env.dualhost.example`
- Modify: `edge-host.env.example`
- Modify: `infra/storage-seed/company_demo_postgres.sql`
- Modify: `scripts/seed-company-demo.sh`
- Modify: `scripts/seed-company-demo.ps1`
- Modify: `DUAL_HOST_OPERATIONS.md`

**Step 1:** Change the tunnel health window default from 120 seconds to 10 seconds and pass it explicitly in Edge compose.

**Step 2:** Configure the all-in-one development certificate fingerprint; require externally supplied bindings in dual-host configuration.

**Step 3:** Change demo tunnel routes to require healthy tunnels.

**Step 4:** Document replica group semantics, certificate rotation, and rollout/rollback requirements.

**Step 5:** Run `docker compose ... config --quiet` for main, Edge, and Intra variants.

### Task 5: Verification

**Files:**
- Verify all modified Rust, compose, environment example, seed, and documentation files.

**Step 1:** Run `cargo fmt --all -- --check`.

**Step 2:** Run `cargo test -p stealth-tunnel-agent -p sag-connector -p http-tunnel-bridge`.

**Step 3:** Run `scripts/verify-project.ps1` or its equivalent individual checks.

**Step 4:** Review the diff for unintended private-key contents, unrelated file changes, and compatibility risks.

**Step 5:** Record any environment-specific verification blocker with the exact failing command.

