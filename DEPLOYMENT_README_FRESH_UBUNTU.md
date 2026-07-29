# SAG 部署执行手册（全新 Ubuntu 版）

> 适用场景：你当前这台服务器是“全新 Ubuntu”，希望按 SAG 现有真实方案自行部署。  
> 仓库地址固定为：`http://192.168.14.10/digital-operation/secure_access_gateway_sag`

**Woo 内网部署机（本手册默认、已写死 IP，无需再填「VM_IP」）**：**`192.168.9.26`**。下文凡写浏览器跨机访问、`SAG_PUBLIC_HOST`、`-VmHost` 均指该地址；若实际换机，全文替换该 IP 即可。

---

## 0. 开始前你只需要确认 4 件事

如果以下 4 项里有未确定项，也可以先按默认值部署，后续再改：

1. **部署模式**：先单机（推荐）还是直接双机 edge/intra。
2. **分支名**：默认 `clean-main`。
3. **证书策略**：先用仓库测试证书跑通，还是直接上正式证书。
4. **服务器放通端口**：是否允许本机访问 `3001/8080/8090/8081/9000/10080/9091/3000/9080/9180`（`5174` 已降级为 legacy）。

---

## 1. 目标结果（先看终态）

当部署成功时，应满足：

- `docker compose ps` 中核心服务为 `Up`
- `curl -i http://127.0.0.1:9000/api/test` 返回 `200`（T1）
- `curl -i http://127.0.0.1:3001/api-zentinel/api/test` 返回 `200`（N1）
- `http://192.168.9.26:3001` 可打开统一前端入口（登录后按角色自动跳转；Woo 内网机固定为该 IP）

---

## 2. 新机初始化（一次性）

```bash
sudo apt update
sudo apt -y install ca-certificates curl gnupg lsb-release git openssl jq
sudo timedatectl set-timezone Asia/Shanghai
```

可选但建议：

```bash
sudo apt -y install net-tools unzip
```

---

## 3. 安装 Docker + Compose（一次性）

```bash
sudo install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg | sudo gpg --dearmor -o /etc/apt/keyrings/docker.gpg
sudo chmod a+r /etc/apt/keyrings/docker.gpg

echo \
"deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu \
$(. /etc/os-release && echo $VERSION_CODENAME) stable" | \
sudo tee /etc/apt/sources.list.d/docker.list > /dev/null

sudo apt update
sudo apt -y install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
sudo systemctl enable --now docker
sudo usermod -aG docker $USER
```

> 执行完 `usermod` 后，请 **退出 SSH 重新登录**，否则当前会话可能仍需 `sudo docker ...`。

验证：

```bash
docker --version
docker compose version
docker run --rm hello-world
```

---

## 4. 拉代码（固定仓库）

```bash
mkdir -p ~/workspace
cd ~/workspace
git clone "http://192.168.14.10/digital-operation/secure_access_gateway_sag" sag-cloud
cd sag-cloud
git checkout clean-main
git pull
```

### 4.1 GitLab HTTP 鉴权失败（`Access denied` / 必须用 token）

GitLab 通常 **不允许用账户登录密码** 做 `git pull`，需要 **Personal Access Token（PAT）**：

1. 浏览器打开：`http://192.168.14.10/-/user_settings/personal_access_tokens`（或 GitLab 帮助里「个人访问令牌」入口）。
2. 新建令牌，勾选 **`read_repository`**（拉代码够用）。
3. 在服务器上再次 `git pull` 时：
   - **Username**：你的 GitLab 用户名（**不要**用 `root` 除非你的 Git 用户就叫 root）
   - **Password**：**粘贴 PAT**（不是登录密码）

可选：改用 SSH（本机已配公钥到 GitLab 时）：

```bash
cd ~/workspace/sag-cloud
git remote set-url origin git@192.168.14.10:digital-operation/secure_access_gateway_sag.git
git pull
```

### 4.2 暂时拉不下来代码时：手动修 `docker-compose.yml`（修 zentinel 静态 IP 报错）

若出现：

`user specified IP address is supported only when connecting to networks with user configured subnets`

说明当前 `docker-compose.yml` **末尾缺少** `networks` 段。在 **`volumes:` 整段之后**追加以下内容（注意 YAML 顶格，`networks` 与 `volumes` 同级）：

```yaml
# Static IPv4 for zentinel requires user-defined subnet
networks:
  default:
    driver: bridge
    ipam:
      config:
        - subnet: 172.19.0.0/16
          gateway: 172.19.0.1
```

然后重建网络并启动：

```bash
cd ~/workspace/sag-cloud
docker compose down
docker network rm sag-cloud_default 2>/dev/null || true
docker compose up -d
```

