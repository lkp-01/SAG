# 中交 4A 与 `sag-auth` 对接

协议细节以 **[中交4A认证协议说明.md](./中交4A认证协议说明.md)** 为准。

## `sag-auth` 已实现（OAuth2 授权码模式）

在以下环境变量**全部配置**时，启用浏览器 SSO：

| 变量 | 含义 |
|------|------|
| `SAG_FOURA_FIRST_URI` | 4A 登录页 |
| `SAG_FOURA_SECOND_URI` | 用 `code` 换 `access_token`（POST form） |
| `SAG_FOURA_THIRD_URI` | 用 `access_token` + `client_id` 取用户信息 |
| `SAG_FOURA_CLIENT_ID` / `SAG_FOURA_CLIENT_SECRET` | 应用凭证 |

回调地址：

- 推荐设置 **`SAG_FOURA_REDIRECT_URI`**（须与在 4A 平台登记的一致），例如 `http://<sag-auth>/api/v1/auth/sso/callback`。
- 若未设置，则使用 `SAG_PUBLIC_BASE_URL` + `/api/v1/auth/sso/callback`（默认 `http://127.0.0.1:8080/...`）。

接口：

- `GET /api/v1/auth/sso/login` — 302 跳转 4A。
- `GET /api/v1/auth/sso/callback?code=...&state=...` — 换票并拉取 `employeeNumber`，签发 **SAG JWT**。
  - 当配置了 `SAG_SSO_PORTAL_REDIRECT_URL` 时，会 **302 跳转到用户门户**并携带 `sso_token`（用于一键登录演示）。

角色：

- 默认 JWT 角色来自 `SAG_FOURA_DEFAULT_ROLES`（逗号分隔，默认 `user`）
- 可选：`SAG_FOURA_ROLE_MAP="boss:boss;alice:tech;bob:ops"` 用于联调演示时的账号到角色映射

安全说明（state 一次性 + 有效期）：

- `state` 用于防 CSRF 与回调重放
- `state` **一次性使用**：回调成功后会从内存中移除，复用旧页面/回退重试会返回 `invalid or expired state`
- `state` 默认有效期约 **10 分钟**

## 模式二（OIDC 标准授权码）

当前代码已支持 OIDC 授权码流程（与 4A 并存）：

- 环境变量：
  - `SAG_OIDC_ISSUER`
  - `SAG_OIDC_CLIENT_ID`
  - `SAG_OIDC_CLIENT_SECRET`
  - `SAG_OIDC_TOKEN_URI`
  - `SAG_OIDC_USERINFO_URI`
  - 可选：`SAG_OIDC_AUTHORIZE_URI`（默认 `${issuer}/authorize`）
  - 可选：`SAG_OIDC_SCOPES`（默认 `openid profile email groups`）
- 入口仍为：
  - `GET /api/v1/auth/sso/login?provider_id=oidc`
- 回调：
  - `GET /api/v1/auth/sso/callback?code=...&state=...`
- `groups` 来源：
  - 优先聚合 token 与 userinfo 的 `groups` 声明（数组或逗号字符串）
  - 写入 SAG JWT `external_groups`，供后续映射引擎使用

## 本地联调：Fake 4A（推荐）

当甲方暂未开放真实 4A 时，可用仓库内轻量模拟服务：

- 目录：`infra/fake-4a`
- 文档：`infra/fake-4a/README.md`
- Compose 服务名：`fake-4a`（默认端口 `19080`）

快速启动：

```bash
docker compose up -d fake-4a sag-auth
```

联调入口：

- 打开 `http://127.0.0.1:8080/api/v1/auth/sso/login`
- 浏览器会跳转到 fake 4A 的授权页，选择测试账号后回调 `sag-auth`
- `sag-auth` 完成换票与 userinfo 拉取后：
  - 若配置了 `SAG_SSO_PORTAL_REDIRECT_URL`：会自动跳转 `frontend-portal` 并完成一键登录
  - 否则：返回标准 SAG JWT JSON
