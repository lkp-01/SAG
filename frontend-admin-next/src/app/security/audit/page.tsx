"use client";

import { useEffect, useState } from "react";
import { PublicReadOnlyGate } from "@/components/auth/PublicReadOnlyGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { controlApi } from "@/lib/api";
import type { AuditLog, PublicSecurityOverview } from "@/lib/types";

export default function PublicSecurityAuditPage() {
  const [logs, setLogs] = useState<AuditLog[]>([]);
  const [overview, setOverview] = useState<PublicSecurityOverview | null>(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    Promise.all([controlApi.publicAuditLogs(), controlApi.publicSecurityOverview()])
      .then(([nextLogs, nextOverview]) => {
        setLogs(nextLogs);
        setOverview(nextOverview);
        setErr("");
      })
      .catch((e) => setErr(String(e)));
  }, []);

  return (
    <PublicReadOnlyGate>
      <TopBar title="公开安全审计" />
      <div className="space-y-4 p-4 md:p-6">
        <Card>
          <CardHeader>
            <CardTitle>安全态势概览</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 text-sm">
            <div>最近一小时审计记录：{overview?.audit_count ?? "—"}</div>
            <div>最近一小时 fault events：{overview?.fault_event_count ?? "—"}</div>
            <div>高危故障事件：{overview?.critical_fault_count ?? "—"}</div>
            <div className="text-xs text-muted-foreground">{overview?.note}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>脱敏审计明细</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            {err ? <div className="text-destructive">{err}</div> : null}
            {logs.length === 0 ? <div className="text-muted-foreground">当前无可展示审计数据。</div> : logs.map((row) => (
              <div key={row.id} className="rounded border p-2">
                <div>{new Date(row.ts_ms).toLocaleString()} | {row.service} | {row.method} {row.path}</div>
                <div className="text-xs text-muted-foreground">user={row.user_id || "—"} app={row.app_id || "—"} result={row.result} latency={row.latency_ms}ms</div>
              </div>
            ))}
          </CardContent>
        </Card>
      </div>
    </PublicReadOnlyGate>
  );
}

