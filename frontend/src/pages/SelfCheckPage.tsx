import { useState } from "react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { AUTH_BASE, BRIDGE_BASE, CONTROL_BASE, POLICY_BASE, ZENTINEL_BASE, dataPlaneProbe, health } from "@/lib/api";
import { controlApi } from "@/lib/api";

type CheckRow = { id: string; ok: boolean | null; title: string; detail: string };

async function policyEvaluate(userId: string, roles: string[], appId: string, path: string) {
  const res = await fetch(`${POLICY_BASE}/api/v1/policy/evaluate`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ user_id: userId, roles, app_id: appId, path, method: "GET" }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}: ${text}`);
  return JSON.parse(text) as { decision: string; reason?: string; matched_policy_id?: string };
}

export function SelfCheckPage() {
  const [rows, setRows] = useState<CheckRow[]>([]);
  const [running, setRunning] = useState(false);

  const run = async () => {
    setRunning(true);
    const out: CheckRow[] = [];

    const push = (r: CheckRow) => {
      out.push(r);
      setRows([...out]);
    };

    // 1) Management health
    for (const [id, url] of [
      ["control-plane-admin", `${CONTROL_BASE}/health`],
      ["sag-auth", `${AUTH_BASE}/health`],
      ["sag-policy", `${POLICY_BASE}/health`],
    ] as const) {
      try {
        const v = await health(url);
        push({ id, ok: v.trim() === "ok", title: `${id} /health`, detail: v.trim() });
      } catch (e) {
        push({
          id,
          ok: false,
          title: `${id} /health`,
          detail: `ERR: ${String(e)}（建议：确认服务已启动，或检查前端代理 target）`,
        });
      }
    }

    // 2) Policy sanity (RBAC)
    try {
      const techToFinance = await policyEvaluate("u-tech-check", ["tech"], "app-finance", "/api/test");
      const ok = techToFinance.decision?.toUpperCase?.() === "DENY";
      push({
        id: "policy-tech-finance",
        ok,
        title: "策略校验：tech -> finance 应拒绝",
        detail: ok
          ? `DENY（OK）`
          : `实际=${techToFinance.decision}（建议：重新执行 seed-company-demo.ps1 或导入 company_demo_postgres.sql）`,
      });
    } catch (e) {
      push({
        id: "policy-tech-finance",
        ok: false,
        title: "策略校验：tech -> finance",
        detail: `ERR: ${String(e)}（建议：检查 sag-policy 是否可达；或 JWT/代理配置）`,
      });
    }

    // 3) Routes sync visibility (requires admin/boss token)
    try {
      const r = await controlApi.listRoutes();
      push({
        id: "routes-list",
        ok: r.length > 0,
        title: "路由列表（需要 admin/boss token）",
        detail: r.length > 0 ? `OK: ${r.length} 条` : "空（建议：先 seed 路由，或确认 control-plane-admin 存储已初始化）",
      });
    } catch (e) {
      push({
        id: "routes-list",
        ok: null,
        title: "路由列表（需要 admin/boss token）",
        detail: `跳过/失败：${String(e)}（建议：先在“登录会话”页用 admin/boss 登录，再重试）`,
      });
    }

    // 4) North/South probe
    try {
      const n = await dataPlaneProbe(ZENTINEL_BASE, "/api/test", "app-001");
      push({
        id: "dataplane-north",
        ok: n.status >= 200 && n.status < 300,
        title: "北向链路（Zentinel -> 隧道 -> APISIX -> upstream）",
        detail: `HTTP ${n.status} body=${n.body.slice(0, 120)}`,
      });
    } catch (e) {
      push({
        id: "dataplane-north",
        ok: false,
        title: "北向链路（Zentinel）",
        detail: `ERR: ${String(e)}（建议：检查 zentinel TLS/证书与 bridge 地址；Windows curl schannel 可用 WSL fallback）`,
      });
    }

    try {
      const t = await dataPlaneProbe(BRIDGE_BASE, "/api/test", "app-001");
      push({
        id: "dataplane-bridge",
        ok: t.status >= 200 && t.status < 300,
        title: "隧道桥（bridge -> agent -> connector -> APISIX）",
        detail: `HTTP ${t.status} body=${t.body.slice(0, 120)}`,
      });
    } catch (e) {
      push({
        id: "dataplane-bridge",
        ok: false,
        title: "隧道桥（bridge）",
        detail: `ERR: ${String(e)}（建议：检查 SAG_TUNNEL_GRPC_ENDPOINT、agent/connector 是否在线）`,
      });
    }

    setRunning(false);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>一键体检</CardTitle>
        <CardDescription>自动检查：管理面、策略、路由、北向与隧道链路，并输出可操作建议。</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <Button disabled={running} onClick={run}>
          {running ? "体检中…" : "开始体检"}
        </Button>

        <div className="space-y-2">
          {rows.map((r) => (
            <div key={r.id} className="flex items-start justify-between gap-4 rounded-md border p-3">
              <div className="space-y-1">
                <div className="font-medium">{r.title}</div>
                <div className="text-sm text-slate-600">{r.detail}</div>
              </div>
              <Badge variant={r.ok === true ? "default" : r.ok === false ? "destructive" : "outline"}>
                {r.ok === true ? "OK" : r.ok === false ? "FAIL" : "SKIP"}
              </Badge>
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

