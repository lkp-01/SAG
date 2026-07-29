"use client";

import { useEffect, useState } from "react";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { authApi } from "@/lib/api";
import type { IdentityProvider } from "@/lib/types";

export default function OpsIdentityPage() {
  return (
    <RoleGate need="ops">
      <IdentityInner />
    </RoleGate>
  );
}

function IdentityInner() {
  const [rows, setRows] = useState<IdentityProvider[]>([]);
  const [err, setErr] = useState("");
  const [form, setForm] = useState<IdentityProvider>({
    id: "foura",
    kind: "foura",
    issuer: "",
    client_id: "",
    client_secret: "",
    scopes: "openid profile email groups",
    enabled: true
  });

  async function reload() {
    try {
      const r = await authApi.listIdentityProviders();
      setRows(r);
      setErr("");
    } catch (e) {
      setErr(String(e));
      setRows([]);
    }
  }

  useEffect(() => {
    reload();
  }, []);

  async function save() {
    try {
      await authApi.upsertIdentityProvider(form);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function remove(id: string) {
    try {
      await authApi.deleteIdentityProvider(id);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <TopBar title="身份源配置" />
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
            <CardTitle className="text-sm">新增 / 编辑身份源</CardTitle>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-2 md:grid-cols-6">
            <Input placeholder="id" value={form.id} onChange={(e) => setForm((p) => ({ ...p, id: e.target.value }))} />
            <Input placeholder="kind(oidc/foura)" value={form.kind} onChange={(e) => setForm((p) => ({ ...p, kind: e.target.value }))} />
            <Input placeholder="issuer(oidc)" value={form.issuer} onChange={(e) => setForm((p) => ({ ...p, issuer: e.target.value }))} />
            <Input placeholder="client_id" value={form.client_id} onChange={(e) => setForm((p) => ({ ...p, client_id: e.target.value }))} />
            <Input
              placeholder="client_secret"
              value={form.client_secret}
              onChange={(e) => setForm((p) => ({ ...p, client_secret: e.target.value }))}
            />
            <Input
              placeholder="enabled(true/false)"
              value={String(form.enabled)}
              onChange={(e) => setForm((p) => ({ ...p, enabled: e.target.value !== "false" }))}
            />
            <Input
              className="md:col-span-6"
              placeholder="scopes"
              value={form.scopes}
              onChange={(e) => setForm((p) => ({ ...p, scopes: e.target.value }))}
            />
            <div className="md:col-span-6 flex gap-2">
              <Button onClick={() => void save()}>保存</Button>
              <Button
                variant="outline"
                onClick={() =>
                  setForm({
                    id: "foura",
                    kind: "foura",
                    issuer: "",
                    client_id: "",
                    client_secret: "",
                    scopes: "openid profile email groups",
                    enabled: true
                  })
                }
              >
                重置
              </Button>
              <Button variant="outline" onClick={reload}>
                刷新
              </Button>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">已配置身份源</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>id</TableHead>
                  <TableHead>kind</TableHead>
                  <TableHead>issuer</TableHead>
                  <TableHead>enabled</TableHead>
                  <TableHead>操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => (
                  <TableRow key={r.id}>
                    <TableCell>{r.id}</TableCell>
                    <TableCell>{r.kind}</TableCell>
                    <TableCell className="text-xs text-muted-foreground">{r.issuer || "-"}</TableCell>
                    <TableCell>{String(r.enabled)}</TableCell>
                    <TableCell>
                      <div className="flex gap-2">
                        <Button size="sm" variant="secondary" onClick={() => setForm(r)}>
                          编辑
                        </Button>
                        <Button size="sm" variant="destructive" onClick={() => void remove(r.id)}>
                          删除
                        </Button>
                      </div>
                    </TableCell>
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

