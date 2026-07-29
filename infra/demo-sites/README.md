# 演示业务小站（company-demo-sites）

用于门户「图标跳转」的可部署静态演示页，避免 `*.internal.com` 无解析时出现 404。

- 默认：`http://127.0.0.1:28080/dev/`、`/finance/` 等
- Compose：`docker compose up -d company-demo-sites`
- 可选：在 hosts 增加 `127.0.0.1 dev.internal.com` 等，仅占位；页面仍推荐用上述端口路径访问
