import { useEffect, useState } from "react";
import { authApi } from "@/lib/api";
import type { UpsertUserRequest, UserRow } from "@/lib/types";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

type Props = { onError: (msg: string) => void };

const emptyForm: UpsertUserRequest = {
  username: "",
  password: "",
  roles: ["tech"],
  display_name: "",
  title: "",
  enabled: true,
};

export function UsersPage({ onError }: Props) {
  const [rows, setRows] = useState<UserRow[]>([]);
  const [form, setForm] = useState<UpsertUserRequest>(emptyForm);

  const reload = async () => {
    try {
      setRows(await authApi.listUsers());
    } catch (e) {
      onError(String(e));
    }
  };

  useEffect(() => {
    reload();
  }, []);

  const submit = async () => {
    try {
      await authApi.upsertUser({
        ...form,
        roles: form.roles?.length ? form.roles : ["tech"],
      });
      await reload();
      setForm(emptyForm);
    } catch (e) {
      onError(String(e));
    }
  };

  const edit = (u: UserRow) => {
    setForm({
      id: u.id,
      username: u.username,
      roles: u.roles,
      display_name: u.display_name ?? "",
      title: u.title ?? "",
      enabled: true,
    });
  };

  const remove = async (username: string) => {
    try {
      await authApi.deleteUser(username);
      await reload();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>用户管理</CardTitle>
        <CardDescription>账号可用英文，展示姓名/岗位使用中文，便于客户演示。</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-6">
          <Input
            value={form.username ?? ""}
            onChange={(e) => setForm({ ...form, username: e.target.value })}
            placeholder="username(英文)"
          />
          <Input
            value={form.display_name ?? ""}
            onChange={(e) => setForm({ ...form, display_name: e.target.value })}
            placeholder="姓名(中文)"
          />
          <Input
            value={form.title ?? ""}
            onChange={(e) => setForm({ ...form, title: e.target.value })}
            placeholder="岗位(中文)"
          />
          <Input
            value={(form.roles ?? []).join(",")}
            onChange={(e) => setForm({ ...form, roles: e.target.value.split(",").map((x) => x.trim()).filter(Boolean) })}
            placeholder="roles(逗号分隔)"
          />
          <Input
            value={form.password ?? ""}
            onChange={(e) => setForm({ ...form, password: e.target.value })}
            placeholder="password(编辑可留空不改)"
            type="password"
          />
          <div className="flex gap-2">
            <Button onClick={submit}>新增/更新</Button>
            <Button variant="secondary" onClick={reload}>
              刷新
            </Button>
          </div>
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>用户名</TableHead>
              <TableHead>姓名</TableHead>
              <TableHead>岗位</TableHead>
              <TableHead>角色(中文)</TableHead>
              <TableHead>角色(原值)</TableHead>
              <TableHead>操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((u) => (
              <TableRow key={u.username}>
                <TableCell>{u.username}</TableCell>
                <TableCell>{u.display_name ?? "-"}</TableCell>
                <TableCell>{u.title ?? "-"}</TableCell>
                <TableCell>{(u.roles_display ?? []).join(",") || "-"}</TableCell>
                <TableCell>{u.roles.join(",")}</TableCell>
                <TableCell>
                  <div className="flex gap-2">
                    <Button size="sm" variant="secondary" onClick={() => edit(u)}>
                      编辑
                    </Button>
                    <Button size="sm" variant="destructive" onClick={() => remove(u.username)}>
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
