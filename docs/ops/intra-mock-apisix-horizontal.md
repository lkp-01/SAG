# Intra：APISIX 与 mock 水平扩展说明

对应 [high-concurrency-reliability-master-plan.md](high-concurrency-reliability-master-plan.md) §1.3.1：在 **`upstream_5xx` 高** 时，优先验证 **mock / APISIX** 是否先于隧道饱和。

## APISIX（`infra/apisix/config.yaml`）

当前仓库内 `config.yaml` 为 **traditional** 部署、`node_listen: 9080`，路由多由 **etcd** 下发（`deployment.etcd`）。水平扩展常见做法：

1. **多 worker 进程**：在 **宿主机或镜像入口** 使用 APISIX 官方推荐方式调大 `worker_processes`（具体文件依安装形态为 `config.yaml` 或 `nginx.conf` 模板）；Docker 单容器内多为 **多 worker** 而非多容器，除非你做 **多 APISIX 实例 + VIP**。  
2. **多实例 + LB**：多个 APISIX 容器前置 L4/L7 负载均衡；**connector** 的 `SAG_APISIX_BASE_URL` 改为 **VIP/DNS**（见主计划 §1.4）。  
3. **限流与超时**：在路由上配置 `limit-req` / `limit-conn` 与 **较短的上游超时**，避免 connector 线程在慢 mock 上堆积（与主计划 §3 一致）。运维判定与 connector/agent 指标见 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md)。

> 本仓库 **未** 默认提交「双 APISIX 容器」compose，以免与单机 9080 端口演示冲突；生产形态请按上条自行加 second 实例与 LB。

## mock-workload（`docker-compose.intra.yml` 中 `mock-workload`）

- 压测瓶颈常在 **Python 单进程 mock**。扩展选项：  
  - **垂直**：为 mock 容器调高 CPU limit、或换更高效 mock。  
  - **水平**：起第二个 mock 容器（不同宿主机端口或仅内网），在 **APISIX upstream** 中配置 **多节点 + 负载均衡**，将原指向 `mock:18080` 的路由改为 upstream 组。  
- 修改 APISIX 路由后需 **回归** `smoke` 与 k6 同口径脚本。

## 验证

- 对齐时间窗： [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md) 中 APISIX / mock 日志与 k6 `upstream_5xx` 峰值。