> 若文件里 **没有** `zentinel` 的 `ipv4_address: 172.19.0.250`（或与 `extra_hosts` 中 `example.com` 一致），则不要加这段；以你仓库实际 compose 为准。

---

## 5. 首次启动（单机主路径）

```bash
cd ~/workspace/sag-cloud
docker compose build
docker compose up -d
```

查看状态：

```bash
docker compose ps
docker compose logs --tail=120 zentinel
```

说明：

- `zentinel` 冷启动第一次可能编译耗时较长（正常）。
- 这属于编译耗时，不代表配置滞后或失效。

---

## 6. 核心验收（必须执行）

### 6.0 冒烟脚本在哪台机器上跑？

**在虚拟机（跑 Docker 的那台 Linux）上跑**，目录为仓库根：

```bash
cd ~/workspace/sag-cloud
bash ./scripts/smoke-dataplane-wsl.sh
```

脚本默认探测 **`127.0.0.1`** 上的各端口，与容器 `ports` 映射一致。若你要在 **Windows 本机** 对 Woo 内网机压测，请使用 `scripts/smoke-remote-windows.ps1`（会自动改成 **`192.168.9.26`** 各端口）。

把完整终端输出（从 `=== [M1]` 到 `SUMMARY`）复制给我即可定位。

> 可选：若你从办公机 SSH 到虚拟机，也是在 **SSH 会话里**执行上述命令，效果相同。

### 6.1 管理面健康

```bash
curl -sS http://127.0.0.1:8090/health
curl -sS http://127.0.0.1:8080/health
curl -sS http://127.0.0.1:8081/health
```

### 6.2 数据面探针（重点）

```bash
# T1: bridge 路径
curl -i http://127.0.0.1:9000/api/test

# N1: 经管理端代理到 zentinel 的北向探针
curl -i http://127.0.0.1:3001/api-zentinel/api/test
```

预期：**两条都是 200**。

#### N1 失败但 T1 / S1 正常（你当前这类结果）

- **含义**：经 **Zentinel `:10080`** 的北向入口还 **没起来**（`curl: (7)` / `HTTP 000` = 连接被拒绝或未监听），**不等于**隧道或 APISIX 坏了。
- **常见原因 1**：`sag-zentinel` 容器里 **`cargo run` 首次编译很慢**（十几分钟很常见），编译完成前 `:10080` 不会监听。
- **常见原因 2**：**未初始化 `proxy/core` 子模块**，`/workspace/proxy/core/Cargo.toml` 不存在，`zentinel` 进程直接退出。

**排查：**

```bash
docker compose ps zentinel
docker compose logs --tail=100 zentinel
test -f ~/workspace/sag-cloud/proxy/core/Cargo.toml && echo "core ok" || echo "need: git submodule update --init --recursive"
```

**冒烟脚本可等待 Zentinel（可选）：**

```bash
cd ~/workspace/sag-cloud
SMOKE_ZENTINEL_WAIT_SEC=600 bash ./scripts/smoke-dataplane-wsl.sh
```

在最多 **600 秒**内每 5 秒重试 N1，适合新机第一次编译。

### 6.3 从 Windows / 办公机访问 Woo 内网机（固定 `192.168.9.26`）

浏览器使用 **`http://192.168.9.26:端口`**，例如统一前端 **`http://192.168.9.26:3001`**。

| 服务 | 端口 |
|------|------|
| 统一前端入口（Next） | 3001 |
| sag-auth | 8080 |
| Fake 4A | 19080 |
| Grafana | 3000 |
| Prometheus | 9091 |

若 **3001 超时**：多半是 **`frontend-admin-next` 容器内未执行过 `npm install`**（旧版 compose 只写了 `npm run dev`）。请 **`git pull` 到已修复的 `docker-compose.yml`** 后执行：

```bash
docker compose up -d --force-recreate frontend-admin-next
docker compose logs -f frontend-admin-next
```

直至日志出现 `Ready`，且虚拟机本机 `curl -sS -o /dev/null -w "%{http_code}\n" http://127.0.0.1:3001/` 为 `200`。

可选：快速检查单域名关键页面可达（VM 上执行）：

```bash
bash ./scripts/check-single-domain-frontend.sh "http://127.0.0.1:3001" "http://127.0.0.1:8080"
```

**Fake 4A 打不开**：不要用本机书签里的 `127.0.0.1:19080`，应使用 **`http://192.168.9.26:19080`**。必要时放行防火墙：

```bash
sudo ufw allow 3001/tcp
sudo ufw allow 19080/tcp
sudo ufw allow 8080/tcp
```

### 6.3.2 从 Windows 直接对 VM 跑冒烟并看延迟

