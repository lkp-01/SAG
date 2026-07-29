"use client";

import { Suspense, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";

const KEY = "sag_public_readonly_token";

export function getPublicReadonlyToken() {
  if (typeof window === "undefined") return "";
  return window.localStorage.getItem(KEY) ?? "";
}

export function setPublicReadonlyToken(token: string) {
  if (typeof window === "undefined") return;
  if (!token) {
    window.localStorage.removeItem(KEY);
    return;
  }
  window.localStorage.setItem(KEY, token);
}

function PublicReadOnlyGateInner({ children }: { children: React.ReactNode }) {
  const search = useSearchParams();
  const [token, setToken] = useState("");
  const hasToken = useMemo(() => token.trim().length > 0, [token]);

  useEffect(() => {
    const fromQuery = search.get("token") ?? "";
    const existing = getPublicReadonlyToken();
    const next = fromQuery || existing;
    if (next) {
      setToken(next);
      setPublicReadonlyToken(next);
    }
  }, [search]);

  if (hasToken) return <>{children}</>;

  return (
    <div className="flex min-h-screen items-center justify-center p-6">
      <Card className="w-full max-w-lg">
        <CardHeader>
          <CardTitle>公开只读安全入口</CardTitle>
          <CardDescription>请输入预共享只读 token 后访问安全审计与渗透演示页面。</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <Input value={token} onChange={(e) => setToken(e.target.value)} placeholder="x-sag-readonly-token" />
          <Button
            onClick={() => {
              setPublicReadonlyToken(token.trim());
              window.location.reload();
            }}
          >
            进入只读模式
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}

/** Next.js 15: useSearchParams() must be under Suspense for static generation / build. */
export function PublicReadOnlyGate({ children }: { children: React.ReactNode }) {
  return (
    <Suspense
      fallback={
        <div className="flex min-h-screen items-center justify-center p-6 text-sm text-muted-foreground">
          加载中…
        </div>
      }
    >
      <PublicReadOnlyGateInner>{children}</PublicReadOnlyGateInner>
    </Suspense>
  );
}

