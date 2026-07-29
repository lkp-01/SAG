"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { TopBar } from "@/components/app-shell/TopBar";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { useAuth } from "@/components/auth/AuthProvider";

export default function LoginPage() {
  const router = useRouter();
  const { login } = useAuth();
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("Admin@123");
  const [err, setErr] = useState("");
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    const u = new URL(window.location.href);
    if (u.searchParams.get("sso_guest") !== "1") return;
    setErr("未认证访客无法完成 4A 单点登录（演示：认证失败）。");
    u.searchParams.delete("sso_guest");
    window.history.replaceState({}, "", u.toString());
  }, []);

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    setErr("");
    setLoading(true);
    try {
      await login(username, password);
      router.push("/app");
    } catch (e2) {
      setErr(String(e2));
    } finally {
      setLoading(false);
    }
  }

  return (
    <>
      <TopBar title="登录" />
      <div className="flex flex-1 items-start justify-center p-6">
        <Card className="w-full max-w-md">
          <CardHeader>
            <CardTitle>登录到 SAG</CardTitle>
          </CardHeader>
          <CardContent>
            <form onSubmit={onSubmit} className="space-y-3">
              <label className="block text-sm">
                <div className="mb-1 text-muted-foreground">用户名</div>
                <Input
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </label>
              <label className="block text-sm">
                <div className="mb-1 text-muted-foreground">密码</div>
                <Input
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </label>
              {err ? <div className="text-xs text-destructive">{err}</div> : null}
              <Button type="submit" disabled={loading} className="w-full">
                {loading ? "登录中..." : "登录"}
              </Button>
              <Button
                type="button"
                variant="outline"
                className="w-full"
                onClick={() => {
                  window.location.href = "/api-auth/api/v1/auth/sso/login";
                }}
              >
                4A 单点登录
              </Button>
              <div className="text-xs text-muted-foreground">
                说明：登录后将按角色自动跳转到用户门户、运维台或老板视图。
              </div>
            </form>
          </CardContent>
        </Card>
      </div>
    </>
  );
}

