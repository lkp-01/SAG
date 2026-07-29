"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { useAuth } from "@/components/auth/AuthProvider";

type ServiceItem = {
  id: string;
  appId: string;
  name: string;
  category: string;
  apiPath: string;
  desc: string;
};

// Dual-host bootstrap only seeds tunnel_routes + intranet_upstream for app-001. All tiles share that
// tunnel identity; paths (/dev/, /oa/, …) distinguish mock responses. Multi-app isolation needs seed-company-demo.
const services: ServiceItem[] = [
  { id: "dev", appId: "app-001", name: "研发门户", category: "研发", apiPath: "/dev/", desc: "代码与研发协作入口" },
  { id: "ci", appId: "app-001", name: "持续集成", category: "研发", apiPath: "/ci/", desc: "构建与发布流水线" },
  { id: "finance", appId: "app-001", name: "财务系统", category: "财务", apiPath: "/finance/", desc: "预算与报销管理" },
  { id: "oa", appId: "app-001", name: "OA办公", category: "办公", apiPath: "/oa/", desc: "日常审批和流程" },
  { id: "hr", appId: "app-001", name: "人事系统", category: "人事", apiPath: "/hr/", desc: "人员与组织信息" },
  { id: "bi", appId: "app-001", name: "老板看板", category: "管理", apiPath: "/bi/", desc: "关键经营指标看板" },
  { id: "vendor", appId: "app-001", name: "外包交付", category: "外协", apiPath: "/vendor/", desc: "外包协同与交付" }
];

type PolicyLabel = "allow" | "deny" | "unknown";

