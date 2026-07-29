"use client";

import { useMemo } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ExternalLink } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/components/auth/AuthProvider";

export function TopBar({ title }: { title: string }) {
  const grafanaUrl = useMemo(() => process.env.NEXT_PUBLIC_GRAFANA_URL ?? "http://127.0.0.1:3000", []);
  const pathname = usePathname();
  const { token, user, logout, isOps, isBoss } = useAuth();
  const isLogin = pathname === "/login";
  return (
    <header className="flex h-14 items-center justify-between border-b bg-background px-4">
      <div className="text-sm font-semibold">{title}</div>
      <div className="flex items-center gap-2">
        {!token ? (
          <>
            <Button asChild size="sm">
              <Link href="/login">账号登录</Link>
            </Button>
            {isLogin ? (
              <Button asChild size="sm" variant="secondary">
                <Link href="/api-auth/api/v1/auth/sso/login">4A 单点登录</Link>
              </Button>
            ) : null}
          </>
        ) : (
          <>
            <span className="text-xs text-muted-foreground">已登录：{user?.username ?? "user"}</span>
            <Button size="sm" variant="outline" onClick={logout}>
              退出
            </Button>
          </>
        )}
        {isOps || isBoss ? (
          <Button asChild variant="outline" size="sm">
            <Link href={grafanaUrl} target="_blank" rel="noreferrer">
              Grafana <ExternalLink />
            </Link>
          </Button>
        ) : null}
      </div>
    </header>
  );
}

