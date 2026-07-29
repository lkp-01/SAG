# Edge / Intra 冒烟与 502 排查（与仓库根 `linux_back` 同步）

容器名以 **`docker-compose.edge.yml` / `docker-compose.intra.yml`** 为准：bridge 服务名为 **`http-tunnel-bridge`**（可用 `docker compose ... ps` 查看实际容器名），agent 为 **`sag-stealth-agent`**，connector 为 **`sag-connector`**。

## 1) 确认 9000 上是不是 bridge（Edge 本机）

```bash
curl -sS -o /dev/null -w "%{http_code}\n" http://127.0.0.1:9000/metrics
curl -sS http://127.0.0.1:9000/metrics | head -n 40
curl -sS http://127.0.0.1:9000/metrics | grep -E 'bridge_sync_inflight|http_requests_total' | head
```

## 2) 绕过 Zentinel，直连 bridge

```bash
curl -sS -w "\nhttp_code=%{http_code}\n" -H "x-sag-app-id: app-dev" "http://127.0.0.1:9000/dev/"
```

## 3) 容器日志与环境（Edge）

```bash
docker ps --format "table {{.Names}}\t{{.Ports}}" | grep -E 'bridge|zentinel|stealth'
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs http-tunnel-bridge 2>&1 | tail -n 80
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec http-tunnel-bridge printenv | grep -E 'SAG_TUNNEL|SAG_GRPC|SAG_BRIDGE|REDIS'
```

## 4) Zentinel → bridge

```bash
docker exec sag-zentinel getent hosts http-tunnel-bridge || true
docker exec sag-zentinel wget -qO- --timeout=3 http://http-tunnel-bridge:9000/metrics 2>&1 | head -n 5
```

## 5) Intra：connector

```bash
docker logs sag-connector 2>&1 | tail -n 60
curl -sS http://127.0.0.1:9103/metrics 2>/dev/null | grep -E 'connector_tunnel_up|connector_forward_' | head
```

## Windows `snapshot-bridge-metrics.ps1` 曾 0 行

已放宽过滤；无匹配时会写 **`metrics-bridge-*.raw.txt`**。冷启动时部分 `bridge_*` 可能仅在首请求后出现；新镜像在启动时会注册 **`bridge_sync_inflight`** 便于 `grep`。
