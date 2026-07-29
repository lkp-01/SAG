#!/usr/bin/env python3
import json
import os
import secrets
import sys
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


CODES = {}
TOKENS = {}
AUDIT_LOGS = []
TOKEN_TTL_SEC = int(os.getenv("FAKE_4A_TOKEN_TTL_SEC", "3600"))
CLIENT_ID = os.getenv("FAKE_4A_CLIENT_ID", "sag-local-client")
CLIENT_SECRET = os.getenv("FAKE_4A_CLIENT_SECRET", "sag-local-secret")
USERS_FILE = os.getenv("FAKE_4A_USERS_FILE", "/workspace/infra/fake-4a/users.json")
AUDIT_LIMIT = int(os.getenv("FAKE_4A_AUDIT_LIMIT", "200"))
PORTAL_URL = os.getenv("FAKE_4A_PORTAL_URL", "http://127.0.0.1:5174")
def _default_login_url() -> str:
    pu = PORTAL_URL.rstrip("/")
    if pu.endswith("/app"):
        return f"{pu[:-4]}/login"
    return f"{pu}/login"


LOGIN_URL = os.getenv("FAKE_4A_LOGIN_URL", "").strip() or _default_login_url()


def load_users():
    default_users = {
        "alice": {"employeeNumber": "alice", "name": "Alice 技术", "dept": "研发"},
        "bob": {"employeeNumber": "bob", "name": "Bob 运维", "dept": "运维"},
        "boss": {"employeeNumber": "boss", "name": "Boss 总经理", "dept": "管理层"},
    }
    try:
        with open(USERS_FILE, "r", encoding="utf-8") as f:
            rows = json.load(f)
        users = {}
        for row in rows:
            username = str(row.get("username", "")).strip()
            emp = str(row.get("employeeNumber", "")).strip()
            if not username or not emp:
                continue
            users[username] = {
                "employeeNumber": emp,
                "name": str(row.get("name", username)),
                "dept": str(row.get("dept", "未知")),
            }
        return users or default_users
    except Exception:
        return default_users


USERS = load_users()


def now_sec() -> int:
    return int(time.time())


def prune():
    now = now_sec()
    for code, row in list(CODES.items()):
        if row["exp"] <= now:
            del CODES[code]
    for token, row in list(TOKENS.items()):
        if row["exp"] <= now:
            del TOKENS[token]


