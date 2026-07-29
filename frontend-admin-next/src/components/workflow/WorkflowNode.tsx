"use client";

import { memo } from "react";
import type { NodeProps } from "@xyflow/react";
import { Handle, Position } from "@xyflow/react";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";

export type WorkflowNodeData = {
  title: string;
  subtitle?: string;
  health?: "up" | "down" | "unknown";
  qps?: number | null;
  errRate?: number | null;
  p95Ms?: number | null;
};

function fmt(v: number | null | undefined, digits = 2) {
  if (v == null || !Number.isFinite(v)) return "—";
  return v.toFixed(digits);
}

function HealthBadge({ health }: { health: WorkflowNodeData["health"] }) {
  if (health === "up") return <Badge className="border-transparent bg-green-600 text-white">健康</Badge>;
  if (health === "down") return <Badge variant="destructive">异常</Badge>;
  return <Badge variant="outline" className="border-amber-300 bg-amber-50 text-amber-700">未知</Badge>;
}

export const WorkflowNode = memo(function WorkflowNode(props: NodeProps) {
  const data = props.data as WorkflowNodeData;
  const border =
    data.health === "down"
      ? "border-destructive/60"
      : data.health === "up"
        ? "border-green-600/50"
        : "border-amber-300/70";
  const bg = data.health === "down" ? "bg-destructive/5" : data.health === "up" ? "bg-green-50/70" : "bg-amber-50/50";

  return (
    <div className="min-w-[220px]">
      <Handle type="target" position={Position.Left} />
      <Card className={cn(border, bg)}>
        <CardHeader className="space-y-1 p-4">
          <div className="flex items-start justify-between gap-2">
            <div>
              <CardTitle className="text-sm">{data.title}</CardTitle>
              {data.subtitle ? <div className="text-xs text-muted-foreground">{data.subtitle}</div> : null}
            </div>
            <HealthBadge health={data.health} />
          </div>
        </CardHeader>
        <CardContent className="space-y-1 p-4 pt-0 text-xs text-muted-foreground">
          <div className="flex justify-between">
            <span>QPS</span>
            <span className="font-medium text-foreground">{fmt(data.qps)}</span>
          </div>
          <div className="flex justify-between">
            <span>错误率</span>
            <span className="font-medium text-foreground">
              {data.errRate == null || !Number.isFinite(data.errRate) ? "—" : `${(data.errRate * 100).toFixed(2)}%`}
            </span>
          </div>
          <div className="flex justify-between">
            <span>P95</span>
            <span className="font-medium text-foreground">
              {data.p95Ms == null || !Number.isFinite(data.p95Ms) ? "—" : `${data.p95Ms.toFixed(0)} ms`}
            </span>
          </div>
        </CardContent>
      </Card>
      <Handle type="source" position={Position.Right} />
    </div>
  );
});

