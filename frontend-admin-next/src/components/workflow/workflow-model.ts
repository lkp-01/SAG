export type WorkflowServiceId =
  | "control-plane-admin"
  | "sag-auth"
  | "sag-policy"
  | "stealth-tunnel-agent"
  | "http-tunnel-bridge"
  | "sag-connector"
  | "apisix"
  | "mock-workload"
  | "zentinel"
  | "prometheus"
  | "grafana";

export const workflowServices: Array<{
  id: WorkflowServiceId;
  title: string;
  subtitle?: string;
  promJob?: string;
  kind: "mgmt" | "tunnel" | "dataplane" | "upstream" | "obs";
}> = [
  { id: "control-plane-admin", title: "control-plane-admin", subtitle: "管理面", promJob: "control-plane-admin", kind: "mgmt" },
  { id: "sag-auth", title: "sag-auth", subtitle: "认证/IAM", promJob: "sag-auth", kind: "mgmt" },
  { id: "sag-policy", title: "sag-policy", subtitle: "策略/PDP", promJob: "sag-policy", kind: "mgmt" },
  { id: "stealth-tunnel-agent", title: "stealth-tunnel-agent", subtitle: "隧道 Agent", promJob: "stealth-tunnel-agent", kind: "tunnel" },
  { id: "http-tunnel-bridge", title: "http-tunnel-bridge", subtitle: "HTTP→gRPC", promJob: "http-tunnel-bridge", kind: "tunnel" },
  { id: "sag-connector", title: "sag-connector", subtitle: "内网连接器", promJob: "sag-connector", kind: "tunnel" },
  { id: "zentinel", title: "zentinel", subtitle: "数据面入口", promJob: "zentinel-proxy", kind: "dataplane" },
  { id: "apisix", title: "APISIX", subtitle: "南向 L7", kind: "dataplane" },
  { id: "mock-workload", title: "mock-workload", subtitle: "上游/工作负载", kind: "upstream" },
  { id: "prometheus", title: "Prometheus", subtitle: "指标存储", promJob: "prometheus", kind: "obs" },
  { id: "grafana", title: "Grafana", subtitle: "指标展示", kind: "obs" }
];

