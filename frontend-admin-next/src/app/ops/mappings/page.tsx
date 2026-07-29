"use client";

import { useEffect, useMemo, useState } from "react";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { authApi } from "@/lib/api";
import type { GroupRoleMapping, IdentityProvider } from "@/lib/types";

export default function OpsMappingsPage() {
  return (
    <RoleGate need="ops">
      <MappingsInner />
    </RoleGate>
  );
}

function MappingsInner() {
  const [providers, setProviders] = useState<IdentityProvider[]>([]);
  const [selectedProvider, setSelectedProvider] = useState("");
  const [rows, setRows] = useState<GroupRoleMapping[]>([]);
  const [err, setErr] = useState("");
  const [form, setForm] = useState<GroupRoleMapping>({
    id: "",
    provider_id: "",
    external_group: "dept:finance",
    local_roles_csv: "finance",
    enabled: true,
    priority: 0
  });

  async function reloadProviders() {
    const ps = await authApi.listIdentityProviders().catch(() => []);
    setProviders(ps);
    if (!selectedProvider && ps[0]?.id) setSelectedProvider(ps[0].id);
  }

  async function reloadMappings(pid?: string) {
    const list = await authApi.listGroupRoleMappings(pid);
    setRows(list);
  }

  async function reloadAll() {
    try {
      await reloadProviders();
      const pid = selectedProvider || providers[0]?.id || "";
      await reloadMappings(pid || undefined);
      setErr("");
    } catch (e) {
      setErr(String(e));
      setRows([]);
    }
  }

  useEffect(() => {
    void reloadAll();
  }, []);

  useEffect(() => {
    if (!selectedProvider) return;
    void (async () => {
      try {
        await reloadMappings(selectedProvider);
        setErr("");
      } catch (e) {
        setErr(String(e));
        setRows([]);
      }
    })();
  }, [selectedProvider]);

  const providerOptions = useMemo(() => providers.map((p) => p.id), [providers]);

  async function save() {
    try {
      await authApi.upsertGroupRoleMapping({
        ...form,
        provider_id: form.provider_id || selectedProvider
      });
      setForm({ id: "", provider_id: "", external_group: "dept:finance", local_roles_csv: "finance", enabled: true, priority: 0 });
      await reloadAll();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function remove(id: string) {
    try {
      await authApi.deleteGroupRoleMapping(id);
      await reloadAll();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <TopBar title="用户/组映射规则" />
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
            <CardTitle className="text-sm">身份源</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            {providerOptions.map((id) => (
              <Button key={id} size="sm" variant={selectedProvider === id ? "default" : "outline"} onClick={() => setSelectedProvider(id)}>
                {id}
              </Button>
            ))}
            <Input
              className="max-w-xs"
              placeholder="或手动输入 provider_id"
              value={selectedProvider}
              onChange={(e) => setSelectedProvider(e.target.value)}
            />
            <Button size="sm" variant="outline" onClick={() => void reloadAll()}>
              刷新
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">新增 / 编辑规则</CardTitle>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-2 md:grid-cols-6">
            <Input placeholder="id(可空)" value={form.id} onChange={(e) => setForm((p) => ({ ...p, id: e.target.value }))} />
            <Input
              placeholder={`provider_id(默认 ${selectedProvider || "-"})`}
              value={form.provider_id}
              onChange={(e) => setForm((p) => ({ ...p, provider_id: e.target.value }))}
            />
            <Input
              placeholder="external_group"
              value={form.external_group}
              onChange={(e) => setForm((p) => ({ ...p, external_group: e.target.value }))}
            />
            <Input
              placeholder="local_roles_csv"
              value={form.local_roles_csv}
              onChange={(e) => setForm((p) => ({ ...p, local_roles_csv: e.target.value }))}
            />
            <Input
              placeholder="priority"
              value={String(form.priority)}
              onChange={(e) => setForm((p) => ({ ...p, priority: Number(e.target.value || 0) }))} />
            <div className="flex gap-2">
              <Button onClick={() => void save()}>保存</Button>
              <Button
                variant="outline"
                onClick={() => setForm({ id: "", provider_id: "", external_group: "dept:finance", local_roles_csv: "finance", enabled: true, priority: 0 })}
              >
                清空
              </Button>
            </div>
            <Input
              className="md:col-span-6"
              placeholder="enabled(true/false)"
              value={String(form.enabled)}
              onChange={(e) => setForm((p) => ({ ...p, enabled: e.target.value !== "false" }))}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">规则列表</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>id</TableHead>
                  <TableHead>provider</TableHead>
                  <TableHead>external_group</TableHead>
                  <TableHead>local_roles</TableHead>
                  <TableHead>priority</TableHead>
                  <TableHead>enabled</TableHead>
                  <TableHead>操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => (
                  <TableRow key={r.id}>
                    <TableCell>{r.id}</TableCell>
                    <TableCell>{r.provider_id}</TableCell>
                    <TableCell>{r.external_group}</TableCell>
                    <TableCell>{r.local_roles_csv}</TableCell>
                    <TableCell>{r.priority}</TableCell>
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

