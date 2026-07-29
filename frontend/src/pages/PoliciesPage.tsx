import { useEffect, useState } from "react";
import { policyApi } from "@/lib/api";
import type { PolicyRow } from "@/lib/types";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

type Props = { onError: (msg: string) => void };

export function PoliciesPage({ onError }: Props) {
  const [rows, setRows] = useState<PolicyRow[]>([]);
  const [form, setForm] = useState<PolicyRow>({
    id: "p-allow-admin",
    effect: "ALLOW",
    subjects: ["role:admin"],
    app_id: null,
    path_prefix: "/api/",
    priority: 1000,
  });
  const [subjectsText, setSubjectsText] = useState("role:admin");

  const reload = async () => {
    try {
      setRows(await policyApi.list());
    } catch (e) {
      onError(String(e));
    }
  };

  useEffect(() => {
    reload();
  }, []);

  const save = async () => {
    try {
      await policyApi.upsert({
        ...form,
        subjects: subjectsText
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
      });
      await reload();
    } catch (e) {
      onError(String(e));
    }
  };

  const remove = async (id: string) => {
    try {
      await policyApi.delete(id);
      await reload();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>策略管理</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-3">
          <Input value={form.id} onChange={(e) => setForm({ ...form, id: e.target.value })} placeholder="policy id" />
          <Input value={form.effect} onChange={(e) => setForm({ ...form, effect: e.target.value as "ALLOW" | "DENY" })} placeholder="ALLOW|DENY" />
          <Input value={String(form.priority)} onChange={(e) => setForm({ ...form, priority: Number(e.target.value) || 1000 })} placeholder="priority" />
          <Input value={form.app_id ?? ""} onChange={(e) => setForm({ ...form, app_id: e.target.value || null })} placeholder="app_id (optional)" />
          <Input value={form.path_prefix ?? ""} onChange={(e) => setForm({ ...form, path_prefix: e.target.value || null })} placeholder="path_prefix (optional)" />
          <Input value={subjectsText} onChange={(e) => setSubjectsText(e.target.value)} placeholder="subjects,comma,separated" />
        </div>
        <div className="flex gap-2">
          <Button onClick={save}>新增/更新策略</Button>
          <Button variant="secondary" onClick={reload}>
            刷新
          </Button>
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>id</TableHead>
              <TableHead>effect</TableHead>
              <TableHead>subjects</TableHead>
              <TableHead>app_id</TableHead>
              <TableHead>path_prefix</TableHead>
              <TableHead>priority</TableHead>
              <TableHead>操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((r) => (
              <TableRow key={r.id}>
                <TableCell>{r.id}</TableCell>
                <TableCell>{r.effect}</TableCell>
                <TableCell>{r.subjects.join(",")}</TableCell>
                <TableCell>{r.app_id ?? "-"}</TableCell>
                <TableCell>{r.path_prefix ?? "-"}</TableCell>
                <TableCell>{r.priority}</TableCell>
                <TableCell>
                  <div className="flex gap-2">
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => {
                        setForm(r);
                        setSubjectsText(r.subjects.join(","));
                      }}
                    >
                      编辑
                    </Button>
                    <Button size="sm" variant="destructive" onClick={() => remove(r.id)}>
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
  );
}