async function api<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  const text = await res.text();
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ""}`);
  return text ? (JSON.parse(text) as T) : (undefined as T);
}

export default function PortalPage() {
  const { user, token, isOps } = useAuth();
  const [policyMap, setPolicyMap] = useState<Record<string, PolicyLabel>>({});
  const [loadingPolicy, setLoadingPolicy] = useState(true);
  const [keyword, setKeyword] = useState("");
  const [message, setMessage] = useState("请选择服务进行探测。");

  const filtered = useMemo(() => {
    const k = keyword.trim().toLowerCase();
    if (!k) return services;
    return services.filter((s) => `${s.name} ${s.category} ${s.desc}`.toLowerCase().includes(k));
  }, [keyword]);

  useEffect(() => {
    if (!user) return;
    let cancelled = false;
    (async () => {
      setLoadingPolicy(true);
      // Parallel evaluate policies to reduce interactive latency.
      const pairs = await Promise.all(
        services.map(async (s) => {
          try {
            const r = await api<{ decision: string }>("/api-policy/api/v1/policy/evaluate", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                user_id: user.id,
                roles: user.roles,
                app_id: s.appId,
                path: s.apiPath,
                method: "GET"
              })
            });
            return [s.id, r.decision.toUpperCase() === "ALLOW" ? "allow" : "deny"] as const;
          } catch {
            return [s.id, "unknown"] as const;
          }
        })
      );
      if (cancelled) return;
      setPolicyMap(Object.fromEntries(pairs));
      setLoadingPolicy(false);
    })();
    return () => {
      cancelled = true;
    };
  }, [user]);

  async function probe(item: ServiceItem) {
    if (!user || !token) return;
    if (policyMap[item.id] === "deny") {
      setMessage(`[${item.name}] 策略拒绝。`);
      return;
    }
    const res = await fetch(`/api-zentinel${item.apiPath}`, {
      headers: {
        Authorization: `Bearer ${token}`,
        "x-sag-app-id": item.appId,
        "x-sag-user-id": user.id,
        "x-sag-user-roles": user.roles.join(",")
      }
    });
    const body = await res.text();
    setMessage(`[${item.name}] 响应 ${res.status}: ${body.slice(0, 220)}`);
  }

  async function enterPage(item: ServiceItem) {
    if (!user || !token) return;
    if (policyMap[item.id] === "deny") {
      setMessage(`[${item.name}] 策略拒绝。`);
      return;
    }
    // Open the window first to avoid popup blockers and show progress.
    const tab = window.open("about:blank", "_blank");
    if (!tab) {
      setMessage(`[${item.name}] 弹窗被拦截：请允许弹窗后重试（否则无法全链路打开新窗口）。`);
      return;
    }
    tab.document.open();
    tab.document.write(`<pre>正在通过全链路进入：${item.name} ...</pre>`);
    tab.document.close();

    try {
      const res = await fetch(`/api-zentinel${item.apiPath}`, {
        headers: {
          Authorization: `Bearer ${token}`,
          "x-sag-app-id": item.appId,
          "x-sag-user-id": user.id,
          "x-sag-user-roles": user.roles.join(","),
          Accept: "text/html,application/json,text/plain,*/*"
        }
      });
      const body = await res.text();
      const contentType = res.headers.get("content-type") || "";
      const escaped = body.replace(/[<>&]/g, (ch) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[ch] as string));
      const headerLine = `status=${res.status} content-type=${contentType || "-"}`;
      tab.document.open();
      if (contentType.includes("text/html")) {
        tab.document.write(body);
      } else {
        tab.document.write(`<pre>${headerLine}\n\n${escaped}</pre>`);
      }
      tab.document.close();
      setMessage(`[${item.name}] 已发起全链路请求，状态 ${res.status}。`);
    } catch (e) {
      tab.document.open();
      tab.document.write(`<pre>全链路请求失败：${String(e).replace(/[<>&]/g, (ch) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[ch] as string))}</pre>`);
      tab.document.close();
      setMessage(`[${item.name}] 全链路请求失败：${String(e)}`);
    }
  }

  return (
    <RoleGate need="portal">
      <TopBar title="用户门户" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        <Card className="glass-card">
          <CardHeader>
            <CardTitle>欢迎，{user?.display_name ?? user?.username ?? "用户"}</CardTitle>
            <CardDescription>统一入口按角色分流；策略判定与网关探测均走同域 API。</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-wrap items-center gap-2">
            <Input value={keyword} onChange={(e) => setKeyword(e.target.value)} placeholder="搜索服务名/分类" className="max-w-xs" />
            {isOps ? (
              <Button asChild variant="outline">
                <Link href="/ops">进入运维台</Link>
              </Button>
            ) : null}
          </CardContent>
        </Card>

        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          {loadingPolicy
            ? Array.from({ length: 6 }).map((_, i) => (
                <Card key={i} className="glass-card">
                  <CardHeader>
                    <Skeleton className="h-5 w-32" />
                  </CardHeader>
                  <CardContent className="space-y-2">
                    <Skeleton className="h-4 w-full" />
                    <Skeleton className="h-4 w-2/3" />
                  </CardContent>
                </Card>
              ))
            : filtered.map((s) => {
            const p = policyMap[s.id] ?? "unknown";
            const deny = p === "deny";
            return (
              <Card key={s.id} className={`glass-card ${deny ? "opacity-75" : ""}`}>
                <CardHeader>
                  <CardTitle className="text-base">{s.name}</CardTitle>
                  <CardDescription>{s.desc}</CardDescription>
                </CardHeader>
                <CardContent className="flex items-center justify-between gap-3">
                  <Badge variant={p === "allow" ? "default" : p === "deny" ? "destructive" : "secondary"}>
                    策略: {p === "allow" ? "允许" : p === "deny" ? "拒绝" : "判定中"}
                  </Badge>
                  <div className="flex items-center gap-2">
                    {deny ? (
                      <Button size="sm" variant="outline" disabled>
                        进入页面
                      </Button>
                    ) : (
                      <Button size="sm" variant="outline" onClick={() => void enterPage(s)}>
                        进入页面
                      </Button>
                    )}
                    <Button size="sm" disabled={deny} onClick={() => void probe(s)}>
                      网关探测
                    </Button>
                  </div>
                </CardContent>
              </Card>
            );
          })}
        </div>
        <Card className="glass-card">
          <CardContent className="pt-6 text-sm text-muted-foreground">{message}</CardContent>
        </Card>
      </div>
    </RoleGate>
  );
}
