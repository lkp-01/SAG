import { getToken } from "@/lib/session";
import type {
  IntranetUpstreamRow,
  LoginResponse,
  PolicyRow,
  RouteRow,
  UpsertUserRequest,
  UserRow,
  VerifyResponse,
} from "@/lib/types";

export const CONTROL_BASE = import.meta.env.VITE_CONTROL_BASE ?? "/api-control";
export const POLICY_BASE = import.meta.env.VITE_POLICY_BASE ?? "/api-policy";
export const AUTH_BASE = import.meta.env.VITE_AUTH_BASE ?? "/api-auth";
export const BRIDGE_BASE = import.meta.env.VITE_BRIDGE_BASE ?? "/api-bridge";
export const ZENTINEL_BASE = import.meta.env.VITE_ZENTINEL_BASE ?? "/api-zentinel";

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers ?? {});
  const method = (init?.method ?? "GET").toUpperCase();
  if (method !== "GET" && method !== "HEAD") {
    headers.set("Content-Type", "application/json");
  }
  const token = getToken();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  const res = await fetch(url, { ...init, headers });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ""}`);
  }
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

export async function health(url: string): Promise<string> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.text();
}

export const controlApi = {
  listRoutes: () => request<RouteRow[]>(`${CONTROL_BASE}/api/v1/agent/routes`, { method: "GET" }),
  upsertRoute: (row: RouteRow) =>
    request<void>(`${CONTROL_BASE}/api/v1/agent/routes`, {
      method: "POST",
      body: JSON.stringify(row),
    }),
  deleteRoute: (host: string) =>
    request<void>(`${CONTROL_BASE}/api/v1/agent/routes/${encodeURIComponent(host)}`, {
      method: "DELETE",
    }),
  upsertIntranet: (row: IntranetUpstreamRow) =>
    request<void>(
      `${CONTROL_BASE}/api/v1/agent/intranet-upstreams?app_id=${encodeURIComponent(row.app_id)}`,
      {
        method: "PUT",
        body: JSON.stringify({ upstream: row.upstream, scheme: row.scheme }),
      }
    ),
};

export const policyApi = {
  list: () => request<PolicyRow[]>(`${POLICY_BASE}/api/v1/policies`, { method: "GET" }),
  upsert: (row: PolicyRow) =>
    request<void>(`${POLICY_BASE}/api/v1/policies`, {
      method: "POST",
      body: JSON.stringify(row),
    }),
  delete: (id: string) =>
    request<void>(`${POLICY_BASE}/api/v1/policies/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
};

export const authApi = {
  login: (username: string, password: string) =>
    request<LoginResponse>(`${AUTH_BASE}/api/v1/auth/login`, {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  verify: (token: string) =>
    request<VerifyResponse>(`${AUTH_BASE}/api/v1/auth/verify`, {
      method: "POST",
      body: JSON.stringify({ token }),
    }),
  listUsers: () => request<UserRow[]>(`${AUTH_BASE}/api/v1/users`, { method: "GET" }),
  upsertUser: (row: UpsertUserRequest) =>
    request<UserRow>(`${AUTH_BASE}/api/v1/users`, {
      method: "POST",
      body: JSON.stringify(row),
    }),
  deleteUser: (username: string) =>
    request<void>(`${AUTH_BASE}/api/v1/users/${encodeURIComponent(username)}`, {
      method: "DELETE",
    }),
};

export async function dataPlaneProbe(baseUrl: string, path: string, appId: string) {
  const res = await fetch(`${baseUrl}${path}`, {
    method: "GET",
    headers: {
      "x-sag-app-id": appId,
      "x-sag-user-id": "ui-admin",
      "x-sag-user-roles": "admin",
      ...(getToken() ? { Authorization: `Bearer ${getToken()}` } : {}),
    },
  });
  const body = await res.text();
  return { status: res.status, body };
}
