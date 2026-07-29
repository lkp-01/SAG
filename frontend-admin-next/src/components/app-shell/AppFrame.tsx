"use client";

import { useEffect } from "react";
import { usePathname } from "next/navigation";
import { useRouter } from "next/navigation";
import { AppSidebar } from "@/components/app-shell/AppSidebar";
import { useAuth } from "@/components/auth/AuthProvider";

export function AppFrame({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const router = useRouter();
  const { loading, token, isOps, isBoss } = useAuth();
  const plain = pathname === "/login";
  const isPortalPath = pathname === "/portal" || pathname?.startsWith("/portal/");
  const isAllowedCommon = pathname === "/app";
  const isPublicSecurityPath = pathname === "/security/audit" || pathname === "/security/pentest";

  useEffect(() => {
    if (loading) return;
    if (!token && !plain && !isPublicSecurityPath) {
      router.replace("/login");
      return;
    }
    // Normal users should only stay in portal workspace.
    const isPrivileged = isOps || isBoss;
    if (token && !isPrivileged && !plain && !isPortalPath && !isAllowedCommon && !isPublicSecurityPath) {
      router.replace("/portal");
    }
  }, [loading, token, plain, isPortalPath, isAllowedCommon, isPublicSecurityPath, isOps, isBoss, router]);

  if (loading && !plain && !isPublicSecurityPath) {
    return <main className="min-h-screen p-8 text-sm text-muted-foreground">正在校验登录态...</main>;
  }

  if (!token && !plain && !isPublicSecurityPath) {
    return null;
  }

  if (plain || isPublicSecurityPath) return <main className="min-h-screen">{children}</main>;
  return (
    <div className="flex min-h-screen">
      <AppSidebar />
      <main className="flex min-h-screen flex-1 flex-col">{children}</main>
    </div>
  );
}
