# SAG 演示与 CRUD 操作手册

适用对象：售前演示、现场讲解、运维同学上手。  
建议使用入口：`http://<服务器IP>:3001/ops`

---

## 1. 演示前准备（5 分钟）

1. 确认服务健康
   - 打开 `http://<服务器IP>:3001/ops/self-check`
   - 看到 `control-plane-admin / sag-auth / sag-policy / workflow` 均为 `ok`
2. 确认登录账号
   - 使用 `boss` 或 `admin` 角色账号登录（`ops/*` 页面有角色门禁）
3. 建议浏览器开 Network 面板
   - 方便演示“操作成功 + 实时生效”

---

## 2. 推荐演示流程（15~20 分钟）

1. **应用与 API 总览**：`/ops/apps`
2. **API 路由管理 CRUD**：`/ops/api-routes`
3. **OpenAPI 批量导入**：`/ops/openapi`
4. **身份源配置 CRUD**：`/ops/identity`
5. **用户/组映射规则 CRUD**：`/ops/mappings`
6. **审计中心查询**：`/ops/audit`
7. **统一监控入口**：`/ops/observability` + `/ops/workflow`

---

## 3. 功能与 CRUD 详细步骤

## 3.1 应用管理（`/ops/apps`）

### 新增
1. 在“应用管理”填写：
   - `app_id`: 例如 `app-fin-demo`
   - 展示名：`财务门户演示`
   - 描述：`用于演示财务 API 接入`
   - `enabled`: `true`
2. 点击“保存应用”

### 查询
- 页面卡片区会出现新的应用卡片（含路由数、请求数、QPS 等）

### 修改
1. 在应用标签区点击“编辑”
2. 修改展示名或描述
3. 点击“保存应用”

### 删除
1. 在应用标签区点击“删除”
2. 卡片消失即成功

---

## 3.2 API 路由管理（`/ops/api-routes`）

### 新增
1. 先点选目标 `app_id`
2. 填写：
   - `method`: `GET`
   - `path`: `/api/finance/report`
   - `enabled`: `true`
   - `description`: `财务报表查询`
3. 点击“保存”

### 查询
- 列表显示该应用的路由记录

### 修改
1. 点击某行“编辑”
2. 改 `path` 或 `method`
3. 点击“保存”

### 删除
1. 点击“删除”
2. 列表中该行消失

---

## 3.3 OpenAPI 导入（`/ops/openapi`）

先选择目标应用，然后粘贴 JSON，点“解析”再点“批量导入”。

### 示例 A（财务）

```json
{
  "openapi": "3.0.0",
  "info": { "title": "Finance API", "version": "1.0.0" },
  "paths": {
    "/api/finance/report": { "get": { "summary": "查询财务报表" } },
    "/api/finance/invoice": { "post": { "summary": "创建发票" } }
  }
}
```

### 示例 B（人事）

```json
{
  "openapi": "3.0.0",
  "info": { "title": "HR API", "version": "1.0.0" },
  "paths": {
    "/api/hr/employee": { "get": { "summary": "员工列表" } },
    "/api/hr/employee/{id}": { "delete": { "summary": "删除员工" } }
  }
}
```

导入后到 `/ops/api-routes` 可看到对应记录。

---

## 3.4 身份源配置（`/ops/identity`）

### 新增
1. 填写：
   - `id`: `foura`
   - `kind`: `oidc` 或 `foura`
   - `issuer`: OIDC issuer 地址
   - `client_id` / `client_secret`
   - `scopes`: 建议 `openid profile email groups`
   - `enabled`: `true`
2. 点击“保存”

### 查询/修改/删除
- 在“已配置身份源”表格中操作“编辑/删除”

---

## 3.5 用户组映射规则（`/ops/mappings`）

### 新增
1. 选择 `provider_id`（如 `foura`）
2. 填写：
   - `external_group`: `dept:finance`
   - `local_roles_csv`: `finance`
   - `priority`: `10`
   - `enabled`: `true`
3. 点击“保存”

### 查询
- 规则列表出现新增记录

### 修改
- 点“编辑”后修改角色或优先级，再“保存”

### 删除
- 点“删除”

---

## 3.6 审计中心（`/ops/audit`）

推荐演示点：
1. 先不加过滤，点“查询”，展示“最近 200 条”
2. 依次选择过滤条件：
   - `service = sag-auth`
   - `result = 200`
   - `department = finance`
3. 说明字段：
   - `user_id / app_id / path / latency_ms / decision / result / trace_id`

---

## 3.7 统一监控与工作流（`/ops/observability`、`/ops/workflow`）

演示要点：
1. `/ops/observability`：一个页面统一进入 Workflow、Apps、Grafana、Prometheus
2. `/ops/workflow`：展示各链路节点健康、QPS、错误率、P95
3. 如果某个 Prom 查询失败，页面应保持可用（单项降级而非整页报错）

---

## 4. 常见演示问题与处理

1. `workflow: ERR: 401 Unauthorized`
   - 先刷新登录态，再到 `/ops/self-check` 点“刷新健康状态”
2. `/ops/apps` 首屏慢
   - 先看 Network 里 `tree/apps/routes` 的耗时
   - 再看后端日志中的 `with_latest`、`routes_ms/latest_ms`
3. 审计没数据
   - 先确认你刚刚确实做过 API 操作（新增/编辑/删除）
   - 再用 `service` 过滤定位（如 `control-plane-admin` / `sag-auth` / `sag-policy`）

---

## 5. 现场演示建议话术（简版）

1. “先看总览与实时指标，再进入具体能力 CRUD。”
2. “OpenAPI 导入用于快速批量落地 API 资产。”
3. “身份源 + 映射规则把外部组织映射成本地权限。”
4. “审计中心可按人、应用、时间、结果追溯。”
5. “统一监控把业务视角和平台视角放在一个入口。”

---

## 6. 故障演示闭环（mentor 建议落地）

推荐在 CRUD 演示结束后加一个 3~5 分钟故障演示：

1. 注入一个慢请求或超时故障（测试环境）。
2. 打开 `/ops/workflow` 展示秒级异常发现（P95/P99 或错误率突变）。
3. 在 `/ops/audit` 按时间窗口查询，展示持久化留痕（`service/path/result/latency_ms/trace_id`）。
4. 解除故障后再次刷新，展示恢复闭环。

详细执行步骤见：`docs/ops/fault-demo-runbook.md`

