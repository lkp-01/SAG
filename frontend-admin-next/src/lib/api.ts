"use client";

import { getToken } from "@/lib/session";
import type {
  AppMetricsResponse,
  AppRecord,
  AppTreeNode,
  ApiRouteRecord,
  AuditLog,
  FaultEvent,
  FaultInjectionToggle,
  GroupRoleMapping,
  IdentityProvider,
  IntranetUpstreamRow,
  LoginResponse,
  PolicyRow,
  RouteRow,
  PublicSecurityOverview,
  UpsertUserRequest,
  UserRow,
  VerifyResponse
} from "@/lib/types";
import { getPublicReadonlyToken } from "@/components/auth/PublicReadOnlyGate";
import { fetchDataplaneWithQueueHandling } from "@/lib/bridge-dataplane";

const FAILOVER_METHODS = new Set(["GET", "HEAD"]);

function parseTargets(csv?: string | null): string[] {
  if (!csv) return [];
  return csv
    .split(",")
    .map((x) => x.trim().replace(/\/+$/, ""))
    .filter((x) => x.length > 0);
}

function targetsForUrl(url: string): string[] {
  const mappings: Array<{ prefix: string; envCsv?: string; envSingle?: string }> = [
    {
      prefix: "/api-control",
      envCsv: process.env.NEXT_PUBLIC_CONTROL_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_CONTROL_PROXY_TARGET
    },
    {
      prefix: "/api-auth",
      envCsv: process.env.NEXT_PUBLIC_AUTH_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_AUTH_PROXY_TARGET
    },
    {
      prefix: "/api-policy",
      envCsv: process.env.NEXT_PUBLIC_POLICY_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_POLICY_PROXY_TARGET
    },
    {
      prefix: "/api-bridge",
      envCsv: process.env.NEXT_PUBLIC_BRIDGE_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_BRIDGE_PROXY_TARGET
    },
    {
      prefix: "/api-zentinel",
      envCsv: process.env.NEXT_PUBLIC_ZENTINEL_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_ZENTINEL_PROXY_TARGET
    },
    {
      prefix: "/api-prom",
      envCsv: process.env.NEXT_PUBLIC_PROM_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_PROM_PROXY_TARGET
    },
    {
      prefix: "/api-grafana",
      envCsv: process.env.NEXT_PUBLIC_GRAFANA_PROXY_TARGETS,
      envSingle: process.env.NEXT_PUBLIC_GRAFANA_PROXY_TARGET
    }
  ];
  const m = mappings.find((x) => url.startsWith(x.prefix));
  if (!m) return [];
  const targets = parseTargets(m.envCsv);
  if (targets.length > 0) return targets;
  return parseTargets(m.envSingle);
}

function candidateUrls(url: string): string[] {
  if (!url.startsWith("/")) return [url];
  const targets = targetsForUrl(url);
  if (targets.length === 0) return [url];
  return targets.map((base) => `${base}${url}`);
}

function shouldFailover(method: string, status?: number, error?: unknown): boolean {
  if (!FAILOVER_METHODS.has(method)) return false;
  if (typeof status === "number") return status >= 500 || status === 429;
  return Boolean(error);
}

async function request<T>(url: string, init?: RequestInit & { allowFailoverForWrite?: boolean }): Promise<T> {
  const headers = new Headers(init?.headers ?? {});
  const method = (init?.method ?? "GET").toUpperCase();
  if (method !== "GET" && method !== "HEAD") {
    headers.set("Content-Type", "application/json");
  }
  const token = getToken();
  if (token) headers.set("Authorization", `Bearer ${token}`);
  const canFailover = FAILOVER_METHODS.has(method) || Boolean(init?.allowFailoverForWrite);
  const urls = canFailover ? candidateUrls(url) : [url];
  let lastErr = "";

  for (let i = 0; i < urls.length; i += 1) {
    const u = urls[i];
    try {
      const res = await fetch(u, {
        ...init,
        headers,
        // no-store on every request causes visible jitter in UI; default caching reduces repeated fetch churn.
        cache: init?.cache ?? "default"
      });
      const text = await res.text();
      if (!res.ok) {
        lastErr = `${res.status} ${res.statusText}${text ? `: ${text}` : ""}`;
        if (i < urls.length - 1 && shouldFailover(method, res.status)) {
          continue;
        }
        throw new Error(lastErr);
      }
      if (!text) return undefined as T;
      return JSON.parse(text) as T;
    } catch (e) {
      lastErr = String(e);
      if (i < urls.length - 1 && shouldFailover(method, undefined, e)) {
        continue;
      }
      throw e;
    }
  }
  throw new Error(lastErr || "request failed");
}

