import { useMemo, useState } from "react";
import { authApi } from "@/lib/api";
import { clearToken, getToken, setToken } from "@/lib/session";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

type Props = { onError: (msg: string) => void };

export function AuthPage({ onError }: Props) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("Admin@123");
  const [verifyResult, setVerifyResult] = useState("");
  const token = useMemo(() => getToken(), [verifyResult]);

  const login = async () => {
    try {
      const r = await authApi.login(username, password);
      setToken(r.token);
      setVerifyResult(`登录成功: ${r.user.username} roles=${r.user.roles.join(",")}`);
    } catch (e) {
      onError(String(e));
    }
  };

  const verify = async () => {
    try {
      const t = getToken();
      if (!t) throw new Error("无 token，请先登录");
      const r = await authApi.verify(t);
      setVerifyResult(JSON.stringify(r));
    } catch (e) {
      onError(String(e));
    }
  };

  const logout = () => {
    clearToken();
    setVerifyResult("已退出");
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>登录会话</CardTitle>
        <CardDescription>用于验证 auth 登录、token 保存、鉴权调用链路。</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
          <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="username" />
          <Input
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="password"
            type="password"
          />
        </div>
        <div className="flex gap-2">
          <Button onClick={login}>登录</Button>
          <Button variant="secondary" onClick={verify}>
            校验token
          </Button>
          <Button variant="outline" onClick={logout}>
            退出
          </Button>
        </div>
        <p className="text-xs text-slate-500">当前token: {token ? `${token.slice(0, 24)}...` : "空"}</p>
        {verifyResult ? (
          <pre className="rounded-md bg-slate-100 p-3 text-xs whitespace-pre-wrap">{verifyResult}</pre>
        ) : null}
      </CardContent>
    </Card>
  );
}
