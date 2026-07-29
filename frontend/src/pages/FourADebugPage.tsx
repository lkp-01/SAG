import { useMemo, useState } from "react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";

export function FourADebugPage() {
  const [firstUri, setFirstUri] = useState("");
  const [secondUri, setSecondUri] = useState("");
  const [thirdUri, setThirdUri] = useState("");
  const [clientId, setClientId] = useState("");
  const [redirectUri, setRedirectUri] = useState("http://127.0.0.1:5173");
  const [code, setCode] = useState("");

  const callbackData = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return {
      code: params.get("code") ?? "",
      state: params.get("state") ?? "",
      error: params.get("error") ?? "",
    };
  }, []);

  const authUrl = useMemo(() => {
    if (!firstUri || !clientId) return "";
    const u = new URL(firstUri);
    u.searchParams.set("client_id", clientId);
    u.searchParams.set("response_type", "code");
    u.searchParams.set("redirect_uri", redirectUri);
    u.searchParams.set("state", "sag-ui-debug");
    return u.toString();
  }, [firstUri, clientId, redirectUri]);

  return (
    <Card>
      <CardHeader>
        <CardTitle>4A 调试占位</CardTitle>
        <CardDescription>此页用于本地联调协议参数与回调信息，不直接接入真实 4A。</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
          <Input value={firstUri} onChange={(e) => setFirstUri(e.target.value)} placeholder="firstUri (authorize)" />
          <Input value={clientId} onChange={(e) => setClientId(e.target.value)} placeholder="client_id" />
          <Input value={redirectUri} onChange={(e) => setRedirectUri(e.target.value)} placeholder="redirect_uri" />
          <Input value={code} onChange={(e) => setCode(e.target.value)} placeholder="auth code (manual)" />
          <Input value={secondUri} onChange={(e) => setSecondUri(e.target.value)} placeholder="secondUri (token)" />
          <Input value={thirdUri} onChange={(e) => setThirdUri(e.target.value)} placeholder="thirdUri (userinfo)" />
        </div>
        <div className="flex gap-2">
          <Button onClick={() => authUrl && window.open(authUrl, "_blank")}>跳转 firstUri</Button>
        </div>
        <Textarea
          value={JSON.stringify(
            {
              authUrl,
              callbackData,
              tokenRequestPreview: {
                url: secondUri,
                grant_type: "authorization_code",
                client_id: clientId,
                code: code || callbackData.code,
                redirect_uri: redirectUri,
              },
              userInfoRequestPreview: {
                url: thirdUri,
                note: "使用 secondUri 返回的 access_token 调用",
              },
            },
            null,
            2
          )}
          readOnly
          className="min-h-[260px]"
        />
      </CardContent>
    </Card>
  );
}
