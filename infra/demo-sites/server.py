#!/usr/bin/env python3
"""Minimal multi-tenant demo pages (Chinese) for company portal links.

Serves:
  /, /dev/, /ci/, /finance/, /oa/, /hr/, /bi/, /vendor/

Optional: set Host to *.internal.com (e.g. via hosts file) — same paths work.
"""
from http.server import HTTPServer, BaseHTTPRequestHandler
import os
import sys
from typing import Dict, Optional, Tuple

HOST = os.environ.get("DEMO_SITES_ADDR", "0.0.0.0")
PORT = int(os.environ.get("DEMO_SITES_PORT", "28080"))

PAGES: Dict[str, Tuple[str, str, Optional[str]]] = {
    "/": ("SAG 演示内网", "选择左侧门户中的「图标跳转」入口，或访问各业务子路径。", None),
    "/dev/": ("研发门户（演示）", "欢迎，研发协同与代码门户演示页。", "app-dev"),
    "/ci/": ("持续集成（演示）", "构建/发布流水线演示页。", "app-ci"),
    "/finance/": ("财务系统（演示）", "预算与报销演示页。", "app-finance"),
    "/oa/": ("OA 办公（演示）", "审批流程演示页。", "app-oa"),
    "/hr/": ("人事系统（演示）", "组织与人员演示页。", "app-hr"),
    "/bi/": ("老板看板（演示）", "经营指标演示页。", "app-bi"),
    "/vendor/": ("外包交付（演示）", "外协协同演示页。", "app-vendor"),
}


def _html(title: str, body: str, app_hint: Optional[str]) -> bytes:
    hint = f'<p class="meta">关联 app_id：{app_hint}</p>' if app_hint else ""
    s = f"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{title}</title>
  <style>
    body {{ font-family: "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif; margin: 2rem; background: #f8fafc; color: #0f172a; }}
    .card {{ max-width: 42rem; margin: 0 auto; background: #fff; border: 1px solid #e2e8f0; border-radius: 12px; padding: 1.5rem; box-shadow: 0 1px 2px rgba(0,0,0,.05); }}
    h1 {{ margin-top: 0; font-size: 1.35rem; }}
    p {{ line-height: 1.6; color: #334155; }}
    .meta {{ font-size: 0.85rem; color: #64748b; }}
    a {{ color: #0f766e; }}
  </style>
</head>
<body>
  <div class="card">
    <h1>{title}</h1>
    <p>{body}</p>
    {hint}
    <p class="meta"><a href="/">返回首页</a></p>
  </div>
</body>
</html>"""
    return s.encode("utf-8")


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _path(self):
        p = self.path.split("?", 1)[0]
        if len(p) > 1 and p.endswith("/"):
            return p
        if p != "/" and not p.endswith("/"):
            return p + "/"
        return p

    def do_GET(self):
        p = self._path()
        if p not in PAGES:
            self.send_error(404, "Not Found")
            return
        title, body, app = PAGES[p]
        raw = _html(title, body, app)
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


if __name__ == "__main__":
    srv = HTTPServer((HOST, PORT), H)
    print("company-demo-sites", flush=True)
    print(f"  http://127.0.0.1:{PORT}/", flush=True)
    print(f"  paths: {', '.join(sorted(PAGES))}", flush=True)
    srv.serve_forever()
