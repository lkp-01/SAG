import { useState } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { HealthPage } from "@/pages/HealthPage";
import { DashboardPage } from "@/pages/DashboardPage";
import { SelfCheckPage } from "@/pages/SelfCheckPage";
import { RoutesPage } from "@/pages/RoutesPage";
import { UpstreamsPage } from "@/pages/UpstreamsPage";
import { PoliciesPage } from "@/pages/PoliciesPage";
import { AuthPage } from "@/pages/AuthPage";
import { FourADebugPage } from "@/pages/FourADebugPage";
import { UsersPage } from "@/pages/UsersPage";

export function App() {
  const [lastErr, setLastErr] = useState("");

  return (
    <div className="mx-auto max-w-7xl space-y-4 p-6">
      <div>
        <h1 className="text-2xl font-semibold">SAG Console</h1>
        <p className="text-sm text-slate-500">React + shadcn/ui（本机联调版）</p>
      </div>

      {lastErr ? (
        <Alert variant="destructive">
          <AlertTitle>请求失败</AlertTitle>
          <AlertDescription>{lastErr}</AlertDescription>
        </Alert>
      ) : null}

      <Tabs defaultValue="health" className="w-full">
        <TabsList>
          <TabsTrigger value="dashboard">概览</TabsTrigger>
          <TabsTrigger value="selfcheck">一键体检</TabsTrigger>
          <TabsTrigger value="health">健康总览</TabsTrigger>
          <TabsTrigger value="routes">路由管理</TabsTrigger>
          <TabsTrigger value="upstreams">上游映射</TabsTrigger>
          <TabsTrigger value="policies">策略管理</TabsTrigger>
          <TabsTrigger value="auth">登录会话</TabsTrigger>
          <TabsTrigger value="users">用户管理</TabsTrigger>
          <TabsTrigger value="foura">4A调试占位</TabsTrigger>
        </TabsList>
        <TabsContent value="dashboard">
          <DashboardPage />
        </TabsContent>
        <TabsContent value="selfcheck">
          <SelfCheckPage />
        </TabsContent>
        <TabsContent value="health">
          <HealthPage />
        </TabsContent>
        <TabsContent value="routes">
          <RoutesPage onError={setLastErr} />
        </TabsContent>
        <TabsContent value="upstreams">
          <UpstreamsPage onError={setLastErr} />
        </TabsContent>
        <TabsContent value="policies">
          <PoliciesPage onError={setLastErr} />
        </TabsContent>
        <TabsContent value="auth">
          <AuthPage onError={setLastErr} />
        </TabsContent>
        <TabsContent value="users">
          <UsersPage onError={setLastErr} />
        </TabsContent>
        <TabsContent value="foura">
          <FourADebugPage />
        </TabsContent>
      </Tabs>
    </div>
  );
}
