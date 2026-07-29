"use client";

import { useEffect, useState } from "react";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { controlApi } from "@/lib/api";
import type { AuditLog } from "@/lib/types";

const serviceOptions = ["", "control-plane-admin", "sag-auth", "sag-policy", "http-tunnel-bridge", "stealth-tunnel-agent", "sag-connector", "public-edge", "zentinel"];
const resultOptions = ["", "200", "4", "5"];
const decisionOptions = ["", "observe", "ALLOW", "DENY"];
const departmentOptions = ["", "tech", "finance", "ops", "management", "vendor"];

export default function OpsAuditPage() {
  return (
    <RoleGate need="ops">
      <AuditInner />
    </RoleGate>
  );
}

function AuditInner() {
  const [rows, setRows] = useState<AuditLog[]>([]);
  const [fromTs, setFromTs] = useState("");
  const [toTs, setToTs] = useState("");
  const [userId, setUserId] = useState("");
  const [appId, setAppId] = useState("");
  const [service, setService] = useState("");
  const [result, setResult] = useState("");
  const [decision, setDecision] = useState("");
  const [pathContains, setPathContains] = useState("");
  const [department, setDepartment] = useState("");
  const [err, setErr] = useState("");

  async function reload() {
    try {
      const list = await controlApi.listAuditLogs({
        from_ts_ms: fromTs ? Number(fromTs) : undefined,
        to_ts_ms: toTs ? Number(toTs) : undefined,
        user_id: userId || undefined,
        app_id: appId || undefined,
        service: service || undefined,
        result: result || undefined,
        decision: decision || undefined,
        path_contains: pathContains || undefined,
        department: department || undefined,
        limit: 200,
      });
      setRows(list);
      setErr("");
    } catch (e) {
      setErr(String(e));
      setRows([]);
    }
  }

  useEffect(() => {
    void reload();
  }, []);

  return (
    <>
      <TopBar title="审计日志中心" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        {err ? (
          <Card className="border-destructive/50">
            <CardHeader>
              <CardTitle className="text-sm">错误</CardTitle>
            </CardHeader>
            <CardContent className="text-sm text-muted-foreground">{err}</CardContent>
          </Card>
        ) : null}
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">查询条件</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            <Input className="max-w-xs" placeholder="from_ts_ms" value={fromTs} onChange={(e) => setFromTs(e.target.value)} />
            <Input className="max-w-xs" placeholder="to_ts_ms" value={toTs} onChange={(e) => setToTs(e.target.value)} />
            <Input className="max-w-xs" placeholder="user_id" value={userId} onChange={(e) => setUserId(e.target.value)} />
            <Input className="max-w-xs" placeholder="app_id" value={appId} onChange={(e) => setAppId(e.target.value)} />
            <select className="h-9 rounded-md border px-2 text-sm" value={service} onChange={(e) => setService(e.target.value)}>
              {serviceOptions.map((v) => (
                <option key={v || "all"} value={v}>{v || "service(全部)"}</option>
              ))}
            </select>
            <select className="h-9 rounded-md border px-2 text-sm" value={result} onChange={(e) => setResult(e.target.value)}>
              {resultOptions.map((v) => (
                <option key={v || "all"} value={v}>{v ? `result=${v}` : "result(全部)"}</option>
              ))}
            </select>
            <select className="h-9 rounded-md border px-2 text-sm" value={decision} onChange={(e) => setDecision(e.target.value)}>
              {decisionOptions.map((v) => (
                <option key={v || "all"} value={v}>{v || "decision(全部)"}</option>
              ))}
            </select>
            <Input className="max-w-xs" placeholder="path_contains" value={pathContains} onChange={(e) => setPathContains(e.target.value)} />
            <select className="h-9 rounded-md border px-2 text-sm" value={department} onChange={(e) => setDepartment(e.target.value)}>
              {departmentOptions.map((v) => (
                <option key={v || "all"} value={v}>{v || "department(全部)"}</option>
              ))}
            </select>
            <Button onClick={() => void reload()}>查询</Button>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">审计日志（最近 {rows.length} 条）</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>service</TableHead>
                  <TableHead>user_id</TableHead>
                  <TableHead>app_id</TableHead>
                  <TableHead>path</TableHead>
                  <TableHead>latency_ms</TableHead>
                  <TableHead>decision/result</TableHead>
                  <TableHead>dept</TableHead>
                  <TableHead>trace_id</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => (
                  <TableRow key={r.id}>
                    <TableCell>{new Date(r.ts_ms).toLocaleString()}</TableCell>
                    <TableCell>{r.service}</TableCell>
                    <TableCell>{r.user_id || "-"}</TableCell>
                    <TableCell>{r.app_id || "-"}</TableCell>
                    <TableCell>{r.path}</TableCell>
                    <TableCell>{r.latency_ms}</TableCell>
                    <TableCell>{`${r.decision}/${r.result}`}</TableCell>
                    <TableCell>{(() => {
                      try {
                        const j = JSON.parse(r.extra_json || "{}");
                        return j.department || "-";
                      } catch {
                        return "-";
                      }
                    })()}</TableCell>
                    <TableCell>{r.trace_id || "-"}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </div>
    </>
  );
}

