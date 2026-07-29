# 前端后端端口与代理映射清单

适用场景：单机部署、同城双活/主备预案、现场故障切换演示。

---

## 1. `frontend-admin-next` 代理清单（Next.js）

来源：`frontend-admin-next/next.config.js` 与 `docker-compose.yml`。

| 前端请求前缀 | 代理环境变量 | 默认目标（容器内） | 默认端口 | 功能说明 |
|---|---|---|---|---|
| `/api-control/:path*` | `CONTROL_PROXY_TARGET` | `http://control-plane-admin:8090` | 8090 | 应用、路由、审计、控制面 API |
| `/api-auth/:path*` | `AUTH_PROXY_TARGET` | `http://sag-auth:8080` | 8080 | 登录、SSO、身份源、用户接口 |
| `/api-policy/:path*` | `POLICY_PROXY_TARGET` | `http://sag-policy:8081` | 8081 | PDP 策略评估与管理 |
| `/api-bridge/:path*` | `BRIDGE_PROXY_TARGET` | `http://http-tunnel-bridge:9000` | 9000 | 隧道桥接链路 |
| `/api-zentinel/:path*` | `ZENTINEL_PROXY_TARGET` | `https://example.com:10080` | 10080 | 数据面入口（经 zentinel） |
| `/api-prom/:path*` | `PROM_PROXY_TARGET` | `http://prometheus:9090` | 9090（容器）/9091（宿主） | 指标查询 |
| `/api-grafana/:path*` | `GRAFANA_PROXY_TARGET` | `http://grafana:3000` | 3000 | 监控面板嵌入 |

> 注：宿主访问 Prometheus 通常用 `9091`，容器内目标是 `prometheus:9090`。

---

## 2. `frontend-admin-next` 对外端口

- 管理端入口：`3001`（`frontend-admin-next`）
- 观测组件：
  - Grafana：`3000`
  - Prometheus：`9091`

---

## 3. 主备切换预留配置样例（当前可先单活）

当前系统单活即可运行，但建议提前按以下变量预留主备地址。

### 3.1 环境变量占位建议

```bash
# control-plane
CONTROL_PROXY_TARGET_PRIMARY=http://control-plane-admin-a:8090
CONTROL_PROXY_TARGET_STANDBY=http://control-plane-admin-b:8090

# auth
AUTH_PROXY_TARGET_PRIMARY=http://sag-auth-a:8080
AUTH_PROXY_TARGET_STANDBY=http://sag-auth-b:8080

# policy
POLICY_PROXY_TARGET_PRIMARY=http://sag-policy-a:8081
POLICY_PROXY_TARGET_STANDBY=http://sag-policy-b:8081
```

### 3.2 切换策略（手工版）

1. 将 `*_PROXY_TARGET` 指向 primary。
2. primary 故障时，改为 standby 并重启 `frontend-admin-next`。
3. 通过 `/ops/self-check` 与 `/ops/workflow` 验证恢复。

---

## 4. 发布检查清单（端口视角）

1. `frontend-admin-next` 环境变量是否与部署拓扑一致。
2. `3001/3000/9091` 防火墙与安全组是否放通。
3. `api-zentinel` 目标证书与 `NODE_EXTRA_CA_CERTS` 是否匹配。
4. 切换后是否在 `/ops/self-check` 与 `/ops/audit` 可观察到恢复事件。