def audit(event: str, **detail):
    AUDIT_LOGS.append({"ts": now_sec(), "event": event, "detail": detail})
    if len(AUDIT_LOGS) > AUDIT_LIMIT:
        del AUDIT_LOGS[: len(AUDIT_LOGS) - AUDIT_LIMIT]


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _path(self):
        return self.path.split("?", 1)[0]

    def _query(self):
        raw = self.path.split("?", 1)[1] if "?" in self.path else ""
        return urllib.parse.parse_qs(raw, keep_blank_values=True)

    def _json(self, code, body):
        b = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def _html(self, code, body):
        b = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        prune()
        p = self._path()
        q = self._query()

        if p in ("/", "/health"):
            return self._json(
                200,
                {
                    "service": "fake-4a",
                    "status": "ok",
                    "endpoints": [
                        "/oauth/authorize",
                        "/oauth/token (POST)",
                        "/oauth/userinfo",
                        "/debug/audit",
                    ],
                },
            )

        if p == "/debug/audit":
            limit_raw = (q.get("limit") or ["50"])[0]
            try:
                limit = max(1, min(500, int(limit_raw)))
            except Exception:
                limit = 50
            return self._json(
                200,
                {
                    "count": len(AUDIT_LOGS[-limit:]),
                    "items": AUDIT_LOGS[-limit:],
                },
            )

        if p == "/oauth/authorize":
            client_id = (q.get("client_id") or [""])[0]
            redirect_uri = (q.get("redirect_uri") or [""])[0]
            state = (q.get("state") or [""])[0]
            scope = (q.get("scope") or [""])[0]
            if not client_id or not redirect_uri:
                audit("authorize_error", reason="missing_params", client_id=client_id)
                return self._json(400, {"errcode": 400, "msg": "missing client_id/redirect_uri"})
            if client_id != CLIENT_ID:
                audit("authorize_error", reason="invalid_client_id", client_id=client_id)
                return self._json(401, {"errcode": 401, "msg": "invalid client_id"})

            links = []
            for username, u in USERS.items():
                code = secrets.token_urlsafe(24)
                CODES[code] = {"user": username, "scope": scope, "exp": now_sec() + 300}
                params = {"code": code, "state": state}
                jump = redirect_uri + ("&" if "?" in redirect_uri else "?") + urllib.parse.urlencode(params)
                links.append(
                    f'<li><a href="{jump}">以 {u["name"]} ({u["employeeNumber"]}) 登录</a></li>'
                )
            guest_jump = f"{LOGIN_URL.rstrip('/')}/?sso_guest=1"
            links.append(
                f'<li><a href="{guest_jump}">以 未认证访客（预期被拦截） 访问</a></li>'
            )
            audit("authorize_ok", client_id=client_id, redirect_uri=redirect_uri, scope=scope, state=state)

            html = (
                "<html><head><meta charset='utf-8'><title>Fake 4A Login</title></head>"
                "<body><h2>Fake 4A 登录页（联调用）</h2>"
                "<p>请选择一个测试账号完成授权码跳转：</p>"
                f"<ul>{''.join(links)}</ul>"
                "</body></html>"
            )
            return self._html(200, html)

        if p == "/oauth/userinfo":
            access_token = (q.get("access_token") or [""])[0]
            client_id = (q.get("client_id") or [""])[0]
            if client_id != CLIENT_ID:
                audit("userinfo_error", reason="invalid_client_id", client_id=client_id)
                return self._json(401, {"errcode": 401, "msg": "invalid client_id"})
            row = TOKENS.get(access_token)
            if not row:
                audit("userinfo_error", reason="invalid_token")
                return self._json(401, {"errcode": 401, "msg": "invalid or expired token"})
            user = USERS[row["user"]]
            audit("userinfo_ok", employeeNumber=user["employeeNumber"], scope=row.get("scope", ""))
            return self._json(
                200,
                {
                    "errcode": 0,
                    "msg": "ok",
                    "employeeNumber": user["employeeNumber"],
                    "name": user["name"],
                    "dept": user["dept"],
                    "scope": row.get("scope", ""),
                },
            )

        return self._json(404, {"errcode": 404, "msg": "not found"})

    def do_POST(self):
        prune()
        p = self._path()
        if p != "/oauth/token":
            return self._json(404, {"errcode": 404, "msg": "not found"})

        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n).decode("utf-8")
        form = urllib.parse.parse_qs(raw, keep_blank_values=True)
        client_id = (form.get("client_id") or [""])[0]
        client_secret = (form.get("client_secret") or [""])[0]
        code = (form.get("code") or [""])[0]
        grant_type = (form.get("grant_type") or [""])[0]
        if grant_type != "authorization_code":
            audit("token_error", reason="unsupported_grant_type", grant_type=grant_type)
            return self._json(
                400,
                {"errcode": 400, "msg": "unsupported grant_type", "error": "unsupported_grant_type"},
            )
        if client_id != CLIENT_ID or client_secret != CLIENT_SECRET:
            audit("token_error", reason="invalid_client_credentials", client_id=client_id)
            return self._json(
                401,
                {"errcode": 401, "msg": "invalid client credentials", "error": "invalid_client"},
            )
        row = CODES.pop(code, None)
        if not row:
            audit("token_error", reason="invalid_code")
            return self._json(400, {"errcode": 400, "msg": "invalid or expired code", "error": "invalid_grant"})

        token = secrets.token_urlsafe(32)
        TOKENS[token] = {"user": row["user"], "scope": row.get("scope", ""), "exp": now_sec() + TOKEN_TTL_SEC}
        audit("token_ok", employeeNumber=USERS[row["user"]]["employeeNumber"], scope=row.get("scope", ""))
        return self._json(
            200,
            {
                "errcode": 0,
                "msg": "ok",
                "access_token": token,
                "token_type": "bearer",
                "expires_in": TOKEN_TTL_SEC,
                "scope": row.get("scope", ""),
            },
        )


def main():
    host = os.getenv("FAKE_4A_HOST", "0.0.0.0")
    port = int(os.getenv("FAKE_4A_PORT", "19080"))
    httpd = ThreadingHTTPServer((host, port), H)
    print(f"fake-4a listening on http://{host}:{port}", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
