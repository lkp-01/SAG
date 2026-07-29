"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import type { ComponentType } from "react";
import { Activity, Cpu, LayoutDashboard, Layers3, LogOut, Settings2, ShieldCheck, Stethoscope, UserRound } from "lucide-react";
import { cn } from "@/lib/utils";
import { useAuth } from "@/components/auth/AuthProvider";

type NavItem = { href: string; label: string; icon: ComponentType<{ className?: string }> };
type NavSection = { title: string; items: NavItem[] };

const navPortal = [{ href: "/portal", label: "用户门户", icon: UserRound }];
const navOpsSections: NavSection[] = [
  {
    title: "运维与诊断",
    items: [
      { href: "/ops", label: "运维概览", icon: LayoutDashboard },
      { href: "/ops/control", label: "控制面板", icon: Settings2 },
      { href: "/ops/self-check", label: "一键体检", icon: Stethoscope }
    ]
  },
  {
    title: "配置中心",
    items: [
      { href: "/ops/apps", label: "应用与 API", icon: Layers3 },
      { href: "/ops/api-routes", label: "API 路由管理", icon: Layers3 },
      { href: "/ops/openapi", label: "OpenAPI 导入", icon: Layers3 }
    ]
  },
  {
    title: "身份与权限",
    items: [
      { href: "/ops/identity", label: "身份源配置", icon: Settings2 },
      { href: "/ops/mappings", label: "组/角色映射", icon: Settings2 }
    ]
  },
  {
    title: "观测与审计",
    items: [
      { href: "/ops/workflow", label: "工作流健康", icon: Activity },
      { href: "/ops/observability", label: "统一监控入口", icon: Activity },
      { href: "/ops/hardware", label: "硬件状态", icon: Cpu },
      { href: "/ops/audit", label: "审计中心", icon: Activity }
    ]
  }
];
const navBoss = [{ href: "/boss", label: "老板视图", icon: ShieldCheck }];

export function AppSidebar() {
  const pathname = usePathname();
  const { user, isOps, isBoss, logout } = useAuth();
  const flatOpsNav = navOpsSections.flatMap((x) => x.items);
  const nav = [...navPortal, ...(isOps ? flatOpsNav : []), ...(isBoss ? navBoss : [])];
  return (
    <aside className="hidden h-screen w-64 flex-col border-r bg-background md:flex">
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="size-7 rounded-md bg-primary" />
        <div className="text-sm font-semibold">SAG 统一入口</div>
      </div>
      <nav className="flex-1 space-y-1 p-2">
        <Link
          href="/app"
          className={cn(
            "mb-2 flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground",
            pathname === "/app" && "bg-accent text-accent-foreground"
          )}
        >
          <LayoutDashboard className="size-4" />
          <span>角色分流</span>
        </Link>
        {navPortal.map((it) => {
          const active = pathname === it.href || pathname?.startsWith(it.href + "/");
          const Icon = it.icon;
          return (
            <Link
              key={it.href}
              href={it.href}
              className={cn(
                "flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground",
                active && "bg-accent text-accent-foreground"
              )}
            >
              <Icon className="size-4" />
              <span>{it.label}</span>
            </Link>
          );
        })}
        {isOps
          ? navOpsSections.map((section) => (
              <div key={section.title} className="pt-2">
                <div className="px-3 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground/80">
                  {section.title}
                </div>
                {section.items.map((it) => {
                  const active = pathname === it.href || pathname?.startsWith(it.href + "/");
                  const Icon = it.icon;
                  return (
                    <Link
                      key={it.href}
                      href={it.href}
                      className={cn(
                        "flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground",
                        active && "bg-accent text-accent-foreground"
                      )}
                    >
                      <Icon className="size-4" />
                      <span>{it.label}</span>
                    </Link>
                  );
                })}
              </div>
            ))
          : null}
        {isBoss
          ? navBoss.map((it) => {
              const active = pathname === it.href || pathname?.startsWith(it.href + "/");
              const Icon = it.icon;
              return (
                <Link
                  key={it.href}
                  href={it.href}
                  className={cn(
                    "mt-2 flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground",
                    active && "bg-accent text-accent-foreground"
                  )}
                >
                  <Icon className="size-4" />
                  <span>{it.label}</span>
                </Link>
              );
            })
          : null}
      </nav>
      <div className="space-y-2 border-t p-3 text-xs text-muted-foreground">
        <div>{user ? `当前: ${user.username}` : "未登录"}</div>
        {user ? (
          <button className="inline-flex items-center gap-1 text-xs hover:text-foreground" onClick={logout}>
            <LogOut className="size-3" /> 退出登录
          </button>
        ) : null}
      </div>
    </aside>
  );
}