async function publicReadonlyRequest<T>(url: string): Promise<T> {
  const headers = new Headers();
  const token = getPublicReadonlyToken();
  if (token) headers.set("x-sag-readonly-token", token);
  const res = await fetch(url, { headers, cache: "no-store" });
  const text = await res.text();
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ""}`);
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

export const controlApi = {
  listRoutes: () => request<RouteRow[]>("/api-control/api/v1/agent/routes", { method: "GET" }),
  listAppsTree: (withLatest = true) =>
    request<AppTreeNode[]>(`/api-control/api/v1/apps/tree?with_latest=${withLatest ? "true" : "false"}`, { method: "GET" }),
  listApps: () => request<AppRecord[]>("/api-control/api/v1/apps", { method: "GET" }),
  upsertApp: (row: AppRecord) =>
    request<void>("/api-control/api/v1/apps", {
      method: "POST",
      body: JSON.stringify(row)
    }),
  deleteApp: (appId: string) =>
    request<void>(`/api-control/api/v1/apps/${encodeURIComponent(appId)}`, {
      method: "DELETE"
    }),
  listApiRoutes: (appId?: string) =>
    request<ApiRouteRecord[]>(
      `/api-control/api/v1/api-routes${appId ? `?app_id=${encodeURIComponent(appId)}` : ""}`,
      { method: "GET", cache: "no-store" }
    ),
  upsertApiRoute: (row: ApiRouteRecord) =>
    request<void>("/api-control/api/v1/api-routes", {
      method: "POST",
      body: JSON.stringify(row)
    }),
  deleteApiRoute: (id: string) =>
    request<void>(`/api-control/api/v1/api-routes/${encodeURIComponent(id)}`, {
      method: "DELETE"
    }),
  listAuditLogs: (q?: {
    from_ts_ms?: number;
    to_ts_ms?: number;
    user_id?: string;
    app_id?: string;
    service?: string;
    result?: string;
    decision?: string;
    path_contains?: string;
    department?: string;
    limit?: number;
  }) => {
    const p = new URLSearchParams();
    if (q?.from_ts_ms) p.set("from_ts_ms", String(q.from_ts_ms));
    if (q?.to_ts_ms) p.set("to_ts_ms", String(q.to_ts_ms));
    if (q?.user_id) p.set("user_id", q.user_id);
    if (q?.app_id) p.set("app_id", q.app_id);
    if (q?.service) p.set("service", q.service);
    if (q?.result) p.set("result", q.result);
    if (q?.decision) p.set("decision", q.decision);
    if (q?.path_contains) p.set("path_contains", q.path_contains);
    if (q?.department) p.set("department", q.department);
    if (q?.limit) p.set("limit", String(q.limit));
    const qs = p.toString();
    return request<AuditLog[]>(`/api-control/api/v1/audit/logs${qs ? `?${qs}` : ""}`, { method: "GET", cache: "no-store" });
  },
  postAuditLog: (row: AuditLog) =>
    request<void>("/api-control/api/v1/audit/logs", { method: "POST", body: JSON.stringify(row) }),
  listFaultEvents: (q?: {
    from_ts_ms?: number;
    to_ts_ms?: number;
    service?: string;
    event_type?: string;
    severity?: string;
    result?: string;
    source?: string;
    limit?: number;
  }) => {
    const p = new URLSearchParams();
    if (q?.from_ts_ms) p.set("from_ts_ms", String(q.from_ts_ms));
    if (q?.to_ts_ms) p.set("to_ts_ms", String(q.to_ts_ms));
    if (q?.service) p.set("service", q.service);
    if (q?.event_type) p.set("event_type", q.event_type);
    if (q?.severity) p.set("severity", q.severity);
    if (q?.result) p.set("result", q.result);
    if (q?.source) p.set("source", q.source);
    if (q?.limit) p.set("limit", String(q.limit));
    const qs = p.toString();
    return request<FaultEvent[]>(`/api-control/api/v1/fault-events${qs ? `?${qs}` : ""}`, { method: "GET", cache: "no-store" });
  },
  postFaultEvent: (row: FaultEvent) =>
    request<void>("/api-control/api/v1/fault-events", { method: "POST", body: JSON.stringify(row) }),
  getFaultInjection: () => request<FaultInjectionToggle>("/api-control/api/v1/fault-injection", { method: "GET", cache: "no-store" }),
  updateFaultInjection: (patch: Partial<FaultInjectionToggle>) =>
    request<FaultInjectionToggle>("/api-control/api/v1/fault-injection", { method: "PUT", body: JSON.stringify(patch), allowFailoverForWrite: true }),
  publicAuditLogs: () => publicReadonlyRequest<AuditLog[]>("/api-control/api/public/security/audit"),
  publicFaultEvents: () => publicReadonlyRequest<FaultEvent[]>("/api-control/api/public/security/fault-events"),
  publicSecurityOverview: () => publicReadonlyRequest<PublicSecurityOverview>("/api-control/api/public/security/overview"),
  getAppsMetrics: (appId?: string, rangeMin = 60) =>
    request<AppMetricsResponse>(
      `/api-control/api/v1/apps/metrics?range_min=${encodeURIComponent(String(rangeMin))}${appId ? `&app_id=${encodeURIComponent(appId)}` : ""}`,
      { method: "GET", cache: "no-store" }
    ),
  upsertRoute: (row: RouteRow) =>
    request<void>("/api-control/api/v1/agent/routes", {
      method: "POST",
      body: JSON.stringify(row)
    }),
  deleteRoute: (host: string) =>
    request<void>(`/api-control/api/v1/agent/routes/${encodeURIComponent(host)}`, {
      method: "DELETE"
    }),
  upsertIntranet: (row: IntranetUpstreamRow) =>
    request<void>(`/api-control/api/v1/agent/intranet-upstreams?app_id=${encodeURIComponent(row.app_id)}`, {
      method: "PUT",
      body: JSON.stringify({ upstream: row.upstream, scheme: row.scheme })
    })
};

export const authApi = {
  login: (username: string, password: string) =>
    request<LoginResponse>("/api-auth/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password })
    }),
  verify: (token: string) =>
    request<VerifyResponse>("/api-auth/api/v1/auth/verify", {
      method: "POST",
      body: JSON.stringify({ token })
    }),
  listUsers: () => request<UserRow[]>("/api-auth/api/v1/users", { method: "GET" }),
  upsertUser: (row: UpsertUserRequest) =>
    request<UserRow>("/api-auth/api/v1/users", {
      method: "POST",
      body: JSON.stringify(row)
    }),
  deleteUser: (username: string) =>
    request<void>(`/api-auth/api/v1/users/${encodeURIComponent(username)}`, {
      method: "DELETE"
    }),
  listIdentityProviders: () =>
    request<IdentityProvider[]>("/api-auth/api/v1/identity/providers", { method: "GET", cache: "no-store" }),
  upsertIdentityProvider: (row: IdentityProvider) =>
    request<void>("/api-auth/api/v1/identity/providers", { method: "POST", body: JSON.stringify(row) }),
  deleteIdentityProvider: (id: string) =>
    request<void>(`/api-auth/api/v1/identity/providers/${encodeURIComponent(id)}`, { method: "DELETE" }),
  listGroupRoleMappings: (providerId?: string) =>
    request<GroupRoleMapping[]>(
      `/api-auth/api/v1/identity/mappings${providerId ? `?provider_id=${encodeURIComponent(providerId)}` : ""}`,
      { method: "GET", cache: "no-store" }
    ),
  upsertGroupRoleMapping: (row: GroupRoleMapping) =>
    request<void>("/api-auth/api/v1/identity/mappings", { method: "POST", body: JSON.stringify(row) }),
  deleteGroupRoleMapping: (id: string) =>
    request<void>(`/api-auth/api/v1/identity/mappings/${encodeURIComponent(id)}`, { method: "DELETE" })
};

export const policyApi = {
  list: () => request<PolicyRow[]>("/api-policy/api/v1/policies", { method: "GET" }),
  upsert: (row: PolicyRow) =>
    request<void>("/api-policy/api/v1/policies", {
      method: "POST",
      body: JSON.stringify(row)
    }),
  delete: (id: string) =>
    request<void>(`/api-policy/api/v1/policies/${encodeURIComponent(id)}`, {
      method: "DELETE"
    })
};

export async function health(url: string): Promise<string> {
  const token = getToken();
  const res = await fetch(url, {
    headers: token ? { Authorization: `Bearer ${token}` } : undefined
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.text();
}

export async function dataPlaneProbe(
  baseUrl: string,
  path: string,
  appId: string,
  opts?: { onQueued?: () => void }
) {
  const token = getToken();
  const url = `${baseUrl}${path}`;
  const headers: Record<string, string> = {
    "x-sag-app-id": appId,
    "x-sag-user-id": "ui-admin",
    "x-sag-user-roles": "admin",
    ...(token ? { Authorization: `Bearer ${token}` } : {})
  };
  const viaBridge =
    baseUrl.includes("/api-bridge") || baseUrl.includes("/api-zentinel");
  const res = viaBridge
    ? await fetchDataplaneWithQueueHandling(
        url,
        { method: "GET", headers },
        { maxWaitMs: 90_000, onQueued: opts?.onQueued }
      )
    : await fetch(url, { method: "GET", headers });
  const body = await res.text();
  return { status: res.status, body };
}

