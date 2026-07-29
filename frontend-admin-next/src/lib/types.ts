export type RouteRow = {
  host: string;
  app_id: string;
  connector_endpoint: string;
  require_healthy_tunnel: boolean;
};

export type IntranetUpstreamRow = {
  app_id: string;
  upstream: string;
  scheme: "http" | "https";
};

export type PolicyRow = {
  id: string;
  effect: "ALLOW" | "DENY";
  subjects: string[];
  app_id?: string | null;
  path_prefix?: string | null;
  priority: number;
};

export type LoginResponse = {
  token: string;
  user: {
    id: string;
    username: string;
    roles: string[];
    roles_display?: string[];
    display_name?: string;
    title?: string;
  };
  expires_in_sec: number;
};

export type VerifyResponse = {
  active: boolean;
  user: {
    id: string;
    username: string;
    roles: string[];
    roles_display?: string[];
    display_name?: string;
    title?: string;
  } | null;
};

export type UserRow = {
  id: string;
  username: string;
  roles: string[];
  roles_display?: string[];
  display_name?: string;
  title?: string;
};

export type UpsertUserRequest = {
  id?: string;
  username: string;
  password?: string;
  roles: string[];
  display_name?: string;
  title?: string;
  enabled?: boolean;
};

export type AppMetricsPoint = {
  ts_minute: number;
  request_count: number;
  pv_count: number;
  uv_count: number;
  unique_ip_count: number;
  err4xx_count: number;
  err5xx_count: number;
  qps_avg: number;
  err4xx_rate: number;
  err5xx_rate: number;
};

export type AppMetricsSeries = {
  app_id: string;
  latest?: AppMetricsPoint | null;
  points: AppMetricsPoint[];
};

export type AppMetricsResponse = {
  generated_at_minute: number;
  series: AppMetricsSeries[];
  note: string;
};

export type AppTreeNode = {
  app_id: string;
  routes: RouteRow[];
  latest?: AppMetricsPoint | null;
};

export type AppRecord = {
  app_id: string;
  display_name: string;
  description: string;
  enabled: boolean;
};

export type ApiRouteRecord = {
  id: string;
  app_id: string;
  method: string;
  path: string;
  enabled: boolean;
  description: string;
};

export type IdentityProvider = {
  id: string;
  kind: string;
  issuer: string;
  client_id: string;
  client_secret: string;
  scopes: string;
  enabled: boolean;
};

export type GroupRoleMapping = {
  id: string;
  provider_id: string;
  external_group: string;
  local_roles_csv: string;
  enabled: boolean;
  priority: number;
};

export type AuditLog = {
  id: string;
  ts_ms: number;
  service: string;
  user_id: string;
  app_id: string;
  path: string;
  method: string;
  latency_ms: number;
  decision: string;
  result: string;
  trace_id: string;
  extra_json: string;
};

export type FaultEvent = {
  id: string;
  ts_ms: number;
  service: string;
  event_type: string;
  severity: string;
  path: string;
  method: string;
  latency_ms: number;
  baseline_ms: number;
  threshold_ms: number;
  status_code: number;
  result: string;
  trace_id: string;
  source: string;
  resolved_at_ms?: number | null;
  meta_json: string;
};

export type FaultInjectionToggle = {
  enabled: boolean;
  ttl_sec: number;
  expires_at_ms: number;
  mode: string;
  service: string;
  path_contains: string;
  delay_ms: number;
  status_code: number;
  hit_percent: number;
};

export type PublicSecurityOverview = {
  audit_count: number;
  fault_event_count: number;
  critical_fault_count: number;
  top_services: Array<{ service: string; count: number }>;
  note: string;
};

