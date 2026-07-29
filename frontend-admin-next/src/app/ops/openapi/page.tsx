"use client";

import { useEffect, useMemo, useState } from "react";
import { TopBar } from "@/components/app-shell/TopBar";
import { RoleGate } from "@/components/auth/RoleGate";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { controlApi } from "@/lib/api";
import type { ApiRouteRecord, AppRecord } from "@/lib/types";

type ParsedRoute = {
  method: string;
  path: string;
  description: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export default function OpsOpenApiPage() {
  return (
    <RoleGate need="ops">
      <OpenApiInner />
    </RoleGate>
  );
}

function OpenApiInner() {
  const [apps, setApps] = useState<AppRecord[]>([]);
  const [selectedApp, setSelectedApp] = useState("");
  const [raw, setRaw] = useState("");
  const [parsed, setParsed] = useState<ParsedRoute[]>([]);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void (async () => {
      const a = await controlApi.listApps().catch(() => []);
      setApps(a);
      if (a.length && !selectedApp) setSelectedApp(a[0].app_id);
    })();
  }, []);

  const appOptions = useMemo(() => apps.map((a) => a.app_id), [apps]);

  function parseOpenApi() {
    try {
      const obj: unknown = JSON.parse(raw);
      const paths = isRecord(obj) && isRecord(obj.paths) ? obj.paths : {};
      const out: ParsedRoute[] = [];
      for (const [p, methods] of Object.entries(paths)) {
        if (!isRecord(methods)) continue;
        for (const [m, detail] of Object.entries(methods)) {
          const method = String(m).toUpperCase();
          if (!["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].includes(method)) continue;
          const detailRecord = isRecord(detail) ? detail : {};
          const description = detailRecord.summary ?? detailRecord.description ?? "";
          const desc = typeof description === "string" ? description : String(description);
          out.push({ method, path: String(p), description: desc });
        }
      }
      setParsed(out.sort((a, b) => (a.path + a.method).localeCompare(b.path + b.method)));
      setErr("");
    } catch (e) {
      setErr(String(e));
      setParsed([]);
    }
  }

  async function importAll() {
    if (!selectedApp) {
      setErr("请选择 app_id");
      return;
    }
    if (!parsed.length) {
      setErr("没有可导入的路由");
      return;
    }
    setBusy(true);
    try {
      for (const r of parsed) {
        const row: ApiRouteRecord = {
          id: "",
          app_id: selectedApp,
          method: r.method,
          path: r.path,
          enabled: true,
          description: r.description
        };
        await controlApi.upsertApiRoute(row);
      }
      setErr("");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <TopBar title="OpenAPI 导入器" />
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
            <CardTitle className="text-sm">目标应用</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            {appOptions.map((id) => (
              <Button key={id} size="sm" variant={selectedApp === id ? "default" : "outline"} onClick={() => setSelectedApp(id)}>
                {id}
              </Button>
            ))}
            <Input
              className="max-w-xs"
              placeholder="或手动输入 app_id"
              value={selectedApp}
              onChange={(e) => setSelectedApp(e.target.value)}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">粘贴 Swagger/OpenAPI JSON</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <Textarea value={raw} onChange={(e) => setRaw(e.target.value)} className="min-h-[240px]" placeholder='{"openapi":"3.0.0","paths":{...}}' />
            <div className="flex gap-2">
              <Button variant="secondary" onClick={parseOpenApi}>
                解析
              </Button>
              <Button disabled={busy || !parsed.length} onClick={() => void importAll()}>
                {busy ? "导入中..." : `批量导入 ${parsed.length} 条`}
              </Button>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">解析预览</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>method</TableHead>
                  <TableHead>path</TableHead>
                  <TableHead>description</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {parsed.slice(0, 200).map((r, i) => (
                  <TableRow key={`${r.method}:${r.path}:${i}`}>
                    <TableCell>{r.method}</TableCell>
                    <TableCell>{r.path}</TableCell>
                    <TableCell className="text-xs text-muted-foreground">{r.description || "-"}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            {parsed.length > 200 ? <div className="mt-2 text-xs text-muted-foreground">仅预览前 200 条</div> : null}
          </CardContent>
        </Card>
      </div>
    </>
  );
}
