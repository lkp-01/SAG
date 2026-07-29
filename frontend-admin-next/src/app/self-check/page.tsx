"use client";

import { useEffect, useState } from "react";
import { TopBar } from "@/components/app-shell/TopBar";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { controlApi, dataPlaneProbe, health } from "@/lib/api";

function debugLog(location: string, message: string, data: Record<string, unknown>, hypothesisId: string, runId = "initial") {
  // #region agent log
  fetch("http://127.0.0.1:7701/ingest/1ccb5b12-5073-4437-a0e2-a9913a1fb79d", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "ac5396" },
    body: JSON.stringify({ sessionId: "ac5396", runId, hypothesisId, location, message, data, timestamp: Date.now() })
  }).catch(() => {});
  // #endregion
}

export default function SelfCheckPage() {
  const [status, setStatus] = useState<Record<string, string>>({});
  const [northResult, setNorthResult] = useState("");
  const [bridgeResult, setBridgeResult] = useState("");
  const [err, setErr] = useState("");
  const [probeAppId, setProbeAppId] = useState("app-001");

  async function reloadHealth() {
    const pairs: [string, string][] = [
      ["control-plane-admin", "/api-control/health"],
      ["sag-auth", "/api-auth/health"],
      ["sag-policy", "/api-policy/health"],
      // Workflow depends on Prometheus query path availability, not app tree auth.
      ["workflow", "/api-prom/-/ready"]
    ];
    const out: Record<string, string> = {};
    for (const [name, url] of pairs) {
      try {
        const result = await health(url);
        out[name] = result.slice(0, 64);
        // #region agent log
        debugLog("self-check/page.tsx:reloadHealth", "health ok", { name, url }, "H16");
        // #endregion
      } catch (e) {
        const msg = (e as Error).message;
        out[name] = `ERR: ${msg}`;
        // #region agent log
        debugLog("self-check/page.tsx:reloadHealth", "health failed", { name, url, error: msg }, "H16");
        // #endregion
      }
    }
    setStatus(out);
  }

  async function runSmoke() {
    try {
      let appId = probeAppId;
      try {
        const routes = await controlApi.listRoutes();
        if (routes.length > 0 && routes[0].app_id) {
          appId = routes[0].app_id;
          if (appId !== probeAppId) setProbeAppId(appId);
        }
      } catch {
        // Keep fallback app id for smoke probe.
      }
      const candidatePaths = buildProbePaths(appId);
      const n = await probeWithFallback("/api-zentinel", candidatePaths, appId);
      setBridgeResult("bridge 探测中…");
      const t = await probeWithFallback("/api-bridge", candidatePaths, appId, () => {
        setBridgeResult("bridge：请求已排队，正在轮询上游结果…");
      });
      setNorthResult(`N1 zentinel path=${n.path} status=${n.status} body=${n.body.slice(0, 160)}`);
      setBridgeResult(`T1 bridge path=${t.path} status=${t.status} body=${t.body.slice(0, 160)}`);
      setErr("");
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => {
    void reloadHealth();
  }, []);

  return (
    <>
      <TopBar title="一键体检" />
      <div className="flex-1 p-4 md:p-6">
        {err ? (
          <Alert variant="destructive" className="mb-4">
            <AlertTitle>体检失败</AlertTitle>
            <AlertDescription>{err}</AlertDescription>
          </Alert>
        ) : null}
        <Card>
          <CardHeader>
            <CardTitle>体检向导</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm text-muted-foreground">
            <div className="flex flex-wrap gap-2">
              {Object.entries(status).map(([k, v]) => (
                <span key={k} className="rounded-md border px-2 py-1 text-xs">
                  {k}: {v}
                </span>
              ))}
            </div>
            <div className="flex gap-2">
              <Button variant="outline" onClick={() => void reloadHealth()}>
                刷新健康状态
              </Button>
              <Button onClick={() => void runSmoke()}>一键冒烟测试</Button>
            </div>
            <div>当前探测 app_id: {probeAppId}</div>
            {northResult ? <div>{northResult}</div> : null}
            {bridgeResult ? <div>{bridgeResult}</div> : null}
          </CardContent>
        </Card>
      </div>
    </>
  );
}

function buildProbePaths(appId: string): string[] {
  const defaultPath = normalizeProbePath(
    process.env.NEXT_PUBLIC_SMOKE_PROBE_PATH ??
      process.env.NEXT_PUBLIC_WORKFLOW_PROBE_PATH ??
      process.env.NEXT_PUBLIC_PATH_REQ ??
      "/dev/"
  );
  const preferredPathByAppId: Record<string, string> = {
    "app-001": "/dev/",
    "app-dev": "/dev/",
    "app-ci": "/ci/",
    "app-finance": "/finance/",
    "app-oa": "/oa/",
    "app-hr": "/hr/",
    "app-bi": "/bi/",
    "app-vendor": "/vendor/"
  };
  const preferred = preferredPathByAppId[appId];
  if (preferred) return uniquePaths([preferred, defaultPath, "/api/test"]);
  return uniquePaths([defaultPath, "/api/test"]);
}

function normalizeProbePath(input: string): string {
  if (!input) return "/dev/";
  return input.startsWith("/") ? input : `/${input}`;
}

function uniquePaths(paths: string[]): string[] {
  return Array.from(new Set(paths));
}

async function probeWithFallback(
  baseUrl: string,
  paths: string[],
  appId: string,
  onQueued?: () => void
) {
  let last = { path: paths[0], status: 0, body: "no probe attempted" };
  for (const path of paths) {
    const res = await dataPlaneProbe(baseUrl, path, appId, { onQueued });
    last = { path, status: res.status, body: res.body };
    // Success and policy-deny are both informative terminal results.
    if ((res.status >= 200 && res.status < 300) || res.status === 403) {
      return last;
    }
  }
  return last;
}

