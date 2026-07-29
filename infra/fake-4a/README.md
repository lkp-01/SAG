# Fake 4A (轻量联调模拟)

用于在甲方暂未开放真实 4A 时，模拟 `sag-auth` 当前已接入的三段式流程：

- `SAG_FOURA_FIRST_URI` -> `/oauth/authorize`
- `SAG_FOURA_SECOND_URI` -> `/oauth/token`（POST form）
- `SAG_FOURA_THIRD_URI` -> `/oauth/userinfo`

## 1) 通过 Docker Compose 启动

在 `sag-cloud` 根目录：

```bash
docker compose up -d fake-4a sag-auth
```

默认 fake 4A 地址：

- `http://127.0.0.1:19080`

## 2) 联调入口

浏览器打开：

- `http://127.0.0.1:8080/api/v1/auth/sso/login`

会跳转到 fake 4A 登录页，点任一测试账号后回调 `sag-auth` 并返回 SAG JWT（JSON）。

说明：

- 若 `sag-auth` 配置了 `SAG_SSO_PORTAL_REDIRECT_URL`，回调后会直接 **302 跳转用户门户**并携带 `sso_token`（用于一键登录演示）。
- 页面提供 “未认证访客（预期被拦截）” 入口，用于展示网关在缺少身份时的拒绝行为（通常为 `403 missing user identity`）。

## 3) 默认测试账号

- `alice`（技术）
- `bob`（运维）
- `boss`（老板）

`employeeNumber` 与账号同名，便于在 `sag-auth` 中映射为用户名/用户ID。

你也可以直接改 `infra/fake-4a/users.json` 自定义测试账号（推荐）。

## 4) 调试追踪（审计）

提供只读审计接口（最近 N 条）：

- `GET /debug/audit?limit=50`

示例：

```bash
curl "http://127.0.0.1:19080/debug/audit?limit=20"
```

可用于排查一次 SSO 流程中 authorize/token/userinfo 的参数与错误。

## 5) 可选环境变量

- `FAKE_4A_HOST`（默认 `0.0.0.0`）
- `FAKE_4A_PORT`（默认 `19080`）
- `FAKE_4A_CLIENT_ID`（默认 `sag-local-client`）
- `FAKE_4A_CLIENT_SECRET`（默认 `sag-local-secret`）
- `FAKE_4A_TOKEN_TTL_SEC`（默认 `3600`）
- `FAKE_4A_USERS_FILE`（默认 `/workspace/infra/fake-4a/users.json`）
- `FAKE_4A_AUDIT_LIMIT`（默认 `200`）
- `FAKE_4A_PORTAL_URL`（默认 `http://127.0.0.1:5174`，用于未认证访客演示跳转）

## 6) 协议细节（增强）

- 支持透传并返回 `scope`（authorize -> token -> userinfo）
- `token` 错误响应增加 OAuth 风格字段：
  - `error=unsupported_grant_type`
  - `error=invalid_client`
  - `error=invalid_grant`

## 7) 常见问题：回退后提示 `invalid or expired state`

这是预期行为：

- `state` 是一次性且有时效（默认约 10 分钟）
- 你在浏览器“回退”会复用旧页面里的旧 `state`，因此会被 `sag-auth` 拒绝

正确做法：每次切换用户都从 `http://127.0.0.1:8080/api/v1/auth/sso/login` 重新进入生成新 `state`。
