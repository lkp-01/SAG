#!/usr/bin/env python3
"""Minimal HTTP mock for APISIX / connector upstream tests (default port 18080)."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from collections import OrderedDict
import json
import os
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit

_stats_lock = threading.Lock()


def _listen_target():
    host = os.environ.get("MOCK_LISTEN_ADDR", "0.0.0.0")
    port = int(os.environ.get("MOCK_LISTEN_PORT", "18080"))
    return host, port


START_TS = time.time()
REQ_TOTAL = 0
REQ_BY_PATH = {}
MUTATION_DISPATCH_BY_KEY = OrderedDict()
MUTATION_EVIDENCE_CAPACITY = max(1, int(os.environ.get("MOCK_MUTATION_EVIDENCE_CAPACITY", "100000")))
LATENCY_BUCKETS = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]
LATENCY_BUCKET_COUNTS = {bucket: 0 for bucket in LATENCY_BUCKETS}
LATENCY_COUNT = 0
LATENCY_SUM = 0.0


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _path(self):
        return self.path.split("?", 1)[0]

    def _correlation(self, body=None):
        query = parse_qs(urlsplit(self.path).query)
        value = query.get("sag_correlation", [""])[0]
        if not value and isinstance(body, dict):
            value = str(body.get("correlation", ""))
        return value

    def _evidence(self, body=None):
        roles = [x.strip() for x in self.headers.get("x-sag-user-roles", "").split(",") if x.strip()]
        return {
            "service": "sag-test-workload",
            "correlation": self._correlation(body),
            "user_id": self.headers.get("x-sag-user-id", ""),
            "roles": roles,
            "path": self.path,
        }

    def _json(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def _record(self, path):
        global REQ_TOTAL, REQ_BY_PATH
        with _stats_lock:
            REQ_TOTAL += 1
            REQ_BY_PATH[path] = REQ_BY_PATH.get(path, 0) + 1

    def _observe_latency(self, duration_seconds):
        global LATENCY_COUNT, LATENCY_SUM
        with _stats_lock:
            LATENCY_COUNT += 1
            LATENCY_SUM += duration_seconds
            for bucket in LATENCY_BUCKETS:
                if duration_seconds <= bucket:
                    LATENCY_BUCKET_COUNTS[bucket] += 1

    def do_GET(self):
        started = time.perf_counter()
        p = self._path()
        self._record(p)
        if p == "/metrics":
            with _stats_lock:
                req_total = REQ_TOTAL
                req_by_path = dict(REQ_BY_PATH)
                latency_bucket_counts = {b: LATENCY_BUCKET_COUNTS[b] for b in LATENCY_BUCKETS}
                latency_count = LATENCY_COUNT
                latency_sum = LATENCY_SUM
            body_lines = [
                "# HELP mock_requests_total Total HTTP requests served by mock-workload",
                "# TYPE mock_requests_total counter",
                f'mock_requests_total{{service="mock-workload"}} {req_total}',
                "# HELP mock_requests_by_path_total Total HTTP requests by path",
                "# TYPE mock_requests_by_path_total counter",
            ]
            for k, v in sorted(req_by_path.items(), key=lambda kv: kv[0]):
                safe = k.replace("\\\\", "\\\\\\\\").replace('"', '\\"')
                body_lines.append(f'mock_requests_by_path_total{{path="{safe}"}} {v}')
            body_lines += [
                "# HELP mock_uptime_seconds Uptime in seconds",
                "# TYPE mock_uptime_seconds gauge",
                f'mock_uptime_seconds {time.time() - START_TS}',
                "# HELP mock_request_duration_seconds Mock workload request duration",
                "# TYPE mock_request_duration_seconds histogram",
                "",
            ]
            for bucket in LATENCY_BUCKETS:
                body_lines.insert(
                    -1,
                    f'mock_request_duration_seconds_bucket{{service="mock-workload",le="{bucket}"}} {latency_bucket_counts[bucket]}'
                )
            body_lines.insert(
                -1,
                f'mock_request_duration_seconds_bucket{{service="mock-workload",le="+Inf"}} {latency_count}'
            )
            body_lines.insert(-1, f'mock_request_duration_seconds_sum{{service="mock-workload"}} {latency_sum}')
            body_lines.insert(-1, f'mock_request_duration_seconds_count{{service="mock-workload"}} {latency_count}')
            raw = ("\n".join(body_lines)).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            self._observe_latency(time.perf_counter() - started)
            return
        if p in ("/", ""):
            self._json(
                200,
                {
                    "service": "sag-test-workload",
                    "status": "ok",
                    "listen": {"host": self.server.server_address[0], "port": self.server.server_address[1]},
                    "endpoints": [
                        "/health",
                        "/metrics",
                        "/dev/",
                        "/ci/",
                        "/finance/",
                        "/oa/",
                        "/hr/",
                        "/bi/",
                        "/vendor/",
                        "/api/test",
                        "/api/whoami",
                        "/api/echo",
                        "POST /api/body",
                    ],
                },
            )
            self._observe_latency(time.perf_counter() - started)
            return
        if p == "/health":
            self._json(
                200,
                {
                    "status": "healthy",
                    "service": "sag-test-workload",
                },
            )
            self._observe_latency(time.perf_counter() - started)
            return
        # Portal tiles (app-001 + path) and smoke /dev/ — same upstream, path names only for UX.
        _portal_prefixes = (
            "/dev/",
            "/ci/",
            "/finance/",
            "/oa/",
            "/hr/",
            "/bi/",
            "/vendor/",
        )
        # Next rewrites /api-zentinel/dev → upstream /dev (no slash); browser uses /dev/.
        _portal_roots = {x.rstrip("/") for x in _portal_prefixes}
        if p in _portal_roots:
            self._json(200, {**self._evidence(), "status": "ok"})
            self._observe_latency(time.perf_counter() - started)
            return
        for prefix in _portal_prefixes:
            if p == prefix or p.startswith(prefix):
                self._json(200, {**self._evidence(), "status": "ok"})
                self._observe_latency(time.perf_counter() - started)
                return
        # APISIX proxy-rewrite maps /api/test → /test/ for company-demo layout; mock accepts both.
        if p == "/test/" or p.startswith("/test/"):
            self._json(200, {"ok": True, "service": "sag-test-workload", "path": self.path})
            self._observe_latency(time.perf_counter() - started)
            return
        if p.startswith("/api/whoami"):
            self._json(200, self._evidence())
            self._observe_latency(time.perf_counter() - started)
            return
        if p.startswith("/api/echo"):
            self._json(200, {**self._evidence(), "echo": "ok"})
            self._observe_latency(time.perf_counter() - started)
            return
        if p.startswith("/api/test"):
            self._json(200, {"ok": True, "service": "sag-test-workload", "path": self.path})
            self._observe_latency(time.perf_counter() - started)
            return
        self.send_error(404)
        self._observe_latency(time.perf_counter() - started)

    def do_POST(self):
        started = time.perf_counter()
        n = int(self.headers.get("Content-Length", "0") or 0)
        raw = self.rfile.read(n) if n else b""
        p = self._path()
        self._record(p)
        body = {}
        if raw:
            try:
                body = json.loads(raw.decode())
            except (UnicodeDecodeError, json.JSONDecodeError):
                body = {}
        portal_prefixes = ("/dev/", "/ci/", "/finance/", "/oa/", "/hr/", "/bi/", "/vendor/")
        if p.startswith("/api/body") or any(p == x.rstrip("/") or p.startswith(x) for x in portal_prefixes):
            key = self.headers.get("Idempotency-Key", "")
            with _stats_lock:
                MUTATION_DISPATCH_BY_KEY[key] = MUTATION_DISPATCH_BY_KEY.get(key, 0) + 1
                MUTATION_DISPATCH_BY_KEY.move_to_end(key)
                while len(MUTATION_DISPATCH_BY_KEY) > MUTATION_EVIDENCE_CAPACITY:
                    MUTATION_DISPATCH_BY_KEY.popitem(last=False)
                side_effect_count = MUTATION_DISPATCH_BY_KEY[key]
            self._json(
                200,
                {
                    **self._evidence(body),
                    "received": len(raw),
                    "side_effect_count": side_effect_count,
                },
            )
            self._observe_latency(time.perf_counter() - started)
            return
        self.send_error(404)
        self._observe_latency(time.perf_counter() - started)


if __name__ == "__main__":
    host, port = _listen_target()
    # ThreadingHTTPServer: k6 高并发时单线程 HTTPServer 会在上游排队/超时表现为 5xx。
    server = ThreadingHTTPServer((host, port), H)
    server.daemon_threads = True
    bound_host, bound_port = server.server_address[:2]
    local_hint = "127.0.0.1" if str(bound_host) == "0.0.0.0" else str(bound_host)
    print("sag-test-workload mock HTTP", flush=True)
    print(f"  listening:  {bound_host}:{bound_port} (connect via http://{local_hint}:{bound_port}/)", flush=True)
    print(f"  health:     http://{local_hint}:{bound_port}/health", flush=True)
    print(f"  smoke path: http://{local_hint}:{bound_port}/api/test", flush=True)
    print("  Ctrl+C to stop", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nmock stopped.", flush=True)