在 Windows PowerShell（仓库根目录）执行：

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
.\scripts\smoke-remote-windows.ps1 -VmHost 192.168.9.26 -Rounds 5
```

说明：

- 脚本会自动把 `smoke-dataplane.ps1` 的 `BRIDGE_URL` / `ZENTINEL_URL` / `SMOKE_*_BASE` 等都改成 **`192.168.9.26`**（与 `-VmHost` 一致）。
- 会连续跑多轮并给出汇总：`avg_ms / min_ms / max_ms`，用于观察跨机器延迟。
- 若你只看北向链路，可加环境变量跳过直连层：

```powershell
$env:SMOKE_SKIP_APISIX_DIRECT = "1"
$env:SMOKE_SKIP_MOCK_DIRECT = "1"
.\scripts\smoke-remote-windows.ps1 -VmHost 192.168.9.26 -Rounds 5
```

### 6.3.3 若日志出现 `Ignoring extra certs from ... ca.crt ... No such file or directory`

说明容器内 **`NODE_EXTRA_CA_CERTS` 指向的文件不存在**。仓库已在主目录提供 **`infra/tls/ca.crt`**（不依赖 `proxy/core` 子模块是否拉取）。请 `git pull` 后重建前端容器：

```bash
docker compose up -d --force-recreate frontend-admin-next
```

说明：当前默认已切到 **Next 生产模式**（`npm ci + build + start`），首启构建会更久，但页面交互延迟会明显低于 `npm run dev`。

完整编译 **Zentinel / Rust 代理链** 仍建议执行：`git submodule update --init --recursive`（若 `proxy/core` 为空）。

### 6.4 门户「账号登录」与数据库说明

- **不需要单独跑“用户表初始化脚本”**：`sag-auth` 启动时会建库表；若没有 `admin` 用户，会用环境变量里的密码 **自动创建**（compose 默认 `SAG_BOOTSTRAP_ADMIN_PASSWORD: Admin@123`）。
- 默认账号：**`admin` / `Admin@123`**（与本地一致，除非你改过 env）。
- 若仍登录失败，在虚拟机上查：

```bash
docker compose logs --tail=80 sag-auth
curl -sS -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"Admin@123"}'
```

返回里应有 `token` 字段。

**从办公机浏览器做 4A/SSO 演示时**：请在 **Woo 内网机**上设置 **`SAG_PUBLIC_HOST=192.168.9.26`**，compose 会自动生成正确外部跳转 URL（`3001/8080/19080`）。然后重建相关服务：

```bash
cd ~/workspace/sag-cloud
SAG_PUBLIC_HOST=192.168.9.26 docker compose up -d --force-recreate sag-auth fake-4a frontend-admin-next
```

仅影响 OAuth/门户外链跳转；**用户名密码登录**只要 `8080` 可访问，一般不受影响。

---

## 7. 若你要演示 4A 单点登录（可选）

```bash
curl -i http://127.0.0.1:8080/api/v1/auth/sso/login
```

预期返回 `307` 跳转到 fake-4a 授权页（或你配置的真实 4A）。

---

## 8. 新服务器证书预检（推荐上线前做）

如果你切换到正式证书，先做离线检查：

```bash
openssl x509 -in <server.crt> -noout -dates -subject -issuer -ext subjectAltName
openssl pkey -in <server.key> -pubout | sha256sum
openssl x509 -in <server.crt> -pubkey -noout | sha256sum
```

检查点：

- 证书未过期
- SAN 包含实际访问域名
- 后两条哈希一致（证书与私钥匹配）

---

## 9. 常见问题快速定位

### 9.1 N1=500，但 T1=200

优先排查 zentinel TLS/SNI/证书链：

- `ZENTINEL_PROXY_TARGET` 对应主机名是否在证书 SAN 里
- 前端是否正确注入 `NODE_EXTRA_CA_CERTS`
- zentinel 是否真正监听在 `:10080`

辅助命令：

```bash
docker compose logs --tail=200 frontend-admin-next zentinel
docker compose ps
```

### 9.2 返回 `connector tunnel is unhealthy`

通常是路由中的 `connector_endpoint` 与实际 `SAG_CONNECTOR_ID` 不一致。

### 9.3 zentinel 启动慢

首次编译耗时正常；若长期无进展，看：

```bash
docker compose logs -f zentinel
```

### 9.4 `invalid endpoint settings` / `user specified IP address is supported only when connecting to networks with user configured subnets`

**原因**：`zentinel` 在 `docker-compose.yml` 里配置了固定 `ipv4_address`（当前为 `172.19.0.250`），但默认网络未声明子网时 Docker 会拒绝创建容器。

**处理**（仓库已在 `docker-compose.yml` 末尾增加 `networks.default.ipam` 定义 `172.19.0.0/16`）：

1. `git pull` 更新到含该修复的版本；或手动在 `docker-compose.yml` 末尾对齐仓库里的 `networks:` 段。
2. 若之前已经创建过旧网络，先拆掉再拉起：

```bash
cd ~/workspace/sag-cloud
docker compose down
docker network rm sag-cloud_default 2>/dev/null || true
docker compose up -d
```

若 `docker network rm` 提示仍被占用，先 `docker compose down` 再执行；仍不行则 `docker ps -a` 看是否有残留容器占用该网络。

### 9.5 `proxy/core` 缺失 / `Cargo.toml does not exist` / 子模块克隆失败

Zentinel 依赖 **`proxy/core`**（上游仓库：`https://github.com/zentinelproxy/zentinel.git`）。目录里必须有 **`proxy/core/Cargo.toml`**，否则容器里 `cargo run` 会直接报错。

