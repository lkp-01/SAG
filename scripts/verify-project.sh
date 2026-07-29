#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run() {
  printf '==> %s\n' "$1"
  shift
  "$@"
}

if command -v cargo >/dev/null 2>&1; then
  run "Rust format" cargo fmt --all -- --check
  run "Rust check" cargo check --workspace --all-targets
  run "Rust clippy" cargo clippy --workspace --all-targets -- -D warnings
  run "Rust tests" cargo test --workspace --all-targets
else
  printf 'SKIPPED Rust checks: cargo is not available\n'
fi

for frontend in frontend frontend-portal frontend-admin-next; do
  run "$frontend typecheck" npm --prefix "$frontend" run typecheck
  run "$frontend lint" npm --prefix "$frontend" run lint
  run "$frontend build" npm --prefix "$frontend" run build
done

if command -v docker >/dev/null 2>&1; then
  run "Compose config: main" docker compose -f docker-compose.yml config --quiet
  run "Compose config: main + release" docker compose -f docker-compose.yml -f docker-compose.release.yml config --quiet
  run "Compose config: edge" docker compose -f docker-compose.edge.yml config --quiet
  run "Compose config: edge + perf" docker compose -f docker-compose.edge.yml -f docker-compose.edge.perf.yml config --quiet
  run "Compose config: edge + hscale" docker compose -f docker-compose.edge.yml -f docker-compose.hscale-edge.yml config --quiet
  run "Compose config: edge + auth hscale" docker compose -f docker-compose.edge.yml -f docker-compose.hscale-auth.yml config --quiet
  run "Compose config: edge + release" docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml config --quiet
  run "Compose config: Intra" docker compose -f docker-compose.intra.yml config --quiet --no-env-resolution
  run "Production invariants" "$ROOT/scripts/ops/verify-production-invariants.sh"
else
  printf 'SKIPPED Compose checks: docker CLI is not available\n'
fi

printf 'All available project checks passed.\n'
