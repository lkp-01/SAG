# Legacy：手工启动（仅调试用）

本文件收纳旧的“逐服务手工启动”流程，避免根 `README.md` 被调试细节淹没。  
日常开发/联调请优先使用 `README.md` 的 Docker Compose 快速启动。

---

## 手工启动（逐服务）

> 说明：以下流程适用于需要逐进程排障、单独复现、或不方便使用 Docker 的场景。

```powershell
cargo check --workspace
```

### 步骤 A2：准备 SQLite 目录（首次运行必做）

`control-plane-admin` 与 `sag-policy` 使用 **`shared_storage::resolve_storage_db_path()`**：默认 **`data/sag-storage/sag.db`**（相对**当前工作目录**），启动时会自动 `mkdir` 父目录。  
**无需再逐个终端设置 `SAG_STORAGE_DB_PATH`**，只要在每个终端里先 **`cd` 到同一 `sag-cloud` 根目录** 再执行 `cargo run` 即可共用同一物理库（含 Windows 原生与 WSL 同盘场景，例如 `D:\…\sag-cloud` 与 `/mnt/d/…/sag-cloud`）。

> 仅当你**有意**把库放到别处时，才对所有相关进程统一设置 `SAG_STORAGE_DB_PATH`。

### 终端启动清单（建议按顺序打开）

**约定**：下文凡 `cargo run`，若无特殊说明，均在 **`sag-cloud` 目录**执行（Windows：`cd d:\…\Secure_Access_Gateway_SAG\sag-cloud`；WSL：`cd /mnt/d/…/sag-cloud`）。

前置（**标准数据面**建议先起；其中 **APISIX 为必配组件**）：

- （Docker）启动 APISIX（须同时暴露 Admin `:9180` + **Data `:9080`**，供 `sag-connector` 使用）
  - UI：`http://127.0.0.1:9180/ui/`
  - Admin API：`http://127.0.0.1:9180/apisix/admin/*`（`X-API-KEY` 与 `config.yaml` 的 `admin_key.key` 一致）
  - Data plane：`http://127.0.0.1:9080/*`
- （Windows）启动内网 Mock Workload（监听 `:18080`）

  ```powershell
  cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\infra\test-workload
  python .\mock_http_server.py
  ```

1. **Terminal 1**（WSL 或 Windows，任选其一）：`cargo run -p control-plane-admin`（`8090`）  
   - 本地演示可加上（表为空时自动插入 `app-001` 隧道路由）：
   （PowerShell：`$env:SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE="true"`；Bash：`export SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true`）

2. **Terminal 2**（Windows 或 WSL）：

   ```powershell
   $env:SAG_JWT_SECRET="dev-jwt-secret"; $env:SAG_BOOTSTRAP_ADMIN_PASSWORD="Admin@123"; cargo run -p sag-auth
   ```

   （Bash：`export SAG_JWT_SECRET=dev-jwt-secret` 等，然后 `cargo run -p sag-auth`。）

3. **Terminal 3**：`cargo run -p sag-policy`（`8081`；与 Terminal 1 **同一 `sag-cloud` cwd 即可**，与 admin 共用默认 `data/sag-storage/sag.db`）

4. **Terminal 4 — sag-connector**（二选一：Windows **或** WSL；勿各启一份）

   **Windows（PowerShell）**：

   ```powershell
   $env:SAG_TUNNEL_ENDPOINT="https://127.0.0.1:50051"
   $env:SAG_CONNECTOR_ID="connector-local-001"
   $env:SAG_APP_ID="app-001"
   $env:SAG_EXTERNAL_HOST="app.internal.com"
   $env:SAG_APISIX_BASE_URL="http://127.0.0.1:9080"
   $env:SAG_MESH_MODE="noop"
   cargo run -p sag-connector
   ```

   **WSL（Bash）**：

   ```bash
   export SAG_TUNNEL_ENDPOINT="https://127.0.0.1:50051"
   export SAG_CONNECTOR_ID="connector-local-001"
   export SAG_APP_ID="app-001"
   export SAG_EXTERNAL_HOST="app.internal.com"
   export SAG_APISIX_BASE_URL="http://127.0.0.1:9080"
   export SAG_MESH_MODE="noop"
   cargo run -p sag-connector
   ```

5. **Terminal 5**（WSL）：`stealth-tunnel-agent`（gRPC `50051`，默认 mTLS）

   ```bash
   cd /mnt/d/lxz/compile/Rust_project/Secure_Access_Gateway_SAG/sag-cloud
   cargo run -p stealth-tunnel-agent
   ```

   **一般不必设置** `SAG_CONTROL_PLANE_SYNC_ENDPOINT`：默认会拉 `http://127.0.0.1:8090/api/v1/agent/routes`；若你还设置了 `http://10.255.255.254:8090/...`（例如来自 `resolv.conf` 的 `nameserver`）且该地址**连不通** Windows 上的 admin，代码会**先尝试 127.0.0.1**（自动追加，除非设置 `SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK=true`）。

   若 `127.0.0.1:8090` 仍不可达，再查 WSL 默认网关（示例）：`GW=$(ip route show default | awk '{print $3; exit}')`，然后 `export SAG_CONTROL_PLANE_SYNC_ENDPOINT="http://${GW}:8090/api/v1/agent/routes"`（或与 `127.0.0.1` **逗号并列**，多地址依次尝试）。

6. **Terminal 6**（WSL）：`cargo run -p http-tunnel-bridge`（HTTP `9000`，须独占该端口）

7. **Terminal 7**（WSL）：Zentinel + 冒烟

   ```bash
   cd /mnt/d/lxz/compile/Rust_project/Secure_Access_Gateway_SAG/sag-cloud
   SAG_WINDOWS_HOST_IP="127.0.0.1" bash ./scripts/start-zentinel-wsl.sh
   ```

   **Windows 冒烟**（另开 PowerShell）：

   ```powershell
   cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
   Set-ExecutionPolicy -ExecutionPolicy Bypass -Scope Process
   .\scripts\smoke-dataplane.ps1
   ```

前端：

```powershell
$env:npm_config_cache="D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\.npm-cache"
npm run dev
```

---

## 兼容旧文档（历史参考）

### 数据面串联验证（兼容旧文档；建议以 `README.md` 快速启动为准）

1. 按上述步骤启动服务  
2. 执行 `.\scripts\smoke-dataplane.ps1`  
3. 若失败，优先检查：端口占用、服务是否全部启动、请求头是否包含 `x-sag-app-id` 和身份信息

#### 可选环境变量（保留为参考）

- `SAG_CONTROL_PLANE_SYNC_ENDPOINT`
  - **作用**：覆盖隧道路由同步地址
  - **默认**：`http://127.0.0.1:8090/api/v1/agent/routes`
  - **示例（容器环境）**：`http://control-plane-admin:8090/api/v1/agent/routes`

