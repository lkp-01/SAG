"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { TopBar } from "@/components/app-shell/TopBar";
import { useAuth } from "@/components/auth/AuthProvider";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";

export default function AppDispatchPage() {
  const { loading, token, user, homePath, isBoss, isOps } = useAuth();
  const router = useRouter();

  useEffect(() => {
    if (loading) return;
    if (!token) {
      router.replace("/login");
      return;
    }
    router.prefetch("/portal");
    router.prefetch("/ops");
    router.prefetch("/boss");
    const t = window.setTimeout(() => router.replace(homePath), 250);
    return () => window.clearTimeout(t);
  }, [loading, token, homePath, router]);

  return (
    <>
      <TopBar title="角色分流" />
      <div className="flex flex-1 items-center justify-center p-6">
        <Card className="glass-card w-full max-w-2xl">
          <CardHeader>
            <CardTitle>正在进入工作台</CardTitle>
            <CardDescription>根据账号角色自动分发到对应界面，减少手动跳转等待。</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="text-sm text-muted-foreground">
              当前用户：{user?.display_name ?? user?.username ?? "未登录"}，角色：{user?.roles?.join(", ") ?? "—"}
            </div>
            <div className="flex flex-wrap gap-2">
              <Button asChild variant="secondary">
                <Link href="/portal">用户门户</Link>
              </Button>
              {isOps ? (
                <Button asChild variant="secondary">
                  <Link href="/ops">运维管理</Link>
                </Button>
              ) : null}
              {isBoss ? (
                <Button asChild variant="secondary">
                  <Link href="/boss">老板视图</Link>
                </Button>
              ) : null}
            </div>
          </CardContent>
        </Card>
      </div>
    </>
  );
}
