"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect } from "react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { useAuth } from "@/components/auth/AuthProvider";

type NeedRole = "portal" | "ops" | "boss";

function canAccess(need: NeedRole, isOps: boolean, isBoss: boolean) {
  if (need === "portal") return true;
  if (need === "ops") return isOps;
  return isBoss;
}

export function RoleGate({ need, children }: { need: NeedRole; children: React.ReactNode }) {
  const { loading, token, isOps, isBoss } = useAuth();
  const router = useRouter();
  const pathname = usePathname();

  useEffect(() => {
    if (loading) return;
    if (!token && pathname !== "/login") {
      router.replace("/login");
    }
  }, [loading, token, pathname, router]);

  if (loading) return <div className="p-8 text-sm text-muted-foreground">正在准备会话...</div>;
  if (!token) return null;
  if (!canAccess(need, isOps, isBoss)) {
    return (
      <div className="flex min-h-[60vh] items-center justify-center p-6">
        <Card className="w-full max-w-lg">
          <CardHeader>
            <CardTitle>无权访问该页面</CardTitle>
            <CardDescription>当前账号角色不足，请返回可访问区域。</CardDescription>
          </CardHeader>
          <CardContent className="flex gap-2">
            <Button asChild>
              <Link href="/app">回到分流页</Link>
            </Button>
            <Button asChild variant="outline">
              <Link href="/portal">用户门户</Link>
            </Button>
          </CardContent>
        </Card>
      </div>
    );
  }
  return <>{children}</>;
}
