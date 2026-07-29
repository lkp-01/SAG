# SAG 用户门户（frontend-portal）

## 目标

面向最终用户的中文门户，提供：

- 登录与会话校验（`sag-auth`）
- 服务图标导航 + 列表查询
- 网关探测（`Zentinel -> APISIX` 链路）
- `admin/boss` 才显示“进入管理端”按钮

## 本地开发

```powershell
cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\frontend-portal
npm install
npm run dev
```

默认地址：`http://127.0.0.1:5174`

## 代理配置

- `vite.config.ts` 中：`/api-auth`、`/api-policy`、`/api-zentinel` 分别代理到环境变量指定的目标。
- Compose 内通常设为：
  - `VITE_AUTH_PROXY_TARGET=http://sag-auth:8080`
  - `VITE_POLICY_PROXY_TARGET=http://sag-policy:8081`
  - `VITE_ZENTINEL_PROXY_TARGET=https://zentinel:10443`
- `VITE_DEMO_SITES_BASE`：图标跳转到演示静态站（默认 `http://127.0.0.1:28080`；浏览器访问宿主机端口，与 `company-demo-sites` 映射一致）。
- `VITE_ADMIN_PLANE_URL`：门户右上角“进入管理端”的跳转地址（默认 `http://127.0.0.1:5173`）。

## 说明

- “图标跳转”打开演示页，便于联调；**网关鉴权与放行以隧道 + `sag-policy` 为准**。
- 管理端入口只做前端可见性控制并不充分，后端管理 API 还会做 `admin/boss` 鉴权。
