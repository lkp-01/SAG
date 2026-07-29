import { useEffect, useState } from "react";
import {
  AUTH_BASE,
  BRIDGE_BASE,
  CONTROL_BASE,
  POLICY_BASE,
  ZENTINEL_BASE,
  dataPlaneProbe,
  health,
} from "@/lib/api";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";

type HealthMap = Record<string, string>;

export function HealthPage() {
  const [state, setState] = useState<HealthMap>({});
  const [northResult, setNorthResult] = useState<string>("");
  const [bridgeResult, setBridgeResult] = useState<string>("");

  const reload = async () => {
    const pairs: [string, string][] = [
      ["control-plane-admin", `${CONTROL_BASE}/health`],
      ["sag-auth", `${AUTH_BASE}/health`],
      ["sag-policy", `${POLICY_BASE}/health`],
    ];
    const out: HealthMap = {};
    for (const [name, url] of pairs) {
      try {
        out[name] = await health(url);
      } catch (e) {
        out[name] = `ERR: ${(e as Error).message}`;
      }
    }
    setState(out);
  };

  const runDataPlaneProbe = async () => {
    const n = await dataPlaneProbe(ZENTINEL_BASE, "/api/test", "app-001");
    const t = await dataPlaneProbe(BRIDGE_BASE, "/api/test", "app-001");
    setNorthResult(`N1 status=${n.status} body=${n.body.slice(0, 120)}`);
    setBridgeResult(`T1 status=${t.status} body=${t.body.slice(0, 120)}`);
  };

  useEffect(() => {
    reload();
  }, []);

  return (
    <Card>
      <CardHeader>
        <CardTitle>健康总览</CardTitle>
        <CardDescription>检查管理面和关键数据面入口状态。</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-wrap gap-2">
          {Object.entries(state).map(([k, v]) => (
            <Badge key={k} variant={v === "ok" ? "default" : "outline"}>
              {k}: {v}
            </Badge>
          ))}
        </div>
        <div className="flex gap-2">
          <Button onClick={reload}>刷新健康状态</Button>
          <Button variant="secondary" onClick={runDataPlaneProbe}>
            一键数据面探测
          </Button>
        </div>
        {northResult ? <p className="text-sm text-slate-700">{northResult}</p> : null}
        {bridgeResult ? <p className="text-sm text-slate-700">{bridgeResult}</p> : null}
      </CardContent>
    </Card>
  );
}
