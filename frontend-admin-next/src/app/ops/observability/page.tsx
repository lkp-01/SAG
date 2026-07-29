"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { controlApi } from "@/lib/api";
import type { FaultInjectionToggle } from "@/lib/types";

function debugLog(location: string, message: string, data: Record<string, unknown>, hypothesisId: string, runId = "initial") {
  // #region agent log
  fetch("http://127.0.0.1:7701/ingest/1ccb5b12-5073-4437-a0e2-a9913a1fb79d", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "ac5396" },
    body: JSON.stringify({
      sessionId: "ac5396",
      runId,
      hypothesisId,
      location,
      message,
      data,
      timestamp: Date.now()
    })
  }).catch(() => {});
  // #endregion
}

export default function OpsObservabilityPage() {
  const [toggle, setToggle] = useState<FaultInjectionToggle | null>(null);
  const resolved = useMemo(() => {
    const host = typeof window !== "undefined" ? window.location.hostname : "127.0.0.1";
    const grafanaExternal = process.env.NEXT_PUBLIC_GRAFANA_URL ?? `http://${host}:3000`;
    const prometheusExternal = process.env.NEXT_PUBLIC_PROMETHEUS_URL ?? `http://${host}:9091`;
    const grafanaEmbed = "/api-grafana";
    const prometheusEmbed = "/api-prom";
    return { grafanaExternal, prometheusExternal, grafanaEmbed, prometheusEmbed, host };
  }, []);

  useEffect(() => {
    debugLog("ops/observability/page.tsx:useEffect", "resolved observability urls", {
      grafanaExternal: resolved.grafanaExternal,
      prometheusExternal: resolved.prometheusExternal,
      grafanaEmbed: resolved.grafanaEmbed,
      prometheusEmbed: resolved.prometheusEmbed,
      host: resolved.host
    }, "H4");
  }, [resolved.grafanaExternal, resolved.prometheusExternal, resolved.grafanaEmbed, resolved.prometheusEmbed, resolved.host]);
  useEffect(() => {
    controlApi.getFaultInjection().then(setToggle).catch(() => setToggle(null));
  }, []);

  const enableFault = async (mode: "delay" | "timeout" | "http_status") => {
    const next = await controlApi.updateFaultInjection({
      enabled: true,
      mode,
      ttl_sec: 120,
      delay_ms: mode === "delay" ? 1500 : 1200,
      status_code: mode === "http_status" ? 503 : 504,
      service: "control-plane-admin",
      hit_percent: 100
    });
    setToggle(next);
  };

  const disableFault = async () => {
    const next = await controlApi.updateFaultInjection({ enabled: false });
    setToggle(next);
  };

  return (
    <RoleGate need="ops">
      <TopBar title="统一监控入口" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">核心入口</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            <Button asChild><Link href="/ops/workflow">工作流健康</Link></Button>
            <Button asChild variant="outline"><Link href="/ops/apps">应用指标</Link></Button>
            <Button asChild variant="outline"><a href={resolved.grafanaExternal} target="_blank" rel="noreferrer">Grafana</a></Button>
            <Button asChild variant="outline"><a href={resolved.prometheusExternal} target="_blank" rel="noreferrer">Prometheus</a></Button>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">故障注入开关（测试环境）</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="text-xs text-muted-foreground">
              当前状态：{toggle?.enabled ? `已启用（${toggle.mode}，剩余至 ${new Date(toggle.expires_at_ms).toLocaleTimeString()}）` : "未启用"}
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" onClick={() => void enableFault("delay")}>注入慢请求</Button>
              <Button variant="outline" onClick={() => void enableFault("timeout")}>注入超时</Button>
              <Button variant="outline" onClick={() => void enableFault("http_status")}>注入 5xx</Button>
              <Button onClick={() => void disableFault()}>关闭注入</Button>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Grafana 预览</CardTitle>
          </CardHeader>
          <CardContent>
            <iframe
              src={resolved.grafanaEmbed}
              className="h-[520px] w-full rounded border"
              onError={() => debugLog("ops/observability/page.tsx:iframe", "grafana iframe error", { src: resolved.grafanaEmbed }, "H8")}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">Prometheus 预览</CardTitle>
          </CardHeader>
          <CardContent>
            <iframe
              src={resolved.prometheusEmbed}
              className="h-[520px] w-full rounded border"
              onError={() => debugLog("ops/observability/page.tsx:iframe", "prometheus iframe error", { src: resolved.prometheusEmbed }, "H8")}
            />
          </CardContent>
        </Card>
      </div>
    </RoleGate>
  );
}
