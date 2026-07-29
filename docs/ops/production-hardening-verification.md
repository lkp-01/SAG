# Production Hardening Verification

This page records how the seven-point production-hardening gates are run and
what constitutes evidence. A transport response in the range HTTP 200–599 is
not a successful business request. Full-chain success requires the scenario's
expected business status and response body, with authentication, policy,
idempotency where applicable, audit, tunnel, APISIX, and workload evidence.

## Reproducible baseline

- Baseline date: 2026-07-26 (Asia/Singapore)
- Workspace: `D:\developer\Secure_Access_Gateway_SAG-clean-main`
- Git SHA: unavailable because the supplied `.git` metadata is not recognized
  by Git. Do not initialize a replacement repository; preserve the task commit
  boundaries from the implementation plan until the correct clone/worktree is
  restored.
- Rust: `rustc 1.97.0 (2d8144b78 2026-07-07)`, Cargo 1.97.0. The active host
  toolchain is `stable-x86_64-pc-windows-msvc`; `link.exe` is not on `PATH`.
  The installed `stable-x86_64-pc-windows-gnu` toolchain is the first local
  fallback to test.
- Docker Compose: v5.1.2.
- PowerShell: Windows PowerShell is available; `pwsh` is not installed.
- WSL: Ubuntu is installed, but Docker Desktop integration and a Linux Rust
  toolchain were not available in the initial check.

The production invariant check renders and parses these normalized models:

1. `docker-compose.edge.yml` + `docker-compose.hscale-edge.yml` +
   `docker-compose.release.edge.yml`
2. `docker-compose.intra.yml` + `docker-compose.release.intra.yml`

`docker compose config --format json --no-env-resolution` is used so the Intra
model can be checked without inventing a production `.env.intra` credential
file. Environment interpolation still comes from the caller and Compose
defaults; production runs must provide their real secret environment.

## Commands

Run the focused production configuration gate:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/ops/verify-production-invariants.ps1
```

The plan's equivalent PowerShell 7 command is:

```powershell
pwsh scripts/ops/verify-production-invariants.ps1
```

Run the complete project verification when the required toolchains are
available:

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets
docker compose -f docker-compose.edge.yml -f docker-compose.hscale-edge.yml config --quiet
docker compose -f docker-compose.intra.yml config --quiet --no-env-resolution
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-project.ps1
```

On Linux, use `scripts/verify-project.sh`; it invokes the shell invariant gate,
which requires Docker Compose and Python 3.

## Initial known failures

The Task 1 gate is intentionally expected to fail against the initial Compose
files. It must enumerate, rather than hide, at least these categories:

- Bridge and Redis host ports are published on all interfaces; Intra etcd and
  APISIX Admin ports are also published on all interfaces.
- The second hscale Bridge lacks the shared mTLS enablement, CA, client
  certificate, and client key settings; the first Bridge resolves empty TLS
  paths without a production secret environment.
- Long-running services lack one or more restart policy, healthcheck, CPU
  limit, or memory limit.
- Redis lacks an AOF command, a `/data` volume, and enforced authentication.
- Release rendering still resolves repository example credentials.

These are expected failing tests, not accepted production exceptions. Tasks 3,
7, and 12 must remove them before the invariant gate can be marked passing.

## Task 1 baseline results

Results captured on 2026-07-26:

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed (exit 0) |
| `cargo test --workspace --all-targets` on default MSVC | Not run to completion: compilation stopped because `link.exe` is absent |
| `cargo +stable-x86_64-pc-windows-gnu test --workspace --all-targets` | Passed (exit 0); Redis-dependent ignored test remained explicitly ignored |
| Edge base + hscale Compose config | Passed (exit 0) |
| Intra Compose config, exact plan command | Not run to completion: required `.env.intra` is absent |
| Intra Compose config with `--no-env-resolution` | Passed static parsing (exit 0) |
| `pwsh scripts/ops/verify-production-invariants.ps1` | Not runnable: `pwsh` is not installed |
| Windows PowerShell invariant gate | Expected failure (exit 1), reporting 89 initial violations |
| Shell invariant gate | Syntax passed; runtime not completed because the available WSL distribution lacks Docker Desktop integration |

The initial gate failure is the intended Task 1 red test. It explicitly listed
the Bridge, Redis, etcd, and APISIX Admin all-interface publications; both
Bridge mTLS problems; runtime guard omissions; Redis durability/authentication
omissions; and resolved example credentials.

## Evidence rules

For every run, retain the exact command, start/end time, host and tool versions,
Git SHA or the explicit reason it is unavailable, Compose file combination,
exit code, and raw output. Later load and fault gates must additionally retain
per-request/job final classifications, raw k6 output, metrics snapshots,
container statistics, and correlation samples. A test that cannot run is
recorded as not run with its concrete environmental blocker; it is never
reported as passed.

## Final repository acceptance (2026-07-26)

The user-approved acceptance boundary is repository/code completeness, not a claim of Linux or production qualification.

| Check | Result |
|---|---|
| `cargo +stable-x86_64-pc-windows-gnu fmt --all -- --check` | Passed |
| `cargo +stable-x86_64-pc-windows-gnu clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo +stable-x86_64-pc-windows-gnu test --workspace --all-targets --no-fail-fast` | Passed: 75 runnable tests; 6 external Redis/PostgreSQL/multi-instance tests explicitly ignored |
| Edge/Intra production Compose invariant renderer | Passed with synthetic, non-routable static-render values; no production credential was used |
| HA topology static contract | Passed; this is not a failover result |
| PowerShell and shell production load/fault validators | Self-tests passed; this proves validator behavior only |
| Three frontend typechecks and production builds | Passed |
| Frontend lint | Exit 0; 16 pre-existing warnings, no errors |

Not run: real PostgreSQL migration rehearsal/old-binary check, Redis 7 crash matrix, two Auth replicas sharing PostgreSQL/Redis, Linux network fault injection, managed PostgreSQL/Redis failover, 350 RPS full-chain fault gate, three steady repeats, and 120-minute soak. The blockers are the unavailable company environment/credentials and the broken `.git` metadata (a real SHA is deliberately required by production gates).
