# SAG Frontend Console

## 本机启动

在 `sag-cloud/frontend` 目录执行：

```bash
npm install
npm run dev
```

默认地址：`http://127.0.0.1:5173`

## 环境变量

可通过 `.env` 覆盖后端基址：

- `VITE_CONTROL_BASE`（默认 `/api-control`，由 Vite 代理到 `8090`）
- `VITE_AUTH_BASE`（默认 `/api-auth`，由 Vite 代理到 `8080`）
- `VITE_POLICY_BASE`（默认 `/api-policy`，由 Vite 代理到 `8081`）
- `VITE_BRIDGE_BASE`（默认 `/api-bridge`，由 Vite 代理到 `9000`）
- `VITE_ZENTINEL_BASE`（默认 `/api-zentinel`，由 Vite 代理到 `10080`）

说明：默认走同源代理，避免浏览器 CORS 导致的 `TypeError: Failed to fetch`。

## 生产构建（内网部署）

```bash
npm run build
```

输出目录：`dist/`。可由 Nginx/Caddy/静态文件服务托管到内网服务器。