快速检查：

```bash
bash ./scripts/verify-proxy-core.sh
```

**首选（能直连 GitHub 时）**：先确保仓库已 `git pull` 含 `.gitmodules`，再：

```bash
cd ~/workspace/sag-cloud
git submodule sync --recursive
git submodule update --init --recursive
```

**若报错 `GnuTLS recv error` / `TLS connection was non-properly terminated` / 超时**  
说明服务器到 **GitHub 不稳定或被墙**。任选其一：

> **注意**：`gitclone.com` 镜像在部分网络会 **`502 Bad Gateway`**，导致 Cargo 拉取 **`pingora` 等 git 依赖失败**。仓库内 `zentinel` 容器 **不再**默认挂载该镜像重写；若方案 A 仍 502，请取消 `insteadOf` 改试直连或其它镜像。

#### 方案 A：Git 全局 URL 替换（镜像加速，按环境试，不保证永久可用）

```bash
git config --global url."https://gitclone.com/github.com/".insteadOf "https://github.com/"
cd ~/workspace/sag-cloud
rm -rf proxy/core
git submodule update --init --recursive
```

若仍失败，可取消该配置：

```bash
git config --global --unset url.https://gitclone.com/github.com/.insteadOf
```

#### 方案 B：在 Windows 开发机上打包，再拷到虚拟机（最稳）

在你 **本机已能完整克隆** 的 `sag-cloud` 目录（确保存在 `proxy\core\Cargo.toml`）执行 PowerShell：

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
tar.exe -czvf $env:TEMP\zentinel-proxy-core.tgz -C proxy core
scp $env:TEMP\zentinel-proxy-core.tgz root@192.168.9.26:/tmp/
```

在 **虚拟机** 上：

```bash
cd ~/workspace/sag-cloud
docker compose stop zentinel 2>/dev/null || true
rm -rf proxy/core
mkdir -p proxy
tar -xzf /tmp/zentinel-proxy-core.tgz -C proxy
test -f proxy/core/Cargo.toml && echo "proxy/core ok"
docker compose up -d zentinel
```

> 说明：手工解压的 `proxy/core` **可以不包含 `.git` 子目录**，只要源码树完整，`cargo` 即可编译 Zentinel。

### 9.6 `zentinel`：`Address already in use`（固定 IP 冲突）

说明 **`172.19.0.x` 已被其他容器占用**。仓库已将 zentinel 固定 IP **改为 `172.19.0.250`** 并同步 `example.com` 的 `extra_hosts`。请 `git pull` 后：

```bash
docker compose down
docker network rm sag-cloud_default 2>/dev/null || true
docker compose up -d
```

### 9.7 `3001` / `19080` 不通

- **3001**：见 **6.3**，确保已使用含 `npm install && npm run dev` 的 `frontend-admin-next` 配置，并查看 `docker compose ps frontend-admin-next` 是否为 `Up`。
- **19080**：确认用 **`http://192.168.9.26:19080`** 而不是 `127.0.0.1`；并检查 `docker compose ps fake-4a` 与 `ufw`。

---

## 10. 生产化建议（你后续再做）

1. 使用预编译镜像，减少冷启动编译时间。  
2. 证书与密钥采用固定挂载路径 + 轮换流程。  
3. 将 T1/N1 探针纳入巡检与发布验收。  
4. 双机部署时再切换到 `docker-compose.edge.yml` / `docker-compose.intra.yml`。

---

## 11. 你下一步只做这三件事

1. 执行第 2~5 步把服务拉起来。  
2. 执行第 6 步确认 T1/N1 都是 200。  
3. 记录当前可访问地址给业务/mentor（Woo 内网机 **`192.168.9.26`**）：
   - `http://192.168.9.26:3001`
   - `http://192.168.9.26:9091`
   - `http://192.168.9.26:3000`

