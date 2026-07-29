# Test workload (mock)

- `python mock_http_server.py` — binds `0.0.0.0:18080`, paths `/api/whoami`, `/api/echo`, `/api/test`, `POST /api/body`.

Pair with APISIX upstream `host.docker.internal:18080` when APISIX runs in Docker.
